//! Other package sources that are switched off, as a release upgrade leaves
//! them, and switching them on again for the installed release.
//!
//! `do-release-upgrade` switches off every source that is not the
//! distribution's own, and nothing switches them on again. It leaves them in
//! one of three forms:
//!
//! - a `.sources` stanza with `Enabled: no`, its suites unchanged;
//! - a `.list` file it could not convert, moved to `<name>.list.disabled`;
//! - before 24.04, a commented-out `.list` line ending in
//!   `disabled on upgrade to <codename>`, its suite already moved.
//!
//! Switching one on moves a suite named after an older Ubuntu release, such
//! as `noble` or `noble-pgdg`, to the installed one. A line from a
//! `.list.disabled` file becomes a stanza in `<name>.sources`, as the upgrader
//! converted the rest.

use crate::release::{host_of, official};
use crate::{Files, Runner};

pub const LIST: &str = "/etc/apt/sources.list";
pub const PARTS: &str = "/etc/apt/sources.list.d";

/// What an upgrader before 24.04 added to the lines it commented out.
const MARK: &str = "disabled on upgrade to ";

/// Ubuntu releases, for when distro-info-data is not installed.
const KNOWN: [&str; 8] = ["bionic", "focal", "jammy", "noble", "oracular", "plucky", "questing", "resolute"];

/// One switched-off source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The file it is in, absolute.
    pub file: String,
    /// Which switched-off source of that file, from 0.
    pub index: usize,
    pub uris: Vec<String>,
    /// The suites it gets once switched on.
    pub suites: Vec<String>,
    /// Whether the repository has those suites; none until asked, or when
    /// it could not be reached.
    pub available: Option<bool>,
}

impl Source {
    /// What the helper is told to switch on.
    /// What the helper is told to switch on: `<file>#<index>#<uri>`. The
    /// URI makes sure the source is still the one shown, should the file
    /// change before the helper runs.
    pub fn reference(&self) -> String {
        format!("{}#{}#{}", self.file, self.index, self.uris.first().map_or("", String::as_str))
    }

    pub fn host(&self) -> String {
        self.uris.iter().find_map(|uri| host_of(uri)).unwrap_or_default().to_string()
    }
}

/// How a file keeps its switched-off sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// deb822: stanzas with `Enabled: no`.
    Stanzas,
    /// One-line format: lines the upgrader commented out and marked.
    Marked,
    /// One-line format moved aside: every line is switched off.
    Disabled,
}

fn kind(file: &str) -> Option<Kind> {
    if file.ends_with(".sources") {
        Some(Kind::Stanzas)
    } else if file.ends_with(".list.disabled") {
        Some(Kind::Disabled)
    } else if file.ends_with(".list") {
        Some(Kind::Marked)
    } else {
        None
    }
}

/// Whether the helper may change this file: the sources list, or a plainly
/// named file among the parts.
pub fn valid_file(file: &str) -> bool {
    let name = match file.strip_prefix(PARTS).and_then(|rest| rest.strip_prefix('/')) {
        Some(name) => name,
        None => return file == LIST || file == "/etc/apt/sources.list.disabled",
    };
    !name.is_empty()
        && !name.starts_with('.')
        && name.chars().all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
        && kind(name).is_some()
}

/// A source the helper is told to switch on: `<file>#<index>#<uri>`.
pub fn parse_reference(reference: &str) -> Option<(&str, usize, &str)> {
    let mut parts = reference.splitn(3, '#');
    let file = parts.next()?;
    let index = parts.next()?.parse().ok()?;
    let uri = parts.next().filter(|uri| uri.contains("://"))?;
    valid_file(file).then_some((file, index, uri))
}

/// The Ubuntu release codenames, from distro-info-data.
pub fn codenames(files: &Files) -> Vec<String> {
    let mut names: Vec<String> = files
        .read("/usr/share/distro-info/ubuntu.csv")
        .map(|table| table.lines().skip(1).filter_map(|line| line.split(',').nth(2)).map(str::to_string).collect())
        .unwrap_or_default();
    names.extend(KNOWN.iter().map(|name| (*name).to_string()));
    names.retain(|name| !name.is_empty());
    names.sort();
    names.dedup();
    names
}

