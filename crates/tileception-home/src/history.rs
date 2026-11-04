//! The last six hours of a climate device's temperature and target, sampled
//! from the reports on the LAN and kept in a state file, for the trend line
//! under the climate widget. Sampling is local: the cloud's device logs are
//! not read.

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};

/// How far back the trend line reaches.
pub const WINDOW: Duration = Duration::from_secs(6 * 60 * 60);
/// The shortest time between two samples.
pub const SAMPLE: Duration = Duration::from_secs(5 * 60);

/// One reading: when, the room temperature and the target, in degrees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub when: SystemTime,
    pub temperature: Option<f64>,
    pub target: Option<f64>,
}

/// Adds a sample when one is due: the first, [`SAMPLE`] after the last, or
/// whenever the target changed, preserving the change's time. Keeps only the
/// window. True when a sample was added.
pub fn push(samples: &mut Vec<Sample>, temperature: Option<f64>, target: Option<f64>, now: SystemTime) -> bool {
    let due = match samples.last() {
        None => true,
        Some(last) => last.target != target || now.duration_since(last.when).map_or(true, |age| age >= SAMPLE),
    };
    if !due {
        return false;
    }
    samples.push(Sample { when: now, temperature, target });
    trim(samples, now);
    true
}

/// Drops samples outside the window, and any excess beyond its sample count.
pub fn trim(samples: &mut Vec<Sample>, now: SystemTime) {
    if let Some(cutoff) = now.checked_sub(WINDOW) {
        samples.retain(|sample| sample.when >= cutoff);
    }
    let cap = (WINDOW.as_secs() / SAMPLE.as_secs() + 2) as usize;
    if samples.len() > cap {
        let excess = samples.len() - cap;
        samples.drain(..excess);
    }
}

/// Where the samples live: `$XDG_STATE_HOME/tileception/home-history.json`
/// or `~/.local/state/tileception/home-history.json`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(base.join("tileception").join("home-history.json"))
}

/// Reads the file, dropping what has aged out. A missing or broken file is
/// an empty history.
pub fn load(path: &Path) -> HashMap<String, Vec<Sample>> {
    let Ok(text) = std::fs::read_to_string(path) else { return HashMap::new() };
    let mut map: HashMap<String, Vec<Sample>> = serde_json::from_str(&text).unwrap_or_default();
    let now = SystemTime::now();
    for samples in map.values_mut() {
        trim(samples, now);
    }
    map.retain(|_, samples| !samples.is_empty());
    map
}

/// Writes the file in one step, so a crash never leaves half of it.
pub fn save(path: &Path, map: &HashMap<String, Vec<Sample>>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(map).map_err(|err| err.to_string())?;
    let partial = path.with_extension("json.partial");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&partial)
        .map_err(|err| format!("{}: {err}", partial.display()))?;
    out.write_all(text.as_bytes())
        .and_then(|()| out.sync_all())
        .map_err(|err| format!("{}: {err}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_secs(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn samples_arrive_when_due_and_age_out() {
        let mut samples = Vec::new();
        assert!(push(&mut samples, Some(21.0), Some(20.0), at_secs(0)));
        // Too soon, and the target unchanged.
        assert!(!push(&mut samples, Some(21.5), Some(20.0), at_secs(60)));
        // The target changed: sampled at once.
        assert!(push(&mut samples, Some(21.5), Some(21.0), at_secs(61)));
        // Five minutes on.
        assert!(push(&mut samples, Some(22.0), Some(21.0), at_secs(61 + 300)));
        assert_eq!(samples.len(), 3);
        // Older than the window is dropped.
        trim(&mut samples, at_secs(61 + 300 + 6 * 3600 + 1));
        assert!(samples.is_empty());
    }

    #[test]
    fn saved_histories_load_back() {
        let dir = std::env::temp_dir().join(format!("tileception-home-history-{}", std::process::id()));
        let path = dir.join("home-history.json");
        let recent = SystemTime::now() - Duration::from_secs(60);
        let mut map = HashMap::new();
        map.insert("dev".to_string(), vec![Sample { when: recent, temperature: Some(20.0), target: Some(21.0) }]);
        save(&path, &map).expect("saved");
        let loaded = load(&path);
        assert_eq!(loaded["dev"].len(), 1);
        assert_eq!(loaded["dev"][0].temperature, Some(20.0));
        // A sample older than the window does not come back.
        let mut old = HashMap::new();
        old.insert("dev".to_string(), vec![Sample { when: at_secs(0), temperature: Some(20.0), target: None }]);
        save(&path, &old).expect("saved");
        assert!(load(&path).is_empty());
        // A broken file is an empty history, not an error.
        std::fs::write(&path, "not json").expect("written");
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
