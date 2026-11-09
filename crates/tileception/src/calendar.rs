//! `[calendar]`: events from .ics files and URLs, read again every few
//! minutes on a thread of their own. The bar shows the next one today
//! beside the clock, and the clock's calendar lists what is coming.

use std::{sync::mpsc, time::Duration};

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
            loop {
                if sender.send(read_all(&config.sources)).is_err() {
                    return;
                }
                // A message or a dropped sender both mean stop.
                if !matches!(stopped.recv_timeout(every), Err(mpsc::RecvTimeoutError::Timeout)) {
                    return;
                }
            }
        });
        if let Err(error) = spawned {
            tracing::warn!("calendar: {error}");
        }
    }

    /// The next event today that has not started yet, for the bar.
    pub(crate) fn calendar_next(&self) -> Option<&Occurrence> {
        let now = now();
        crate::ics::next_today(&self.calendar.events, now, local_midnight(now, 1))
    }
}

/// Every source's occurrences from the start of today, a week ahead.
fn read_all(sources: &[String]) -> Read {
    let now = now();
    let (from, until) = (local_midnight(now, 0), local_midnight(now, DAYS_AHEAD + 1));
    let named = |tzid: &str| TimeZone::named(tzid).map(|zone| Box::new(zone) as Box<dyn Zone>);
    let zones = Zones { local: &LocalZone, named: &named };
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for source in sources {
        match fetch(source) {
            Ok(text) => events.extend(crate::ics::occurrences(&text, &zones, from, until)),
            Err(error) => errors.push(format!("{source}: {error}")),
        }
    }
    events.sort_by_key(|event| (event.start, event.end));
    Read { events, errors }
}

/// A file's text, or a URL's through curl, bounded in size and time.
fn fetch(source: &str) -> Result<String, String> {
    let url = match source.strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => source.to_string(),
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        let path = match source.strip_prefix("~/") {
            Some(rest) => std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
            None => std::path::PathBuf::from(source),
        };
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
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
        return Err(message.trim().trim_start_matches("curl: ").to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
