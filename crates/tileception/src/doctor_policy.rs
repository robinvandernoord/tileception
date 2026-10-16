//! What the doctor's agent may do without asking, in each permission mode,
//! and the diffs and backups that go with its changes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tileception_acp::{Action, Choice};

/// How much the agent may do without asking. Remembered between sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Every change is shown and confirmed.
    #[default]
    Ask,
    /// Tileception's own files and commands go ahead; the rest asks.
    Config,
    /// Nothing asks. Backups and undo still apply.
    Yolo,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Ask => "Ask first",
            Mode::Config => "Tileception config",
            Mode::Yolo => "Yolo",
        }
    }

    pub fn next(self) -> Mode {
        match self {
            Mode::Ask => Mode::Config,
            Mode::Config => Mode::Yolo,
            Mode::Yolo => Mode::Ask,
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "ask" => Some(Mode::Ask),
            "config" | "tileception" => Some(Mode::Config),
            "yolo" => Some(Mode::Yolo),
            _ => None,
        }
    }
}

/// Tileception's own places.
#[derive(Debug, Clone)]
pub struct Scope {
    /// ~/.config/tileception: the agent may change it in config mode.
    pub config: PathBuf,
    /// ~/.local/state/tileception: the log and the defaults, to read.
    pub state: PathBuf,
}

impl Scope {
    pub fn changeable(&self, path: &Path) -> bool {
        within(path, &self.config)
    }

    pub fn readable(&self, path: &Path) -> bool {
        within(path, &self.config) || within(path, &self.state)
    }
}

/// Whether `path` lies under `dir`, without `..` tricks.
fn within(path: &Path, dir: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|c| matches!(c, std::path::Component::ParentDir))
        && path.starts_with(dir)
}

/// The choice to make without asking, if the mode allows the action.
pub fn auto_choice(mode: Mode, action: &Action, choices: &[Choice], scope: &Scope) -> Option<String> {
    let allow = choices
        .iter()
        .find(|c| c.kind == "allow_once")
        .or_else(|| choices.iter().find(|c| c.kind.starts_with("allow")))?;
    let allowed = match mode {
        Mode::Yolo => true,
        Mode::Ask => false,
        Mode::Config => match action.kind.as_deref() {
            Some("think") => true,
            Some("read" | "search") => !action.paths.is_empty() && action.paths.iter().all(|p| scope.readable(p)),
            Some("edit" | "delete" | "move") => {
                let paths: Vec<&Path> = action
                    .paths
                    .iter()
                    .map(PathBuf::as_path)
                    .chain(action.diffs.iter().map(|d| d.0.as_path()))
                    .collect();
                !paths.is_empty() && paths.iter().all(|p| scope.changeable(p))
            }
            Some("execute") => action.command.as_deref().is_some_and(tileception_command),
            _ => false,
        },
    };
    allowed.then(|| allow.id.clone())
}

/// Compositor commands the agent may run unasked in config mode: ones that
/// look or adjust, not `spawn`, `exec`, `doctor` (its own mode), pretend
/// input, `screenshot` (writes anywhere), `quit` or `unlock`.
const SAFE_VERBS: [&str; 6] = ["tree", "reload", "volume", "mic", "brightness", "media"];

/// `tileception msg <safe verb> ...` and nothing chained onto it.
fn tileception_command(command: &str) -> bool {
    let Some(tail) = command.trim().strip_prefix("tileception msg ") else { return false };
    if tail.contains(['|', ';', '&', '`', '$', '>', '<', '\n', '\r']) {
        return false;
    }
    tail.split_whitespace().next().is_some_and(|verb| SAFE_VERBS.contains(&verb))
}

/// Reading goes ahead inside Tileception's places, and anywhere in yolo mode.
pub fn may_read(mode: Mode, path: &Path, scope: &Scope) -> bool {
    mode == Mode::Yolo || scope.readable(path)
}

