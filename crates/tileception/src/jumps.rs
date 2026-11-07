//! Often and recently visited folders from rupa's z (`~/.z`) and zoxide,
//! ranked by their own frecency, for the bundled folders plugin (`z words`
//! in the launcher).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// How long zoxide gets to list its folders.
const ZOXIDE_TIMEOUT: Duration = Duration::from_millis(300);

/// A visited folder and how strongly it is preferred, from 0 to 1.
#[derive(Debug, Clone, PartialEq)]
pub struct Folder {
    pub path: PathBuf,
    pub score: f64,
}

/// Every folder z and zoxide know, best first. `now` is in seconds since
/// the epoch.
pub fn load(home: &Path, now: i64) -> Vec<Folder> {
    let data = std::env::var_os("_Z_DATA").map(PathBuf::from).unwrap_or_else(|| home.join(".z"));
    // z.sh takes a folder in `_Z_DATA` to mean the file `.z` inside it.
    let data = if data.is_dir() { data.join(".z") } else { data };
    let from_z = std::fs::read_to_string(data).map(|data| parse_z(&data, now)).unwrap_or_default();
    let from_zoxide = zoxide().map(|output| parse_zoxide(&output)).unwrap_or_default();
    merge([from_z, from_zoxide])
}

/// The folders matching `words`, best first: each word must appear in the
/// path, in that order, ignoring case. Folders that are gone are left out.
pub fn matching<'a>(folders: &'a [Folder], words: &[&str], limit: usize) -> Vec<&'a Folder> {
    let words: Vec<String> = words.iter().map(|word| word.to_lowercase()).collect();
    folders
        .iter()
        .filter(|folder| {
            let path = folder.path.to_string_lossy().to_lowercase();
            let mut rest = path.as_str();
            words.iter().all(|word| match rest.find(word.as_str()) {
                Some(start) => {
                    rest = &rest[start + word.len()..];
                    true
                }
                None => false,
            })
        })
        .filter(|folder| folder.path.is_dir())
        .take(limit)
        .collect()
}

/// `~/.z` lines are `path|rank|time`; the score is z.sh's own frecency,
/// which lets a folder's rank fade over the days since its last visit.
fn parse_z(data: &str, now: i64) -> Vec<Folder> {
    data.lines()
        .filter_map(|line| {
            let mut fields = line.rsplitn(3, '|');
            let time: i64 = fields.next()?.trim().parse().ok()?;
            let rank: f64 = fields.next()?.trim().parse().ok()?;
            let path = fields.next()?;
            let age = (now - time).max(0) as f64;
            let score = 10_000.0 * rank * (3.75 / ((0.0001 * age + 1.0) + 0.25));
            (!path.is_empty()).then(|| Folder { path: PathBuf::from(path), score })
        })
        .collect()
}

/// `zoxide query --list --score` prints `score path` lines, already aged.
fn parse_zoxide(output: &str) -> Vec<Folder> {
    output
        .lines()
        .filter_map(|line| {
            let (score, path) = line.trim_start().split_once(' ')?;
            Some(Folder { path: PathBuf::from(path.trim_start()), score: score.parse().ok()? })
        })
        .collect()
}

/// One list from several sources: each source's scores are scaled to its
/// best folder, and a folder both know keeps its better score.
fn merge<const N: usize>(sources: [Vec<Folder>; N]) -> Vec<Folder> {
    let mut best: HashMap<PathBuf, f64> = HashMap::new();
    for source in sources {
        let top = source.iter().map(|folder| folder.score).fold(0.0_f64, f64::max);
        if top <= 0.0 {
            continue;
        }
        for folder in source {
            let score = folder.score / top;
            let entry = best.entry(folder.path).or_insert(score);
            *entry = entry.max(score);
        }
    }
    let mut folders: Vec<Folder> = best.into_iter().map(|(path, score)| Folder { path, score }).collect();
    folders.sort_by(|left, right| right.score.total_cmp(&left.score).then_with(|| left.path.cmp(&right.path)));
    folders
}

/// zoxide's list, when zoxide is installed and answers in time.
fn zoxide() -> Option<String> {
    let mut child = Command::new("zoxide")
        .args(["query", "--list", "--score"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < ZOXIDE_TIMEOUT => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::warn!("zoxide did not list its folders in time");
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_152_000;

    #[test]
    fn recent_visits_outrank_old_ones() {
        let data = format!("/old|100|{}\n/new|10|{}\nbroken line\n/a|b|c|5|{NOW}\n", NOW - 90 * 86_400, NOW - 60);
        let folders = parse_z(&data, NOW);
        assert_eq!(folders.len(), 3);
        let score = |path: &str| folders.iter().find(|folder| folder.path == Path::new(path)).unwrap().score;
        assert!(score("/new") > score("/old"));
        // A `|` in a path stays part of it.
        assert!(score("/a|b|c") > 0.0);
    }

    #[test]
    fn zoxide_scores() {
        let folders = parse_zoxide("  12.5 /home/me/src\n   4.0 /home/me/with space\nnonsense\n");
        assert_eq!(
            folders,
            [
                Folder { path: "/home/me/src".into(), score: 12.5 },
                Folder { path: "/home/me/with space".into(), score: 4.0 },
            ]
        );
    }

    #[test]
    fn sources_are_scaled_before_merging() {
        let from_z = vec![Folder { path: "/a".into(), score: 50_000.0 }, Folder { path: "/b".into(), score: 25_000.0 }];
        let zoxide = vec![Folder { path: "/b".into(), score: 8.0 }, Folder { path: "/c".into(), score: 2.0 }];
        let merged = merge([from_z, zoxide]);
        let paths: Vec<_> = merged.iter().map(|folder| folder.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["/a", "/b", "/c"]);
        assert_eq!(merged[1].score, 1.0);
    }

    #[test]
    fn words_match_in_order_ignoring_case() {
        let root = std::env::temp_dir().join(format!("tileception-jumps-{}", std::process::id()));
        for folder in ["Projects/web", "web/projects", "Projects/api"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        let folder = |path: &str, score| Folder { path: root.join(path), score };
        let folders = [
            folder("Projects/web", 1.0),
            folder("web/projects", 0.9),
            folder("Projects/api", 0.8),
            folder("Projects/gone", 0.7),
        ];
        let found = |words: &[&str]| -> Vec<PathBuf> {
            matching(&folders, words, 10).into_iter().map(|folder| folder.path.clone()).collect()
        };
        assert_eq!(found(&["proj", "web"]), [root.join("Projects/web")]);
        assert_eq!(found(&["PROJ"]).len(), 3, "the folder that is gone is left out");
        assert_eq!(found(&[]).len(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }
}
