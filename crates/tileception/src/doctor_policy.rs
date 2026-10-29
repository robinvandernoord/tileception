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
            Mode::Config => "Desktop tweaks",
            Mode::Yolo => "Yolo",
        }
    }

    pub const ALL: [Mode; 3] = [Mode::Ask, Mode::Config, Mode::Yolo];

    /// The name on the panel's switch.
    pub fn short(self) -> &'static str {
        match self {
            Mode::Ask => "Ask",
            Mode::Config => "Tweak",
            Mode::Yolo => "Yolo",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Mode::Ask => "Every change is shown and confirmed first",
            Mode::Config => "Tweaks to Tileception's own settings go ahead; the rest asks",
            Mode::Yolo => "Nothing asks; backups and undo still apply",
        }
    }

    pub fn next(self) -> Mode {
        match self {
            Mode::Ask => Mode::Config,
            Mode::Config => Mode::Yolo,
            Mode::Yolo => Mode::Ask,
        }
    }

    pub fn parse(name: &str) -> Option<Mode> {
        match name {
            "ask" => Some(Mode::Ask),
            "tweak" | "config" | "tileception" => Some(Mode::Config),
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
        && !path.components().any(|component| matches!(component, std::path::Component::ParentDir))
        && path.starts_with(dir)
}

/// The choice to make without asking, if the mode allows the action.
pub fn auto_choice(mode: Mode, action: &Action, choices: &[Choice], scope: &Scope) -> Option<String> {
    let allow = choices
        .iter()
        .find(|choice| choice.kind == "allow_once")
        .or_else(|| choices.iter().find(|choice| choice.kind.starts_with("allow")))?;
    let allowed = match mode {
        Mode::Yolo => true,
        Mode::Ask => false,
        Mode::Config => match action.kind.as_deref() {
            Some("think") => true,
            Some("read" | "search") => !action.paths.is_empty() && action.paths.iter().all(|path| scope.readable(path)),
            Some("edit" | "delete" | "move") => {
                let paths: Vec<&Path> = action
                    .paths
                    .iter()
                    .map(PathBuf::as_path)
                    .chain(action.diffs.iter().map(|change| change.0.as_path()))
                    .collect();
                !paths.is_empty() && paths.iter().all(|path| scope.changeable(path))
            }
            Some("execute") => action.command.as_deref().is_some_and(tileception_command),
            _ => false,
        },
    };
    allowed.then(|| allow.choice_id.clone())
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
        Mode::Ask => approved.iter().any(|approved_path| approved_path == path),
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
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    // Lines the same at both ends need no table.
    let prefix = old_lines.iter().zip(&new_lines).take_while(|(old_line, new_line)| old_line == new_line).count();
    let suffix = old_lines[prefix..]
        .iter()
        .rev()
        .zip(new_lines[prefix..].iter().rev())
        .take_while(|(old_line, new_line)| old_line == new_line)
        .count();
    let (a_mid, b_mid) = (&old_lines[prefix..old_lines.len() - suffix], &new_lines[prefix..new_lines.len() - suffix]);
    let mut all: Vec<(Sign, &str)> = old_lines[..prefix].iter().map(|line| (Sign::Same, *line)).collect();
    if a_mid.len().saturating_mul(b_mid.len()) > MAX_DIFF_CELLS {
        // Too big to compare line by line on the compositor's thread: the
        // whole middle is shown as replaced.
        all.extend(a_mid.iter().map(|line| (Sign::Removed, *line)));
        all.extend(b_mid.iter().map(|line| (Sign::Added, *line)));
    } else {
        // Longest common subsequence, from the end.
        let (old_part, new_part) = (a_mid, b_mid);
        let mut lcs = vec![vec![0u32; new_part.len() + 1]; old_part.len() + 1];
        for old_index in (0..old_part.len()).rev() {
            for new_index in (0..new_part.len()).rev() {
                lcs[old_index][new_index] = if old_part[old_index] == new_part[new_index] {
                    lcs[old_index + 1][new_index + 1] + 1
                } else {
                    lcs[old_index + 1][new_index].max(lcs[old_index][new_index + 1])
                };
            }
        }
        let (mut old_index, mut new_index) = (0, 0);
        while old_index < old_part.len() || new_index < new_part.len() {
            if old_index < old_part.len() && new_index < new_part.len() && old_part[old_index] == new_part[new_index] {
                all.push((Sign::Same, old_part[old_index]));
                old_index += 1;
                new_index += 1;
            } else if old_index < old_part.len()
                && (new_index == new_part.len() || lcs[old_index + 1][new_index] >= lcs[old_index][new_index + 1])
            {
                // Removed lines before the added ones that replace them.
                all.push((Sign::Removed, old_part[old_index]));
                old_index += 1;
            } else {
                all.push((Sign::Added, new_part[new_index]));
                new_index += 1;
            }
        }
    }
    all.extend(old_lines[old_lines.len() - suffix..].iter().map(|line| (Sign::Same, *line)));
    let added = all.iter().filter(|(sign, _)| *sign == Sign::Added).count();
    let removed = all.iter().filter(|(sign, _)| *sign == Sign::Removed).count();
    let near = |index: usize| {
        let start = index.saturating_sub(2);
        let end = (index + 3).min(all.len());
        all[start..end].iter().any(|(sign, _)| *sign != Sign::Same)
    };
    let mut out: Vec<DiffLine> = Vec::new();
    for (index, (sign, text)) in all.iter().enumerate() {
        if *sign != Sign::Same || near(index) {
            out.push(DiffLine { sign: *sign, text: (*text).to_string() });
        } else if out.last().is_none_or(|last| last.sign != Sign::Skipped) {
            out.push(DiffLine { sign: Sign::Skipped, text: String::new() });
        }
    }
    while out.last().is_some_and(|last| last.sign == Sign::Skipped) {
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
    std::fs::create_dir_all(backups).map_err(|err| format!("{}: {err}", backups.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_millis());
    let name = path.file_name().map_or_else(|| "file".into(), |file_name| file_name.to_string_lossy().into_owned());
    let copy = backups.join(format!("{name}.{stamp}"));
    std::fs::copy(path, &copy).map_err(|err| format!("could not back up {}: {err}", path.display()))?;
    if let Ok(entries) = std::fs::read_dir(backups) {
        let mut files: Vec<_> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
        // Oldest first, by the stamp after the name (not by the name).
        files.sort_by_key(|backup_path| {
            backup_path
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(|digits| digits.parse::<u128>().ok())
        });
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
    let take = limit.map_or(usize::MAX, |count| count as usize);
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
            Choice { choice_id: "always".into(), name: "Always".into(), kind: "allow_always".into() },
            Choice { choice_id: "once".into(), name: "Allow".into(), kind: "allow_once".into() },
            Choice { choice_id: "no".into(), name: "Reject".into(), kind: "reject_once".into() },
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
        let places = scope();
        let config = "/home/r/.config/tileception/config.toml";
        let edit = action("edit", &[config], None);
        assert_eq!(auto_choice(Mode::Ask, &edit, &choices(), &places), None);
        assert_eq!(auto_choice(Mode::Config, &edit, &choices(), &places).as_deref(), Some("once"));
        let elsewhere = action("edit", &["/home/r/.bashrc"], None);
        assert_eq!(auto_choice(Mode::Config, &elsewhere, &choices(), &places), None);
        assert_eq!(auto_choice(Mode::Yolo, &elsewhere, &choices(), &places).as_deref(), Some("once"));
        let sneaky = action("edit", &["/home/r/.config/tileception/../../.bashrc"], None);
        assert_eq!(auto_choice(Mode::Config, &sneaky, &choices(), &places), None);
        let msg = action("execute", &[], Some("tileception msg tree"));
        assert!(auto_choice(Mode::Config, &msg, &choices(), &places).is_some());
        let chained = action("execute", &[], Some("tileception msg tree; rm -rf ~"));
        assert_eq!(auto_choice(Mode::Config, &chained, &choices(), &places), None);
        for risky in ["tileception msg spawn rm -rf ~", "tileception msg doctor mode yolo", "tileception msg type hi"] {
            assert_eq!(auto_choice(Mode::Config, &action("execute", &[], Some(risky)), &choices(), &places), None);
        }
        let log = action("read", &["/home/r/.local/state/tileception/tileception.log"], None);
        assert!(auto_choice(Mode::Config, &log, &choices(), &places).is_some());
        // Nothing to allow with: nothing is chosen.
        let rejects = vec![Choice { choice_id: "no".into(), name: "No".into(), kind: "reject_once".into() }];
        assert_eq!(auto_choice(Mode::Yolo, &edit, &rejects, &places), None);
    }

    #[test]
    fn files_follow_the_mode() {
        let places = scope();
        let config = Path::new("/home/r/.config/tileception/config.toml");
        let other = Path::new("/etc/passwd");
        assert!(
            may_read(Mode::Ask, config, &places)
                && !may_read(Mode::Ask, other, &places)
                && may_read(Mode::Yolo, other, &places)
        );
        assert!(!may_write(Mode::Ask, config, &places, &[]));
        assert!(may_write(Mode::Ask, config, &places, &[config.to_path_buf()]));
        assert!(may_write(Mode::Config, config, &places, &[]) && !may_write(Mode::Config, other, &places, &[]));
        assert!(may_write(Mode::Yolo, other, &places, &[]));
    }

    #[test]
    fn diffs_show_changes_with_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\n";
        let (lines, added, removed) = diff(old, new);
        assert_eq!((added, removed), (2, 1));
        let shown: Vec<String> = lines
            .iter()
            .map(|line| match line.sign {
                Sign::Same => format!(" {}", line.text),
                Sign::Added => format!("+{}", line.text),
                Sign::Removed => format!("-{}", line.text),
                Sign::Skipped => "...".into(),
            })
            .collect();
        assert_eq!(shown, ["...", " c", " d", "-e", "+E", " f", " g", " h", "+i"]);
        assert_eq!(diff("same\n", "same\n").1, 0);
        // Huge files are not compared line by line, but still counted.
        let big: String = (0..3000).map(|number| format!("{number}\n")).collect();
        let other: String = (0..3000).map(|number| format!("x{number}\n")).collect();
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