/// Writing goes ahead in yolo mode, inside the config folder in config mode,
/// and in ask mode once the user allowed an edit of that file.
pub fn may_write(mode: Mode, path: &Path, scope: &Scope, approved: &[PathBuf]) -> bool {
    match mode {
        Mode::Yolo => true,
        Mode::Config => scope.changeable(path),
        Mode::Ask => approved.iter().any(|p| p == path),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Same,
    Added,
    Removed,
    /// Unchanged lines left out.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub sign: Sign,
    pub text: String,
}

/// The largest comparison table [`diff`] builds (about 16 MB).
const MAX_DIFF_CELLS: usize = 4_000_000;

/// A line diff with two lines of context around changes, plus how many
/// lines were added and removed.
pub fn diff(old: &str, new: &str) -> (Vec<DiffLine>, usize, usize) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // Lines the same at both ends need no table.
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a_mid, b_mid) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut all: Vec<(Sign, &str)> = a[..prefix].iter().map(|l| (Sign::Same, *l)).collect();
    if a_mid.len().saturating_mul(b_mid.len()) > MAX_DIFF_CELLS {
        // Too big to compare line by line on the compositor's thread: the
        // whole middle is shown as replaced.
        all.extend(a_mid.iter().map(|l| (Sign::Removed, *l)));
        all.extend(b_mid.iter().map(|l| (Sign::Added, *l)));
    } else {
        // Longest common subsequence, from the end.
        let (a, b) = (a_mid, b_mid);
        let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                all.push((Sign::Same, a[i]));
                i += 1;
                j += 1;
            } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
                // Removed lines before the added ones that replace them.
                all.push((Sign::Removed, a[i]));
                i += 1;
            } else {
                all.push((Sign::Added, b[j]));
                j += 1;
            }
        }
    }
    all.extend(a[a.len() - suffix..].iter().map(|l| (Sign::Same, *l)));
    let added = all.iter().filter(|(s, _)| *s == Sign::Added).count();
    let removed = all.iter().filter(|(s, _)| *s == Sign::Removed).count();
    let near = |k: usize| {
        let lo = k.saturating_sub(2);
        let hi = (k + 3).min(all.len());
        all[lo..hi].iter().any(|(s, _)| *s != Sign::Same)
    };
    let mut out: Vec<DiffLine> = Vec::new();
    for (k, (sign, text)) in all.iter().enumerate() {
        if *sign != Sign::Same || near(k) {
            out.push(DiffLine { sign: *sign, text: (*text).to_string() });
        } else if out.last().is_none_or(|l| l.sign != Sign::Skipped) {
            out.push(DiffLine { sign: Sign::Skipped, text: String::new() });
        }
    }
    while out.last().is_some_and(|l| l.sign == Sign::Skipped) {
        out.pop();
    }
    (out, added, removed)
}

