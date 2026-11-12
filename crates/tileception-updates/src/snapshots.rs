//! A snapshot of the system before kernel, Mesa and release upgrades, the
//! updates most likely to leave a machine that does not start or draw.
//! Taken with Timeshift or Snapper when one is installed, else directly
//! where the root file system is btrfs. Elsewhere there is nothing to take.

use std::path::Path;
use std::process::{Command, Stdio};

/// What takes the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Timeshift,
    Snapper,
    /// A read-only btrfs snapshot of `/`, in [`BTRFS_FOLDER`].
    Btrfs,
}

/// Where btrfs snapshots go when neither Timeshift nor Snapper manages them.
pub const BTRFS_FOLDER: &str = "/.tileception-snapshots";

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Timeshift => "Timeshift",
            Tool::Snapper => "Snapper",
            Tool::Btrfs => "a btrfs snapshot",
        }
    }

    /// The commands that take a snapshot named after `reason`, as root.
    pub fn commands(self, reason: &str, stamp: &str) -> Vec<Vec<String>> {
        let comment = format!("Tileception: before {reason}");
        let strings = |parts: &[&str]| parts.iter().map(|part| (*part).to_string()).collect::<Vec<_>>();
        match self {
            Tool::Timeshift => vec![strings(&["timeshift", "--create", "--scripted", "--comments", &comment])],
            Tool::Snapper => vec![strings(&["snapper", "create", "--type", "single", "--description", &comment])],
            Tool::Btrfs => vec![
                strings(&["mkdir", "-p", BTRFS_FOLDER]),
                strings(&["btrfs", "subvolume", "snapshot", "-r", "/", &format!("{BTRFS_FOLDER}/{stamp}")]),
            ],
        }
    }
}

/// Whether the root file system is btrfs, from `/proc/self/mounts`.
pub fn btrfs_root(mounts: &str) -> bool {
    mounts.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.nth(1) == Some("/") && fields.next() == Some("btrfs")
    })
}

/// The tool available here, if any.
pub fn tool() -> Option<Tool> {
    let has = |path: &str| Path::new(path).exists();
    if has("/usr/bin/timeshift") {
        Some(Tool::Timeshift)
    } else if has("/usr/bin/snapper") && has("/etc/snapper/configs/root") {
        Some(Tool::Snapper)
    } else if has("/usr/bin/btrfs")
        && std::fs::read_to_string("/proc/self/mounts").is_ok_and(|mounts| btrfs_root(&mounts))
    {
        Some(Tool::Btrfs)
    } else {
        None
    }
}

/// Whether installing this package deserves a snapshot first: a kernel or
/// part of Mesa.
pub fn wants(name: &str) -> bool {
    const KERNEL: [&str; 4] = ["linux-image", "linux-modules", "linux-generic", "linux-hwe"];
    KERNEL.iter().any(|prefix| name.starts_with(prefix)) || name.contains("mesa") || name.starts_with("libgbm")
}

/// What a set of packages is, for the snapshot's comment.
pub fn reason(names: &[String]) -> Option<&'static str> {
    let kernel = names.iter().any(|name| wants(name) && name.starts_with("linux"));
    let mesa = names.iter().any(|name| wants(name) && !name.starts_with("linux"));
    match (kernel, mesa) {
        (true, true) => Some("a kernel and Mesa update"),
        (true, false) => Some("a kernel update"),
        (false, true) => Some("a Mesa update"),
        (false, false) => None,
    }
}

/// The time as `2026-11-12_09-21-33`, in UTC, for a snapshot's folder.
pub fn stamp() -> String {
    let output = Command::new("date")
        .args(["-u", "+%Y-%m-%d_%H-%M-%S"])
        .stdin(Stdio::null())
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    if output.is_empty() { "snapshot".into() } else { output }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernels_and_mesa_want_a_snapshot() {
        assert!(wants("linux-image-6.14.0-36-generic"));
        assert!(wants("libgl1-mesa-dri"));
        assert!(wants("mesa-vulkan-drivers"));
        assert!(wants("libgbm1"));
        assert!(!wants("linux-firmware") && !wants("curl"));
        let names = |list: &[&str]| list.iter().map(|name| (*name).to_string()).collect::<Vec<_>>();
        assert_eq!(reason(&names(&["curl", "linux-image-6.14.0-36-generic"])), Some("a kernel update"));
        assert_eq!(reason(&names(&["libegl-mesa0"])), Some("a Mesa update"));
        assert_eq!(reason(&names(&["curl"])), None);
    }

    #[test]
    fn a_btrfs_root_is_seen() {
        assert!(btrfs_root("/dev/nvme0n1p2 / btrfs rw,relatime,subvol=/@ 0 0\nproc /proc proc rw 0 0\n"));
        assert!(!btrfs_root("/dev/sda2 / ext4 rw 0 0\n/dev/sdb1 /data btrfs rw 0 0\n"));
    }

    #[test]
    fn each_tool_has_its_commands() {
        let timeshift = Tool::Timeshift.commands("a kernel update", "x");
        assert_eq!(timeshift[0][..3], ["timeshift", "--create", "--scripted"]);
        assert_eq!(timeshift[0].last().map(String::as_str), Some("Tileception: before a kernel update"));
        let btrfs = Tool::Btrfs.commands("a Mesa update", "2026-11-12_09-21-33");
        assert_eq!(btrfs[1].last().map(String::as_str), Some("/.tileception-snapshots/2026-11-12_09-21-33"));
    }
}
