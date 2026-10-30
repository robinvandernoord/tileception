//! Application discovery and search without a desktop-session dependency.

mod autostart;
mod desktop;
mod icons;

pub use autostart::{autostart_entries, scan_autostart, AutostartEntry};
pub use icons::IconLookup;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

#[derive(Clone, Debug)]
pub struct App {
    pub id: String,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub keywords: Vec<String>,
    pub icon: Option<String>,
    /// A shell-quoted command with desktop field codes removed.
    pub exec: String,
    /// The same command as separate, unquoted arguments.
    pub args: Vec<String>,
    /// Where in `args` extra arguments go (see [`command_line_with`]).
    pub extra_args_at: usize,
    pub terminal: bool,
    pub categories: Vec<String>,
    pub wm_class: Option<String>,
    pub hidden: bool,
    /// Extra entry points from `[Desktop Action ...]` groups, in `Actions=` order.
    pub actions: Vec<AppAction>,
}

/// One desktop action, such as Firefox's "New Private Window".
#[derive(Clone, Debug)]
pub struct AppAction {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    /// Shell-quoted like [`App::exec`].
    pub exec: String,
}

#[derive(Clone, Debug)]
pub struct Usage {
    pub count: u32,
    pub last_used: SystemTime,
}

struct Searchable {
    name: String,
    secondary: Vec<String>,
    /// Action names, in the order of `App::actions`.
    actions: Vec<String>,
}

pub struct AppIndex {
    apps: BTreeMap<String, App>,
    normalized: HashMap<String, Searchable>,
    dirs: Vec<PathBuf>,
    desktop: String,
    locale: String,
    path: Vec<PathBuf>,
    snapshot: BTreeMap<PathBuf, (Option<SystemTime>, u64)>,
}

impl AppIndex {
    pub fn scan(dirs: &[PathBuf], desktop: &str) -> Self {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|key| env::var(key).ok().filter(|value| !value.is_empty()))
            .unwrap_or_default();
        let path = env::var_os("PATH").map(|value| env::split_paths(&value).collect::<Vec<_>>()).unwrap_or_default();
        Self::scan_with_options(dirs, desktop, &locale, &path)
    }

    /// Explicit locale and executable search paths make scans deterministic.
    pub fn scan_with_options(dirs: &[PathBuf], desktop: &str, locale: &str, path: &[PathBuf]) -> Self {
        let mut index = Self {
            apps: BTreeMap::new(),
            normalized: HashMap::new(),
            dirs: dirs.to_vec(),
            desktop: desktop.to_owned(),
            locale: locale.to_owned(),
            path: path.to_vec(),
            snapshot: BTreeMap::new(),
        };
        index.refresh();
        index
    }

    pub fn scan_default() -> Self {
        let dirs = data_dirs().into_iter().map(|dir| dir.join("applications")).collect::<Vec<_>>();
        let desktop = env::var("XDG_CURRENT_DESKTOP")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Tileception".to_owned());
        Self::scan(&dirs, &desktop)
    }

    pub fn get(&self, id: &str) -> Option<&App> {
        self.apps.get(id)
    }

    /// The application whose entry runs `program`, compared by file name so
    /// `/usr/bin/keepassxc` finds `Exec=keepassxc`. Shown entries come first.
    pub fn by_program(&self, program: &Path) -> Option<&App> {
        let name = program.file_name()?;
        let runs = |app: &&App| app.args.first().and_then(|first| Path::new(first).file_name()) == Some(name);
        let mut found = self.apps.values().filter(runs);
        let first = found.next()?;
        if !first.hidden {
            return Some(first);
        }
        found.find(|app| !app.hidden).or(Some(first))
    }

    pub fn refresh(&mut self) {
        self.apps.clear();
        self.normalized.clear();
        let mut seen = HashSet::new();
        for dir in &self.dirs {
            let mut files = Vec::new();
            walk(dir, &mut files, &mut BTreeMap::new());
            files.sort();
            for file in files {
                let Some(relative) = file.strip_prefix(dir).ok() else {
                    continue;
                };
                let id = relative.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "-");
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Some(app) = fs::read_to_string(&file)
                    .ok()
                    .and_then(|text| desktop::parse(&text, &id, &self.desktop, &self.locale, &self.path))
                else {
                    continue;
                };
                self.normalized.insert(
                    id.clone(),
                    Searchable {
                        name: normalize(&app.name),
                        secondary: app
                            .generic_name
                            .iter()
                            .chain(app.keywords.iter())
                            .map(|value| normalize(value))
                            .chain(std::iter::once(normalize(&desktop::program(&app.exec))))
                            .collect(),
                        actions: app.actions.iter().map(|action| normalize(&action.name)).collect(),
                    },
                );
                self.apps.insert(id, app);
            }
        }
        self.snapshot = snapshot(&self.dirs);
    }

    pub fn changed_since_scan(&self) -> bool {
        self.snapshot != snapshot(&self.dirs)
    }

    pub fn search(&self, query: &str, usage: &HashMap<String, Usage>, limit: usize) -> Vec<&App> {
        let query = normalize(query.trim());
        let mut matches = self
            .apps
            .values()
            .filter(|app| !app.hidden)
            .filter_map(|app| {
                let normalized = &self.normalized[&app.id];
                let rank = if query.is_empty() || normalized.name == query {
                    0
                } else if normalized.name.starts_with(&query) {
                    1
                } else if normalized.name.split(|c: char| !c.is_alphanumeric()).any(|word| word.starts_with(&query)) {
                    2
                } else if normalized.name.contains(&query) {
                    3
                } else if subsequence(&query, &normalized.name) {
                    4
                } else if normalized.secondary.iter().any(|value| value.contains(&query)) {
                    5
                } else {
                    return None;
                };
                Some((rank, app))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_rank, left), (right_rank, right)| {
            let left_usage = usage.get(&left.id);
            let right_usage = usage.get(&right.id);
            left_rank
                .cmp(right_rank)
                .then_with(|| right_usage.map_or(0, |u| u.count).cmp(&left_usage.map_or(0, |u| u.count)))
                .then_with(|| right_usage.map(|u| u.last_used).cmp(&left_usage.map(|u| u.last_used)))
                .then_with(|| self.normalized[&left.id].name.cmp(&self.normalized[&right.id].name))
                .then_with(|| left.id.cmp(&right.id))
        });
        matches.into_iter().take(limit).map(|(_, app)| app).collect()
    }
}

