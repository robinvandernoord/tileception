//! Pending updates, read from an `apt-get` simulation, and the commands that
//! install them.

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use crate::{Files, Runner};

/// What an update is, for its badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    Kernel,
    /// Tileception itself.
    Desktop,
    #[default]
    System,
    /// An app from Flatpak or Snap.
    App,
    Firmware,
}

/// Where an update comes from, and so what installs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub enum Source {
    #[default]
    Apt,
    /// A Flatpak in the system installation.
    Flatpak,
    /// A Flatpak installed for this user alone.
    FlatpakUser,
    Snap,
    /// A device's firmware, through fwupd.
    Firmware,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Apt => "APT",
            Source::Flatpak | Source::FlatpakUser => "Flatpak",
            Source::Snap => "Snap",
            Source::Firmware => "Firmware",
        }
    }

    /// How commands and saved state write it: `apt`, `flatpak`,
    /// `flatpak-user`, `snap` or `firmware`.
    pub fn ident(self) -> &'static str {
        match self {
            Source::Apt => "apt",
            Source::Flatpak => "flatpak",
            Source::FlatpakUser => "flatpak-user",
            Source::Snap => "snap",
            Source::Firmware => "firmware",
        }
    }

    pub fn from_ident(ident: &str) -> Option<Source> {
        [Source::Apt, Source::Flatpak, Source::FlatpakUser, Source::Snap, Source::Firmware]
            .into_iter()
            .find(|source| source.ident() == ident)
    }
}

/// What tells an update apart from the rest: the same name can come from
/// two sources, such as a Flatpak in both installations, or a package and
/// a snap.
pub type Key = (Source, String);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Update {
    /// What the source installs it by: a package, a Flatpak ref
    /// (`org.mozilla.firefox//stable`), a snap or a device ID.
    pub name: String,
    pub source: Source,
    /// A name for people where `name` is not one: the app or the device.
    pub title: String,
    /// The source package an APT package is built from; empty elsewhere.
    pub package: String,
    /// One line about what it is.
    pub summary: String,
    /// The installed version; none for a package the update pulls in.
    pub old: Option<String>,
    pub new: String,
    /// The source's update identity when the displayed version is not unique.
    pub identity: String,
    /// Where it comes from, as apt names it: `Ubuntu:24.04/noble-security`.
    pub origin: String,
    pub security: bool,
    pub kind: Kind,
    /// Bytes still to download.
    pub size: u64,
}

impl Update {
    /// What a skip matches, independently of the version shown to people.
    pub fn skip_identity(&self) -> &str {
        if self.identity.is_empty() { &self.new } else { &self.identity }
    }

    /// The name to show: the app or device, else the package.
    pub fn label(&self) -> &str {
        if self.title.is_empty() { &self.name } else { &self.title }
    }

    pub fn key(&self) -> Key {
        (self.source, self.name.clone())
    }

    /// Its name with the source in front, as commands take it when the
    /// name alone is not enough: `snap:firefox`.
    pub fn qualified(&self) -> String {
        format!("{}:{}", self.source.ident(), self.name)
    }

    /// The source package it belongs to, or its own name.
    pub fn group(&self) -> &str {
        if self.package.is_empty() { &self.name } else { &self.package }
    }
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
        ..Update::default()
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

/// What `dpkg-query` says of installed packages: name, source package and
/// summary, one per line separated by tabs.
pub fn parse_descriptions(text: &str) -> HashMap<String, (String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.trim();
            let source = fields.next().unwrap_or_default().trim();
            let summary = fields.next().unwrap_or_default().trim();
            // `source:Package` can carry a version: `glibc (2.39-0ubuntu8)`.
            let source = source.split_whitespace().next().unwrap_or(name);
            (!name.is_empty()).then(|| (name.to_string(), (source.to_string(), summary.to_string())))
        })
        .collect()
}

/// Fills in each update's source package and summary. A package that is
/// not installed yet keeps its own name as its group.
pub fn describe(runner: &dyn Runner, updates: &mut [Update]) {
    let names: Vec<&str> = updates.iter().map(|update| update.name.as_str()).collect();
    if names.is_empty() {
        return;
    }
    let mut args = vec!["-W", "-f=${Package}\t${source:Package}\t${binary:Summary}\n"];
    args.extend(names);
    let known = runner.run("dpkg-query", &args).map(|text| parse_descriptions(&text)).unwrap_or_default();
    for update in updates {
        if let Some((source, summary)) = known.get(&update.name) {
            update.package = source.clone();
            update.summary = summary.clone();
        }
    }
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
    describe(runner, &mut updates);
    updates.sort_by(|left, right| right.security.cmp(&left.security).then_with(|| left.name.cmp(&right.name)));
    Ok(Listing { updates, plan })
}

