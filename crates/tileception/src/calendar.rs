//! `[calendar]`: events from .ics files and URLs, read again every few
//! minutes on a thread of their own. The bar shows the one on now, or else
//! the next one today, beside the clock, and the clock's calendar lists what
//! is coming.

use std::{collections::HashMap, sync::mpsc, time::Duration};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::{RegistrationToken, channel};

use crate::{
    Tileception,
    ics::{Occurrence, TimeZone, Zone, Zones},
};

const DAY: i64 = 86_400;
/// How far ahead the calendar looks.
pub const DAYS_AHEAD: i64 = 7;
/// A calendar larger than this is not read.
const MAX_BYTES: &str = "8M";
/// How soon a round with a source that could not be read is tried again.
const RETRY: Duration = Duration::from_secs(60);

/// The `[calendar]` section.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarConfig {
    /// .ics files (`~/` is the home folder) and http(s) or webcal URLs.
    pub sources: Vec<String>,
    /// Minutes between reading them again.
    pub refresh_minutes: u64,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        CalendarConfig { sources: Vec::new(), refresh_minutes: 15 }
    }
}

/// The events read last, and the thread reading them.
#[derive(Default)]
pub struct Agenda {
    /// From the start of the day they were read on, a week ahead, by start.
    pub events: Vec<Occurrence>,
    /// Sources that could not be read, the last time.
    pub errors: Vec<String>,
    /// Ends the thread when dropped.
    stop: Option<mpsc::Sender<()>>,
    source: Option<RegistrationToken>,
}

/// What the thread reports after each round.
struct Read {
    events: Vec<Occurrence>,
    errors: Vec<String>,
}

impl Tileception {
    /// Starts or stops reading the calendars after `[calendar]` changed, or
    /// at startup.
    pub fn apply_calendar(&mut self) {
        self.calendar.stop = None;
        if let Some(source) = self.calendar.source.take() {
            self.loop_handle.remove(source);
        }
        self.calendar.events.clear();
        self.calendar.errors.clear();
        let config = self.config.file.calendar.clone();
        if config.sources.is_empty() {
            self.schedule_render();
            return;
        }
        let (sender, receiver) = channel::channel();
        let source = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(Read { events, errors }) = event {
                for error in errors.iter().filter(|error| !state.calendar.errors.contains(error)) {
                    tracing::warn!("calendar: {error}");
                }
                state.calendar.events = events;
                state.calendar.errors = errors;
                state.schedule_render();
            }
        });
        match source {
            Ok(source) => self.calendar.source = Some(source),
            Err(error) => {
                tracing::warn!("calendar: {error}");
                return;
            }
        }
        let (stop, stopped) = mpsc::channel();
        self.calendar.stop = Some(stop);
        let spawned = std::thread::Builder::new().name("tileception-calendar".into()).spawn(move || {
            let every = Duration::from_secs(config.refresh_minutes.max(1) * 60);
            // Each source's last data that could be read, shown while it cannot.
            let mut last_good = HashMap::new();
            loop {
                let read = read_all(&config.sources, &mut last_good);
                let wait = if read.errors.is_empty() { every } else { every.min(RETRY) };
                if sender.send(read).is_err() {
                    return;
                }
                // A message or a dropped sender both mean stop.
                if !matches!(stopped.recv_timeout(wait), Err(mpsc::RecvTimeoutError::Timeout)) {
                    return;
                }
            }
        });
        if let Err(error) = spawned {
            tracing::warn!("calendar: {error}");
        }
    }

    /// The event for the bar, and whether it is on now: the one on now,
    /// otherwise the next today that has not started yet.
    pub(crate) fn calendar_chip(&self) -> Option<(&Occurrence, bool)> {
        let now = now();
        if let Some(event) = crate::ics::current(&self.calendar.events, now) {
            return Some((event, true));
        }
        crate::ics::next_today(&self.calendar.events, now, local_midnight(now, 1)).map(|event| (event, false))
    }
}

/// Every source's occurrences from the start of today, a week ahead.
fn read_all(sources: &[String], last_good: &mut HashMap<String, Vec<u8>>) -> Read {
    let now = now();
    let (from, until) = (local_midnight(now, 0), local_midnight(now, DAYS_AHEAD + 1));
    let named = |tzid: &str| TimeZone::named(tzid).map(|zone| Box::new(zone) as Box<dyn Zone>);
    let zones = Zones { local: &LocalZone, named: &named };
    read_sources(sources, last_good, fetch, &zones, from, until)
}

/// One round over the sources. A source that cannot be read is named in
/// the errors, and its last data that could be read still shows, so a
/// moment offline does not empty the calendar.
fn read_sources(
    sources: &[String],
    last_good: &mut HashMap<String, Vec<u8>>,
    fetch: impl Fn(&str) -> Result<Vec<u8>, String>,
    zones: &Zones,
    from: i64,
    until: i64,
) -> Read {
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for source in sources {
        match fetch(source) {
            Ok(data) => {
                last_good.insert(source.clone(), data);
            }
            Err(error) => errors.push(format!("{}: {error}", source_name(source))),
        }
        if let Some(data) = last_good.get(source) {
            events.extend(crate::ics::occurrences(data, zones, from, until));
        }
    }
    events.sort_by_key(|event| (event.start, event.end));
    Read { events, errors }
}

