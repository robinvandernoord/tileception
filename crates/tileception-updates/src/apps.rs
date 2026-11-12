//! Updates for apps from Flatpak and Snap, read from their own tools.
//! Both ask a server, so they are listed only when the network may be used.

use std::collections::HashMap;

use crate::Runner;
use crate::apt::{Kind, Source, Update};

/// A size as Flatpak and Snap print it: `52.1 MB`, `91MB`, `312 bytes`.
pub fn parse_size(text: &str) -> u64 {
    let text = text.trim().replace('\u{a0}', " ");
    let split = text.find(|character: char| !(character.is_ascii_digit() || character == '.')).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let Ok(number) = number.parse::<f64>() else { return 0 };
    let scale = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" | "byte" | "bytes" => 1.0,
        "kb" | "k" => 1e3,
        "mb" | "m" => 1e6,
        "gb" | "g" => 1e9,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return 0,
    };
    (number * scale).round() as u64
}

/// The columns Flatpak is asked for, in order.
const FLATPAK_COLUMNS: &str = "application,name,version,branch,origin,download-size";

/// `flatpak list --columns=application,branch,version`: the versions
/// installed, by application and branch.
pub fn parse_flatpak_installed(text: &str) -> HashMap<(String, String), String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let application = fields.next()?.trim();
            let branch = fields.next().unwrap_or_default().trim();
            let version = fields.next().unwrap_or_default().trim();
            (!application.is_empty()).then(|| ((application.to_string(), branch.to_string()), version.to_string()))
        })
        .collect()
}

/// `flatpak remote-ls --updates` with [`FLATPAK_COLUMNS`], tab-separated.
pub fn parse_flatpak_updates(text: &str, installed: &HashMap<(String, String), String>, source: Source) -> Vec<Update> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
            let [application, title, version, branch, origin, size, ..] = fields.as_slice() else { return None };
            if application.is_empty() || application.starts_with("Application") {
                return None;
            }
            let reference =
                if branch.is_empty() { (*application).to_string() } else { format!("{application}//{branch}") };
            let old = installed.get(&((*application).to_string(), (*branch).to_string())).filter(|old| !old.is_empty());
            // Runtimes often have no version: the branch says which they are.
            let new = if version.is_empty() { (*branch).to_string() } else { (*version).to_string() };
            Some(Update {
                name: reference,
                source,
                title: (*title).to_string(),
                old: old.cloned(),
                new,
                origin: (*origin).to_string(),
                kind: Kind::App,
                size: parse_size(size),
                ..Update::default()
            })
        })
        .collect()
}

/// The pending Flatpak updates, of the system installation and the
/// user's own. None where Flatpak is not installed.
pub fn flatpak(runner: &dyn Runner) -> Option<Vec<Update>> {
    let mut updates = Vec::new();
    let mut found = false;
    for (flag, source) in [("--system", Source::Flatpak), ("--user", Source::FlatpakUser)] {
        let installed = match runner.run("flatpak", &["list", flag, "--columns=application,branch,version"]) {
            Ok(text) => parse_flatpak_installed(&text),
            Err(_) => continue,
        };
        found = true;
        if installed.is_empty() {
            continue;
        }
        let columns = format!("--columns={FLATPAK_COLUMNS}");
        if let Ok(text) = runner.run("flatpak", &["remote-ls", flag, "--updates", &columns]) {
            updates.extend(parse_flatpak_updates(&text, &installed, source));
        }
    }
    found.then_some(updates)
}

/// The columns of a `snap` table, found by where each heading starts.
fn columns(header: &str) -> Vec<(String, usize)> {
    let mut found = Vec::new();
    let mut previous = ' ';
    for (index, character) in header.char_indices() {
        if character != ' ' && previous == ' ' {
            let name = header[index..].split(' ').next().unwrap_or_default();
            found.push((name.to_string(), index));
        }
        previous = character;
    }
    found
}

/// A `snap` table as rows of named cells. Cells are cut where the headings
/// start, since a publisher's name can hold a space.
fn table(text: &str) -> Vec<HashMap<String, String>> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(header) = lines.next() else { return Vec::new() };
    let columns = columns(header);
    lines
        .map(|line| {
            let characters: Vec<char> = line.chars().collect();
            columns
                .iter()
                .enumerate()
                .map(|(index, (name, start))| {
                    let end = columns.get(index + 1).map_or(characters.len(), |(_, next)| *next);
                    let cell: String =
                        characters.get(*start..end.min(characters.len())).unwrap_or_default().iter().collect();
                    (name.clone(), cell.trim().to_string())
                })
                .collect()
        })
        .collect()
}