/// `apt-cache policy` for some packages: each one's installed and candidate
/// versions, where both are known.
pub fn parse_policy(text: &str) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    let mut name: Option<&str> = None;
    let mut installed: Option<&str> = None;
    for line in text.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            name = line.strip_suffix(':');
            installed = None;
            continue;
        }
        let line = line.trim();
        if let Some(version) = line.strip_prefix("Installed: ") {
            installed = Some(version).filter(|version| *version != "(none)");
        } else if let Some(candidate) = line.strip_prefix("Candidate: ")
            && let (Some(name), Some(old)) = (name, installed)
            && candidate != "(none)"
        {
            found.push((name.to_string(), old.to_string(), candidate.to_string()));
        }
    }
    found
}

/// The packages held with `apt-mark hold` that have a newer version: the
/// updates apt leaves alone until the hold is lifted. They show muted.
pub fn held(runner: &dyn Runner) -> Vec<Update> {
    let names: Vec<String> = runner
        .run("apt-mark", &["showhold"])
        .map(|text| text.lines().map(str::trim).filter(|name| valid_name(name)).map(str::to_string).collect())
        .unwrap_or_default();
    if names.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["policy"];
    args.extend(names.iter().map(String::as_str));
    let policy = runner.run("apt-cache", &args).unwrap_or_default();
    let mut held: Vec<Update> = parse_policy(&policy)
        .into_iter()
        .filter(|(_, old, new)| old != new)
        .map(|(name, old, new)| Update { kind: kind_of(&name), name, old: Some(old), new, ..Update::default() })
        .collect();
    describe(runner, &mut held);
    held
}

/// The entries of a Debian changelog newer than `since`, the version that
/// is installed. Each entry starts with `name (version) suite; urgency=`.
pub fn changelog_since(text: &str, since: Option<&str>) -> String {
    let mut kept = Vec::new();
    for line in text.lines() {
        let heading = !line.starts_with(' ') && line.contains(" (") && line.contains(';');
        if heading
            && let Some(since) = since
            && line
                .split_once(" (")
                .and_then(|(_, rest)| rest.split_once(')'))
                .is_some_and(|(version, _)| version == since)
        {
            break;
        }
        kept.push(line);
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    kept.join("\n")
}

/// What changed in a package since the installed version, from
/// `apt-get changelog`, which asks the archive.
pub fn changelog(runner: &dyn Runner, name: &str, since: Option<&str>) -> Result<String, String> {
    if !valid_name(name) {
        return Err(format!("{name:?} is not a package name"));
    }
    let text = runner.run("apt-get", &["changelog", "-qq", name])?;
    let notes = changelog_since(&text, since);
    Ok(if notes.trim().is_empty() { text } else { notes })
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
    fn packages_know_their_source() {
        let known = parse_descriptions(
            "libc6\tglibc (2.39-0ubuntu8)\tGNU C Library: Shared libraries\nlibc-bin\tglibc\tGNU C Library: Binaries\ncurl\t\t\n",
        );
        assert_eq!(known.get("libc6"), Some(&("glibc".into(), "GNU C Library: Shared libraries".into())));
        assert_eq!(known.get("libc-bin").map(|(source, _)| source.as_str()), Some("glibc"));
        assert_eq!(known.get("curl").map(|(source, _)| source.as_str()), Some("curl"));
        let update = Update { name: "libc6".into(), package: "glibc".into(), ..Update::default() };
        assert_eq!(update.group(), "glibc");
        assert_eq!(Update { name: "curl".into(), ..Update::default() }.group(), "curl");
    }

    #[test]
    fn holds_with_newer_versions() {
        let policy = "\
docker-ce:
  Installed: 5:29.9.2-1~ubuntu.24.04~noble
  Candidate: 5:29.9.3-1~ubuntu.24.04~noble
  Version table:
     5:29.9.3-1~ubuntu.24.04~noble 500
nvidia-driver-550:amd64:
  Installed: (none)
  Candidate: 550.120-0ubuntu1
";
        assert_eq!(
            parse_policy(policy),
            [(
                "docker-ce".to_string(),
                "5:29.9.2-1~ubuntu.24.04~noble".to_string(),
                "5:29.9.3-1~ubuntu.24.04~noble".to_string()
            )]
        );
    }

    #[test]
    fn changelogs_stop_at_the_installed_version() {
        let text = "\
curl (8.5.0-2ubuntu10.6) noble-security; urgency=medium

  * SECURITY UPDATE: cookie leak
    - debian/patches/CVE-2026-0001.patch

 -- Someone <someone@ubuntu.com>  Mon, 09 Nov 2026 10:00:00 +0000

curl (8.5.0-2ubuntu10.5) noble-updates; urgency=medium

  * Older fix.
";
        let notes = changelog_since(text, Some("8.5.0-2ubuntu10.5"));
        assert!(notes.starts_with("curl (8.5.0-2ubuntu10.6)"));
        assert!(notes.ends_with("+0000"), "{notes}");
        assert!(!notes.contains("Older fix"));
        assert!(changelog_since(text, None).contains("Older fix"));
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
