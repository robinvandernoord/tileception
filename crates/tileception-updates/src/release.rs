//! The installed release, the next one, and what to look at before upgrading.

use crate::{Files, Runner};

/// The installed system, from `/etc/os-release`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    /// `ubuntu`, `debian`, `linuxmint`.
    pub distributor: String,
    /// `Ubuntu 24.04.3 LTS`.
    pub pretty: String,
    /// `24.04`.
    pub version: String,
    /// `noble`.
    pub codename: String,
    /// The Ubuntu release the package archive is for: `noble`, also on
    /// Linux Mint, whose own codename is `zena`.
    pub archive: String,
}

pub fn parse_system(text: &str) -> System {
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|value| value.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    System {
        distributor: field("ID"),
        pretty: field("PRETTY_NAME"),
        version: field("VERSION_ID"),
        codename: field("VERSION_CODENAME"),
        archive: Some(field("UBUNTU_CODENAME")).filter(|name| !name.is_empty()).unwrap_or(field("VERSION_CODENAME")),
    }
}

pub fn system(files: &Files) -> System {
    files.read("/etc/os-release").map(|text| parse_system(&text)).unwrap_or_default()
}

/// Which releases `do-release-upgrade` offers: `Prompt=` in
/// `/etc/update-manager/release-upgrades`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Track {
    /// Long-term support releases only.
    #[default]
    Lts,
    /// Every release.
    Normal,
    Never,
}

pub fn parse_track(text: &str) -> Track {
    let prompt = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| line.strip_prefix("Prompt")?.trim_start().strip_prefix('='));
    match prompt.map(str::trim) {
        Some("normal") => Track::Normal,
        Some("never") => Track::Never,
        _ => Track::Lts,
    }
}

pub fn track(files: &Files) -> Track {
    files.read("/etc/update-manager/release-upgrades").map(|text| parse_track(&text)).unwrap_or_default()
}

/// The release named in `do-release-upgrade -c`: `New release '26.04 LTS' available.`
pub fn parse_offer(output: &str) -> Option<String> {
    let rest = output.lines().find_map(|line| line.trim().strip_prefix("New release '"))?;
    rest.split_once('\'').map(|(name, _)| name.to_string())
}

/// What the next release is, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Offer {
    /// Not asked yet.
    #[default]
    Unknown,
    /// This system has no `do-release-upgrade`.
    Unsupported,
    None,
    Release(String),
}

/// Asks Ubuntu whether a newer release is out. Uses the network.
pub fn offer(runner: &dyn Runner) -> Offer {
    match runner.run("do-release-upgrade", &["-c"]) {
        Ok(output) => parse_offer(&output).map_or(Offer::None, Offer::Release),
        Err(error) if error.contains("No such file") || error.contains("not found") => Offer::Unsupported,
        Err(_) => Offer::None,
    }
}

/// Whether earlier updates wait for a restart.
pub fn restart_required(files: &Files) -> bool {
    files.exists("/var/run/reboot-required") || files.exists("/run/reboot-required")
}

