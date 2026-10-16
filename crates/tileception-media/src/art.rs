//! Cover art that players serve over http(s), fetched in the background with
//! curl into a small cache, so the compositor needs no HTTP stack of its own.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

/// Files kept in the cache; older ones go first.
const KEEP: usize = 64;
const MAX_BYTES: &str = "8000000";
const MAX_SECONDS: &str = "8";

pub struct Fetcher {
    requests: Option<mpsc::Sender<String>>,
    /// Asked for already, fetched or not: failures are not retried.
    asked: HashSet<String>,
    dir: PathBuf,
}

impl Fetcher {
    /// `done` is called with each URL once its file is in place.
    pub fn start(dir: PathBuf, done: impl Fn(String) + Send + 'static) -> Self {
        let (requests, incoming) = mpsc::channel::<String>();
        let worker_dir = dir.clone();
        let started = thread::Builder::new().name("tileception-art".into()).spawn(move || {
            for url in incoming {
                match fetch(&worker_dir, &url) {
                    Ok(()) => done(url),
                    Err(error) => tracing::debug!("no cover art from {url}: {error}"),
                }
            }
        });
        let requests = match started {
            Ok(_) => Some(requests),
            Err(error) => {
                tracing::warn!("cover art downloads unavailable: {error}");
                None
            }
        };
        Self { requests, asked: HashSet::new(), dir }
    }

    /// The local file for a cover art URL: `file://` as it is, http(s) from
    /// the cache, starting a download when it is not there yet.
    pub fn get(&mut self, url: &str) -> Option<PathBuf> {
        if let Some(path) = crate::mpris::local_path(url) {
            return Some(path);
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return None;
        }
        let path = file_for(&self.dir, url);
        if path.exists() {
            return Some(path);
        }
        if self.asked.insert(url.to_string())
            && let Some(requests) = &self.requests
        {
            let _ = requests.send(url.to_string());
        }
        None
    }
}

/// Where a URL's image is cached.
pub fn file_for(dir: &Path, url: &str) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    dir.join(format!("{:016x}", hasher.finish()))
}

pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    base.join("tileception/art")
}

fn fetch(dir: &Path, url: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = file_for(dir, url);
    let partial = path.with_extension("part");
    // Only http(s), also after redirects; bounded in size and time.
    let status = Command::new("curl")
        .args(["--fail", "--silent", "--location", "--max-redirs", "3"])
        .args([
            "--user-agent",
            concat!("Tileception/", env!("CARGO_PKG_VERSION"), " (cover art; +https://tileception.space)"),
        ])
        .args(["--proto", "=https,http", "--proto-redir", "=https,http"])
        .args(["--max-filesize", MAX_BYTES, "--max-time", MAX_SECONDS, "--output"])
        .arg(&partial)
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("curl exited with {status}"));
    }
    std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    prune(dir);
    Ok(())
}

/// Keeps the newest few files.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<_> =
        entries.filter_map(Result::ok).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    if files.len() <= KEEP {
        return;
    }
    files.sort();
    for (_, path) in &files[..files.len() - KEEP] {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_by_url_and_keeps_local_files() {
        let dir = Path::new("/cache");
        assert_eq!(file_for(dir, "https://a/1.jpg"), file_for(dir, "https://a/1.jpg"));
        assert_ne!(file_for(dir, "https://a/1.jpg"), file_for(dir, "https://a/2.jpg"));
        let mut fetcher = Fetcher { requests: None, asked: HashSet::new(), dir: dir.into() };
        assert_eq!(fetcher.get("file:///tmp/a.png"), Some(PathBuf::from("/tmp/a.png")));
        assert_eq!(fetcher.get("ftp://host/a.png"), None);
        assert_eq!(fetcher.get("https://example.invalid/a.png"), None);
        assert!(fetcher.asked.contains("https://example.invalid/a.png"));
    }

    #[test]
    fn prunes_the_oldest_files() {
        let dir = std::env::temp_dir().join(format!("tileception-art-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create cache");
        for i in 0..KEEP + 3 {
            std::fs::write(dir.join(format!("{i:03}")), b"x").expect("write file");
            let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64);
            std::fs::File::options()
                .write(true)
                .open(dir.join(format!("{i:03}")))
                .and_then(|f| f.set_modified(when))
                .expect("set time");
        }
        prune(&dir);
        let left = std::fs::read_dir(&dir).expect("read cache").count();
        assert_eq!(left, KEEP);
        assert!(!dir.join("000").exists() && dir.join(format!("{:03}", KEEP + 2)).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