/// A suite moved to `release`: `noble` and `noble-pgdg` become
/// `resolute` and `resolute-pgdg`; `stable` stays.
pub fn move_suite(suite: &str, release: &str, codenames: &[String]) -> String {
    if release.is_empty() {
        return suite.to_string();
    }
    for old in codenames.iter().filter(|old| *old != release) {
        if let Some(rest) = suite.strip_prefix(old.as_str())
            && (rest.is_empty() || rest.starts_with('-'))
        {
            return format!("{release}{rest}");
        }
    }
    suite.to_string()
}

fn third_party(uris: &[String]) -> bool {
    !uris.is_empty() && uris.iter().all(|uri| host_of(uri).is_some_and(|host| !official(host)))
}

/// A one-line source: `deb [options] uri suite components # comment`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    kind: String,
    /// What is between the brackets, as written.
    options: Option<String>,
    uri: String,
    suite: String,
    components: Vec<String>,
    comment: Option<String>,
}

impl Line {
    /// Reads a line without its leading `#`.
    fn parse(text: &str) -> Option<Line> {
        let (body, comment) = match text.split_once('#') {
            Some((body, comment)) => (body, Some(comment.to_string())),
            None => (text, None),
        };
        let body = body.trim();
        let (kind, rest) = body.split_once(char::is_whitespace)?;
        if kind != "deb" && kind != "deb-src" {
            return None;
        }
        let rest = rest.trim_start();
        let (options, rest) = match rest.strip_prefix('[') {
            Some(inside) => {
                let (options, rest) = inside.split_once(']')?;
                (Some(options.trim().to_string()), rest)
            }
            None => (None, rest),
        };
        let mut words = rest.split_whitespace();
        let uri = words.next()?.to_string();
        let suite = words.next()?.to_string();
        if !uri.contains(':') {
            return None;
        }
        Some(Line {
            kind: kind.to_string(),
            options,
            uri,
            suite,
            components: words.map(str::to_string).collect(),
            comment,
        })
    }

    fn render(&self) -> String {
        let mut line = self.kind.clone();
        if let Some(options) = &self.options {
            line.push_str(&format!(" [{options}]"));
        }
        line.push_str(&format!(" {} {}", self.uri, self.suite));
        for component in &self.components {
            line.push(' ');
            line.push_str(component);
        }
        if let Some(comment) = self.comment.as_deref().map(str::trim_end).filter(|comment| !comment.trim().is_empty()) {
            line.push_str(&format!(" #{comment}"));
        }
        line
    }

    /// The same source as a deb822 stanza; an option it cannot carry over
    /// is an error.
    fn stanza(&self) -> Result<String, String> {
        let mut stanza = format!("Types: {}\nURIs: {}\nSuites: {}\n", self.kind, self.uri, self.suite);
        if !self.components.is_empty() {
            stanza.push_str(&format!("Components: {}\n", self.components.join(" ")));
        }
        for option in self.options.iter().flat_map(|options| options.split_whitespace()) {
            let (key, value) = option.split_once('=').ok_or_else(|| format!("cannot convert the option {option:?}"))?;
            let field = match key {
                "arch" => "Architectures",
                "lang" => "Languages",
                "target" => "Targets",
                "signed-by" => "Signed-By",
                "trusted" => "Trusted",
                "pdiffs" => "PDiffs",
                "by-hash" => "By-Hash",
                "allow-insecure" => "Allow-Insecure",
                "allow-weak" => "Allow-Weak",
                "allow-downgrade-to-insecure" => "Allow-Downgrade-To-Insecure",
                "check-valid-until" => "Check-Valid-Until",
                "valid-until-min" => "Valid-Until-Min",
                "valid-until-max" => "Valid-Until-Max",
                "check-date" => "Check-Date",
                "date-max-future" => "Date-Max-Future",
                "inrelease-path" => "InRelease-Path",
                "snapshot" => "Snapshot",
                _ => return Err(format!("cannot convert the option {option:?}")),
            };
            stanza.push_str(&format!("{field}: {}\n", value.replace(',', " ")));
        }
        Ok(stanza)
    }
}