impl AppIndex {
    /// Desktop actions of visible apps whose name matches every word of the
    /// query, alone or together with the app's name ("private window",
    /// "firefox private"). Actions named from the start come first.
    pub fn search_actions(&self, query: &str, limit: usize) -> Vec<(&App, &AppAction)> {
        let query = normalize(query.trim());
        let words = query.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() {
            return Vec::new();
        }
        let starts_word =
            |text: &str, word: &str| text.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(word));
        let mut matches = self
            .apps
            .values()
            .filter(|app| !app.hidden)
            .flat_map(|app| {
                let normalized = &self.normalized[&app.id];
                let words = &words;
                let query = &query;
                app.actions.iter().zip(&normalized.actions).filter_map(move |(action, name)| {
                    let rank = if name.starts_with(query.as_str()) {
                        0
                    } else if words.iter().all(|word| starts_word(name, word)) {
                        1
                    } else if words.iter().all(|word| starts_word(name, word) || starts_word(&normalized.name, word))
                        && words.iter().any(|word| starts_word(name, word))
                    {
                        2
                    } else {
                        return None;
                    };
                    Some((rank, app, action))
                })
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_rank, left, _), (right_rank, right, _)| {
            left_rank.cmp(right_rank).then_with(|| self.normalized[&left.id].name.cmp(&self.normalized[&right.id].name))
        });
        matches.into_iter().take(limit).map(|(_, app, action)| (app, action)).collect()
    }
}

/// The command line for a desktop action, with the app's terminal setting.
pub fn action_command_line(app: &App, action: &AppAction, terminal: &str) -> String {
    if app.terminal && !terminal.trim().is_empty() {
        format!("{} {}", terminal.trim(), action.exec)
    } else {
        action.exec.clone()
    }
}

/// The terminal prefix is trusted caller configuration, interpreted by the shell.
pub fn command_line(app: &App, terminal: &str) -> String {
    command_line_with(app, terminal, &[])
}

/// Like [`command_line`], with `extra` added as literal arguments where the
/// entry would take files or URLs (ahead of a Flatpak forwarding marker), or
/// at the end. An option the entry already sets wins: `--opt=value` or `--opt`
/// is skipped when the entry has `--opt` or `--opt=...` itself. For a bare
/// `--opt`, the following argument is treated as its value unless it starts
/// with `-`, and is skipped too.
pub fn command_line_with(app: &App, terminal: &str, extra: &[String]) -> String {
    let given = |arg: &str| {
        let Some(option) = arg.split('=').next().filter(|name| name.starts_with("--") && name.len() > 2) else {
            return false;
        };
        app.args.iter().any(|own| own == option || own.strip_prefix(option).is_some_and(|rest| rest.starts_with('=')))
    };
    let mut args = extra.iter().peekable();
    let mut extra = Vec::new();
    while let Some(arg) = args.next() {
        if given(arg) {
            if !arg.contains('=') && args.peek().is_some_and(|value| !value.starts_with('-')) {
                args.next();
            }
        } else {
            extra.push(desktop::shell_quote(arg));
        }
    }
    let exec = if extra.is_empty() {
        app.exec.clone()
    } else {
        let at = app.extra_args_at.min(app.args.len());
        let quoted = app.args.iter().map(|arg| desktop::shell_quote(arg)).collect::<Vec<_>>();
        quoted[..at].iter().chain(&extra).chain(&quoted[at..]).cloned().collect::<Vec<_>>().join(" ")
    };
    if app.terminal && !terminal.trim().is_empty() {
        format!("{} {}", terminal.trim(), exec)
    } else {
        exec
    }
}

fn normalize(text: &str) -> String {
    text.nfkd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase).collect()
}

fn subsequence(query: &str, text: &str) -> bool {
    let mut chars = text.chars();
    query.chars().all(|needle| chars.by_ref().any(|c| c == needle))
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    {
        dirs.push(home);
    }
    let system = env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(env::split_paths(&system).filter(|path| path.is_absolute()));
    dirs
}

fn snapshot(dirs: &[PathBuf]) -> BTreeMap<PathBuf, (Option<SystemTime>, u64)> {
    let mut result = BTreeMap::new();
    for dir in dirs {
        walk(dir, &mut Vec::new(), &mut result);
    }
    result
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>, state: &mut BTreeMap<PathBuf, (Option<SystemTime>, u64)>) {
    if let Ok(metadata) = fs::metadata(dir) {
        state.insert(dir.to_owned(), (metadata.modified().ok(), metadata.len()));
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            walk(&path, files, state);
        } else if path.extension().is_some_and(|ext| ext == "desktop") {
            if let Ok(metadata) = fs::metadata(&path) {
                state.insert(path.clone(), (metadata.modified().ok(), metadata.len()));
            }
            files.push(path);
        }
    }
}
