//! Choosing an Ubuntu mirror: which one the package sources use, how fast
//! each candidate answers and how far behind the main archive it is.
//! Testing a mirror uses the network, so it happens only when asked.

use crate::{Files, Runner};

/// The main archive, which the others copy from.
pub const ARCHIVE: &str = "http://archive.ubuntu.com/ubuntu";

/// Where Ubuntu lists the mirrors near the asking machine.
pub const NEARBY: &str = "http://mirrors.ubuntu.com/mirrors.txt";

/// The deb822 file of Ubuntu's own sources, and the one-line form before it.
pub const SOURCES: &str = "/etc/apt/sources.list.d/ubuntu.sources";
pub const LIST: &str = "/etc/apt/sources.list";

/// How long a mirror may lag before it counts as stale.
pub const STALE_AFTER: i64 = 24 * 3600;

/// A mirror's address without a trailing slash, the way they compare.
fn normal(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// Whether a URI in the sources is the main archive or a copy of it,
/// rather than the security pocket, a PPA or someone else's repository.
fn archive_like(uri: &str) -> bool {
    let uri = normal(uri);
    (uri.starts_with("http://") || uri.starts_with("https://"))
        && !uri.contains("security.ubuntu.com")
        && !uri.contains("ppa.launchpad")
        && !uri.contains("ports.ubuntu.com")
        && (uri.ends_with("/ubuntu") || uri.contains("archive.ubuntu.com") || uri.contains("mirror"))
}

/// The stanzas of a deb822 file as (URIs, Suites) pairs.
fn stanzas(text: &str) -> Vec<(Vec<String>, Vec<String>)> {
    text.split("\n\n")
        .map(|stanza| {
            let field = |key: &str| -> Vec<String> {
                stanza
                    .lines()
                    .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
                    .map(|value| value.split_whitespace().map(str::to_string).collect())
                    .unwrap_or_default()
            };
            (field("URIs"), field("Suites"))
        })
        .collect()
}

/// The mirror the sources use for the release and its updates.
pub fn current_in(sources: Option<&str>, list: Option<&str>) -> Option<String> {
    if let Some(text) = sources {
        for (uris, suites) in stanzas(text) {
            if suites.iter().any(|suite| suite.ends_with("-security")) && suites.len() == 1 {
                continue;
            }
            if let Some(uri) = uris.iter().find(|uri| archive_like(uri)) {
                return Some(normal(uri));
            }
        }
    }
    list?.lines().map(str::trim).filter(|line| line.starts_with("deb ")).find_map(|line| {
        let uri = line.split_whitespace().skip(1).find(|word| !word.starts_with('['))?;
        let suite = line.split_whitespace().skip_while(|word| *word != uri).nth(1).unwrap_or_default();
        (archive_like(uri) && !suite.ends_with("-security")).then(|| normal(uri))
    })
}

pub fn current(files: &Files) -> Option<String> {
    current_in(files.read(SOURCES).as_deref(), files.read(LIST).as_deref())
}

/// The sources with `from` replaced by `to` wherever it is the archive,
/// leaving the security pocket and everything else as it is.
pub fn rewrite(text: &str, from: &str, into: &str) -> String {
    let from = normal(from);
    let into = normal(into);
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let replaced = if line.trim_start().starts_with('#') {
            line.to_string()
        } else {
            let mut words: Vec<String> = Vec::new();
            for word in line.split(' ') {
                let bare = word.trim_end_matches('\n');
                if normal(bare) == from {
                    words.push(word.replacen(bare, &into, 1));
                } else {
                    words.push(word.to_string());
                }
            }
            words.join(" ")
        };
        out.push_str(&replaced);
    }
    out
}

/// Whether this is a mirror address the helper writes into the sources.
pub fn valid(url: &str) -> bool {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && rest.chars().next().is_some_and(|first| first.is_ascii_alphanumeric())
            && rest.chars().all(|character| character.is_ascii_alphanumeric() || "-._~/:".contains(character))
    })
}

/// The candidate mirrors: the one in use, the main archive, and the ones
/// Ubuntu lists for this country, each once.
pub fn candidates(current: Option<&str>, nearby: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let listed = nearby.lines().map(normal).filter(|url| valid(url));
    for url in current.map(normal).into_iter().chain([normal(ARCHIVE)]).chain(listed) {
        if !found.contains(&url) {
            found.push(url);
        }
    }
    found
}

/// One mirror, as last tested.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mirror {
    pub url: String,
    /// Seconds until its release file arrived.
    pub latency: Option<f64>,
    /// Bytes per second over a larger file.
    pub speed: Option<u64>,
    /// When its copy of the release was made, in seconds since 1970.
    pub published: Option<i64>,
    /// Why it could not be reached.
    pub error: Option<String>,
}

