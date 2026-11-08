//! Chosen applications: the default for a MIME type from the XDG MIME
//! Applications spec (`mimeapps.list`), and the preferred terminal from the
//! proposed Default Terminal Execution spec (`xdg-terminals.list`).

use std::{collections::BTreeSet, env, fs, path::PathBuf};

use crate::{data_dirs, App, AppIndex};

/// The association files, read once for looking up many types.
pub struct Associations {
    /// `mimeapps.list` files, in lookup order.
    lists: Vec<String>,
    /// `mimeinfo.cache` files, the user's first.
    caches: Vec<String>,
}

impl Associations {
    /// The user's and the system's files, as they are now.
    pub fn load() -> Self {
        Self::from_files(
            &mimeapps_files(&desktops(), &config_dirs(), &data_dirs()),
            &data_dirs().into_iter().map(|dir| dir.join("applications/mimeinfo.cache")).collect::<Vec<_>>(),
        )
    }

    pub(crate) fn from_files(lists: &[PathBuf], caches: &[PathBuf]) -> Self {
        let read = |files: &[PathBuf]| files.iter().filter_map(|file| fs::read_to_string(file).ok()).collect();
        Self { lists: read(lists), caches: read(caches) }
    }
}

impl AppIndex {
    /// The application that opens `mime`, as `xdg-mime query default`
    /// finds it: the first installed one `mimeapps.list` names, else the
    /// first `mimeinfo.cache` names, else one whose entry says it opens the
    /// type (shown entries first).
    pub fn default_for(&self, mime: &str) -> Option<&App> {
        self.default_with(mime, &Associations::load())
    }

