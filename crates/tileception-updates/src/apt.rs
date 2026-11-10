//! Pending updates, read from an `apt-get` simulation, and the commands that
//! install them.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::{Files, Runner};

/// What an update is, for its badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Kernel,
    /// Tileception itself.
    Desktop,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub name: String,
    /// The installed version; none for a package the update pulls in.
    pub old: Option<String>,
    pub new: String,
    /// Where it comes from, as apt names it: `Ubuntu:24.04/noble-security`.
    pub origin: String,
    pub security: bool,
    pub kind: Kind,
    /// Bytes still to download.
    pub size: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub count: usize,
    pub security: usize,
    pub bytes: u64,
}

pub fn totals(updates: &[Update]) -> Totals {
    Totals {
        count: updates.len(),
        security: updates.iter().filter(|update| update.security).count(),
        bytes: updates.iter().map(|update| update.size).sum(),
    }
}

fn kind_of(name: &str) -> Kind {
    const KERNEL: [&str; 6] =
        ["linux-image", "linux-headers", "linux-modules", "linux-generic", "linux-tools", "linux-hwe"];
    if KERNEL.iter().any(|prefix| name.starts_with(prefix)) {
        Kind::Kernel
    } else if name.starts_with("tileception") {
        Kind::Desktop
    } else {
        Kind::System
    }
}

/// The `Inst` lines of `apt-get -s dist-upgrade`:
/// `Inst apt [2.8.3] (2.8.4 Ubuntu:24.04/noble-updates, Ubuntu:24.04/noble-security [amd64])`.
pub fn parse_simulation(text: &str) -> Vec<Update> {
    text.lines().filter_map(parse_inst).collect()
}

fn parse_inst(line: &str) -> Option<Update> {
    let rest = line.strip_prefix("Inst ")?;
    let (name, rest) = rest.split_once(' ')?;
    let rest = rest.trim_start();
    let (old, rest) = match rest.strip_prefix('[') {
        Some(inner) => {
            let (old, after) = inner.split_once(']')?;
            (Some(old.to_string()), after.trim_start())
        }
        None => (None, rest),
    };
    let inner = rest.strip_prefix('(')?;
    let inner = &inner[..inner.rfind(')')?];
    let (new, origin) = inner.split_once(' ').unwrap_or((inner, ""));
    // The architecture closes the list of origins.
    let origin = origin.rfind(" [").map_or(origin, |end| &origin[..end]).trim();
    let security = origin.split(", ").any(|part| part.contains("-security") || part.contains("Debian-Security"));
    Some(Update {
        name: name.to_string(),
        old,
        new: new.to_string(),
        origin: origin.to_string(),
        security,
        kind: kind_of(name),
        size: 0,
    })
}

/// Download sizes by package, from `apt-get --print-uris`:
/// `'http://…/apt_2.8.4_amd64.deb' apt_2.8.4_amd64.deb 1379000 MD5Sum:…`.
pub fn parse_uris(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            fields.next().filter(|uri| uri.starts_with('\''))?;
            let file = fields.next()?;
            let size = fields.next()?.parse().ok()?;
            Some((file.split('_').next()?.to_string(), size))
        })
        .collect()
}

/// The pending updates: security first, then by name.
pub fn list(runner: &dyn Runner) -> Result<Vec<Update>, String> {
    let simulation = runner.run("apt-get", &["-s", "-o", "Debug::NoLocking=true", "dist-upgrade"])?;
    let mut updates = parse_simulation(&simulation);
    if !updates.is_empty() {
        // Sizes are a nicety: without them the list still stands.
        let sizes = runner
            .run("apt-get", &["--print-uris", "-qq", "-o", "Debug::NoLocking=true", "dist-upgrade"])
            .map(|text| parse_uris(&text))
            .unwrap_or_default();
        for update in &mut updates {
            update.size = sizes.get(&update.name).copied().unwrap_or(0);
        }
    }
    updates.sort_by(|left, right| right.security.cmp(&left.security).then_with(|| left.name.cmp(&right.name)));
    Ok(updates)
}