/// How the calendar and the log name a source: a file by its path, a URL
/// by its scheme and host only, as a calendar's address is often a secret
/// that would otherwise show in screenshots and logs.
fn source_name(source: &str) -> String {
    let Some((scheme, rest)) = source.split_once("://") else { return source.to_string() };
    if !["http", "https", "webcal"].iter().any(|known| scheme.eq_ignore_ascii_case(known)) {
        return source.to_string();
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let tail = &rest[authority.len()..];
    if tail.is_empty() || tail == "/" { format!("{scheme}://{host}") } else { format!("{scheme}://{host}/…") }
}

/// A file's data, or a URL's through curl, bounded in size and time.
fn fetch(source: &str) -> Result<Vec<u8>, String> {
    let url = match source.strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => source.to_string(),
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        let path = match source.strip_prefix("~/") {
            Some(rest) => std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
            None => std::path::PathBuf::from(source),
        };
        return std::fs::read(&path).map_err(|error| error.to_string());
    }
    let output = std::process::Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--max-redirs", "5"])
        .args(["--proto", "=https,http", "--proto-redir", "=https,http"])
        .args([
            "--user-agent",
            concat!("Tileception/", env!("CARGO_PKG_VERSION"), " (calendar; +https://tileception.space)"),
        ])
        .args(["--max-filesize", MAX_BYTES, "--max-time", "60", "--"])
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("curl: {error}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        // Its own message may quote the address.
        let message = message.replace(&url, &source_name(&url)).replace(source, &source_name(source));
        return Err(message.trim().trim_start_matches("curl: ").to_string());
    }
    Ok(output.stdout)
}

/// Seconds since 1970, UTC.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
}

/// UTC time of local midnight `days` after the day `utc` falls on.
pub fn local_midnight(utc: i64, days: i64) -> i64 {
    let Some(today) = crate::ui::LocalTime::at(utc) else { return utc - utc.rem_euclid(DAY) + days * DAY };
    let day = crate::bar::days_from_civil(today.year, today.month as i32 + 1, today.day);
    LocalZone.to_utc((day + days) * DAY)
}

/// The system's local time zone, through libc.
struct LocalZone;

impl Zone for LocalZone {
    #[allow(unsafe_code, reason = "mktime applies the local time zone, as localtime_r does for the clock")]
    fn to_utc(&self, wall: i64) -> i64 {
        let (year, month, day) = crate::bar::civil_from_days(wall.div_euclid(DAY));
        let seconds = wall.rem_euclid(DAY);
        // SAFETY: tm is plain data; zeroed is a valid value that mktime overwrites.
        let mut calendar: libc::tm = unsafe { std::mem::zeroed() };
        calendar.tm_year = year - 1900;
        calendar.tm_mon = month - 1;
        calendar.tm_mday = day;
        calendar.tm_hour = (seconds / 3600) as i32;
        calendar.tm_min = (seconds / 60 % 60) as i32;
        calendar.tm_sec = (seconds % 60) as i32;
        // Let the zone's rules decide whether summer time applies.
        calendar.tm_isdst = -1;
        // SAFETY: mktime reads and normalizes the caller-owned `calendar`.
        let utc = unsafe { libc::mktime(&mut calendar) };
        if utc == -1 { wall } else { utc }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ics::Utc;

    #[test]
    fn a_source_that_fails_keeps_its_last_events() {
        let sources = vec!["/calendars/team.ics".to_string(), "https://example.org/private-123/basic.ics".to_string()];
        let data = |summary: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{summary}\r\nDTSTART:19700101T100000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
            .into_bytes()
        };
        let named = |_: &str| -> Option<Box<dyn Zone>> { None };
        let zones = Zones { local: &Utc, named: &named };
        let summaries =
            |read: &Read| -> Vec<String> { read.events.iter().map(|event| event.summary.clone()).collect() };
        let mut last_good = HashMap::new();
        let working = |source: &str| -> Result<Vec<u8>, String> {
            Ok(data(if source.ends_with("team.ics") { "Team" } else { "Private" }))
        };
        let read = read_sources(&sources, &mut last_good, working, &zones, 0, DAY);
        assert_eq!(summaries(&read), ["Team", "Private"]);
        assert!(read.errors.is_empty());
        // Offline: both still show, and both are named as failing.
        let offline = |_: &str| -> Result<Vec<u8>, String> { Err("Could not resolve host".into()) };
        let read = read_sources(&sources, &mut last_good, offline, &zones, 0, DAY);
        assert_eq!(summaries(&read), ["Team", "Private"]);
        assert_eq!(
            read.errors,
            ["/calendars/team.ics: Could not resolve host", "https://example.org/…: Could not resolve host"]
        );
        // A source never read has nothing to show.
        let read = read_sources(&sources, &mut HashMap::new(), offline, &zones, 0, DAY);
        assert!(read.events.is_empty());
    }

    #[test]
    fn a_url_source_is_named_by_its_host() {
        assert_eq!(
            source_name("https://calendar.invalid/ical/me%40example.org/private-0123456789abcdef/basic.ics"),
            "https://calendar.invalid/…"
        );
        assert_eq!(source_name("webcal://user:secret@example.org:8443/cal?token=x"), "webcal://example.org:8443/…");
        assert_eq!(source_name("http://127.0.0.1:8765/"), "http://127.0.0.1:8765");
        assert_eq!(source_name("https://example.org?key=1"), "https://example.org/…");
        assert_eq!(source_name("~/Calendars/family.ics"), "~/Calendars/family.ics");
        assert_eq!(source_name("/srv/a://b.ics"), "/srv/a://b.ics");
    }
}