/// A commented-out line the upgrader marked, as (the line, the release it
/// names).
fn marked(line: &str) -> Option<(Line, String)> {
    let body = line.trim_start().strip_prefix('#')?.trim_start_matches('#');
    let parsed = Line::parse(body)?;
    let comment = parsed.comment.as_deref()?;
    let release = comment.split(MARK).nth(1)?.split_whitespace().next()?.to_string();
    Some((parsed, release))
}

/// The lines of each stanza of a deb822 file, as (first line, end).
fn stanzas(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut start = None;
    for (number, line) in lines.iter().enumerate() {
        match (line.trim().is_empty(), start) {
            (false, None) => start = Some(number),
            (true, Some(first)) => {
                found.push((first, number));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(first) = start {
        found.push((first, lines.len()));
    }
    found
}

/// A field of a stanza, with its continuation lines: (first line, end, value).
fn field(lines: &[&str], (start, end): (usize, usize), key: &str) -> Option<(usize, usize, String)> {
    let first = (start..end).find(|&number| {
        let line = lines[number];
        !line.starts_with('#') && line.split_once(':').is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case(key))
    })?;
    let mut value = lines[first].split_once(':').map(|(_, value)| value.trim().to_string()).unwrap_or_default();
    let mut last = first + 1;
    while last < end && (lines[last].starts_with(' ') || lines[last].starts_with('\t')) {
        value.push(' ');
        value.push_str(lines[last].trim());
        last += 1;
    }
    Some((first, last, value))
}

fn words(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_string).collect()
}

/// The switched-off stanzas of a deb822 file, with what they hold.
/// A switched-off stanza: its lines, URIs and suites.
type Stanza = ((usize, usize), Vec<String>, Vec<String>);

fn off_stanzas(lines: &[&str]) -> Vec<Stanza> {
    stanzas(lines)
        .into_iter()
        .filter_map(|stanza| {
            let (_, _, enabled) = field(lines, stanza, "Enabled")?;
            if !matches!(enabled.to_ascii_lowercase().as_str(), "no" | "false" | "without" | "off" | "0") {
                return None;
            }
            let types = words(&field(lines, stanza, "Types")?.2);
            let uris = words(&field(lines, stanza, "URIs")?.2);
            let suites = words(&field(lines, stanza, "Suites")?.2);
            (types.iter().any(|kind| kind == "deb") && third_party(&uris)).then_some((stanza, uris, suites))
        })
        .collect()
}

/// A line of a `.list.disabled` file: the upgrader writes each source it
/// could not convert behind one `# `. A line that was commented out before
/// has a second `#` and stays off.
fn moved_aside(line: &str) -> Option<Line> {
    let line = line.trim_start();
    let rest = match line.strip_prefix('#') {
        Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
        None => line,
    };
    if rest.trim_start().starts_with('#') {
        return None;
    }
    Line::parse(rest)
}

/// The switched-off lines of a one-line file, with what they hold.
fn off_lines(lines: &[&str], kind: Kind, release: &str) -> Vec<(usize, Line)> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(number, line)| {
            let parsed = match kind {
                Kind::Marked => marked(line).filter(|(_, target)| target == release).map(|(line, _)| line)?,
                Kind::Disabled => moved_aside(line)?,
                Kind::Stanzas => return None,
            };
            (parsed.kind == "deb" && third_party(std::slice::from_ref(&parsed.uri))).then_some((number, parsed))
        })
        .collect()
}