/// When the package lists were last refreshed.
pub fn last_refresh(files: &Files) -> Option<SystemTime> {
    ["/var/lib/apt/periodic/update-success-stamp", "/var/cache/apt/pkgcache.bin", "/var/lib/apt/lists"]
        .iter()
        .find_map(|path| std::fs::metadata(files.path(path)).and_then(|meta| meta.modified()).ok())
}

/// Whether this is a package name apt would take as one, never as an option.
pub fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters.next().is_some_and(|first| first.is_ascii_alphanumeric())
        && characters.all(|character| character.is_ascii_alphanumeric() || "+-.:_~".contains(character))
}

fn checked(names: &[String]) -> Result<(), String> {
    match names.iter().find(|name| !valid_name(name)) {
        Some(name) => Err(format!("{name:?} is not a package name")),
        None => Ok(()),
    }
}

/// Keeps changed configuration files as they are, so nothing asks a question.
const QUIET: [&str; 5] = ["-y", "-o", "Dpkg::Options::=--force-confdef", "-o", "Dpkg::Options::=--force-confold"];

/// `apt-get` arguments that install these updates, or all of them for none.
pub fn upgrade_args(names: &[String]) -> Result<Vec<String>, String> {
    checked(names)?;
    let mut args: Vec<String> = QUIET.iter().map(|arg| (*arg).to_string()).collect();
    if names.is_empty() {
        args.push("dist-upgrade".into());
    } else {
        args.extend(["install".to_string(), "--only-upgrade".to_string()]);
        args.extend(names.iter().cloned());
    }
    Ok(args)
}

/// `apt-get` arguments that install packages not installed yet.
pub fn install_args(names: &[String]) -> Result<Vec<String>, String> {
    checked(names)?;
    if names.is_empty() {
        return Err("nothing to install".into());
    }
    let mut args: Vec<String> = QUIET.iter().map(|arg| (*arg).to_string()).collect();
    args.push("install".into());
    args.extend(names.iter().cloned());
    Ok(args)
}

/// `apt-get` arguments that remove exactly these packages, with their
/// configuration; `simulate` only prints what would happen.
pub fn purge_args(names: &[String], simulate: bool) -> Result<Vec<String>, String> {
    checked(names)?;
    if names.is_empty() {
        return Err("nothing to remove".into());
    }
    let mut args = vec![if simulate { "-s" } else { "-y" }.to_string(), "purge".to_string()];
    args.extend(names.iter().cloned());
    Ok(args)
}

/// A step `apt-get` reports while it works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Download(String),
    Unpack(String),
    /// A package is in place: these count towards the total.
    Setup(String),
    Remove(String),
}

impl Progress {
    pub fn label(&self) -> String {
        match self {
            Progress::Download(name) => format!("downloading {name}"),
            Progress::Unpack(name) => format!("unpacking {name}"),
            Progress::Setup(name) => format!("setting up {name}"),
            Progress::Remove(name) => format!("removing {name}"),
        }
    }
}

