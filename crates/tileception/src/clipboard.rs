//! Optional persistence for the regular clipboard, managed with the config.

use std::{process::Child, time::Duration};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::Tileception;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClipboardConfig {
    pub persist: bool,
}

#[derive(Default)]
pub struct Clipboard {
    child: Option<Child>,
}

impl Clipboard {
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Kill only our helper, then reap it before another can start.
            if let Err(err) = child.kill() {
                tracing::warn!("could not stop clipboard persistence: {err}");
            }
            if let Err(err) = child.wait() {
                tracing::warn!("could not reap clipboard persistence: {err}");
            }
        }
    }

    fn check(&mut self) {
        let Some(child) = self.child.as_mut() else { return };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.child = None;
                tracing::error!("clipboard persistence exited ({status}); reload the config to retry");
            }
            Ok(None) => {}
            Err(err) => tracing::error!("could not check clipboard persistence: {err}"),
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Tileception {
    pub fn init_clipboard(&mut self) {
        self.apply_clipboard();
        self.loop_handle
            .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
                state.clipboard.check();
                TimeoutAction::ToDuration(Duration::from_secs(1))
            })
            .expect("clipboard helper watcher");
    }

    pub fn apply_clipboard(&mut self) {
        self.clipboard.check();
        if !self.config.file.clipboard.persist {
            self.clipboard.stop();
            return;
        }
        // The greeter never retains clipboard contents.
        if self.greeter.is_some() || self.clipboard.child.is_some() {
            return;
        }
        // Installed next to the compositor, with a private name so installation
        // never replaces a separately managed wl-clip-persist. Source-only
        // development builds can still use an upstream helper from PATH.
        let helper = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|directory| directory.join("tileception-clip-persist")))
            .filter(|path| path.is_file());
        let program = helper.as_deref().map_or_else(|| "wl-clip-persist".into(), |path| path.to_string_lossy());
        // Sensitive copies (see `launcher::PASSWORD_HINT`) are left alone.
        let arguments = [
            "--clipboard".into(),
            "regular".into(),
            "--all-mime-type-regex".into(),
            format!("^(?!{}$)", crate::launcher::PASSWORD_HINT),
        ];
        match self.command(&program, &arguments).spawn() {
            Ok(child) => {
                self.clipboard.child = Some(child);
                tracing::info!("clipboard persistence started");
            }
            Err(err) => tracing::error!(
                "could not start clipboard persistence ({program}): {err}; rebuild with make build or reinstall Tileception, then reload the config"
            ),
        }
    }
}
