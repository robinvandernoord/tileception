//! Save topology and match reopened windows by app id and occurrence, without launching apps.

use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use tileception_core::{Placement, WindowId, world::LayoutSnapshot};

use crate::Tileception;

#[derive(Serialize, Deserialize)]
struct WindowRecord {
    id: WindowId,
    app: String,
}

#[derive(Serialize, Deserialize)]
struct SessionLayout {
    version: u32,
    layout: LayoutSnapshot,
    windows: Vec<WindowRecord>,
}

#[derive(Default)]
pub struct Persistence {
    saved: Option<SessionLayout>,
    matched: HashMap<WindowId, WindowId>,
}

impl Persistence {
    pub fn load() -> Self {
        let saved = std::fs::read_to_string(layout_path())
            .ok()
            .filter(|s| s.len() <= 1024 * 1024)
            .and_then(|text| toml::from_str::<SessionLayout>(&text).ok())
            .filter(|s| s.version == 1);
        Self { saved, matched: HashMap::new() }
    }
}

impl Tileception {
    pub fn restore_window_layout(&mut self, id: WindowId) {
        if !self.config.file.general.restore_layout {
            return;
        }
        self.persistence.matched.retain(|_, live| self.windows.contains_key(live));
        let Some(saved) = &self.persistence.saved else {
            self.persist_layout();
            return;
        };
        let app = self.windows.get(&id).map(crate::state::app_id).unwrap_or_default();
        if !app.is_empty()
            && let Some(record) =
                saved.windows.iter().find(|r| r.app == app && !self.persistence.matched.contains_key(&r.id))
        {
            self.persistence.matched.insert(record.id, id);
        }
        let extras: Vec<_> = self
            .world
            .tree()
            .windows_in(self.world.tree().root())
            .into_iter()
            .filter(|live| !self.persistence.matched.values().any(|id| id == live))
            .map(|id| (id, self.world.float_rect(id)))
            .collect();
        let now = self.now();
        if !self.world.restore_layout(&saved.layout, &self.persistence.matched, now) {
            tracing::warn!("saved layout is invalid; keeping the current layout");
            self.persistence.saved = None;
            return;
        }
        for (extra, float) in extras {
            self.world.add_window(extra, if float.is_some() { Placement::Float } else { Placement::Dock }, now);
            if let Some(rect) = float {
                self.world.set_float_rect(extra, rect);
            }
        }
        self.world.focus_window(id, now);
        let complete = self.persistence.matched.len() >= saved.windows.len();
        if complete {
            self.persist_layout();
        }
    }

    pub fn persist_layout(&mut self) {
        if !self.config.file.general.restore_layout {
            return;
        }
        // Empty desktops and application teardown at logout must not erase the layout.
        if self.nested || self.headless.is_some() && std::env::var_os("TILECEPTION_PERSIST_HEADLESS").is_none() {
            return;
        }
        let mut ids = self.world.tree().windows_in(self.world.tree().root());
        ids.sort();
        if ids.is_empty() {
            return;
        }
        let windows = ids
            .into_iter()
            .map(|id| WindowRecord { id, app: self.windows.get(&id).map(crate::state::app_id).unwrap_or_default() })
            .collect();
        let snapshot = SessionLayout { version: 1, layout: self.world.snapshot(), windows };
        let result = (|| {
            let data = toml::to_string(&snapshot).map_err(std::io::Error::other)?;
            let path = layout_path();
            std::fs::create_dir_all(path.parent().expect("state directory"))?;
            let temporary = path.with_extension("toml.tmp");
            std::fs::write(&temporary, data)?;
            std::fs::rename(temporary, path)
        })();
        if let Err(error) = result {
            tracing::warn!("could not save layout: {error}");
        } else {
            self.persistence.saved = None;
            self.persistence.matched.clear();
        }
    }
}

fn layout_path() -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception/layout.toml")
}