fn official(host: &str) -> bool {
    ["ubuntu.com", "canonical.com", "debian.org", "linuxmint.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn host_of(uri: &str) -> Option<&str> {
    let rest = uri.split_once("://")?.1;
    rest.split(['/', ':']).next().filter(|host| !host.is_empty())
}

/// Hosts of enabled package sources in one file's text that are not the
/// distribution's own. An upgrade switches these off.
pub fn parse_sources(text: &str, deb822: bool) -> Vec<String> {
    let mut hosts = Vec::new();
    if deb822 {
        for stanza in text.split("\n\n") {
            let lines = || stanza.lines().map(str::trim).filter(|line| !line.starts_with('#'));
            if lines().any(|line| line.eq_ignore_ascii_case("Enabled: no")) {
                continue;
            }
            let uris = lines().filter_map(|line| line.strip_prefix("URIs:"));
            hosts.extend(uris.flat_map(str::split_whitespace).filter_map(host_of).map(str::to_string));
        }
    } else {
        for line in text.lines().map(str::trim).filter(|line| line.starts_with("deb ")) {
            let uri = line.split_whitespace().skip(1).find(|word| word.contains("://"));
            hosts.extend(uri.and_then(host_of).map(str::to_string));
        }
    }
    hosts.retain(|host| !official(host));
    hosts
}

/// The other sources this system installs from.
pub fn third_party(files: &Files) -> Vec<String> {
    let mut hosts = files.read("/etc/apt/sources.list").map(|text| parse_sources(&text, false)).unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir(files.path("/etc/apt/sources.list.d")) {
        for path in entries.flatten().map(|entry| entry.path()) {
            let deb822 = match path.extension().and_then(|extension| extension.to_str()) {
                Some("sources") => true,
                Some("list") => false,
                _ => continue,
            };
            if let Ok(text) = std::fs::read_to_string(&path) {
                hosts.extend(parse_sources(&text, deb822));
            }
        }
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Free bytes on the root filesystem, from `df -Pk /`.
pub fn parse_free(output: &str) -> Option<u64> {
    let available: u64 = output.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()?;
    Some(available * 1024)
}

pub fn free_space(runner: &dyn Runner) -> Option<u64> {
    parse_free(&runner.run("df", &["-Pk", "/"]).ok()?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    /// Look at this first.
    Attention,
    /// Worth doing, not needed.
    Optional,
}

/// One thing to look at before upgrading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub title: String,
    pub detail: String,
    pub state: State,
}

/// What the checks are made from.
#[derive(Debug, Clone, Default)]
pub struct Readiness {
    pub free: Option<u64>,
    pub pending: usize,
    pub security: usize,
    pub third_party: Vec<String>,
    /// Running on battery; none on a desktop.
    pub on_battery: Option<bool>,
    pub restart: bool,
}

/// Space an upgrade needs for its downloads and unpacking.
const NEEDED: u64 = 9 * 1000 * 1000 * 1000;

pub fn checks(readiness: &Readiness) -> Vec<Check> {
    let check = |title: &str, detail: String, state| Check { title: title.to_string(), detail, state };
    let mut list = Vec::new();
    list.push(match readiness.free {
        Some(free) if free >= NEEDED => check(
            "Disk space",
            format!("{} free, about {} needed", crate::human_size(free), crate::human_size(NEEDED)),
            State::Ready,
        ),
        Some(free) => check(
            "Not enough disk space",
            format!(
                "{} free, about {} needed. Old kernels are a quick win.",
                crate::human_size(free),
                crate::human_size(NEEDED)
            ),
            State::Attention,
        ),
        None => check("Disk space", "Could not be read; about 9 GB is needed.".into(), State::Attention),
    });
    list.push(if readiness.pending == 0 {
        check("Up to date", "Every current update is installed.".into(), State::Ready)
    } else {
        let security = match readiness.security {
            0 => String::new(),
            count => format!(", {count} for security"),
        };
        check("Install current updates first", format!("{} waiting{security}", readiness.pending), State::Attention)
    });
    if readiness.restart {
        list.push(check(
            "Restart first",
            "Earlier updates wait for a restart. Upgrade from the freshly started system.".into(),
            State::Attention,
        ));
    }
    list.push(if readiness.third_party.is_empty() {
        check("Package sources", "Only the distribution's own sources are enabled.".into(), State::Ready)
    } else {
        let count = readiness.third_party.len();
        check(
            &format!(
                "{count} other source{} get{} switched off",
                if count == 1 { "" } else { "s" },
                if count == 1 { "s" } else { "" }
            ),
            format!("{}. Switch them on again afterwards.", readiness.third_party.join(", ")),
            State::Attention,
        )
    });
    match readiness.on_battery {
        Some(true) => list.push(check(
            "Connect the charger",
            "An upgrade takes about an hour and must not be cut short.".into(),
            State::Attention,
        )),
        Some(false) => list.push(check("On mains power", "The charger is connected.".into(), State::Ready)),
        None => {}
    }
    list
}

/// Whether nothing stands in the way.
pub fn ready(checks: &[Check]) -> bool {
    !checks.iter().any(|check| check.state == State::Attention)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_installed_system() {
        let parsed = parse_system(
            "PRETTY_NAME=\"Ubuntu 24.04.5 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n",
        );
        assert_eq!(
            parsed,
            System {
                distributor: "ubuntu".into(),
                pretty: "Ubuntu 24.04.5 LTS".into(),
                version: "24.04".into(),
                codename: String::new(),
                archive: String::new(),
            }
        );
    }

    #[test]
    fn reads_the_release_track_and_offer() {
        assert_eq!(parse_track("# comment\n[DEFAULT]\nPrompt=lts\n"), Track::Lts);
        assert_eq!(parse_track("[DEFAULT]\nPrompt = normal\n"), Track::Normal);
        assert_eq!(parse_track("#Prompt=never\n"), Track::Lts);
        assert_eq!(
            parse_offer(
                "Checking for a new Ubuntu release\nNew release '26.04 LTS' available.\nRun 'do-release-upgrade' to upgrade to it.\n"
            ),
            Some("26.04 LTS".into())
        );
        assert_eq!(parse_offer("Checking for a new Ubuntu release\nNo new release found.\n"), None);
    }

    #[test]
    fn finds_sources_an_upgrade_switches_off() {
        let list = "# deb https://old.example.com/apt stable main\n\
                    deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu noble stable\n\
                    deb http://archive.ubuntu.com/ubuntu noble main\n";
        assert_eq!(parse_sources(list, false), ["download.docker.com"]);
        let sources = "Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: noble\n\n\
                       Types: deb\nURIs: https://apt.tileception.space\nSuites: stable\n\n\
                       Types: deb\nURIs: https://off.example.com/apt\nEnabled: no\n";
        assert_eq!(parse_sources(sources, true), ["apt.tileception.space"]);
    }

    #[test]
    fn reads_free_space() {
        let output = "Filesystem     1024-blocks     Used Available Capacity Mounted on\n/dev/nvme0n1p2   479079112 91234567 363456789      21% /\n";
        assert_eq!(parse_free(output), Some(363_456_789 * 1024));
    }

    #[test]
    fn pending_updates_and_sources_hold_an_upgrade_back() {
        let mut readiness = Readiness { free: Some(38_000_000_000), on_battery: Some(false), ..Default::default() };
        assert!(ready(&checks(&readiness)));
        readiness.pending = 12;
        readiness.security = 3;
        readiness.third_party = vec!["download.docker.com".into()];
        let list = checks(&readiness);
        assert!(!ready(&list));
        assert_eq!(list[1].detail, "12 waiting, 3 for security");
        assert_eq!(list[2].title, "1 other source gets switched off");
        readiness.free = Some(2_000_000_000);
        assert_eq!(checks(&readiness)[0].state, State::Attention);
    }
}