impl Mirror {
    /// How far behind `reference` it is, in seconds.
    pub fn lag(&self, reference: Option<i64>) -> Option<i64> {
        Some((reference? - self.published?).max(0))
    }

    /// Whether it should not be used: unreachable, or a day or more behind.
    pub fn stale(&self, reference: Option<i64>) -> bool {
        self.error.is_some() || self.lag(reference).is_some_and(|lag| lag >= STALE_AFTER)
    }
}

/// Days since 1970 for a date in the proleptic Gregorian calendar.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_cycle - 719_468
}

/// The `Date:` of a release file: `Thu, 12 Nov 2026 09:21:33 UTC`.
pub fn parse_date(release: &str) -> Option<i64> {
    let value = release.lines().find_map(|line| line.strip_prefix("Date: "))?;
    let parts: Vec<&str> = value.split_whitespace().collect();
    let [_, day, month, year, time, ..] = parts.as_slice() else { return None };
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = MONTHS.iter().position(|name| name == month)? as i64 + 1;
    let mut clock = time.split(':').map(|part| part.parse::<i64>().ok());
    let (hours, minutes, seconds) = (clock.next()??, clock.next()??, clock.next()??);
    Some(days(year.parse().ok()?, month, day.parse().ok()?) * 86_400 + hours * 3600 + minutes * 60 + seconds)
}

/// What curl printed after the body: `\n<seconds> <bytes per second>`.
fn timing(output: &str) -> (&str, Option<f64>, Option<u64>) {
    let Some((body, last)) = output.trim_end().rsplit_once('\n') else { return (output, None, None) };
    let mut numbers = last.split_whitespace();
    let seconds = numbers.next().and_then(|value| value.parse().ok());
    let speed = numbers.next().and_then(|value| value.parse::<f64>().ok()).map(|value| value as u64);
    (body, seconds, speed)
}

/// Fetches part of a file with curl: its text, the seconds it took and the speed.
fn fetch(runner: &dyn Runner, url: &str, bytes: u32) -> Result<(String, Option<f64>, Option<u64>), String> {
    let range = format!("0-{}", bytes.saturating_sub(1));
    let output = runner
        .run("curl", &["-fsSL", "--max-time", "8", "-r", &range, "-w", "\n%{time_total} %{speed_download}", url])?;
    let (body, seconds, speed) = timing(&output);
    Ok((body.to_string(), seconds, speed))
}

/// Tests one mirror: how quickly its release file comes, how fast a larger
/// file arrives, and when its copy of the release was made.
pub fn test(runner: &dyn Runner, url: &str, codename: &str) -> Mirror {
    let url = normal(url);
    let mut mirror = Mirror { url: url.clone(), ..Mirror::default() };
    if !valid(&url) || codename.is_empty() {
        mirror.error = Some("not a mirror address".into());
        return mirror;
    }
    match fetch(runner, &format!("{url}/dists/{codename}-updates/InRelease"), 4096) {
        Ok((body, seconds, _)) => {
            mirror.latency = seconds;
            mirror.published = parse_date(&body);
            if mirror.published.is_none() {
                mirror.error = Some("its release file has no date".into());
            }
        }
        Err(error) => {
            mirror.error = Some(error.trim_start_matches("curl: ").to_string());
            return mirror;
        }
    }
    // A quarter of a megabyte says more about speed than a small file.
    if let Ok((_, _, speed)) = fetch(runner, &format!("{url}/dists/{codename}/Contents-amd64.gz"), 256 * 1024) {
        mirror.speed = speed;
    }
    mirror
}

/// Tests the mirror in use against the main archive, for the warning.
pub fn check_current(runner: &dyn Runner, files: &Files, codename: &str) -> Option<(Mirror, Option<i64>)> {
    let current = current(files)?;
    let reference = test(runner, ARCHIVE, codename).published;
    Some((test(runner, &current, codename), reference))
}

