//! What apt did before, from `/var/log/apt/history.log`.

use crate::Files;

/// One package in a transaction, with its versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    /// `2.8.3 → 2.8.4` for an upgrade, the version otherwise.
    pub version: String,
}

/// One apt transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// `2026-11-09 23:44:58`, local time.
    pub date: String,
    pub command: String,
    /// The user it ran for; none for unattended upgrades and scripts.
    pub requested_by: Option<String>,
    pub upgraded: Vec<Change>,
    pub installed: Vec<Change>,
    pub removed: Vec<Change>,
}

impl Entry {
    /// "8 updated, 2 installed".
    pub fn summary(&self) -> String {
        let parts: Vec<String> =
            [(&self.upgraded, "updated"), (&self.installed, "installed"), (&self.removed, "removed")]
                .iter()
                .filter(|(changes, _)| !changes.is_empty())
                .map(|(changes, verb)| format!("{} {verb}", changes.len()))
                .collect();
        if parts.is_empty() { "Nothing changed".into() } else { parts.join(", ") }
    }

    /// The first packages by name, for a one-line preview.
    pub fn names(&self, limit: usize) -> String {
        let all: Vec<&str> = self
            .upgraded
            .iter()
            .chain(&self.installed)
            .chain(&self.removed)
            .map(|change| change.name.as_str())
            .collect();
        let shown = all.iter().take(limit).copied().collect::<Vec<_>>().join(", ");
        if all.len() > limit { format!("{shown}, {} more", all.len() - limit) } else { shown }
    }

    /// Who it ran for: a user, the unattended upgrades, or the system
    /// (root, an installer or a script).
    pub fn who(&self) -> &str {
        match &self.requested_by {
            Some(user) => user,
            None if self.command.contains("unattended-upgrade") => "automatic",
            None => "system",
        }
    }
}

/// `apt:amd64 (2.8.3, 2.8.4), libfoo:amd64 (1.0, automatic)`.
fn parse_changes(list: &str) -> Vec<Change> {
    list.split("), ")
        .filter_map(|item| {
            let (name, versions) = item.trim_end_matches(')').split_once(" (")?;
            let name = name.split(':').next().unwrap_or(name).trim();
            let mut versions = versions.split(", ").filter(|version| *version != "automatic");
            let version = match (versions.next(), versions.next()) {
                (Some(before), Some(after)) => format!("{before} \u{2192} {after}"),
                (Some(only), None) => only.to_string(),
                _ => String::new(),
            };
            (!name.is_empty()).then(|| Change { name: name.to_string(), version })
        })
        .collect()
}

/// The transactions in a log, oldest first.
pub fn parse(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut current: Option<Entry> = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(": ") else { continue };
        let value = value.trim();
        if key == "Start-Date" {
            entries.extend(current.take());
            current =
                Some(Entry { date: value.split_whitespace().collect::<Vec<_>>().join(" "), ..Default::default() });
            continue;
        }
        let Some(entry) = current.as_mut() else { continue };
        match key {
            "Commandline" => entry.command = value.to_string(),
            "Requested-By" => entry.requested_by = value.split_whitespace().next().map(str::to_string),
            "Upgrade" => entry.upgraded = parse_changes(value),
            "Install" => entry.installed = parse_changes(value),
            "Remove" | "Purge" => entry.removed.extend(parse_changes(value)),
            _ => {}
        }
    }
    entries.extend(current);
    entries
}

/// How many transactions the history keeps.
const KEPT: usize = 60;

/// The latest transactions, newest first.
pub fn read(files: &Files) -> Vec<Entry> {
    let mut entries = Vec::new();
    for path in ["/var/log/apt/history.log.1", "/var/log/apt/history.log"] {
        entries.extend(files.read(path).map(|text| parse(&text)).unwrap_or_default());
    }
    entries.reverse();
    entries.truncate(KEPT);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "
Start-Date: 2026-11-03  06:30:12
Commandline: /usr/bin/unattended-upgrade
Upgrade: libxml2:amd64 (2.9.14+dfsg-1.3ubuntu3.5, 2.9.14+dfsg-1.3ubuntu3.6), apt:amd64 (2.8.3, 2.8.4)
End-Date: 2026-11-03  06:30:20

Start-Date: 2026-11-05  09:12:40
Commandline: apt-get -y dist-upgrade
Requested-By: robin (1000)
Install: linux-image-6.14.0-36-generic:amd64 (6.14.0-36.36, automatic)
Upgrade: mesa-utils:amd64 (9.0.0-1, 9.0.0-2)
Purge: linux-image-6.8.0-79-generic:amd64 (6.8.0-79.79)
End-Date: 2026-11-05  09:13:55
";

    #[test]
    fn transactions_are_read_in_order() {
        let entries = parse(LOG);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].date, "2026-11-03 06:30:12");
        assert_eq!(entries[0].who(), "automatic");
        assert_eq!(entries[0].upgraded[1], Change { name: "apt".into(), version: "2.8.3 \u{2192} 2.8.4".into() });
        assert_eq!(entries[0].summary(), "2 updated");
        assert_eq!(entries[1].who(), "robin");
        assert_eq!(entries[1].installed[0].version, "6.14.0-36.36");
        assert_eq!(entries[1].summary(), "1 updated, 1 installed, 1 removed");
        assert_eq!(entries[1].names(2), "mesa-utils, linux-image-6.14.0-36-generic, 1 more");
    }
}
