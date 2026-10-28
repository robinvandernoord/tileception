//! XDG session autostart discovery, separate from application-menu discovery.

use std::{collections::HashSet, env, fs, path::PathBuf};

use crate::{command_line, desktop, App};

/// An enabled entry, including its working directory and optional startup delay.
#[derive(Clone, Debug)]
pub struct AutostartEntry {
    pub app: App,
    pub file: PathBuf,
    pub directory: Option<PathBuf>,
    pub delay_seconds: u32,
}

impl AutostartEntry {
    /// A safely quoted shell command using the same terminal prefix as the launcher.
    /// Sleeping happens in the child, so delayed entries never block the session.
    pub fn command_line(&self, terminal: &str) -> String {
        let mut line = String::new();
        if self.delay_seconds > 0 {
            line.push_str(&format!("sleep {} && ", self.delay_seconds));
        }
        if let Some(directory) = &self.directory {
            line.push_str(&format!("cd {} && ", desktop::shell_quote(&directory.to_string_lossy())));
        }
        line.push_str("exec ");
        line.push_str(&command_line(&self.app, terminal));
        line
    }
}

/// Scan the user's configuration first, then system directories in XDG order.
pub fn autostart_entries() -> Vec<AutostartEntry> {
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let config_dirs =
        env::var_os("XDG_CONFIG_DIRS").filter(|value| !value.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    let dirs = config_home
        .into_iter()
        .chain(env::split_paths(&config_dirs).filter(|dir| dir.is_absolute()))
        .map(|dir| dir.join("autostart"))
        .collect::<Vec<_>>();
    let desktop =
        env::var("XDG_CURRENT_DESKTOP").ok().filter(|value| !value.is_empty()).unwrap_or_else(|| "Tileception".into());
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| env::var(key).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default();
    let path = env::var_os("PATH").map(|value| env::split_paths(&value).collect::<Vec<_>>()).unwrap_or_default();
    scan_autostart(&dirs, &desktop, &locale, &path)
}

/// Directories are ordered from highest to lowest priority. A user entry masks
/// the system entry even when disabled, invalid, or excluded from this desktop.
pub fn scan_autostart(dirs: &[PathBuf], desktop: &str, locale: &str, path: &[PathBuf]) -> Vec<AutostartEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for dir in dirs {
        let Ok(files) = fs::read_dir(dir) else { continue };
        let mut files = files.filter_map(Result::ok).map(|entry| entry.path()).collect::<Vec<_>>();
        files.sort();
        for file in files {
            if file.extension().is_none_or(|ext| ext != "desktop") || !file.is_file() {
                continue;
            }
            let Some(id) = file.file_name().and_then(|name| name.to_str()) else { continue };
            if !seen.insert(id.to_owned()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&file) else { continue };
            let mut groups = desktop::groups(&text);
            let Some(values) = groups.get_mut("Desktop Entry") else { continue };
            if values.get("Hidden").is_some_and(|value| value == "true")
                || values.get("X-GNOME-Autostart-enabled").is_some_and(|value| value == "false")
            {
                continue;
            }
            // NoDisplay only hides an entry from menus, not session startup.
            values.remove("NoDisplay");
            if values.get("TryExec").is_some_and(String::is_empty) {
                values.remove("TryExec");
            }
            let directory = values
                .get("Path")
                .map(|value| desktop::unescape(value))
                .filter(|value| !value.is_empty())
                .map(PathBuf::from);
            let delay_seconds = values.get("X-GNOME-Autostart-Delay").and_then(|value| value.parse().ok()).unwrap_or(0);
            let Some(mut app) = desktop::parse_groups(&groups, id, desktop, locale, path).filter(|app| !app.hidden)
            else {
                continue;
            };
            let Some(exec) = groups.get("Desktop Entry").and_then(|values| values.get("Exec")) else { continue };
            let Some((args, extra_args_at)) = desktop::exec_line_with_context(
                &desktop::unescape(exec),
                Some((&app.name, &file, app.icon.as_deref())),
            ) else {
                continue;
            };
            app.exec = args.iter().map(|arg| desktop::shell_quote(arg)).collect::<Vec<_>>().join(" ");
            app.args = args;
            app.extra_args_at = extra_args_at;
            entries.push(AutostartEntry { app, file, directory, delay_seconds });
        }
    }
    entries
}