    /// [`AppIndex::default_for`] with files already read.
    pub fn default_with(&self, mime: &str, files: &Associations) -> Option<&App> {
        let mut removed = Vec::new();
        let mut candidates = Vec::new();
        for text in &files.lists {
            for (group, ids) in associations(text, mime) {
                match group.as_str() {
                    "Removed Associations" => removed.extend(ids),
                    "Default Applications" | "Added Associations" => {
                        candidates.extend(ids.into_iter().filter(|id| !removed.contains(id)));
                    }
                    _ => {}
                }
            }
        }
        for text in &files.caches {
            for (group, ids) in associations(text, mime) {
                if group == "MIME Cache" {
                    candidates.extend(ids.into_iter().filter(|id| !removed.contains(id)));
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

    /// Shown applications that open any of `types`, by name.
    pub fn opening(&self, types: &[&str]) -> Vec<&App> {
        let mut apps: Vec<&App> = self
            .apps
            .values()
            .filter(|app| !app.hidden && app.mime_types.iter().any(|opened| types.contains(&opened.as_str())))
            .collect();
        apps.sort_by_key(|app| app.name.to_lowercase());
        apps
    }

    /// Every type a shown application says it opens.
    pub fn mime_types(&self) -> BTreeSet<&str> {
        self.apps.values().filter(|app| !app.hidden).flat_map(|app| app.mime_types.iter().map(String::as_str)).collect()
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

/// Makes `id` the default for `types` in the user's `mimeapps.list`, as
/// `xdg-mime default` does. A desktop-specific file of the user's that
/// already sets one of the types is changed instead, since it comes first.
/// Returns the file written.
pub fn set_default(types: &[&str], id: &str) -> Result<PathBuf, String> {
    let config = config_dirs().into_iter().next().ok_or("no config folder")?;
    let specific = desktops().into_iter().map(|desktop| config.join(format!("{desktop}-mimeapps.list"))).find(|file| {
        fs::read_to_string(file).is_ok_and(|text| {
            types.iter().any(|mime| associations(&text, mime).iter().any(|(group, _)| group == "Default Applications"))
        })
    });
    let file = specific.unwrap_or_else(|| config.join("mimeapps.list"));
    let text = fs::read_to_string(&file).unwrap_or_default();
    fs::create_dir_all(&config).map_err(|error| error.to_string())?;
    let temporary = file.with_extension("list.tmp");
    fs::write(&temporary, with_default(&text, types, id))
        .and_then(|()| fs::rename(&temporary, &file))
        .map_err(|error| format!("{}: {error}", file.display()))?;
    Ok(file)
}

/// `text` with `id` as the default for `types`: their lines in
/// `[Default Applications]` replaced or added, and `id` taken out of their
/// `[Removed Associations]`. Everything else stays as it was.
fn with_default(text: &str, types: &[&str], id: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut group = String::new();
    let mut written: Vec<&str> = Vec::new();
    let mut defaults_end = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            if group == "Default Applications" {
                defaults_end = Some(lines.len());
            }
            group = name.to_owned();
            lines.push(line.to_owned());
            continue;
        }
        let key = trimmed.split_once('=').map(|(key, _)| key.trim());
        match (group.as_str(), key) {
            ("Default Applications", Some(key)) if types.contains(&key) => {
                if !written.contains(&key) {
                    lines.push(format!("{key}={id};"));
                    written.push(types[types.iter().position(|mime| *mime == key).unwrap_or(0)]);
                }
            }
            ("Removed Associations", Some(key)) if types.contains(&key) => {
                let value = trimmed.split_once('=').map_or("", |(_, value)| value);
                let kept: Vec<&str> =
                    value.split(';').map(str::trim).filter(|other| !other.is_empty() && *other != id).collect();
                if !kept.is_empty() {
                    lines.push(format!("{key}={};", kept.join(";")));
                }
            }
            _ => lines.push(line.to_owned()),
        }
    }
    if group == "Default Applications" {
        defaults_end = Some(lines.len());
    }
    let missing: Vec<String> =
        types.iter().filter(|mime| !written.contains(mime)).map(|mime| format!("{mime}={id};")).collect();
    match defaults_end {
        Some(end) => {
            // After the group's last line that is not blank.
            let mut at = end;
            while at > 0 && lines[at - 1].trim().is_empty() {
                at -= 1;
            }
            lines.splice(at..at, missing);
        }
        None => {
            if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("[Default Applications]".into());
            lines.extend(missing);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
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
        let cache = [dir.join("data/applications/mimeinfo.cache")];
        let default =
            |mime: &str| index.default_with(mime, &Associations::from_files(&files, &cache)).map(|app| app.id.clone());
        // Without a list, an entry that opens the type.
        assert!(default("text/plain").is_some());
        assert!(default("image/png").is_none());
        // The cache's order comes before the entries' own.
        fs::write(&cache[0], "[MIME Cache]\ntext/plain=other.desktop;editor.desktop;\n").unwrap();
        assert_eq!(default("text/plain").as_deref(), Some("other.desktop"));
        fs::write(
            dir.join("config/mimeapps.list"),
            "[Default Applications]\ntext/plain=missing.desktop;editor.desktop;\n",
        )
        .unwrap();
        assert_eq!(default("text/plain").as_deref(), Some("editor.desktop"));
        // A desktop-specific list comes first, and removals are respected.
        fs::write(
            dir.join("config/sway-mimeapps.list"),
            "[Removed Associations]\ntext/plain=editor.desktop;\n[Default Applications]\ntext/plain=other.desktop\n",
        )
        .unwrap();
        assert_eq!(default("text/plain").as_deref(), Some("other.desktop"));
        assert_eq!(index.opening(&["text/plain"]).len(), 2);
        assert!(index.mime_types().contains("text/markdown"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn setting_a_default_keeps_the_rest_of_the_file() {
        let text = "# mine\n[Added Associations]\ntext/plain=vim.desktop;\n\n[Default Applications]\n\
                    text/plain=gedit.desktop;\nimage/png=eog.desktop;\n\n[Removed Associations]\n\
                    text/html=zed.desktop;other.desktop;\n";
        let changed = with_default(text, &["text/plain", "text/html"], "zed.desktop");
        assert_eq!(
            changed,
            "# mine\n[Added Associations]\ntext/plain=vim.desktop;\n\n[Default Applications]\n\
             text/plain=zed.desktop;\nimage/png=eog.desktop;\ntext/html=zed.desktop;\n\n[Removed Associations]\n\
             text/html=other.desktop;\n"
        );
        assert_eq!(
            with_default("", &["inode/directory"], "files.desktop"),
            "[Default Applications]\ninode/directory=files.desktop;\n"
        );
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