/// Copies `path` into the backups folder before a change. `None` when there
/// was nothing to keep (a new file). Keeps the newest few.
pub fn backup(path: &Path, backups: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(backups).map_err(|e| format!("{}: {e}", backups.display()))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let name = path.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let copy = backups.join(format!("{name}.{stamp}"));
    std::fs::copy(path, &copy).map_err(|e| format!("could not back up {}: {e}", path.display()))?;
    if let Ok(entries) = std::fs::read_dir(backups) {
        let mut files: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        // Oldest first, by the stamp after the name (not by the name).
        files.sort_by_key(|p| p.extension().and_then(|s| s.to_str()).and_then(|s| s.parse::<u128>().ok()));
        let excess = files.len().saturating_sub(50);
        for old in &files[..excess] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(Some(copy))
}

/// A text file's lines from `line` (1-based), at most `limit` of them.
pub fn read_lines(text: &str, line: Option<u64>, limit: Option<u64>) -> String {
    if line.is_none() && limit.is_none() {
        return text.to_string();
    }
    let skip = line.unwrap_or(1).saturating_sub(1) as usize;
    let take = limit.map_or(usize::MAX, |l| l as usize);
    let mut out: String = text.lines().skip(skip).take(take).collect::<Vec<_>>().join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope { config: "/home/r/.config/tileception".into(), state: "/home/r/.local/state/tileception".into() }
    }

    fn choices() -> Vec<Choice> {
        vec![
            Choice { id: "always".into(), name: "Always".into(), kind: "allow_always".into() },
            Choice { id: "once".into(), name: "Allow".into(), kind: "allow_once".into() },
            Choice { id: "no".into(), name: "Reject".into(), kind: "reject_once".into() },
        ]
    }

    fn action(kind: &str, paths: &[&str], command: Option<&str>) -> Action {
        Action {
            kind: Some(kind.into()),
            paths: paths.iter().map(PathBuf::from).collect(),
            command: command.map(String::from),
            diffs: Vec::new(),
        }
    }

    #[test]
    fn modes_decide() {
        let s = scope();
        let config = "/home/r/.config/tileception/config.toml";
        let edit = action("edit", &[config], None);
        assert_eq!(auto_choice(Mode::Ask, &edit, &choices(), &s), None);
        assert_eq!(auto_choice(Mode::Config, &edit, &choices(), &s).as_deref(), Some("once"));
        let elsewhere = action("edit", &["/home/r/.bashrc"], None);
        assert_eq!(auto_choice(Mode::Config, &elsewhere, &choices(), &s), None);
        assert_eq!(auto_choice(Mode::Yolo, &elsewhere, &choices(), &s).as_deref(), Some("once"));
        let sneaky = action("edit", &["/home/r/.config/tileception/../../.bashrc"], None);
        assert_eq!(auto_choice(Mode::Config, &sneaky, &choices(), &s), None);
        let msg = action("execute", &[], Some("tileception msg tree"));
        assert!(auto_choice(Mode::Config, &msg, &choices(), &s).is_some());
        let chained = action("execute", &[], Some("tileception msg tree; rm -rf ~"));
        assert_eq!(auto_choice(Mode::Config, &chained, &choices(), &s), None);
        for risky in ["tileception msg spawn rm -rf ~", "tileception msg doctor mode yolo", "tileception msg type hi"] {
            assert_eq!(auto_choice(Mode::Config, &action("execute", &[], Some(risky)), &choices(), &s), None);
        }
        let log = action("read", &["/home/r/.local/state/tileception/tileception.log"], None);
        assert!(auto_choice(Mode::Config, &log, &choices(), &s).is_some());
        // Nothing to allow with: nothing is chosen.
        let rejects = vec![Choice { id: "no".into(), name: "No".into(), kind: "reject_once".into() }];
        assert_eq!(auto_choice(Mode::Yolo, &edit, &rejects, &s), None);
    }

    #[test]
    fn files_follow_the_mode() {
        let s = scope();
        let config = Path::new("/home/r/.config/tileception/config.toml");
        let other = Path::new("/etc/passwd");
        assert!(may_read(Mode::Ask, config, &s) && !may_read(Mode::Ask, other, &s) && may_read(Mode::Yolo, other, &s));
        assert!(!may_write(Mode::Ask, config, &s, &[]));
        assert!(may_write(Mode::Ask, config, &s, &[config.to_path_buf()]));
        assert!(may_write(Mode::Config, config, &s, &[]) && !may_write(Mode::Config, other, &s, &[]));
        assert!(may_write(Mode::Yolo, other, &s, &[]));
    }

    #[test]
    fn diffs_show_changes_with_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\n";
        let (lines, added, removed) = diff(old, new);
        assert_eq!((added, removed), (2, 1));
        let shown: Vec<String> = lines
            .iter()
            .map(|l| match l.sign {
                Sign::Same => format!(" {}", l.text),
                Sign::Added => format!("+{}", l.text),
                Sign::Removed => format!("-{}", l.text),
                Sign::Skipped => "...".into(),
            })
            .collect();
        assert_eq!(shown, ["...", " c", " d", "-e", "+E", " f", " g", " h", "+i"]);
        assert_eq!(diff("same\n", "same\n").1, 0);
        // Huge files are not compared line by line, but still counted.
        let big: String = (0..3000).map(|i| format!("{i}\n")).collect();
        let other: String = (0..3000).map(|i| format!("x{i}\n")).collect();
        let (_, added, removed) = diff(&big, &other);
        assert_eq!((added, removed), (3000, 3000));
    }

    #[test]
    fn reads_parts_of_files() {
        assert_eq!(read_lines("1\n2\n3\n4\n", Some(2), Some(2)), "2\n3\n");
        assert_eq!(read_lines("1\n2\n", None, None), "1\n2\n");
        assert_eq!(read_lines("1\n2\n", Some(9), None), "");
    }
}
