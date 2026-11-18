//! Updates for apps from Flatpak and Snap, and for the Python tools the
//! user installed with uvenv, read from their own tools. Each asks a server,
//! so they are listed only when the network may be used.

use std::collections::HashMap;
use std::path::PathBuf;

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
const FLATPAK_COLUMNS: &str = "application,name,version,branch,origin,download-size,commit";

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
                identity: fields
                    .get(6)
                    .filter(|commit| !commit.is_empty())
                    .map(|commit| format!("commit:{commit}"))
                    .unwrap_or_default(),
                origin: (*origin).to_string(),
                kind: Kind::App,
                size: parse_size(size),
                ..Update::default()
            })
        })
        .collect()
}

/// What asking Flatpak and Snap found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    pub updates: Vec<Update>,
    /// The sources that could not be asked, such as during an outage:
    /// what they listed before still stands.
    pub failed: Vec<Source>,
}

/// The pending Flatpak updates, of the system installation and the
/// user's own. Nothing where Flatpak is not installed.
pub fn flatpak(runner: &dyn Runner, found: &mut Found) {
    for (flag, source) in [("--system", Source::Flatpak), ("--user", Source::FlatpakUser)] {
        let installed = match runner.run("flatpak", &["list", flag, "--columns=application,branch,version"]) {
            Ok(text) => parse_flatpak_installed(&text),
            Err(_) => {
                found.failed.push(source);
                continue;
            }
        };
        if installed.is_empty() {
            continue;
        }
        let columns = format!("--columns={FLATPAK_COLUMNS}");
        match runner.run("flatpak", &["remote-ls", flag, "--updates", &columns]) {
            Ok(text) => found.updates.extend(parse_flatpak_updates(&text, &installed, source)),
            Err(_) => found.failed.push(source),
        }
    }
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
                identity: row.get("Rev").cloned().unwrap_or_default(),
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

/// The pending snap refreshes. Nothing where snapd is not installed.
pub fn snap(runner: &dyn Runner, found: &mut Found) {
    let Ok(installed) = runner.run("snap", &["list"]) else {
        found.failed.push(Source::Snap);
        return;
    };
    // With nothing to refresh, snap says so on its standard error and
    // still succeeds: a failure is a store it could not ask.
    match runner.run("snap", &["refresh", "--list"]) {
        Ok(refresh) => found.updates.extend(parse_snaps(&refresh, &installed)),
        Err(_) => found.failed.push(Source::Snap),
    }
}

/// uvenv, on `PATH` or where its installer puts it: the session's `PATH`
/// may not have `~/.local/bin`.
pub fn uvenv_program() -> Option<String> {
    let home = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(home)
        .map(|dir| dir.join("uvenv"))
        .find(|program| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(program).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .map(|program| program.display().to_string())
}

/// Where uvenv's updates come from.
const PACKAGE_INDEX: &str = "PyPI";

/// `uvenv outdated --json`: the tools with a newer version, and uvenv
/// itself when a newer one is out. Also whether looking any up failed.
pub fn parse_uvenv(text: &str) -> Result<(Vec<Update>, bool), String> {
    let report: serde_json::Value = serde_json::from_str(text).map_err(|error| format!("uvenv: {error}"))?;
    let text_of = |value: &serde_json::Value, key: &str| {
        value.get(key).and_then(serde_json::Value::as_str).unwrap_or_default().to_string()
    };
    let update = |name: String, old: String, new: String| Update {
        name,
        source: Source::Uvenv,
        old: (!old.is_empty()).then_some(old),
        new,
        origin: PACKAGE_INDEX.into(),
        kind: Kind::App,
        ..Update::default()
    };
    let mut updates = Vec::new();
    let mut failed = false;
    if let Some(uvenv) = report.get("self").filter(|uvenv| !uvenv.is_null()) {
        failed |= uvenv.get("error").is_some_and(|error| !error.is_null());
        if uvenv.get("outdated").and_then(serde_json::Value::as_bool) == Some(true) {
            updates.push(update(
                "uvenv".into(),
                text_of(uvenv, "installed_version"),
                text_of(uvenv, "available_version"),
            ));
        }
    }
    for package in report.get("outdated").and_then(serde_json::Value::as_array).into_iter().flatten() {
        let name = text_of(package, "name");
        let new = text_of(package, "available_version");
        if valid_python_package(&name) && !new.is_empty() {
            updates.push(update(name, text_of(package, "installed_version"), new));
        }
    }
    failed |= report.get("failed").and_then(serde_json::Value::as_array).is_some_and(|failed| !failed.is_empty());
    Ok((updates, failed))
}

/// The pending updates of the user's uvenv tools. Nothing where uvenv is
/// not installed, or is too old to have `uvenv outdated`.
pub fn uvenv(runner: &dyn Runner, program: Option<&str>, found: &mut Found) {
    let Some(program) = program else { return };
    match runner.run(program, &["outdated", "--json"]).and_then(|text| parse_uvenv(&text)) {
        // What could be looked up is shown. With nothing to show and lookups
        // that failed, as without a network, what was listed before stands.
        Ok((updates, failed)) => {
            if failed && updates.is_empty() {
                found.failed.push(Source::Uvenv);
            }
            found.updates.extend(updates);
        }
        Err(_) => {
            let supported = runner.run(program, &["--help"]).is_ok_and(|help| help.contains("outdated"));
            if supported {
                found.failed.push(Source::Uvenv);
            }
        }
    }
}

/// Every app update, from Flatpak, Snap and uvenv.
pub fn list(runner: &dyn Runner) -> Found {
    let mut found = Found::default();
    flatpak(runner, &mut found);
    snap(runner, &mut found);
    uvenv(runner, uvenv_program().as_deref(), &mut found);
    found
}

/// Whether this is a Python package name as uvenv names its environments.
pub fn valid_python_package(name: &str) -> bool {
    name.chars().next().is_some_and(|first| first.is_ascii_alphanumeric())
        && name.chars().all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

/// Whether this is a Flatpak ref the update command takes as one:
/// `org.mozilla.firefox`, optionally with `//branch`.
pub fn valid_flatpak(reference: &str) -> bool {
    let mut characters = reference.chars();
    characters.next().is_some_and(|first| first.is_ascii_alphanumeric())
        && characters.all(|character| character.is_ascii_alphanumeric() || "._-/".contains(character))
}

/// Whether this is a snap name, optionally followed by an instance key.
pub fn valid_snap(name: &str) -> bool {
    let (base, instance) = name.split_once('_').map_or((name, None), |(base, key)| (base, Some(key)));
    !base.is_empty()
        && !base.starts_with('-')
        && base
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
        && instance.is_none_or(|key| {
            !key.is_empty()
                && key.len() <= 10
                && key.chars().all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}

/// What changed in a Flatpak: the commit messages of its remote.
pub fn flatpak_notes(runner: &dyn Runner, update: &Update) -> Result<String, String> {
    if !valid_flatpak(&update.name) || !valid_flatpak(&update.origin) {
        return Err("not a Flatpak".into());
    }
    let flag = if update.source == Source::FlatpakUser { "--user" } else { "--system" };
    runner.run("flatpak", &["remote-info", flag, "--log", &update.origin, &update.name])
}

/// What a uvenv tool is, from the package index, with where its changes are
/// written down. uvenv's own notes are its changelog.
pub fn uvenv_notes(runner: &dyn Runner, update: &Update) -> Result<String, String> {
    if !valid_python_package(&update.name) {
        return Err("not a Python package".into());
    }
    if update.name == "uvenv"
        && let Some(program) = uvenv_program()
    {
        return runner.run(&program, &["self", "changelog"]);
    }
    let url = format!("https://pypi.org/pypi/{}/json", update.name);
    let text = runner.run("curl", &["-fsSL", "--max-time", "20", &url])?;
    Ok(describe_pypi(&text, &update.name, &update.new))
}

/// The summary and links PyPI has for a package.
pub fn describe_pypi(text: &str, name: &str, version: &str) -> String {
    let project: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    let info = project.get("info").cloned().unwrap_or_default();
    let mut lines = Vec::new();
    if let Some(summary) = info.get("summary").and_then(serde_json::Value::as_str).filter(|text| !text.is_empty()) {
        lines.push(summary.to_string());
        lines.push(String::new());
    }
    if let Some(urls) = info.get("project_urls").and_then(serde_json::Value::as_object) {
        for (label, url) in urls {
            if let Some(url) = url.as_str() {
                lines.push(format!("{label}: {url}"));
            }
        }
    }
    lines.push(format!("PyPI: https://pypi.org/project/{name}/{version}/"));
    lines.join("\n")
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

    /// Answers as Flatpak and Snap would, with the store unreachable when
    /// `offline` names the program.
    struct Fake {
        offline: &'static str,
    }

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
            let asks_server = args.first().is_some_and(|first| ["remote-ls", "refresh"].contains(first));
            if program == self.offline && asks_server {
                return Err(format!("{program}: could not reach the server"));
            }
            Ok(match (program, args.first().copied(), args.get(1).copied()) {
                ("flatpak", Some("list"), Some("--system")) => "org.mozilla.firefox\tstable\t132.0\n".into(),
                ("flatpak", Some("list"), _) => String::new(),
                ("flatpak", Some("remote-ls"), _) => {
                    "org.mozilla.firefox\tFirefox\t133.0\tstable\tflathub\t1 MB\n".into()
                }
                ("snap", Some("list"), _) => "Name Version\nhello 1\n".into(),
                ("snap", _, _) => "Name Version\nhello 2\n".into(),
                _ => return Err(format!("{program}: not found")),
            })
        }
    }

    #[test]
    fn a_source_that_cannot_be_asked_is_not_empty() {
        let found = list(&Fake { offline: "" });
        assert_eq!(found.updates.len(), 2);
        assert!(found.failed.is_empty());
        let found = list(&Fake { offline: "flatpak" });
        assert_eq!(found.failed, vec![Source::Flatpak]);
        assert_eq!(found.updates.iter().map(|update| update.source).collect::<Vec<_>>(), vec![Source::Snap]);
        let found = list(&Fake { offline: "snap" });
        assert_eq!(found.failed, vec![Source::Snap]);
    }

    #[test]
    fn uvenv_reports_what_it_could_look_up() {
        let report = r#"{
            "self": {"installed_version": "3.12.0", "available_version": "3.13.0", "outdated": true},
            "outdated": [
                {"name": "ruff", "installed_version": "0.16.10", "available_version": "0.17.5", "install_spec": "ruff"},
                {"name": "--bad", "installed_version": "1", "available_version": "2", "install_spec": "x"}
            ],
            "failed": [],
            "skipped": [{"name": "tileception-odoo", "reason": "installed from git"}]
        }"#;
        let (updates, failed) = parse_uvenv(report).expect("a report");
        assert!(!failed);
        let names: Vec<&str> = updates.iter().map(|update| update.name.as_str()).collect();
        assert_eq!(names, ["uvenv", "ruff"]);
        assert_eq!((updates[1].old.as_deref(), updates[1].new.as_str()), (Some("0.16.10"), "0.17.5"));
        assert!(updates.iter().all(|update| update.source == Source::Uvenv && update.kind == Kind::App));

        let offline = r#"{"self": {"installed_version": "3.12.0", "available_version": "", "outdated": false,
            "error": "offline"}, "outdated": [], "failed": [{"name": "ruff", "error": "offline"}], "skipped": []}"#;
        assert_eq!(parse_uvenv(offline), Ok((Vec::new(), true)));
        // A snap of uvenv updates itself: no `self`.
        assert_eq!(
            parse_uvenv(r#"{"self": null, "outdated": [], "failed": [], "skipped": []}"#),
            Ok((Vec::new(), false))
        );
        assert!(parse_uvenv("error: unrecognized subcommand").is_err());
    }

    /// uvenv as it answers: `outdated` only when `new`, else too old for it.
    struct Uvenv {
        new: bool,
        report: Result<&'static str, &'static str>,
    }

    impl Runner for Uvenv {
        fn run(&self, _program: &str, args: &[&str]) -> Result<String, String> {
            match args {
                ["outdated", "--json"] if self.new => self.report.map(String::from).map_err(String::from),
                ["outdated", ..] => Err("uvenv: unrecognized subcommand 'outdated'".into()),
                ["--help"] => {
                    Ok(if self.new { "Commands:\n  outdated  List packages" } else { "Commands:\n  list" }.into())
                }
                _ => Err("unexpected".into()),
            }
        }
    }

    #[test]
    fn uvenv_without_outdated_is_left_out() {
        let lookup = |runner: &Uvenv, program: Option<&str>| {
            let mut found = Found::default();
            uvenv(runner, program, &mut found);
            found
        };
        let report = r#"{"self": null, "outdated": [{"name": "ruff", "installed_version": "1", "available_version": "2"}], "failed": [], "skipped": []}"#;
        let found = lookup(&Uvenv { new: true, report: Ok(report) }, Some("uvenv"));
        assert_eq!((found.updates.len(), found.failed.len()), (1, 0));
        // Too old, or not installed: nothing, and nothing failed.
        assert_eq!(lookup(&Uvenv { new: false, report: Ok(report) }, Some("uvenv")), Found::default());
        assert_eq!(lookup(&Uvenv { new: true, report: Ok(report) }, None), Found::default());
        // A uvenv that has `outdated` but could not run it failed.
        let found = lookup(&Uvenv { new: true, report: Err("uvenv: broken") }, Some("uvenv"));
        assert_eq!(found.failed, [Source::Uvenv]);
    }

    #[test]
    fn pypi_describes_a_package() {
        let json = r#"{"info": {"summary": "A linter", "project_urls": {"Changelog": "https://example.com/changes"}}}"#;
        assert_eq!(
            describe_pypi(json, "ruff", "0.17.5"),
            "A linter\n\nChangelog: https://example.com/changes\nPyPI: https://pypi.org/project/ruff/0.17.5/"
        );
        assert_eq!(describe_pypi("", "ruff", "1"), "PyPI: https://pypi.org/project/ruff/1/");
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_python_package("mistral-vibe"));
        assert!(valid_python_package("zope.interface"));
        assert!(!valid_python_package("-x"));
        assert!(!valid_python_package("x; rm"));
        assert!(valid_flatpak("org.mozilla.firefox//stable"));
        assert!(!valid_flatpak("--system"));
        assert!(!valid_flatpak("org.x; reboot"));
        assert!(valid_snap("my-tool"));
        assert!(!valid_snap("-oops"));
        assert!(!valid_snap("Firefox"));
    }
}
