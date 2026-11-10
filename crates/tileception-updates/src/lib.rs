//! What the update manager knows about the system: pending APT updates,
//! installed kernels and what can go, the next Ubuntu release, and the apt
//! history. Everything here reads; the commands that change the system are
//! built here and run by the compositor's worker, as root.
//!
//! Programs are run through a [`Runner`] and files read below a root folder,
//! so the parsers are tested on captured output.

pub mod apt;
pub mod history;
pub mod kernels;
pub mod release;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// Runs a program and returns what it printed.
pub trait Runner {
    /// The program's standard output. An error when it could not start, or
    /// failed without printing anything.
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

/// Runs programs from `PATH`, in the C locale so their output parses.
pub struct System;

impl Runner for System {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("{program}: {error}"))?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() && text.trim().is_empty() {
            let reason = String::from_utf8_lossy(&output.stderr);
            let reason = reason.lines().rfind(|line| !line.trim().is_empty()).unwrap_or("failed").trim().to_string();
            return Err(format!("{program}: {reason}"));
        }
        Ok(text)
    }
}

/// Where system files are read from: `/`, or a folder standing in for it.
#[derive(Debug, Clone)]
pub struct Files {
    root: PathBuf,
}

impl Default for Files {
    fn default() -> Self {
        Files { root: PathBuf::from("/") }
    }
}

impl Files {
    pub fn below(root: impl Into<PathBuf>) -> Files {
        Files { root: root.into() }
    }

    /// The place of an absolute path below the root.
    pub fn path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }

    pub fn read(&self, absolute: &str) -> Option<String> {
        std::fs::read_to_string(self.path(absolute)).ok()
    }

    pub fn exists(&self, absolute: &str) -> bool {
        self.path(absolute).exists()
    }
}

/// Whether this system is managed with APT.
pub fn supported() -> bool {
    ["/usr/bin/apt-get", "/bin/apt-get"].iter().any(|path| Path::new(path).exists())
}

/// Everything read in one pass.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub updates: Vec<apt::Update>,
    /// Kernel images, headers and their support packages.
    pub packages: Vec<kernels::Package>,
    /// Kernel images apt offers that are newer than the running one.
    pub offered: Vec<String>,
    /// `uname -r`.
    pub running: String,
    pub system: release::System,
    /// A restart is needed to finish earlier updates.
    pub restart: bool,
    /// When the package lists were last refreshed.
    pub checked: Option<SystemTime>,
    pub history: Vec<history::Entry>,
    /// Why the updates could not be listed.
    pub error: Option<String>,
}

impl Snapshot {
    pub fn kernels(&self) -> Vec<kernels::Kernel> {
        kernels::kernels(&self.packages, &self.offered, &self.running)
    }

    pub fn totals(&self) -> apt::Totals {
        apt::totals(&self.updates)
    }
}

/// Reads the system. Nothing here needs root or the network.
pub fn scan(runner: &dyn Runner, files: &Files) -> Snapshot {
    let (updates, error) = match apt::list(runner) {
        Ok(updates) => (updates, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let running = runner.run("uname", &["-r"]).map(|text| text.trim().to_string()).unwrap_or_default();
    let packages = kernels::installed(runner);
    let offered = kernels::offered(runner, &packages, &running);
    Snapshot {
        updates,
        packages,
        offered,
        running,
        system: release::system(files),
        restart: release::restart_required(files),
        checked: apt::last_refresh(files),
        history: history::read(files),
        error,
    }
}

/// A size for people: "96 MB", "1.4 GB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.5 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 9.95 { format!("{value:.1} {}", UNITS[unit]) } else { format!("{value:.0} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2_100_000), "2.1 MB");
        assert_eq!(human_size(96_000_000), "96 MB");
        assert_eq!(human_size(1_400_000_000), "1.4 GB");
        assert_eq!(human_size(999_900), "1.0 MB");
    }

    #[test]
    fn files_resolve_below_their_root() {
        let files = Files::below("/tmp/elsewhere");
        assert_eq!(files.path("/etc/os-release"), PathBuf::from("/tmp/elsewhere/etc/os-release"));
    }
}
