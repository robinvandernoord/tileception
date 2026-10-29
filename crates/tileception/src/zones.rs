//! Time zones from the system's zoneinfo files (TZif), and calendar
//! arithmetic, for the launcher's "time in tokyo" and date answers. Nothing
//! here touches the process's own time zone settings.

use std::path::{Path, PathBuf};

const ZONEINFO: &str = "/usr/share/zoneinfo";

pub const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
pub const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Days since 1970-01-01 of a proleptic Gregorian date (month and day from 1).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day count since 1970-01-01: (year, month, day).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_index = (5 * doy + 2) / 153;
    let day = (doy - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// Monday is 0.
pub fn weekday(days: i64) -> usize {
    (days + 3).rem_euclid(7) as usize
}

fn days_in_month(year: i64, month: u32) -> u32 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Whether a date exists.
pub fn valid_date(year: i64, month: u32, day: u32) -> bool {
    (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)
}

/// A wall-clock moment, broken down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// Monday is 0.
    pub weekday: usize,
}

impl Civil {
    pub fn from_seconds(seconds: i64) -> Civil {
        let days = seconds.div_euclid(86_400);
        let secs = seconds.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        Civil { year, month, day, hour: secs / 3600, minute: secs / 60 % 60, second: secs % 60, weekday: weekday(days) }
    }

    /// "Saturday 17 October"
    pub fn long_date(&self) -> String {
        format!("{} {} {}", WEEKDAYS[self.weekday], self.day, MONTHS[self.month as usize - 1])
    }

    /// "2026-10-17 14:05:00"
    pub fn iso(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// "UTC+9", "UTC-3:30", "UTC"
pub fn offset_label(offset: i64) -> String {
    if offset == 0 {
        return "UTC".into();
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let (hours, minutes) = (offset.abs() / 3600, offset.abs() / 60 % 60);
    if minutes == 0 { format!("UTC{sign}{hours}") } else { format!("UTC{sign}{hours}:{minutes:02}") }
}

/// One local time type: its offset from UTC in seconds and its abbreviation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    pub offset: i64,
    pub abbreviation: String,
}

/// A time zone read from a TZif file.
#[derive(Debug, Clone)]
pub struct Zone {
    transitions: Vec<(i64, usize)>,
    types: Vec<Local>,
    /// The POSIX TZ rule for times after the last transition.
    rule: Option<Rule>,
}

impl Zone {
    /// Loads a zone such as "Asia/Tokyo" from the system zoneinfo.
    pub fn load(name: &str) -> Option<Zone> {
        if name.split('/').any(|part| part.is_empty() || part == "..") {
            return None;
        }
        Zone::parse(&std::fs::read(Path::new(ZONEINFO).join(name)).ok()?)
    }

    pub fn parse(data: &[u8]) -> Option<Zone> {
        let header = |data: &[u8]| -> Option<[usize; 6]> {
            if data.get(..4)? != b"TZif" {
                return None;
            }
            let mut counts = [0; 6];
            for (index, count) in counts.iter_mut().enumerate() {
                let offset = 20 + index * 4;
                *count = u32::from_be_bytes(data.get(offset..offset + 4)?.try_into().ok()?) as usize;
            }
            Some(counts)
        };
        let [isut, isstd, leap, time, kinds, chars] = header(data)?;
        let version = *data.get(4)?;
        let v1_len = time * 5 + kinds * 6 + chars + leap * 8 + isstd + isut;
        // Version 2 and later repeat the data with 64-bit times, then a TZ rule.
        let (data, size) = if version >= b'2' { (data.get(44 + v1_len..)?, 8) } else { (data, 4) };
        let [isut, isstd, leap, time, kinds, chars] = header(data)?;
        let mut cursor = 44;
        let mut take = |length: usize| -> Option<&[u8]> {
            let slice = data.get(cursor..cursor + length)?;
            cursor += length;
            Some(slice)
        };
        let times = take(time * size)?;
        let indices = take(time)?;
        let infos = take(kinds * 6)?;
        let names = take(chars)?;
        take(leap * (size + 4) + isstd + isut)?;
        let end = 44 + time * (size + 1) + kinds * 6 + chars + leap * (size + 4) + isstd + isut;
        let footer = if size == 8 { &data[end..] } else { &[][..] };
        let types = infos
            .chunks(6)
            .map(|info| {
                let offset = i64::from(i32::from_be_bytes(info[..4].try_into().ok()?));
                let name = &names.get(usize::from(info[5])..)?;
                let end = name.iter().position(|byte| *byte == 0).unwrap_or(name.len());
                Some(Local { offset, abbreviation: String::from_utf8_lossy(&name[..end]).into_owned() })
            })
            .collect::<Option<Vec<_>>>()?;
        let transitions = times
            .chunks(size)
            .zip(indices)
            .map(|(time, index)| {
                let time = if size == 8 {
                    i64::from_be_bytes(time.try_into().ok()?)
                } else {
                    i64::from(i32::from_be_bytes(time.try_into().ok()?))
                };
                (usize::from(*index) < types.len()).then_some((time, usize::from(*index)))
            })
            .collect::<Option<Vec<_>>>()?;
        let rule = std::str::from_utf8(footer)
            .ok()
            .map(str::trim)
            .filter(|rule_text| !rule_text.is_empty())
            .and_then(Rule::parse);
        if types.is_empty() && rule.is_none() {
            return None;
        }
        Some(Zone { transitions, types, rule })
    }

    /// The local time type in force at `seconds` (seconds since the epoch, UTC).
    pub fn local(&self, seconds: i64) -> Local {
        let after_last = self.transitions.last().is_none_or(|(last, _)| seconds >= *last);
        if after_last && let Some(rule) = &self.rule {
            return rule.local(seconds);
        }
        match self.transitions.iter().rposition(|(time, _)| *time <= seconds) {
            Some(index) => self.types[self.transitions[index].1].clone(),
            None => self.types.first().cloned().unwrap_or(Local { offset: 0, abbreviation: "UTC".into() }),
        }
    }
}

/// A POSIX TZ string such as `CET-1CEST,M3.5.0,M10.5.0/3`.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    standard: Local,
    summer: Option<(Local, Change, Change)>,
}

/// When summer time starts or ends: a date rule and the local time of day.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Change {
    /// The d'th weekday (Sunday 0) of week w (5 = last) of month m.
    Month { month: u32, week: u32, day: u32, time: i64 },
    /// Day n of the year, 1 to 365, never counting 29 February.
    Julian { day: u32, time: i64 },
    /// Day n of the year, 0 to 365, counting 29 February.
    Day { day: u32, time: i64 },
}

