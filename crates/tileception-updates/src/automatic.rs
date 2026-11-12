//! Automatic updates through `unattended-upgrades`, where the distribution
//! has it. Tileception only turns its periodic run on or off and says what
//! it covers; the schedule stays apt's own timers.

use crate::{Files, Runner};

/// The file apt's periodic run reads its switches from.
pub const CONFIG: &str = "/etc/apt/apt.conf.d/20auto-upgrades";

/// How automatic updates are set up here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Automatic {
    /// `unattended-upgrades` is installed.
    pub available: bool,
    /// Its periodic run is on.
    pub enabled: bool,
    /// What it installs, in words: "security updates", "Docker CE:noble".
    pub covers: Vec<String>,
}

/// The value of one `apt-config dump` entry: `APT::Periodic::Unattended-Upgrade "1";`.
fn value<'line>(line: &'line str, key: &str) -> Option<&'line str> {
    let rest = line.strip_prefix(key)?.strip_prefix(' ')?;
    Some(rest.trim().trim_end_matches(';').trim_matches('"'))
}

/// An origin pattern in words, for the distribution named `distro`.
fn describe(pattern: &str, distro: &str, codename: &str) -> String {
    let pattern = pattern
        .replace("${distro_id}", distro)
        .replace("${distro_codename}", codename)
        .replace("origin=", "")
        .replace(",archive=", ":")
        .replace(",codename=", ":");
    let suffix = |name: &str| pattern.ends_with(&format!("{codename}-{name}"));
    if pattern.contains("ESMApps") {
        "Ubuntu Pro apps (ESM)".into()
    } else if pattern.contains("ESM") {
        "Ubuntu Pro infrastructure (ESM)".into()
    } else if suffix("security") {
        "security updates".into()
    } else if suffix("updates") {
        "recommended updates".into()
    } else if suffix("proposed") {
        "pre-release updates (proposed)".into()
    } else if suffix("backports") {
        "backports".into()
    } else if pattern == format!("{distro}:{codename}") {
        "the release itself".into()
    } else {
        pattern
    }
}

/// What `apt-config dump` says about unattended upgrades.
pub fn parse(dump: &str, distro: &str, codename: &str) -> (bool, Vec<String>) {
    let mut enabled = false;
    let mut covers: Vec<String> = Vec::new();
    for line in dump.lines() {
        if let Some(setting) = value(line, "APT::Periodic::Unattended-Upgrade") {
            enabled = setting != "0" && !setting.is_empty() && setting != "false";
        }
        let pattern = value(line, "Unattended-Upgrade::Allowed-Origins::")
            .or_else(|| value(line, "Unattended-Upgrade::Origins-Pattern::"));
        if let Some(pattern) = pattern.filter(|pattern| !pattern.is_empty()) {
            let words = describe(pattern, distro, codename);
            if !covers.contains(&words) {
                covers.push(words);
            }
        }
    }
    (enabled, covers)
}

/// Reads how automatic updates are set up.
pub fn read(runner: &dyn Runner, files: &Files) -> Automatic {
    let available = files.exists("/usr/bin/unattended-upgrade");
    if !available {
        return Automatic::default();
    }
    let release = files.read("/etc/os-release").unwrap_or_default();
    let field = |key: &str| {
        release
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or_default()
            .trim_matches('"')
            .to_string()
    };
    let distro = match field("ID").as_str() {
        "ubuntu" => "Ubuntu".to_string(),
        "debian" => "Debian".to_string(),
        other => other.to_string(),
    };
    let dump = runner.run("apt-config", &["dump"]).unwrap_or_default();
    let (enabled, covers) = parse(&dump, &distro, &field("VERSION_CODENAME"));
    Automatic { available, enabled, covers }
}

/// The switches file with the periodic run turned on or off, keeping what
/// else it says. Turning it on also refreshes the lists, which it needs.
pub fn rewrite(existing: &str, enable: bool) -> String {
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|line| !line.trim_start().starts_with("APT::Periodic::Unattended-Upgrade"))
        .filter(|line| !(enable && line.trim_start().starts_with("APT::Periodic::Update-Package-Lists")))
        .map(str::to_string)
        .collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if enable {
        lines.push("APT::Periodic::Update-Package-Lists \"1\";".into());
    }
    lines.push(format!("APT::Periodic::Unattended-Upgrade \"{}\";", if enable { 1 } else { 0 }));
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"APT::Periodic "";
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
Unattended-Upgrade "";
Unattended-Upgrade::Allowed-Origins "";
Unattended-Upgrade::Allowed-Origins:: "${distro_id}:${distro_codename}";
Unattended-Upgrade::Allowed-Origins:: "${distro_id}:${distro_codename}-security";
Unattended-Upgrade::Allowed-Origins:: "${distro_id}ESMApps:${distro_codename}-apps-security";
Unattended-Upgrade::Allowed-Origins:: "${distro_id}ESM:${distro_codename}-infra-security";
Unattended-Upgrade::Origins-Pattern:: "origin=Docker,codename=${distro_codename}";
"#;

    #[test]
    fn coverage_is_said_in_words() {
        let (enabled, covers) = parse(DUMP, "Ubuntu", "noble");
        assert!(enabled);
        assert_eq!(
            covers,
            [
                "the release itself",
                "security updates",
                "Ubuntu Pro apps (ESM)",
                "Ubuntu Pro infrastructure (ESM)",
                "Docker:noble"
            ]
        );
        let (enabled, _) =
            parse(&DUMP.replace("Unattended-Upgrade \"1\"", "Unattended-Upgrade \"0\""), "Ubuntu", "noble");
        assert!(!enabled);
    }

    #[test]
    fn the_switch_keeps_the_rest() {
        let existing =
            "// kept\nAPT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"1\";\n";
        assert_eq!(
            rewrite(existing, false),
            "// kept\nAPT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"0\";\n"
        );
        assert_eq!(
            rewrite("", true),
            "APT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"1\";\n"
        );
    }
}
