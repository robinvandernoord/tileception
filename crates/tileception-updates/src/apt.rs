//! Pending updates, read from an `apt-get` simulation, and the commands that
//! install them.

use std::collections::{HashMap, HashSet};
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

/// What a transaction does besides the updates it installs, read from the
/// lists `apt-get` prints before it starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Packages already installed that get a newer version.
    pub upgraded: Vec<String>,
    /// Packages the transaction brings in.
    pub installed: Vec<String>,
    /// Packages the transaction takes away.
    pub removed: Vec<String>,
    /// Updates apt leaves out because installing them needs more than an
    /// upgrade may do.
    pub kept_back: Vec<String>,
    /// Updates the archive hands out to a share of the machines at a time.
    pub phased: Vec<String>,
    /// Installed as a dependency and needed by nothing any more: what
    /// `autoremove` would take.
    pub unneeded: Vec<String>,
}

impl Plan {
    /// Whether the transaction does more than install what was asked for.
    pub fn removes(&self) -> bool {
        !self.removed.is_empty()
    }

    /// "installs 3, removes 1", for a line of status.
    pub fn summary(&self) -> String {
        let parts: Vec<String> = [
            (&self.upgraded, "updates"),
            (&self.installed, "installs"),
            (&self.removed, "removes"),
            (&self.kept_back, "keeps back"),
            (&self.phased, "defers"),
        ]
        .iter()
        .filter(|(names, _)| !names.is_empty())
        .map(|(names, verb)| format!("{verb} {}", names.len()))
        .collect();
        if parts.is_empty() { "changes nothing".into() } else { parts.join(", ") }
    }
}

/// The package lists of an `apt-get` run or simulation: each starts with a
/// sentence ending in a colon and holds indented names. A purge marks what
/// goes with a star.
pub fn parse_plan(text: &str) -> Plan {
    let mut plan = Plan::default();
    let mut list: Option<&mut Vec<String>> = None;
    for line in text.lines() {
        if !line.starts_with(' ') {
            list = if !line.ends_with(':') {
                None
            } else if line.contains("will be REMOVED") {
                Some(&mut plan.removed)
            } else if line.contains("NEW packages will be installed") {
                Some(&mut plan.installed)
            } else if line.contains("will be upgraded") {
                Some(&mut plan.upgraded)
            } else if line.contains("kept back") {
                Some(&mut plan.kept_back)
            } else if line.contains("deferred due to phasing") {
                Some(&mut plan.phased)
            } else if line.contains("no longer required") {
                Some(&mut plan.unneeded)
            } else {
                None
            };
            continue;
        }
        if let Some(list) = list.as_deref_mut() {
            list.extend(line.split_whitespace().map(|name| name.trim_end_matches('*').to_string()));
        }
    }
    plan
}

/// Whether what `apt-get` or `dpkg` printed says the package system must be
/// repaired before anything else can run.
pub fn needs_repair(output: &str) -> bool {
    ["dpkg --configure -a", "--fix-broken", "Unmet dependencies", "dpkg was interrupted"]
        .iter()
        .any(|sign| output.contains(sign))
}

/// The pending updates, and what installing all of them would do besides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Security first, then by name.
    pub updates: Vec<Update>,
    pub plan: Plan,
}

/// The pending updates: security first, then by name.
pub fn list(runner: &dyn Runner) -> Result<Listing, String> {
    let simulation = runner.run("apt-get", &["-s", "-o", "Debug::NoLocking=true", "dist-upgrade"])?;
    let plan = parse_plan(&simulation);
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
    Ok(Listing { updates, plan })
}