impl Rule {
    pub fn parse(text: &str) -> Option<Rule> {
        let mut rest = text;
        let standard_name = name(&mut rest)?;
        let standard_offset = -signed_time(&mut rest)?;
        let standard = Local { offset: standard_offset, abbreviation: standard_name };
        if rest.is_empty() {
            return Some(Rule { standard, summer: None });
        }
        let summer_name = name(&mut rest)?;
        let summer_offset = if rest.starts_with(',') { standard_offset + 3600 } else { -signed_time(&mut rest)? };
        // Without rules, POSIX falls back to the US rules.
        let (start, end) = match rest.strip_prefix(',') {
            Some(rules) => {
                let (start, end) = rules.split_once(',')?;
                (Change::parse(start)?, Change::parse(end)?)
            }
            None if rest.is_empty() => (Change::parse("M3.2.0")?, Change::parse("M11.1.0")?),
            None => return None,
        };
        let summer = Local { offset: summer_offset, abbreviation: summer_name };
        Some(Rule { standard, summer: Some((summer, start, end)) })
    }

    fn local(&self, seconds: i64) -> Local {
        let Some((summer, start, end)) = &self.summer else { return self.standard.clone() };
        let (year, _, _) = civil_from_days(seconds.div_euclid(86_400));
        // Both changes are given in the local time in force before them.
        let starts = start.in_year(year) - self.standard.offset;
        let ends = end.in_year(year) - summer.offset;
        let in_summer =
            if starts < ends { (starts..ends).contains(&seconds) } else { !(ends..starts).contains(&seconds) };
        if in_summer { summer.clone() } else { self.standard.clone() }
    }
}

impl Change {
    fn parse(text: &str) -> Option<Change> {
        let (date, time) = match text.split_once('/') {
            Some((date, time)) => {
                let mut time = time;
                (date, signed_time(&mut time).filter(|_| time.is_empty())?)
            }
            None => (text, 7200),
        };
        if let Some(rest) = date.strip_prefix('M') {
            let mut parts = rest.split('.').map(|part| part.parse::<u32>().ok());
            let (month, week, day) = (parts.next()??, parts.next()??, parts.next()??);
            let valid = (1..=12).contains(&month) && (1..=5).contains(&week) && day <= 6 && parts.next().is_none();
            return valid.then_some(Change::Month { month, week, day, time });
        }
        if let Some(day) = date.strip_prefix('J') {
            return day
                .parse()
                .ok()
                .filter(|number| (1..=365).contains(number))
                .map(|day| Change::Julian { day, time });
        }
        date.parse().ok().filter(|number| *number <= 365).map(|day| Change::Day { day, time })
    }