/// The switched-off sources in one file's text.
pub fn parse(file: &str, text: &str, release: &str, codenames: &[String]) -> Vec<Source> {
    let lines: Vec<&str> = text.lines().collect();
    let moved = |suites: &[String]| suites.iter().map(|suite| move_suite(suite, release, codenames)).collect();
    let source = |index, uris, suites: &[String]| Source {
        file: file.to_string(),
        index,
        uris,
        suites: moved(suites),
        available: None,
    };
    match kind(file) {
        None => Vec::new(),
        Some(Kind::Stanzas) => off_stanzas(&lines)
            .into_iter()
            .enumerate()
            .map(|(index, (_, uris, suites))| source(index, uris, &suites))
            .collect(),
        Some(kind) => off_lines(&lines, kind, release)
            .into_iter()
            .enumerate()
            .map(|(index, (_, line))| source(index, vec![line.uri], &[line.suite]))
            .collect(),
    }
}

/// Every switched-off source on this system, for the installed `release`.
pub fn find(files: &Files, release: &str, codenames: &[String]) -> Vec<Source> {
    let mut paths = vec![LIST.to_string(), format!("{LIST}.disabled")];
    if let Ok(entries) = std::fs::read_dir(files.path(PARTS)) {
        let mut names: Vec<String> =
            entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()).collect();
        names.sort();
        paths.extend(names.into_iter().map(|name| format!("{PARTS}/{name}")));
    }
    paths
        .into_iter()
        .filter(|path| valid_file(path))
        .filter_map(|path| Some(parse(&path, &files.read(&path)?, release, codenames)))
        .flatten()
        .collect()
}

/// Where a suite's release file is.
fn release_url(uri: &str, suite: &str, name: &str) -> String {
    let uri = uri.trim_end_matches('/');
    match suite.strip_suffix('/') {
        // A flat repository names a folder instead of a suite.
        Some(folder) => format!("{uri}/{}/{name}", folder.trim_start_matches('/')).replace("/./", "/"),
        None => format!("{uri}/dists/{suite}/{name}"),
    }
}

/// Whether curl failed because the server has no such file.
fn missing(error: &str) -> bool {
    ["error: 404", "error: 410"].iter().any(|status| error.contains(status))
}

/// Whether the repository has every suite of a source: none when it could
/// not be reached. Uses the network.
pub fn available(runner: &dyn Runner, source: &Source) -> Option<bool> {
    let uri = source.uris.iter().find(|uri| uri.starts_with("http://") || uri.starts_with("https://"))?;
    let mut all = true;
    for suite in &source.suites {
        let mut found = None;
        for name in ["InRelease", "Release"] {
            let url = release_url(uri, suite, name);
            match runner.run("curl", &["-fsS", "-L", "--max-time", "8", "-o", "/dev/null", &url]) {
                Ok(_) => {
                    found = Some(true);
                    break;
                }
                // Only a file that is not there says the suite is missing: a
                // private repository answers 401 to curl and serves APT,
                // which has the password.
                Err(error) if missing(&error) => found = found.or(Some(false)),
                Err(_) => {}
            }
        }
        match found {
            Some(true) => {}
            Some(false) => all = false,
            None => return None,
        }
    }
    Some(all)
}

/// Whether `apt-get update` failed over the sources just switched on, or
/// over the configuration they are part of, rather than another source.
pub fn failed(errors: &str, references: &[String]) -> bool {
    let ours = references.iter().filter_map(|reference| parse_reference(reference));
    errors.lines().any(|line| {
        let line = line.trim();
        line.starts_with("E: ")
            && (line.contains("could not be read")
                || line.contains("Conflicting values")
                || line.contains("Malformed")
                || ours.clone().any(|(file, _, uri)| line.contains(uri.trim_end_matches('/')) || line.contains(file)))
    })
}

/// A change to one file: its new text, or none to remove it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub file: String,
    pub text: Option<String>,
}

/// Where a line from a `.list.disabled` file goes as a stanza.
fn converted(file: &str) -> String {
    if file == format!("{LIST}.disabled") {
        format!("{PARTS}/third-party.sources")
    } else {
        format!("{}.sources", file.trim_end_matches(".list.disabled"))
    }
}