/// `snap refresh --list`, with the versions from `snap list`.
pub fn parse_snaps(refresh: &str, installed: &str) -> Vec<Update> {
    let versions: HashMap<String, String> = table(installed)
        .into_iter()
        .filter_map(|row| Some((row.get("Name")?.clone(), row.get("Version")?.clone())))
        .collect();
    table(refresh)
        .into_iter()
        .filter_map(|row| {
            let name = row.get("Name")?.clone();
            let publisher = row.get("Publisher").map(|publisher| publisher.trim_end_matches(['✓', '*'])).unwrap_or("");
            Some(Update {
                old: versions.get(&name).cloned(),
                new: row.get("Version").cloned().unwrap_or_default(),
                origin: if publisher.is_empty() { "Snap Store".into() } else { format!("Snap Store, {publisher}") },
                size: row.get("Size").map(|size| parse_size(size)).unwrap_or(0),
                source: Source::Snap,
                kind: Kind::App,
                name,
                ..Update::default()
            })
        })
        .collect()
}

/// The pending snap refreshes. None where snapd is not installed.
pub fn snap(runner: &dyn Runner) -> Option<Vec<Update>> {
    let installed = runner.run("snap", &["list"]).ok()?;
    // With nothing to refresh, snap says so on its standard error.
    let refresh = runner.run("snap", &["refresh", "--list"]).unwrap_or_default();
    Some(parse_snaps(&refresh, &installed))
}

/// Whether this is a Flatpak ref the update command takes as one:
/// `org.mozilla.firefox`, optionally with `//branch`.
pub fn valid_flatpak(reference: &str) -> bool {
    let mut characters = reference.chars();
    characters.next().is_some_and(|first| first.is_ascii_alphanumeric())
        && characters.all(|character| character.is_ascii_alphanumeric() || "._-/".contains(character))
}

/// Whether this is a snap's name: lower-case letters, digits and hyphens.
pub fn valid_snap(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
}

/// What changed in a Flatpak: the commit messages of its remote.
pub fn flatpak_notes(runner: &dyn Runner, update: &Update) -> Result<String, String> {
    if !valid_flatpak(&update.name) || !valid_flatpak(&update.origin) {
        return Err("not a Flatpak".into());
    }
    let flag = if update.source == Source::FlatpakUser { "--user" } else { "--system" };
    runner.run("flatpak", &["remote-info", flag, "--log", &update.origin, &update.name])
}

/// What a snap is and which versions its channels offer.
pub fn snap_notes(runner: &dyn Runner, name: &str) -> Result<String, String> {
    if !valid_snap(name) {
        return Err("not a snap".into());
    }
    runner.run("snap", &["info", name])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_back() {
        assert_eq!(parse_size("52.1\u{a0}MB"), 52_100_000);
        assert_eq!(parse_size("91MB"), 91_000_000);
        assert_eq!(parse_size("312 bytes"), 312);
        assert_eq!(parse_size("1.5 kB"), 1500);
        assert_eq!(parse_size("-"), 0);
    }

    #[test]
    fn flatpak_updates_carry_their_installed_version() {
        let installed = parse_flatpak_installed(
            "org.mozilla.firefox\tstable\t132.0\norg.freedesktop.Platform.GL.default\t23.08\t\n",
        );
        let updates = parse_flatpak_updates(
            "org.mozilla.firefox\tFirefox\t133.0\tstable\tflathub\t52.1\u{a0}MB\n\
             org.freedesktop.Platform.GL.default\tMesa\t\t23.08\tflathub\t1.2\u{a0}MB\n",
            &installed,
            Source::Flatpak,
        );
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].name, "org.mozilla.firefox//stable");
        assert_eq!(updates[0].label(), "Firefox");
        assert_eq!(updates[0].old.as_deref(), Some("132.0"));
        assert_eq!(updates[0].new, "133.0");
        assert_eq!(updates[0].size, 52_100_000);
        assert_eq!(updates[1].old, None);
        assert_eq!(updates[1].new, "23.08");
        assert!(updates.iter().all(|update| update.kind == Kind::App && !update.security));
    }

    #[test]
    fn snap_tables_are_cut_by_their_headings() {
        let installed = "\
Name      Version         Rev    Tracking         Publisher   Notes
core22    20241001        1663   latest/stable    canonical✓  base
firefox   132.0-1         5187   latest/stable/…  mozilla✓    -
";
        let refresh = "\
Name     Version   Rev   Size   Publisher     Notes
firefox  133.0-1   5273  91MB   mozilla✓      -
my-tool  2.0       12    4kB    Jane Doe      -
";
        let updates = parse_snaps(refresh, installed);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].name, "firefox");
        assert_eq!(updates[0].old.as_deref(), Some("132.0-1"));
        assert_eq!(updates[0].new, "133.0-1");
        assert_eq!(updates[0].size, 91_000_000);
        assert_eq!(updates[0].origin, "Snap Store, mozilla");
        assert_eq!(updates[1].origin, "Snap Store, Jane Doe");
        assert_eq!(updates[1].source, Source::Snap);
        assert!(parse_snaps("", installed).is_empty());
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_flatpak("org.mozilla.firefox//stable"));
        assert!(!valid_flatpak("--system"));
        assert!(!valid_flatpak("org.x; reboot"));
        assert!(valid_snap("my-tool"));
        assert!(!valid_snap("-oops"));
        assert!(!valid_snap("Firefox"));
    }
}