    /// The change in `year`, as seconds since the epoch in local time.
    fn in_year(self, year: i64) -> i64 {
        let (days, time) = match self {
            Change::Month { month, week, day, time } => {
                let first = days_from_civil(year, month, 1);
                // Sunday is 0 here, Monday 0 in `weekday`.
                let first_weekday = (weekday(first) as i64 + 1) % 7;
                let mut date = first + (i64::from(day) - first_weekday).rem_euclid(7) + 7 * (i64::from(week) - 1);
                while date >= first + i64::from(days_in_month(year, month)) {
                    date -= 7;
                }
                (date, time)
            }
            Change::Julian { day, time } => {
                let jan1 = days_from_civil(year, 1, 1);
                let leap_skip = i64::from(day > 59 && days_in_month(year, 2) == 29);
                (jan1 + i64::from(day) - 1 + leap_skip, time)
            }
            Change::Day { day, time } => (days_from_civil(year, 1, 1) + i64::from(day), time),
        };
        days * 86_400 + time
    }
}

/// A zone abbreviation: letters, or anything quoted in angle brackets.
fn name(rest: &mut &str) -> Option<String> {
    if let Some(quoted) = rest.strip_prefix('<') {
        let (name, after) = quoted.split_once('>')?;
        *rest = after;
        return Some(name.to_string());
    }
    let end = rest.find(|character: char| !character.is_ascii_alphabetic()).unwrap_or(rest.len());
    if end < 3 {
        return None;
    }
    let name = rest[..end].to_string();
    *rest = &rest[end..];
    Some(name)
}

/// `[+-]hh[:mm[:ss]]`, in seconds.
fn signed_time(rest: &mut &str) -> Option<i64> {
    let (sign, body) = match rest.as_bytes().first() {
        Some(b'-') => (-1, &rest[1..]),
        Some(b'+') => (1, &rest[1..]),
        _ => (1, *rest),
    };
    let end = body.find(|character: char| !character.is_ascii_digit() && character != ':').unwrap_or(body.len());
    if end == 0 {
        return None;
    }
    let mut seconds = 0;
    for (index, part) in body[..end].split(':').enumerate() {
        let value: i64 = part.parse().ok()?;
        seconds += value * [3600, 60, 1].get(index)?;
    }
    *rest = &body[end..];
    Some(sign * seconds)
}

