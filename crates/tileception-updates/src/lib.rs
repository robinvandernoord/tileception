//! What the update manager knows about the system: pending APT, Flatpak,
//! Snap and firmware updates, installed kernels and what can go, the next
//! Ubuntu release, and the apt history. Everything here reads; the commands that change the system are
//! built here and run by the compositor's worker, as root.
//!
//! Programs are run through a [`Runner`] and files read below a root folder,
//! so the parsers are tested on captured output.

pub mod apps;
pub mod apt;
pub mod automatic;
pub mod firmware;
pub mod history;
pub mod kernels;
mod mint;
pub mod mirrors;
pub mod release;
pub mod task;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// Runs a program and returns what it printed.
pub trait Runner {
    /// The program's standard output. An error when it could not start or
    /// failed, except for partial `dpkg-query` results and fwupd's
    /// no-updates status.
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
        // fwupdmgr uses 2 for "nothing to do", distinct from query failures.
        if program == "fwupdmgr" && args.first() == Some(&"get-updates") && output.status.code() == Some(2) {
            return Ok(String::new());
        }
        // dpkg-query can return installed matches while failing for unmatched
        // patterns. Other commands, especially APT simulations, must succeed.
        let partial = program == "dpkg-query" && !text.trim().is_empty();
        if !output.status.success() && !partial {
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
    /// APT updates, security first, then firmware. Flatpak and Snap are
    /// in `apps`, since listing them takes the network.
    pub updates: Vec<apt::Update>,
    /// Flatpak and Snap updates, when this look asked for them, and the
    /// sources among them that could not be asked.
    pub apps: Option<apps::Found>,
    /// Held packages that have a newer version.
    pub held: Vec<apt::Update>,
    /// What installing every update would do besides: removals, updates
    /// kept back or phased, and the dependencies that are no longer needed.
    pub plan: apt::Plan,
    /// Kernel images, headers and their support packages.
    pub packages: Vec<kernels::Package>,
    /// Every versioned kernel image and header package apt knows.
    pub offers: kernels::Offers,
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
    /// Firmware was asked for: `[updates] firmware` is on.
    pub firmware: bool,
    /// Why firmware could not be listed, independently of APT.
    pub firmware_error: Option<String>,
}

impl Snapshot {
    /// The kernels: the running flavour's, or every variant.
    pub fn kernels(&self, all_variants: bool) -> Vec<kernels::Kernel> {
        kernels::kernels(&self.packages, &self.offers, &self.running, all_variants)
    }

    pub fn totals(&self) -> apt::Totals {
        apt::totals(&self.updates)
    }
}

/// Reads the system. Nothing here needs root; only `network` lets Flatpak
/// and Snap ask their servers. fwupd is asked only with `firmware`.
pub fn scan(runner: &dyn Runner, files: &Files, network: bool, firmware: bool) -> Snapshot {
    let (mut listing, error) = match apt::list(runner) {
        Ok(listing) => (listing, None),
        Err(error) => (apt::Listing::default(), Some(error)),
    };
    let firmware_error = match firmware.then(|| firmware::list(runner)) {
        Some(Ok(updates)) => {
            listing.updates.extend(updates.unwrap_or_default());
            None
        }
        Some(Err(error)) => Some(error),
        None => None,
    };
    let apps = network.then(|| apps::list(runner));
    let running = runner.run("uname", &["-r"]).map(|text| text.trim().to_string()).unwrap_or_default();
    let packages = kernels::installed(runner);
    let offers = kernels::offers(runner);
    Snapshot {
        updates: listing.updates,
        apps,
        held: apt::held(runner),
        plan: listing.plan,
        packages,
        offers,
        running,
        system: release::system(files),
        restart: release::restart_required(files),
        checked: apt::last_refresh(files),
        history: history::read(files),
        error,
        firmware,
        firmware_error,
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

    /// Answers every program with nothing, and notes what was run.
    #[derive(Default)]
    struct Quiet {
        ran: std::cell::RefCell<Vec<String>>,
    }

    impl Runner for Quiet {
        fn run(&self, program: &str, _args: &[&str]) -> Result<String, String> {
            self.ran.borrow_mut().push(program.to_string());
            Ok(String::new())
        }
    }

    #[test]
    fn firmware_is_left_alone_unless_asked() {
        let runner = Quiet::default();
        let files = Files::below("/nonexistent");
        let snapshot = scan(&runner, &files, false, false);
        assert!(!snapshot.firmware);
        assert_eq!(snapshot.firmware_error, None);
        assert!(!runner.ran.borrow().iter().any(|program| program == "fwupdmgr"));
        assert!(snapshot.updates.iter().all(|update| update.source != apt::Source::Firmware));
        assert!(scan(&runner, &files, false, true).firmware);
    }

    #[test]
    fn files_resolve_below_their_root() {
        let files = Files::below("/tmp/elsewhere");
        assert_eq!(files.path("/etc/os-release"), PathBuf::from("/tmp/elsewhere/etc/os-release"));
    }
}