pub fn parse_progress(line: &str) -> Option<Progress> {
    let line = line.trim();
    let named = |rest: &str| rest.split([' ', ':']).next().filter(|name| !name.is_empty()).map(str::to_string);
    if let Some(rest) = line.strip_prefix("Unpacking ") {
        named(rest).map(Progress::Unpack)
    } else if let Some(rest) = line.strip_prefix("Setting up ") {
        named(rest).map(Progress::Setup)
    } else if let Some(rest) = line.strip_prefix("Removing ") {
        named(rest).map(Progress::Remove)
    } else if line.starts_with("Get:") {
        // Get:3 http://archive.ubuntu.com/ubuntu noble-updates/main amd64 apt amd64 2.8.4 [1379 kB]
        line.split_whitespace().nth(4).map(|name| Progress::Download(name.to_string()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMULATION: &str = "\
Reading package lists...
The following packages will be upgraded:
  apt docker-ce libpcre2-dev
3 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.
Inst apt [2.8.3] (2.8.4ubuntu0.1 Ubuntu:24.04/noble-updates, Ubuntu:24.04/noble-security [amd64])
Conf apt (2.8.4ubuntu0.1 Ubuntu:24.04/noble-updates, Ubuntu:24.04/noble-security [amd64])
Inst docker-ce [5:29.9.2-1~ubuntu.24.04~noble] (5:29.9.3-1~ubuntu.24.04~noble Docker CE:noble [amd64])
Inst libpcre2-dev [10.42-4ubuntu2.1] (10.42-4ubuntu2.2 Ubuntu:24.04/noble-updates [amd64]) []
Inst linux-image-6.8.0-149-generic (6.8.0-149.149 Ubuntu:24.04/noble-updates [amd64])
Inst tileception [0.12.0] (0.13.0 apt.tileception.space:stable [amd64])
";

    #[test]
    fn simulation_lines_become_updates() {
        let updates = parse_simulation(SIMULATION);
        assert_eq!(updates.len(), 5);
        assert_eq!(updates[0].name, "apt");
        assert_eq!(updates[0].old.as_deref(), Some("2.8.3"));
        assert_eq!(updates[0].new, "2.8.4ubuntu0.1");
        assert!(updates[0].security);
        assert_eq!(updates[1].origin, "Docker CE:noble");
        assert!(!updates[1].security && !updates[2].security);
        assert_eq!(updates[3].old, None);
        assert_eq!(updates[3].kind, Kind::Kernel);
        assert_eq!(updates[4].kind, Kind::Desktop);
        assert_eq!(totals(&updates), Totals { count: 5, security: 1, bytes: 0 });
    }

    #[test]
    fn uris_give_download_sizes() {
        let sizes = parse_uris(
            "'https://download.docker.com/x/docker-ce_29.9.3_amd64.deb' docker-ce_5%3a29.9.3-1~noble_amd64.deb 24586336 MD5Sum:e0\n\
             'http://archive.ubuntu.com/a/apt_2.8.4_amd64.deb' apt_2.8.4ubuntu0.1_amd64.deb 1379000 MD5Sum:63\n\
             E: something else\n",
        );
        assert_eq!(sizes.get("docker-ce"), Some(&24_586_336));
        assert_eq!(sizes.get("apt"), Some(&1_379_000));
        assert_eq!(sizes.len(), 2);
    }

    #[test]
    fn commands_refuse_what_is_not_a_package() {
        assert!(upgrade_args(&["--allow-unauthenticated".into()]).is_err());
        assert!(purge_args(&["linux; rm".into()], true).is_err());
        assert!(purge_args(&[], false).is_err());
        let all = upgrade_args(&[]).unwrap_or_default();
        assert_eq!(all.last().map(String::as_str), Some("dist-upgrade"));
        let some = upgrade_args(&["libssl3t64".into(), "g++-13".into()]).unwrap_or_default();
        assert_eq!(&some[some.len() - 4..], ["install", "--only-upgrade", "libssl3t64", "g++-13"]);
        let purge = purge_args(&["linux-image-6.8.0-85-generic".into()], true).unwrap_or_default();
        assert_eq!(purge, ["-s", "purge", "linux-image-6.8.0-85-generic"]);
    }

    #[test]
    fn progress_lines() {
        assert_eq!(
            parse_progress("Get:3 http://archive.ubuntu.com/ubuntu noble-updates/main amd64 apt amd64 2.8.4 [1379 kB]"),
            Some(Progress::Download("apt".into()))
        );
        assert_eq!(
            parse_progress("Unpacking apt (2.8.4ubuntu0.1) over (2.8.3) ..."),
            Some(Progress::Unpack("apt".into()))
        );
        assert_eq!(
            parse_progress("Setting up libssl3t64:amd64 (3.0.13-0ubuntu3.6) ..."),
            Some(Progress::Setup("libssl3t64".into()))
        );
        assert_eq!(
            parse_progress("Removing linux-image-6.8.0-85-generic (6.8.0-85.85) ..."),
            Some(Progress::Remove("linux-image-6.8.0-85-generic".into()))
        );
        assert_eq!(parse_progress("Processing triggers for man-db ..."), None);
    }
}