/// Places people ask for by a name that is not in a zone's own name.
const ALIASES: &[(&str, &str)] = &[
    ("utc", "Etc/UTC"),
    ("gmt", "Etc/GMT"),
    ("uk", "Europe/London"),
    ("england", "Europe/London"),
    ("britain", "Europe/London"),
    ("netherlands", "Europe/Amsterdam"),
    ("holland", "Europe/Amsterdam"),
    ("nederland", "Europe/Amsterdam"),
    ("den haag", "Europe/Amsterdam"),
    ("the hague", "Europe/Amsterdam"),
    ("rotterdam", "Europe/Amsterdam"),
    ("utrecht", "Europe/Amsterdam"),
    ("belgium", "Europe/Brussels"),
    ("germany", "Europe/Berlin"),
    ("munich", "Europe/Berlin"),
    ("france", "Europe/Paris"),
    ("spain", "Europe/Madrid"),
    ("barcelona", "Europe/Madrid"),
    ("italy", "Europe/Rome"),
    ("milan", "Europe/Rome"),
    ("switzerland", "Europe/Zurich"),
    ("geneva", "Europe/Zurich"),
    ("sweden", "Europe/Stockholm"),
    ("norway", "Europe/Oslo"),
    ("denmark", "Europe/Copenhagen"),
    ("finland", "Europe/Helsinki"),
    ("poland", "Europe/Warsaw"),
    ("portugal", "Europe/Lisbon"),
    ("greece", "Europe/Athens"),
    ("turkey", "Europe/Istanbul"),
    ("ukraine", "Europe/Kyiv"),
    ("kiev", "Europe/Kyiv"),
    ("russia", "Europe/Moscow"),
    ("ireland", "Europe/Dublin"),
    ("japan", "Asia/Tokyo"),
    ("osaka", "Asia/Tokyo"),
    ("china", "Asia/Shanghai"),
    ("beijing", "Asia/Shanghai"),
    ("korea", "Asia/Seoul"),
    ("south korea", "Asia/Seoul"),
    ("india", "Asia/Kolkata"),
    ("mumbai", "Asia/Kolkata"),
    ("delhi", "Asia/Kolkata"),
    ("new delhi", "Asia/Kolkata"),
    ("bangalore", "Asia/Kolkata"),
    ("vietnam", "Asia/Ho_Chi_Minh"),
    ("thailand", "Asia/Bangkok"),
    ("indonesia", "Asia/Jakarta"),
    ("bali", "Asia/Makassar"),
    ("philippines", "Asia/Manila"),
    ("israel", "Asia/Jerusalem"),
    ("tel aviv", "Asia/Jerusalem"),
    ("uae", "Asia/Dubai"),
    ("abu dhabi", "Asia/Dubai"),
    ("iran", "Asia/Tehran"),
    ("pakistan", "Asia/Karachi"),
    ("australia", "Australia/Sydney"),
    ("canberra", "Australia/Sydney"),
    ("new zealand", "Pacific/Auckland"),
    ("nz", "Pacific/Auckland"),
    ("wellington", "Pacific/Auckland"),
    ("hawaii", "Pacific/Honolulu"),
    ("usa", "America/New_York"),
    ("us", "America/New_York"),
    ("nyc", "America/New_York"),
    ("washington", "America/New_York"),
    ("boston", "America/New_York"),
    ("miami", "America/New_York"),
    ("atlanta", "America/New_York"),
    ("california", "America/Los_Angeles"),
    ("la", "America/Los_Angeles"),
    ("san francisco", "America/Los_Angeles"),
    ("sf", "America/Los_Angeles"),
    ("seattle", "America/Los_Angeles"),
    ("silicon valley", "America/Los_Angeles"),
    ("texas", "America/Chicago"),
    ("dallas", "America/Chicago"),
    ("houston", "America/Chicago"),
    ("austin", "America/Chicago"),
    ("colorado", "America/Denver"),
    ("canada", "America/Toronto"),
    ("montreal", "America/Toronto"),
    ("mexico", "America/Mexico_City"),
    ("brazil", "America/Sao_Paulo"),
    ("rio", "America/Sao_Paulo"),
    ("argentina", "America/Argentina/Buenos_Aires"),
    ("buenos aires", "America/Argentina/Buenos_Aires"),
    ("chile", "America/Santiago"),
    ("peru", "America/Lima"),
    ("colombia", "America/Bogota"),
    ("egypt", "Africa/Cairo"),
    ("south africa", "Africa/Johannesburg"),
    ("cape town", "Africa/Johannesburg"),
    ("nigeria", "Africa/Lagos"),
    ("kenya", "Africa/Nairobi"),
    ("morocco", "Africa/Casablanca"),
];

/// The zone for a place: an alias, a zone name ("Asia/Tokyo", "cet"), or a
/// city in the zoneinfo tree ("tokyo", "new york", "sao paulo").
pub fn find(place: &str) -> Option<(String, Zone)> {
    let place = place.trim().trim_end_matches('?').trim().to_lowercase();
    if place.is_empty() {
        return None;
    }
    if let Some((_, zone)) = ALIASES.iter().find(|(alias, _)| *alias == place) {
        return Zone::load(zone).map(|loaded| (zone.to_string(), loaded));
    }
    let wanted = place.replace([' ', '-'], "_");
    let name = zone_names().iter().find(|name| {
        let lower = name.to_lowercase();
        lower == wanted || lower.rsplit('/').next() == Some(wanted.as_str())
    })?;
    Zone::load(name).map(|loaded| (name.clone(), loaded))
}

/// Every zone name under the zoneinfo directory, read once.
fn zone_names() -> &'static [String] {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = Vec::new();
        let root = PathBuf::from(ZONEINFO);
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else { continue };
                let Ok(relative) = path.strip_prefix(&root) else { continue };
                let relative = relative.to_string_lossy().into_owned();
                // Duplicates of the main tree and files that are not zones.
                if ["posix", "right", "SystemV"].iter().any(|skip| relative.starts_with(skip))
                    || relative.contains('.')
                    || relative == "localtime"
                    || relative == "posixrules"
                    || relative == "Factory"
                {
                    continue;
                }
                if kind.is_dir() {
                    stack.push(path);
                } else {
                    names.push(relative);
                }
            }
        }
        // Region zones before the old top-level aliases, and stable.
        names.sort_by_key(|name| (!name.contains('/'), name.clone()));
        names
    })
}

/// The local time zone's offset at `seconds`, through libc.
#[allow(unsafe_code, reason = "the local time zone is applied through libc's reentrant localtime_r")]
pub fn local_offset(seconds: i64) -> i64 {
    let now = seconds as libc::time_t;
    let mut calendar = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: localtime_r reads `now` and fills the caller-owned `calendar`, which lives past the call.
    let filled = unsafe { libc::localtime_r(&now, calendar.as_mut_ptr()) };
    if filled.is_null() {
        return 0;
    }
    // SAFETY: a non-null result means localtime_r initialised `calendar`.
    unsafe { calendar.assume_init() }.tm_gmtoff
}