/// When the package lists were last refreshed: the newest of the marks
/// that the daily timer and a refresh by hand leave behind.
pub fn last_refresh(files: &Files) -> Option<SystemTime> {
    [
        "/var/lib/apt/periodic/update-success-stamp",
        "/var/cache/apt/pkgcache.bin",
        "/var/lib/apt/lists",
        "/var/lib/apt/lists/partial",
        "/var/lib/apt/lists/lock",
    ]
    .iter()
    .filter_map(|path| std::fs::metadata(files.path(path)).and_then(|meta| meta.modified()).ok())
    .max()
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

/// How `apt-get` arguments start: answering every question for a real run,
/// or `-s` to only print what would happen.
fn start(simulate: bool) -> Vec<String> {
    if simulate { vec!["-s".into()] } else { QUIET.iter().map(|arg| (*arg).to_string()).collect() }
}

/// `apt-get` arguments that install these updates, or all of them for none;
/// `simulate` only prints what would happen.
pub fn upgrade_args(names: &[String], simulate: bool) -> Result<Vec<String>, String> {
    checked(names)?;
    let mut args = start(simulate);
    if names.is_empty() {
        args.push("dist-upgrade".into());
    } else {
        // A dist-upgrade can introduce packages, such as versioned kernels.
        args.push("install".to_string());
        args.extend(names.iter().cloned());
    }
    Ok(args)
}

/// The packages among `names` to mark as automatically installed again after
/// `apt-get install` named them, which marks each as installed by hand:
/// those `apt-mark showmanual` does not list. Otherwise ticking an update
/// would keep a dependency out of every later `autoremove`.
pub fn automatic(names: &[String], manual: &str) -> Vec<String> {
    let manual: HashSet<&str> = manual.lines().map(str::trim).collect();
    names.iter().filter(|name| !manual.contains(name.as_str())).cloned().collect()
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

/// `apt-get` arguments for one transaction that installs some packages and
/// removes others (`name-`), as kerntop queues them. `purge` also removes the
/// configuration of what goes; `simulate` only prints what would happen.
pub fn transaction_args(
    installs: &[String],
    removals: &[String],
    purge: bool,
    simulate: bool,
) -> Result<Vec<String>, String> {
    checked(installs)?;
    checked(removals)?;
    if installs.is_empty() && removals.is_empty() {
        return Err("nothing is queued".into());
    }
    let mut args = start(simulate);
    args.push("install".into());
    if purge {
        args.push("--purge".into());
    }
    args.extend(installs.iter().cloned());
    args.extend(removals.iter().map(|name| format!("{name}-")));
    Ok(args)
}

/// `apt-get` arguments that remove the dependencies that are no longer needed.
pub fn autoremove_args(simulate: bool) -> Vec<String> {
    let mut args = start(simulate);
    args.push("autoremove".into());
    args
}

/// `apt-get` arguments that settle broken dependencies.
pub fn repair_args(simulate: bool) -> Vec<String> {
    let mut args = start(simulate);
    args.extend(["--fix-broken".to_string(), "install".to_string()]);
    args
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
        assert!(upgrade_args(&["--allow-unauthenticated".into()], false).is_err());
        assert!(purge_args(&["linux; rm".into()], true).is_err());
        assert!(purge_args(&[], false).is_err());
        let all = upgrade_args(&[], false).unwrap_or_default();
        assert_eq!(all.last().map(String::as_str), Some("dist-upgrade"));
        let some = upgrade_args(&["libssl3t64".into(), "g++-13".into()], false).unwrap_or_default();
        assert_eq!(&some[some.len() - 3..], ["install", "libssl3t64", "g++-13"]);
        assert_eq!(upgrade_args(&["curl".into()], true).unwrap_or_default(), ["-s", "install", "curl"]);
        assert_eq!(autoremove_args(true), ["-s", "autoremove"]);
        assert_eq!(autoremove_args(false).last().map(String::as_str), Some("autoremove"));
        assert_eq!(repair_args(true), ["-s", "--fix-broken", "install"]);
        let purge = purge_args(&["linux-image-6.8.0-85-generic".into()], true).unwrap_or_default();
        assert_eq!(purge, ["-s", "purge", "linux-image-6.8.0-85-generic"]);
        let both = transaction_args(
            &["linux-image-6.14.0-36-generic".into()],
            &["linux-image-6.8.0-85-generic".into()],
            true,
            true,
        )
        .unwrap_or_default();
        assert_eq!(
            both,
            ["-s", "install", "--purge", "linux-image-6.14.0-36-generic", "linux-image-6.8.0-85-generic-"]
        );
        assert!(transaction_args(&[], &[], false, true).is_err());
        assert!(transaction_args(&[], &["-oops".into()], false, true).is_err());
    }

    const PLAN: &str = "\
Reading package lists...
Building dependency tree...
Reading state information...
Calculating upgrade...
The following packages were automatically installed and are no longer required:
  libllvm17t64 linux-headers-6.8.0-85
  python3-netifaces
Use 'apt autoremove' to remove them.
The following packages will be REMOVED:
  libfoo1* old-thing
The following NEW packages will be installed:
  linux-image-6.8.0-149-generic
The following upgrades have been deferred due to phasing:
  gnome-shell mutter-common
The following packages have been kept back:
  docker-ce
The following packages will be upgraded:
  apt libpcre2-dev
2 upgraded, 1 newly installed, 2 to remove and 3 not upgraded.
Inst apt [2.8.3] (2.8.4 Ubuntu:24.04/noble-updates [amd64])
";

    #[test]
    fn a_simulation_says_everything_it_does() {
        let plan = parse_plan(PLAN);
        assert_eq!(plan.unneeded, ["libllvm17t64", "linux-headers-6.8.0-85", "python3-netifaces"]);
        assert_eq!(plan.removed, ["libfoo1", "old-thing"]);
        assert_eq!(plan.installed, ["linux-image-6.8.0-149-generic"]);
        assert_eq!(plan.phased, ["gnome-shell", "mutter-common"]);
        assert_eq!(plan.kept_back, ["docker-ce"]);
        assert_eq!(plan.upgraded, ["apt", "libpcre2-dev"]);
        assert!(plan.removes());
        assert_eq!(plan.summary(), "updates 2, installs 1, removes 2, keeps back 1, defers 2");
        let nothing = parse_plan(SIMULATION.replace("will be upgraded", "are fine").as_str());
        assert_eq!(nothing, Plan::default());
        assert_eq!(nothing.summary(), "changes nothing");
    }

    #[test]
    fn broken_package_states_ask_for_repair() {
        assert!(needs_repair(
            "E: dpkg was interrupted, you must manually run 'dpkg --configure -a' to correct the problem."
        ));
        assert!(needs_repair("E: Unmet dependencies. Try 'apt --fix-broken install' with no packages"));
        assert!(!needs_repair("E: Unable to locate package nothing"));
    }

    #[test]
    fn dependencies_stay_automatic() {
        let names = ["libssl3t64".to_string(), "firefox".to_string(), "linux-image-6.8.0-50-generic".to_string()];
        assert_eq!(automatic(&names, "firefox\nvim\n"), ["libssl3t64", "linux-image-6.8.0-50-generic"]);
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