fn joined(lines: &[String], text: &str) -> String {
    let mut joined = lines.join("\n");
    if text.ends_with('\n') || joined.is_empty() {
        joined.push('\n');
    }
    joined
}

/// The edits that switch on the `index`th switched-off source of `file`,
/// whose text is `text`. `read` gives another file's text, for the
/// `.sources` file a converted line joins.
pub fn switch_on(
    file: &str,
    text: &str,
    index: usize,
    uri: &str,
    release: &str,
    codenames: &[String],
    read: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<Edit>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let missing = || format!("{file} has no disabled source {index} for {uri}; refresh and try again");
    let kind = kind(file).ok_or_else(missing)?;
    if kind == Kind::Stanzas {
        let (stanza, _, suites) = off_stanzas(&lines)
            .into_iter()
            .nth(index)
            .filter(|(_, uris, _)| uris.iter().any(|known| known == uri))
            .ok_or_else(missing)?;
        let (enabled, enabled_end, _) = field(&lines, stanza, "Enabled").ok_or_else(missing)?;
        let (suites_at, suites_end, _) = field(&lines, stanza, "Suites").ok_or_else(missing)?;
        let moved: Vec<String> = suites.iter().map(|suite| move_suite(suite, release, codenames)).collect();
        let mut out = Vec::new();
        for (number, line) in lines.iter().enumerate() {
            if (enabled..enabled_end).contains(&number) || (suites_at + 1..suites_end).contains(&number) {
                continue;
            }
            if number == suites_at {
                let key = line.split_once(':').map_or("Suites", |(key, _)| key);
                out.push(format!("{key}: {}", moved.join(" ")));
            } else {
                out.push((*line).to_string());
            }
        }
        return Ok(vec![Edit { file: file.to_string(), text: Some(joined(&out, text)) }]);
    }
    let (number, mut line) = off_lines(&lines, kind, release)
        .into_iter()
        .nth(index)
        .filter(|(_, line)| line.uri == uri)
        .ok_or_else(missing)?;
    line.suite = move_suite(&line.suite, release, codenames);
    let mut out: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();
    if kind == Kind::Marked {
        if let Some(comment) = &line.comment {
            let cut = comment.find(MARK).unwrap_or(comment.len());
            line.comment = Some(comment[..cut].to_string());
        }
        out[number] = line.render();
        return Ok(vec![Edit { file: file.to_string(), text: Some(joined(&out, text)) }]);
    }
    let target = converted(file);
    let stanza = line.stanza()?;
    let existing = read(&target).unwrap_or_default();
    let joined_target =
        if existing.trim().is_empty() { stanza } else { format!("{}\n\n{stanza}", existing.trim_end_matches('\n')) };
    out.remove(number);
    // Kept while it holds another source, even one switched off before.
    let rest = out.iter().any(|line| Line::parse(line.trim_start_matches(['#', ' ', '\t'])).is_some());
    Ok(vec![
        Edit { file: target, text: Some(joined_target) },
        Edit { file: file.to_string(), text: rest.then(|| joined(&out, text)) },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCKER: &str = "https://download.docker.com/linux/ubuntu";

    fn names() -> Vec<String> {
        KNOWN.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn suites_move_to_the_new_release() {
        let names = names();
        assert_eq!(move_suite("noble", "resolute", &names), "resolute");
        assert_eq!(move_suite("noble-pgdg", "resolute", &names), "resolute-pgdg");
        assert_eq!(move_suite("resolute", "resolute", &names), "resolute");
        assert_eq!(move_suite("stable", "resolute", &names), "stable");
        assert_eq!(move_suite("noblesse", "resolute", &names), "noblesse");
    }

    #[test]
    fn switched_off_stanzas_are_found_and_switched_on() {
        let text = "## THIS FILE IS AUTOMATICALLY CONFIGURED ###\n\
                    Types: deb\nURIs: https://apt.tileception.space\nSuites: noble\nComponents: main\n\
                    Signed-By: /usr/share/keyrings/tileception.gpg\nEnabled: no\n\n\
                    Types: deb\nURIs: https://repo.vivaldi.com/stable/deb/\nSuites: stable\nComponents: main\n\n\
                    Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: noble\nEnabled: no\n\n\
                    Types: deb\nURIs: https://cli.github.com/packages\nSuites: stable\nComponents: main\nEnabled: no\n";
        let file = "/etc/apt/sources.list.d/mixed.sources";
        let found = parse(file, text, "resolute", &names());
        assert_eq!(found.len(), 2, "Ubuntu's own and enabled sources are left out: {found:?}");
        assert_eq!(found[0].host(), "apt.tileception.space");
        assert_eq!(found[0].suites, ["resolute"]);
        assert_eq!(found[1].suites, ["stable"]);
        assert_eq!(found[1].reference(), "/etc/apt/sources.list.d/mixed.sources#1#https://cli.github.com/packages");

        // A source that is no longer where it was is left alone.
        assert!(switch_on(file, text, 0, "https://cli.github.com/packages", "resolute", &names(), &|_| None).is_err());
        let edits = switch_on(file, text, 0, "https://apt.tileception.space", "resolute", &names(), &|_| None)
            .expect("switches on");
        let switched = edits[0].text.as_deref().expect("keeps the file");
        assert!(switched.starts_with("## THIS FILE"));
        assert!(switched.contains("URIs: https://apt.tileception.space\nSuites: resolute\nComponents: main\n"));
        assert!(switched.contains("Signed-By: /usr/share/keyrings/tileception.gpg\n\nTypes"));
        assert!(switched.ends_with("Suites: stable\nComponents: main\nEnabled: no\n"));
        // The other one is now the first.
        let left = parse(file, switched, "resolute", &names());
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].host(), "cli.github.com");
    }

    #[test]
    fn marked_lines_of_an_older_upgrade_are_switched_on() {
        let text = "# deb [arch=amd64] https://download.docker.com/linux/ubuntu resolute stable # disabled on upgrade to resolute\n\
                    # deb https://example.com/apt noble main # mine disabled on upgrade to noble\n\
                    # deb https://example.com/old stable main\n";
        let file = "/etc/apt/sources.list.d/docker.list";
        let found = parse(file, text, "resolute", &names());
        assert_eq!(found.len(), 1, "only lines marked for this release: {found:?}");
        assert_eq!(found[0].suites, ["resolute"]);
        let edits = switch_on(file, text, 0, DOCKER, "resolute", &names(), &|_| None).expect("switches on");
        let switched = edits[0].text.as_deref().expect("keeps the file");
        assert_eq!(
            switched.lines().next(),
            Some("deb [arch=amd64] https://download.docker.com/linux/ubuntu resolute stable")
        );
        assert_eq!(switched.lines().count(), 3);
    }

    #[test]
    fn moved_aside_lists_become_stanzas() {
        // As the upgrader writes it: each source behind one `# `.
        let text = "# This file could not be automatically migrated to a .sources file during the upgrade.\n\
                    # Please see sources.list(5) (or https://manpages.ubuntu.com/manpages/man5/sources.list.5.html)\n\
                    # for details on how to migrate manually.\n\n\
                    # deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu noble stable\n\
                    # # deb [signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu noble test\n";
        let file = "/etc/apt/sources.list.d/docker.list.disabled";
        let found = parse(file, text, "resolute", &names());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].suites, ["resolute"]);
        let edits = switch_on(file, text, 0, DOCKER, "resolute", &names(), &|_| None).expect("switches on");
        assert_eq!(
            edits[0],
            Edit {
                file: "/etc/apt/sources.list.d/docker.sources".into(),
                text: Some(
                    "Types: deb\nURIs: https://download.docker.com/linux/ubuntu\nSuites: resolute\nComponents: stable\n\
                     Architectures: amd64\nSigned-By: /etc/apt/keyrings/docker.asc\n"
                        .into()
                ),
            }
        );
        let kept = edits[1].text.as_deref().expect("keeps the line that was off before");
        assert!(parse(file, kept, "resolute", &names()).is_empty());
        let alone = text.lines().filter(|line| !line.starts_with("# #")).collect::<Vec<_>>().join("\n");
        let edits = switch_on(file, &alone, 0, DOCKER, "resolute", &names(), &|_| None).expect("switches on");
        assert_eq!(edits[1], Edit { file: file.into(), text: None }, "nothing is left");
        let two = format!("{text}# deb https://example.com/apt noble main\n");
        let edits = switch_on(file, &two, 0, DOCKER, "resolute", &names(), &|_| None).expect("switches on");
        let kept = edits[1].text.as_deref().expect("keeps the other source");
        assert_eq!(parse(file, kept, "resolute", &names()).len(), 1);

        let existing = |_: &str| Some("Types: deb\nURIs: https://example.com\nSuites: stable\n".to_string());
        let edits = switch_on(file, text, 0, DOCKER, "resolute", &names(), &existing).expect("switches on");
        assert!(
            edits[0]
                .text
                .as_deref()
                .expect("keeps the file")
                .starts_with("Types: deb\nURIs: https://example.com\nSuites: stable\n\nTypes: deb\n")
        );
        assert!(switch_on(file, text, 1, DOCKER, "resolute", &names(), &|_| None).is_err());
    }

    #[test]
    fn the_helper_takes_only_source_files() {
        assert_eq!(
            parse_reference("/etc/apt/sources.list.d/docker.sources#2#https://download.docker.com/linux/ubuntu"),
            Some(("/etc/apt/sources.list.d/docker.sources", 2, "https://download.docker.com/linux/ubuntu"))
        );
        assert_eq!(
            parse_reference("/etc/apt/sources.list#0#http://example.com/a#b"),
            Some(("/etc/apt/sources.list", 0, "http://example.com/a#b"))
        );
        assert_eq!(
            parse_reference("/etc/apt/sources.list.d/docker.list.disabled#0#https://x.org").map(|(_, index, _)| index),
            Some(0)
        );
        for bad in [
            "/etc/apt/sources.list.d/../../shadow.sources#0#https://x.org",
            "/etc/passwd#0#https://x.org",
            "/etc/apt/sources.list.d/sub/x.sources#0#https://x.org",
            "/etc/apt/sources.list.d/x.conf#0#https://x.org",
            "/etc/apt/sources.list.d/x.sources",
            "/etc/apt/sources.list.d/x.sources#-1#https://x.org",
            "/etc/apt/sources.list.d/x.sources#0",
            "/etc/apt/sources.list.d/x.sources#0#not-a-uri",
        ] {
            assert_eq!(parse_reference(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_missing_file_says_the_suite_is_missing() {
        assert!(missing("curl: (22) The requested URL returned error: 404"));
        assert!(!missing("curl: (22) The requested URL returned error: 401"));
        assert!(!missing("curl: (6) Could not resolve host: example.org"));
    }

    #[test]
    fn only_failures_of_these_sources_switch_them_off_again() {
        let ours = vec![format!("/etc/apt/sources.list.d/docker.sources#0#{DOCKER}")];
        let other =
            "E: The repository 'https://ppa.example.org/ubuntu resolute Release' does not have a Release file.\n";
        assert!(!failed(other, &ours));
        let mine = format!("E: The repository '{DOCKER} resolute Release' does not have a Release file.\n");
        assert!(failed(&mine, &ours));
        let conflict = "E: Conflicting values set for option Signed-By regarding source https://example.org/ stable\n\
                        E: The list of sources could not be read.\n";
        assert!(failed(conflict, &ours));
        assert!(!failed("W: Some index files failed to download.\n", &ours));
    }

    #[test]
    fn release_files_are_found_for_flat_repositories_too() {
        assert_eq!(
            release_url("https://download.docker.com/linux/ubuntu/", "resolute", "InRelease"),
            "https://download.docker.com/linux/ubuntu/dists/resolute/InRelease"
        );
        assert_eq!(release_url("https://example.com/repo", "./", "Release"), "https://example.com/repo/Release");
    }
}
