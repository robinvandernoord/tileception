//! Chosen applications: the default for a MIME type from the XDG MIME
//! Applications spec (`mimeapps.list`), and the preferred terminal from the
//! proposed Default Terminal Execution spec (`xdg-terminals.list`).

use std::{env, fs, path::PathBuf};

use crate::{data_dirs, App, AppIndex};

impl AppIndex {
    /// The application that opens `mime`: the first installed one the
    /// user's or system's `mimeapps.list` names, else one whose entry says
    /// it opens the type (shown entries first).
    pub fn default_for(&self, mime: &str) -> Option<&App> {
        self.default_in(mime, &mimeapps_files(&desktops(), &config_dirs(), &data_dirs()))
    }

    /// The terminal the user prefers in `xdg-terminals.list`, when one
    /// listed there is installed.
    pub fn preferred_terminal(&self) -> Option<&App> {
        self.terminal_in(&terminal_files(&desktops(), &config_dirs(), &data_dirs()))
    }

    /// Shown entries in the `TerminalEmulator` category.
    pub fn terminal_emulators(&self) -> impl Iterator<Item = &App> {
        self.apps
            .values()
            .filter(|app| !app.hidden && app.categories.iter().any(|category| category == "TerminalEmulator"))
    }

    pub(crate) fn default_in(&self, mime: &str, files: &[PathBuf]) -> Option<&App> {
        let mut removed = Vec::new();
        let mut candidates = Vec::new();
        for file in files {
            let Ok(text) = fs::read_to_string(file) else { continue };
            for (group, ids) in associations(&text, mime) {
                match group.as_str() {
                    "Removed Associations" => removed.extend(ids),
                    "Default Applications" | "Added Associations" => {
                        candidates.extend(ids.into_iter().filter(|id| !removed.contains(id)));
                    }
                    _ => {}
                }
            }
        }
        candidates.iter().find_map(|id| self.apps.get(id)).or_else(|| {
            let opens = |app: &&App| app.mime_types.iter().any(|opened| opened == mime) && !removed.contains(&app.id);
            let mut found = self.apps.values().filter(opens);
            let first = found.next()?;
            if first.hidden {
                found.find(|app| !app.hidden).or(Some(first))
            } else {
                Some(first)
            }
        })
    }

    pub(crate) fn terminal_in(&self, files: &[PathBuf]) -> Option<&App> {
        files.iter().filter_map(|file| fs::read_to_string(file).ok()).find_map(|text| {
            text.lines().find_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with(['#', '-']) {
                    return None;
                }
                // `+id` asks for it explicitly; `id:action` names an action.
                let id = line.trim_start_matches('+').split(':').next()?;
                self.apps.get(id)
            })
        })
    }
}

/// The desktop entry ids for `mime`, by group, from one `mimeapps.list`.
fn associations(text: &str, mime: &str) -> Vec<(String, Vec<String>)> {
    let mut group = String::new();
    let mut found = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|line| line.strip_suffix(']')) {
            group = name.to_owned();
        } else if let Some((key, value)) = line.split_once('=') {
            if key.trim() == mime {
                let ids = value.split(';').map(str::trim).filter(|id| !id.is_empty()).map(String::from).collect();
                found.push((group.clone(), ids));
            }
        }
    }
    found
}

/// `XDG_CURRENT_DESKTOP`'s names in lower case, for `<desktop>-` files.
fn desktops() -> Vec<String> {
    env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|name| !name.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The user's config folder, then the system's.
fn config_dirs() -> Vec<PathBuf> {
    let home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let system = env::var_os("XDG_CONFIG_DIRS").filter(|value| !value.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    home.into_iter().chain(env::split_paths(&system).filter(|dir| dir.is_absolute())).collect()
}

/// `mimeapps.list` files in the spec's order: desktop-specific before
/// general, config folders before data folders.
pub(crate) fn mimeapps_files(desktops: &[String], config: &[PathBuf], data: &[PathBuf]) -> Vec<PathBuf> {
    let named = |dir: PathBuf| {
        desktops
            .iter()
            .map(|desktop| dir.join(format!("{desktop}-mimeapps.list")))
            .chain([dir.join("mimeapps.list")])
            .collect::<Vec<_>>()
    };
    config
        .iter()
        .cloned()
        .flat_map(named)
        .chain(data.iter().map(|dir| dir.join("applications")).flat_map(named))
        .collect::<Vec<_>>()
}

/// `xdg-terminals.list` files in the proposal's order.
pub(crate) fn terminal_files(desktops: &[String], config: &[PathBuf], data: &[PathBuf]) -> Vec<PathBuf> {
    let named = |dir: PathBuf| {
        desktops
            .iter()
            .map(|desktop| dir.join(format!("{desktop}-xdg-terminals.list")))
            .chain([dir.join("xdg-terminals.list")])
            .collect::<Vec<_>>()
    };
    config
        .iter()
        .cloned()
        .flat_map(named)
        .chain(data.iter().map(|dir| dir.join("xdg-terminal-exec")).flat_map(named))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions")]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("tileception-defaults-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("data/applications")).unwrap();
        fs::create_dir_all(dir.join("config")).unwrap();
        let entry = |id: &str, extra: &str| {
            let text = format!("[Desktop Entry]\nType=Application\nName={id}\nExec={id} %F\n{extra}");
            fs::write(dir.join(format!("data/applications/{id}.desktop")), text).unwrap();
        };
        entry("editor", "MimeType=text/plain;\n");
        entry("other", "MimeType=text/plain;text/markdown;\n");
        entry("foot", "Categories=System;TerminalEmulator;\n");
        entry("kitty", "Categories=System;TerminalEmulator;\n");
        dir
    }

    fn index(dir: &std::path::Path) -> AppIndex {
        AppIndex::scan_with_options(&[dir.join("data/applications")], "Tileception", "C", &[])
    }

    #[test]
    fn the_listed_default_wins_over_what_entries_claim() {
        let dir = folder("mime");
        let index = index(&dir);
        let files = mimeapps_files(&["sway".into()], &[dir.join("config")], &[dir.join("data")]);
        assert_eq!(files[0], dir.join("config/sway-mimeapps.list"));
        // Without a list, an entry that opens the type.
        assert!(index.default_in("text/plain", &files).is_some());
        assert!(index.default_in("image/png", &files).is_none());
        fs::write(
            dir.join("config/mimeapps.list"),
            "[Default Applications]\ntext/plain=missing.desktop;other.desktop;\n",
        )
        .unwrap();
        assert_eq!(index.default_in("text/plain", &files).unwrap().id, "other.desktop");
        // A desktop-specific list comes first, and removals are respected.
        fs::write(
            dir.join("config/sway-mimeapps.list"),
            "[Removed Associations]\ntext/plain=other.desktop;\n[Default Applications]\ntext/plain=editor.desktop\n",
        )
        .unwrap();
        assert_eq!(index.default_in("text/plain", &files).unwrap().id, "editor.desktop");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminals_follow_the_preference_list() {
        let dir = folder("terminals");
        let index = index(&dir);
        let files = terminal_files(&[], &[dir.join("config")], &[dir.join("data")]);
        assert!(index.terminal_in(&files).is_none());
        assert_eq!(index.terminal_emulators().count(), 2);
        fs::write(
            dir.join("config/xdg-terminals.list"),
            "# mine\n-foot.desktop\nmissing.desktop\n+kitty.desktop:new-window\n",
        )
        .unwrap();
        assert_eq!(index.terminal_in(&files).unwrap().id, "kitty.desktop");
        fs::remove_dir_all(dir).unwrap();
    }
}