/// Lists and tests the candidates, the main archive first so the others
/// compare against it.
pub fn survey(runner: &dyn Runner, files: &Files, codename: &str) -> Vec<Mirror> {
    let nearby = runner.run("curl", &["-fsSL", "--max-time", "8", NEARBY]).unwrap_or_default();
    let mut found: Vec<Mirror> =
        candidates(current(files).as_deref(), &nearby).iter().take(12).map(|url| test(runner, url, codename)).collect();
    // Fastest first, unreachable ones last; the order is stable for equals.
    found.sort_by(|left, right| match (left.speed, right.speed, &left.error, &right.error) {
        (_, _, None, Some(_)) => std::cmp::Ordering::Less,
        (_, _, Some(_), None) => std::cmp::Ordering::Greater,
        (Some(left), Some(right), ..) => right.cmp(&left),
        (Some(_), None, ..) => std::cmp::Ordering::Less,
        (None, Some(_), ..) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    found
}

/// A lag for people: "up to date", "3 hours behind", "2 days behind".
pub fn describe_lag(lag: Option<i64>) -> String {
    match lag {
        None => "age unknown".into(),
        Some(lag) if lag < 3600 => "up to date".into(),
        Some(lag) if lag < 2 * 3600 => "1 hour behind".into(),
        Some(lag) if lag < 2 * 86_400 => format!("{} hours behind", lag / 3600),
        Some(lag) => format!("{} days behind", lag / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &str = "\
## comment URIs: http://nowhere/ubuntu
Types: deb
URIs: http://nl.archive.ubuntu.com/ubuntu/
Suites: noble noble-updates noble-backports
Components: main restricted universe multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg

Types: deb
URIs: http://security.ubuntu.com/ubuntu/
Suites: noble-security
Components: main restricted universe multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
";

    #[test]
    fn the_mirror_in_use_is_found() {
        assert_eq!(current_in(Some(SOURCES), None).as_deref(), Some("http://nl.archive.ubuntu.com/ubuntu"));
        let list = "deb http://security.ubuntu.com/ubuntu jammy-security main\n\
                    deb [arch=amd64] http://ftp.snt.utwente.nl/pub/os/linux/ubuntu/ jammy main\n";
        assert_eq!(current_in(None, Some(list)).as_deref(), Some("http://ftp.snt.utwente.nl/pub/os/linux/ubuntu"));
        assert_eq!(current_in(None, Some("deb http://ppa.launchpad.net/x/ubuntu noble main\n")), None);
    }

    #[test]
    fn rewriting_leaves_security_and_comments() {
        let rewritten = rewrite(SOURCES, "http://nl.archive.ubuntu.com/ubuntu/", "https://mirror.example.org/ubuntu");
        assert!(rewritten.contains("URIs: https://mirror.example.org/ubuntu\n"));
        assert!(rewritten.contains("URIs: http://security.ubuntu.com/ubuntu/\n"));
        assert!(rewritten.contains("## comment URIs: http://nowhere/ubuntu"));
        assert_eq!(current_in(Some(&rewritten), None).as_deref(), Some("https://mirror.example.org/ubuntu"));
    }

    #[test]
    fn addresses_are_checked() {
        assert!(valid("https://mirror.example.org/ubuntu"));
        assert!(valid("http://ftp.snt.utwente.nl:80/pub/os/linux/ubuntu"));
        assert!(!valid("ftp://mirror.example.org/ubuntu"));
        assert!(!valid("http://mirror example/ubuntu"));
        assert!(!valid("http://-o/ubuntu"));
        assert!(!valid("http://x/ubuntu\nURIs: http://evil"));
    }

    #[test]
    fn candidates_come_once() {
        let nearby = "http://nl.archive.ubuntu.com/ubuntu/\nhttps://mirror.example.org/ubuntu/\nnonsense\n";
        assert_eq!(
            candidates(Some("http://nl.archive.ubuntu.com/ubuntu/"), nearby),
            ["http://nl.archive.ubuntu.com/ubuntu", ARCHIVE, "https://mirror.example.org/ubuntu"]
        );
    }

    #[test]
    fn release_dates_and_lag() {
        let release = "Origin: Ubuntu\nDate: Thu, 12 Nov 2026 09:21:33 UTC\nSuite: noble-updates\n";
        let published = parse_date(release);
        assert_eq!(published, Some(1_794_475_293));
        assert_eq!(parse_date("Date: Thu, 01 Jan 1970 00:00:00 UTC"), Some(0));
        // The archive pads a one-digit hour with a space.
        assert_eq!(parse_date("Date: Thu, 12 Nov 2026  8:29:02 UTC"), Some(1_794_475_293 - 52 * 60 - 31));
        let mirror = Mirror { published: published.map(|time| time - 3 * 3600), ..Mirror::default() };
        assert_eq!(describe_lag(mirror.lag(published)), "3 hours behind");
        assert!(!mirror.stale(published));
        let old = Mirror { published: published.map(|time| time - 3 * 86_400), ..Mirror::default() };
        assert!(old.stale(published));
        assert_eq!(describe_lag(old.lag(published)), "3 days behind");
        assert!(Mirror { error: Some("timed out".into()), ..Mirror::default() }.stale(published));
    }

    #[test]
    fn curl_timing_follows_the_body() {
        let (body, seconds, speed) = timing("Date: x\nmore\n0.231 1083421.000");
        assert_eq!(body, "Date: x\nmore");
        assert_eq!(seconds, Some(0.231));
        assert_eq!(speed, Some(1_083_421));
    }
}