/// Seconds since the epoch of a local wall-clock time.
pub fn from_local(civil_seconds: i64) -> i64 {
    // The offset at the guess, then again at the corrected guess, settles
    // on the right side of a change.
    let first = civil_seconds - local_offset(civil_seconds);
    civil_seconds - local_offset(first)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(weekday(0), 3, "1970-01-01 was a Thursday");
        assert_eq!(weekday(days_from_civil(2026, 10, 16)), 4, "a Friday");
        assert!(valid_date(2024, 2, 29));
        assert!(!valid_date(2026, 2, 29));
        assert!(!valid_date(2026, 13, 1));
        assert_eq!(Civil::from_seconds(1_700_000_000).iso(), "2023-11-14 22:13:20");
        assert_eq!(Civil::from_seconds(0).long_date(), "Thursday 1 January");
        assert_eq!(offset_label(9 * 3600), "UTC+9");
        assert_eq!(offset_label(-(3 * 3600 + 1800)), "UTC-3:30");
    }

    #[test]
    fn posix_rules() {
        let cet = Rule::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        // 2026: summer time from 29 March 01:00 UTC to 25 October 01:00 UTC.
        let march_29 = days_from_civil(2026, 3, 29) * 86_400;
        assert_eq!(cet.local(march_29 + 3600 - 1).abbreviation, "CET");
        assert_eq!(cet.local(march_29 + 3600).offset, 7200);
        let october_25 = days_from_civil(2026, 10, 25) * 86_400;
        assert_eq!(cet.local(october_25 + 3600 - 1).abbreviation, "CEST");
        assert_eq!(cet.local(october_25 + 3600).offset, 3600);
        // The southern hemisphere: summer over the new year.
        let sydney = Rule::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(sydney.local(days_from_civil(2026, 1, 15) * 86_400).abbreviation, "AEDT");
        assert_eq!(sydney.local(days_from_civil(2026, 7, 15) * 86_400).abbreviation, "AEST");
        let tokyo = Rule::parse("JST-9").unwrap();
        assert_eq!(tokyo.local(0), Local { offset: 9 * 3600, abbreviation: "JST".into() });
        let india = Rule::parse("IST-5:30").unwrap();
        assert_eq!(india.local(0).offset, 5 * 3600 + 1800);
        let quoted = Rule::parse("<+0330>-3:30").unwrap();
        assert_eq!(quoted.local(0).abbreviation, "+0330");
        let new_york = Rule::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(new_york.local(days_from_civil(2026, 7, 1) * 86_400).offset, -4 * 3600);
        assert!(Rule::parse("X").is_none());
        assert!(Rule::parse("CET-1CEST,M13.5.0,M10.5.0").is_none());
    }

    /// Against the system's own `date`, where zoneinfo is installed.
    #[test]
    fn zoneinfo_matches_date() {
        if !Path::new(ZONEINFO).join("Asia/Tokyo").exists() {
            return;
        }
        let (name, tokyo) = find("tokyo").unwrap();
        assert_eq!(name, "Asia/Tokyo");
        assert_eq!(tokyo.local(1_700_000_000).offset, 9 * 3600);
        assert_eq!(find("new york").unwrap().0, "America/New_York");
        assert_eq!(find("Japan").unwrap().0, "Asia/Tokyo");
        assert!(find("atlantis").is_none());
        assert!(Zone::load("../etc/passwd").is_none());
        for (zone, seconds) in [
            ("Europe/Amsterdam", 1_700_000_000),
            ("Europe/Amsterdam", 1_790_000_000),
            ("America/New_York", 1_720_000_000),
            ("Australia/Sydney", 1_705_000_000),
            ("America/Sao_Paulo", 2_200_000_000),
            ("Asia/Kolkata", 1_000_000_000),
        ] {
            let Ok(out) = std::process::Command::new("date")
                .env("TZ", zone)
                .args(["-d", &format!("@{seconds}"), "+%z %Z"])
                .output()
            else {
                return;
            };
            let expected = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let local = Zone::load(zone).unwrap().local(seconds);
            let sign = if local.offset < 0 { '-' } else { '+' };
            let got = format!(
                "{sign}{:02}{:02} {}",
                local.offset.abs() / 3600,
                local.offset.abs() / 60 % 60,
                local.abbreviation
            );
            assert_eq!(got, expected, "{zone} at {seconds}");
        }
    }
}
