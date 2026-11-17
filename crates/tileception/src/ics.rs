//! Calendar events from iCalendar (.ics, RFC 5545) data, for the bar.
//!
//! Only what showing upcoming events needs: `VEVENT`s with `DTSTART`,
//! `DTEND` or `DURATION`, `SUMMARY`, `LOCATION`, `STATUS`, `EXDATE` and
//! `RECURRENCE-ID`. Times are UTC, floating (local) or in a `TZID` zone,
//! read from the system's zoneinfo. `RRULE` repeats daily, weekly (on the
//! `BYDAY` weekdays, from its `WKST` week start), monthly or yearly with
//! `INTERVAL`, `COUNT` and `UNTIL`, and a `BYMONTHDAY` or `BYMONTH` that
//! only repeats the start's own day and month; a rule with other parts
//! shows only its first occurrence.
//! Everything else is ignored.

use std::collections::{BTreeMap, BTreeSet};

use crate::bar::{civil_from_days, days_from_civil};

const DAY: i64 = 86_400;
/// A rule repeating more often than this within the window is cut off.
const MAX_STEPS: usize = 100_000;

/// One occurrence of an event, in UTC seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub summary: String,
    pub location: String,
}

/// A time zone, for turning wall-clock times into UTC.
pub trait Zone {
    /// The UTC time for seconds since 1970 on this zone's wall clock.
    fn to_utc(&self, wall: i64) -> i64;
}

/// UTC itself.
pub struct Utc;

impl Zone for Utc {
    fn to_utc(&self, wall: i64) -> i64 {
        wall
    }
}

/// Where the events' times come from: UTC, floating times and all-day
/// dates use `local`; `TZID`s are looked up by `named`.
pub struct Zones<'zone> {
    pub local: &'zone dyn Zone,
    pub named: &'zone dyn Fn(&str) -> Option<Box<dyn Zone>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Until {
    Utc(i64),
    /// A wall time in the event's zone, or a date at its midnight.
    Wall(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<usize>,
    until: Option<Until>,
    /// 0 for Monday up to 6 for Sunday, for weekly rules.
    weekdays: Vec<i64>,
    /// The day weeks start on, 0 for Monday up to 6 for Sunday.
    week_start: i64,
    /// A single `BYMONTHDAY`, followed only when it is the start's own day.
    month_day: Option<i32>,
    /// A single `BYMONTH`, followed only when it is the start's own month.
    month: Option<i32>,
}

impl Rule {
    /// Whether the rule only repeats the start's own day of the month and
    /// month, which is all `walls` follows of `BYMONTHDAY` and `BYMONTH`.
    fn follows(&self, first: i64) -> bool {
        let (_, month, day) = civil_from_days(first.div_euclid(DAY));
        self.month_day.is_none_or(|wanted| wanted == day) && self.month.is_none_or(|wanted| wanted == month)
    }
}

/// A date or date-time as written, before its zone is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    /// Seconds since 1970 on the wall clock.
    wall: i64,
    date_only: bool,
    utc: bool,
    tzid: Option<String>,
}

#[derive(Debug, Default)]
struct Vevent {
    uid: String,
    summary: String,
    location: String,
    start: Option<Stamp>,
    end: Option<Stamp>,
    duration: Option<i64>,
    /// None when there is none; Err for one we cannot follow.
    rule: Option<Result<Rule, ()>>,
    exdates: Vec<Stamp>,
    recurrence_id: Option<Stamp>,
    cancelled: bool,
}

/// Every occurrence that overlaps `from..until`, sorted by start. The data
/// is UTF-8, read line by line once folded lines are joined.
pub fn occurrences(data: &[u8], zones: &Zones, from: i64, until: i64) -> Vec<Occurrence> {
    let events = parse(data);
    let resolve = |stamp: &Stamp| -> i64 {
        if stamp.utc {
            return stamp.wall;
        }
        if !stamp.date_only
            && let Some(zone) = stamp.tzid.as_deref().and_then(|tzid| (zones.named)(tzid))
        {
            return zone.to_utc(stamp.wall);
        }
        zones.local.to_utc(stamp.wall)
    };
    // Moved or changed occurrences replace the ones their rule makes.
    let mut replaced: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
    for event in &events {
        if let Some(moved) = &event.recurrence_id {
            replaced.entry(event.uid.as_str()).or_default().insert(resolve(moved));
        }
    }
    let mut found = Vec::new();
    for event in &events {
        let Some(start) = &event.start else { continue };
        if event.cancelled {
            continue;
        }
        let all_day = start.date_only;
        // An all-day event ending on the day it starts, which exporters
        // write, still takes its day.
        let shortest = if all_day { DAY } else { 0 };
        let length = match (&event.end, event.duration) {
            (Some(end), _) => (end.wall - start.wall).max(shortest),
            (None, Some(duration)) => duration.max(shortest),
            (None, None) if all_day => DAY,
            (None, None) => 0,
        };
        let zone: Box<dyn Zone> = match (&start.tzid, start.utc, all_day) {
            (_, true, _) => Box::new(Utc),
            (Some(tzid), false, false) => (zones.named)(tzid).unwrap_or_else(|| Box::new(Local(zones.local))),
            _ => Box::new(Local(zones.local)),
        };
        // An end in another zone than the start: its own UTC time.
        let end_of = |begin_wall: i64, begin: i64| -> i64 {
            match &event.end {
                Some(end) if end.tzid != start.tzid || end.utc != start.utc => {
                    begin + (resolve(end) - resolve(start)).max(0)
                }
                _ => zone.to_utc(begin_wall + length).max(begin),
            }
        };
        let excluded: BTreeSet<i64> = event
            .exdates
            .iter()
            .map(|stamp| {
                if stamp.date_only == all_day && stamp.tzid == start.tzid && !stamp.utc {
                    zone.to_utc(stamp.wall)
                } else {
                    resolve(stamp)
                }
            })
            .chain(
                (event.recurrence_id.is_none())
                    .then(|| replaced.get(event.uid.as_str()))
                    .flatten()
                    .into_iter()
                    .flatten()
                    .copied(),
            )
            .collect();
        let mut add = |wall: i64| {
            let begin = zone.to_utc(wall);
            let end = end_of(wall, begin);
            if excluded.contains(&begin) || end.max(begin + 1) <= from || begin >= until {
                return;
            }
            found.push(Occurrence {
                start: begin,
                end,
                all_day,
                summary: event.summary.clone(),
                location: event.location.clone(),
            });
        };
        match &event.rule {
            Some(Ok(rule)) if event.recurrence_id.is_none() && rule.follows(start.wall) => {
                let within_rule = |wall: i64| match rule.until {
                    None => true,
                    Some(Until::Utc(limit)) => zone.to_utc(wall) <= limit,
                    Some(Until::Wall(limit)) => wall <= limit,
                };
                // Stop once a start lies past the window and no earlier one can.
                let last_start = until + DAY;
                for wall in walls(rule, start.wall).take(MAX_STEPS) {
                    if !within_rule(wall) || zone.to_utc(wall) >= last_start {
                        break;
                    }
                    add(wall);
                }
            }
            _ => add(start.wall),
        }
    }
    found.sort_by(|left, right| (left.start, left.end, &left.summary).cmp(&(right.start, right.end, &right.summary)));
    found
}

/// The local zone where `Box<dyn Zone>` is wanted.
struct Local<'zone>(&'zone dyn Zone);

impl<'zone> Zone for Local<'zone> {
    fn to_utc(&self, wall: i64) -> i64 {
        self.0.to_utc(wall)
    }
}

/// The wall times a rule makes from `first`, in order, honouring `COUNT`
/// but not `UNTIL`.
fn walls<'rule>(rule: &'rule Rule, first: i64) -> impl Iterator<Item = i64> + 'rule {
    let day = first.div_euclid(DAY);
    let time = first.rem_euclid(DAY);
    let (year, month, dom) = civil_from_days(day);
    let interval = rule.interval.max(1);
    let mut weekdays = rule.weekdays.clone();
    if weekdays.is_empty() {
        weekdays.push(crate::bar::monday_index(day));
    }
    // Days into the week, from its start, so a week holds its days in order.
    let mut offsets: Vec<i64> = weekdays.iter().map(|weekday| (weekday - rule.week_start).rem_euclid(7)).collect();
    offsets.sort_unstable();
    offsets.dedup();
    // With an interval, which weeks count depends on where they start.
    let week_start = day - (crate::bar::monday_index(day) - rule.week_start).rem_euclid(7);
    (0..)
        .flat_map(move |step: i64| -> Vec<i64> {
            match rule.freq {
                Freq::Daily => vec![day + step * interval],
                Freq::Weekly => {
                    let start = week_start + step * interval * 7;
                    offsets.iter().map(|offset| start + offset).filter(|candidate| *candidate >= day).collect()
                }
                Freq::Monthly => {
                    let (shifted_year, shifted_month) = crate::bar::add_months(year, month, (step * interval) as i32);
                    valid_date(shifted_year, shifted_month, dom).into_iter().collect()
                }
                Freq::Yearly => valid_date(year + (step * interval) as i32, month, dom).into_iter().collect(),
            }
        })
        .map(move |candidate| candidate * DAY + time)
        .take(rule.count.unwrap_or(usize::MAX))
}

/// The day number of a date that exists: no 31 April or 29 February 2027.
fn valid_date(year: i32, month: i32, day: i32) -> Option<i64> {
    let days = days_from_civil(year, month, day);
    (civil_from_days(days) == (year, month, day)).then_some(days)
}

/// The VEVENTs in the text, skipping what we cannot read.
fn parse(data: &[u8]) -> Vec<Vevent> {
    let mut events = Vec::new();
    // Components we are inside, innermost last.
    let mut stack: Vec<String> = Vec::new();
    let mut event = Vevent::default();
    for line in unfold(data) {
        let Some((name, params, value)) = content_line(&line) else { continue };
        match name.as_str() {
            "BEGIN" => {
                let component = value.to_ascii_uppercase();
                if component == "VEVENT" {
                    event = Vevent::default();
                }
                stack.push(component);
                continue;
            }
            "END" => {
                if stack.pop().as_deref() == Some("VEVENT") {
                    events.push(std::mem::take(&mut event));
                }
                continue;
            }
            _ => {}
        }
        // Properties of alarms and time zones are not the event's.
        if stack.last().map(String::as_str) != Some("VEVENT") {
            continue;
        }
        let tzid = params.get("TZID").cloned();
        let date_only = params.get("VALUE").is_some_and(|kind| kind.eq_ignore_ascii_case("DATE"));
        match name.as_str() {
            "UID" => event.uid = value,
            "SUMMARY" => event.summary = unescape(&value),
            "LOCATION" => event.location = unescape(&value),
            "STATUS" => event.cancelled = value.eq_ignore_ascii_case("CANCELLED"),
            "DTSTART" => event.start = stamp(&value, date_only, tzid),
            "DTEND" => event.end = stamp(&value, date_only, tzid),
            "DURATION" => event.duration = duration(&value),
            "RRULE" => event.rule = Some(rule(&value).ok_or(())),
            "RECURRENCE-ID" => event.recurrence_id = stamp(&value, date_only, tzid),
            "EXDATE" => event.exdates.extend(value.split(',').filter_map(|part| stamp(part, date_only, tzid.clone()))),
            _ => {}
        }
    }
    events
}

/// Joins folded lines: a line starting with a space or tab continues the
/// one before. A fold may split a UTF-8 character, so lines are joined as
/// bytes and only then read as text.
fn unfold(data: &[u8]) -> Vec<String> {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    for raw in data.split(|byte| *byte == b'\n') {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        match (raw.first(), lines.last_mut()) {
            (Some(b' ' | b'\t'), Some(last)) => last.extend_from_slice(&raw[1..]),
            _ if raw.is_empty() => {}
            _ => lines.push(raw.to_vec()),
        }
    }
    lines.into_iter().map(|line| String::from_utf8_lossy(&line).into_owned()).collect()
}

/// `NAME;PARAM=value;PARAM="quoted:value":the value`, with the name and
/// parameter names in upper case.
fn content_line(line: &str) -> Option<(String, BTreeMap<String, String>, String)> {
    let mut quoted = false;
    let mut cut = None;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                cut = Some(index);
                break;
            }
            _ => {}
        }
    }
    let cut = cut?;
    let (head, value) = (&line[..cut], &line[cut + 1..]);
    let mut parts = Vec::new();
    let mut part = String::new();
    quoted = false;
    for character in head.chars() {
        match character {
            '"' => quoted = !quoted,
            ';' if !quoted => parts.push(std::mem::take(&mut part)),
            _ => part.push(character),
        }
    }
    parts.push(part);
    let mut parts = parts.into_iter();
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|param| {
            let (key, value) = param.split_once('=')?;
            Some((key.trim().to_ascii_uppercase(), value.to_string()))
        })
        .collect();
    Some((name, params, value.to_string()))
}

/// TEXT values: `\n` is a line break, `\,` `\;` `\\` the character.
fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// `20261109`, `20261109T143000` or `20261109T143000Z`.
fn stamp(value: &str, date_only: bool, tzid: Option<String>) -> Option<Stamp> {
    let value = value.trim();
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = value.get(range)?;
        digits.bytes().all(|byte| byte.is_ascii_digit()).then(|| digits.parse().ok()).flatten()
    };
    let (year, month, day) = (number(0..4)?, number(4..6)?, number(6..8)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = valid_date(year as i32, month as i32, day as i32)?;
    if date_only || value.len() == 8 {
        return Some(Stamp { wall: days * DAY, date_only: true, utc: false, tzid: None });
    }
    if value.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    let (hour, minute, second) = (number(9..11)?, number(11..13)?, number(13..15)?);
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let utc = match &value[15..] {
        "" => false,
        "Z" | "z" => true,
        _ => return None,
    };
    let wall = days * DAY + hour * 3600 + minute * 60 + second.min(59);
    Some(Stamp { wall, date_only: false, utc, tzid: if utc { None } else { tzid } })
}

/// `PT1H30M`, `P1D`, `P2W`, `-PT15M`: seconds.
fn duration(value: &str) -> Option<i64> {
    let value = value.trim();
    let (sign, rest) = match value.as_bytes().first()? {
        b'-' => (-1, &value[1..]),
        b'+' => (1, &value[1..]),
        _ => (1, value),
    };
    let rest = rest.strip_prefix('P')?;
    let mut total = 0;
    let mut number = String::new();
    let mut in_time = false;
    for character in rest.chars() {
        match character {
            '0'..='9' => number.push(character),
            'T' if number.is_empty() => in_time = true,
            unit => {
                let amount: i64 = number.parse().ok()?;
                number.clear();
                total += amount
                    * match (unit, in_time) {
                        ('W', false) => 7 * DAY,
                        ('D', false) => DAY,
                        ('H', true) => 3600,
                        ('M', true) => 60,
                        ('S', true) => 1,
                        _ => return None,
                    };
            }
        }
    }
    number.is_empty().then_some(sign * total)
}

/// An RRULE we can follow, or None.
fn rule(value: &str) -> Option<Rule> {
    let mut rule = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        weekdays: Vec::new(),
        week_start: 0,
        month_day: None,
        month: None,
    };
    let mut freq = None;
    for part in value.split(';').filter(|part| !part.is_empty()) {
        let (key, value) = part.split_once('=')?;
        match key.to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match value.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None,
                });
            }
            "INTERVAL" => rule.interval = value.parse().ok().filter(|interval| *interval >= 1)?,
            "COUNT" => rule.count = Some(value.parse().ok()?),
            "UNTIL" => {
                let until = stamp(value, false, None)?;
                rule.until = Some(if until.utc {
                    Until::Utc(until.wall)
                } else {
                    Until::Wall(until.wall + if until.date_only { DAY - 1 } else { 0 })
                });
            }
            "BYDAY" => {
                for day in value.split(',') {
                    rule.weekdays.push(weekday(day)?);
                }
            }
            "WKST" => rule.week_start = weekday(value)?,
            // One positive day or month; `Rule::follows` checks it is the start's.
            "BYMONTHDAY" => rule.month_day = Some(value.parse().ok().filter(|day| (1..=31).contains(day))?),
            "BYMONTH" => rule.month = Some(value.parse().ok().filter(|month| (1..=12).contains(month))?),
            _ => return None,
        }
    }
    rule.freq = freq?;
    if !rule.weekdays.is_empty() && rule.freq != Freq::Weekly {
        return None;
    }
    // BYMONTHDAY repeats within the period of a monthly rule, or of a
    // yearly one in the month BYMONTH names; elsewhere it means more.
    let month_day_fits = match rule.freq {
        Freq::Monthly => rule.month.is_none(),
        Freq::Yearly => rule.month.is_some(),
        Freq::Daily | Freq::Weekly => false,
    };
    if rule.month_day.is_some() && !month_day_fits || rule.month.is_some() && rule.freq != Freq::Yearly {
        return None;
    }
    Some(rule)
}

/// `MO` to `SU`: 0 for Monday up to 6 for Sunday.
fn weekday(name: &str) -> Option<i64> {
    let index = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"].iter().position(|day| name.eq_ignore_ascii_case(day))?;
    Some(index as i64)
}

/// A zone from a zoneinfo (TZif) file: its transitions, then the POSIX
/// rule in its footer for times after the last one.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeZone {
    /// UTC times and the offset that starts at each.
    transitions: Vec<(i64, i64)>,
    /// The offset before the first transition.
    initial: i64,
    rule: Option<Posix>,
}

impl TimeZone {
    /// The zone a TZID names in /usr/share/zoneinfo. Prefixes such as
    /// `/mozilla.org/20050126_1/` are tried away.
    pub fn named(tzid: &str) -> Option<TimeZone> {
        let tzid = tzid.trim().trim_matches('"');
        let parts: Vec<&str> = tzid.split('/').filter(|part| !part.is_empty()).collect();
        if parts.iter().any(|part| *part == ".." || *part == ".") {
            return None;
        }
        (0..parts.len()).find_map(|skip| {
            let path = std::path::Path::new("/usr/share/zoneinfo").join(parts[skip..].join("/"));
            TimeZone::from_tzif(&std::fs::read(path).ok()?)
        })
    }

    /// A zone with only a POSIX rule, such as `CET-1CEST,M3.5.0,M10.5.0/3`.
    #[cfg(test)]
    pub fn from_posix(text: &str) -> Option<TimeZone> {
        let rule = Posix::parse(text)?;
        Some(TimeZone { transitions: Vec::new(), initial: rule.std_offset, rule: Some(rule) })
    }

    pub fn from_tzif(data: &[u8]) -> Option<TimeZone> {
        let header = |offset: usize| -> Option<(u8, [usize; 6])> {
            if data.get(offset..offset + 4)? != b"TZif" {
                return None;
            }
            let version = *data.get(offset + 4)?;
            let mut counts = [0; 6];
            for (index, count) in counts.iter_mut().enumerate() {
                let start = offset + 20 + index * 4;
                *count = u32::from_be_bytes(data.get(start..start + 4)?.try_into().ok()?) as usize;
            }
            Some((version, counts))
        };
        let size = |counts: [usize; 6], time: usize| {
            let [isut, isstd, leap, times, types, chars] = counts;
            times * time + times + types * 6 + chars + leap * (time + 4) + isstd + isut
        };
        let (version, counts) = header(0)?;
        let (block_start, counts, time) = if version >= b'2' {
            let second = 44 + size(counts, 4);
            let (_, counts) = header(second)?;
            (second + 44, counts, 8)
        } else {
            (44, counts, 4)
        };
        let [_, _, _, times, types, _] = counts;
        let block = data.get(block_start..block_start + size(counts, time))?;
        let read_time = |index: usize| -> Option<i64> {
            let bytes = block.get(index * time..(index + 1) * time)?;
            Some(if time == 8 {
                i64::from_be_bytes(bytes.try_into().ok()?)
            } else {
                i64::from(i32::from_be_bytes(bytes.try_into().ok()?))
            })
        };
        let kinds = block.get(times * time..times * time + times)?;
        let infos = times * time + times;
        let offset = |kind: usize| -> Option<i64> {
            let start = infos + kind * 6;
            Some(i64::from(i32::from_be_bytes(block.get(start..start + 4)?.try_into().ok()?)))
        };
        if types == 0 {
            return None;
        }
        let mut transitions = Vec::with_capacity(times);
        for index in 0..times {
            transitions.push((read_time(index)?, offset(usize::from(*kinds.get(index)?))?));
        }
        let rule = if version >= b'2' {
            let footer = data.get(block_start + block.len()..)?;
            std::str::from_utf8(footer).ok().and_then(|text| Posix::parse(text.trim()))
        } else {
            None
        };
        Some(TimeZone { transitions, initial: offset(0)?, rule })
    }

    /// Seconds east of UTC at a UTC time.
    pub fn offset_at(&self, utc: i64) -> i64 {
        let after = self.transitions.partition_point(|(since, _)| *since <= utc);
        match (after, &self.rule) {
            (after, Some(rule)) if after == self.transitions.len() => rule.offset_at(utc),
            (0, _) => self.initial,
            (after, _) => self.transitions[after - 1].1,
        }
    }
}

impl Zone for TimeZone {
    /// A wall time skipped when clocks go forward is taken as the hour after;
    /// one that happens twice is the first.
    fn to_utc(&self, wall: i64) -> i64 {
        let guess = wall - self.offset_at(wall);
        let first = wall - self.offset_at(guess - 6 * 3600).max(self.offset_at(guess + 6 * 3600));
        let offset = self.offset_at(first);
        if first + offset == wall {
            return first;
        }
        let other = wall - offset;
        if other + self.offset_at(other) == wall { other } else { wall - self.offset_at(guess - 6 * 3600) }
    }
}

/// A POSIX TZ rule: a standard offset and, perhaps, a summer time with
/// the dates it starts and ends (`Mm.w.d/time` only).
#[derive(Debug, Clone, PartialEq)]
struct Posix {
    std_offset: i64,
    dst: Option<(i64, Change, Change)>,
}

/// The `m`th month's `w`th `d`ay (week 5 is the last), at a wall time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Change {
    month: i32,
    week: i64,
    weekday: i64,
    time: i64,
}

impl Posix {
    fn parse(text: &str) -> Option<Posix> {
        let mut rest = text;
        rest = skip_name(rest)?;
        let (std_west, after) = posix_time(rest)?;
        rest = after;
        if rest.is_empty() {
            return Some(Posix { std_offset: -std_west, dst: None });
        }
        rest = skip_name(rest)?;
        let mut dst_offset = -std_west + 3600;
        if !rest.starts_with(',')
            && let Some((dst_west, after)) = posix_time(rest)
        {
            dst_offset = -dst_west;
            rest = after;
        }
        let mut changes = rest.strip_prefix(',')?.split(',');
        let start = Change::parse(changes.next()?)?;
        let end = Change::parse(changes.next()?)?;
        Some(Posix { std_offset: -std_west, dst: Some((dst_offset, start, end)) })
    }

    fn offset_at(&self, utc: i64) -> i64 {
        let Some((dst_offset, start, end)) = self.dst else { return self.std_offset };
        let (year, _, _) = civil_from_days((utc + self.std_offset).div_euclid(DAY));
        let begins = start.wall(year) - self.std_offset;
        let ends = end.wall(year) - dst_offset;
        let summer = if begins < ends { begins <= utc && utc < ends } else { !(ends <= utc && utc < begins) };
        if summer { dst_offset } else { self.std_offset }
    }
}

impl Change {
    fn parse(text: &str) -> Option<Change> {
        let (date, time) = match text.split_once('/') {
            Some((date, time)) => (date, posix_time(time).filter(|(_, rest)| rest.is_empty())?.0),
            None => (text, 7200),
        };
        let mut fields = date.strip_prefix('M')?.split('.');
        let month = fields.next()?.parse().ok().filter(|month| (1..=12).contains(month))?;
        let week = fields.next()?.parse().ok().filter(|week| (1..=5).contains(week))?;
        let weekday = fields.next()?.parse().ok().filter(|weekday| (0..=6).contains(weekday))?;
        Some(Change { month, week, weekday, time })
    }

    /// Wall seconds of this change in a year.
    fn wall(&self, year: i32) -> i64 {
        let first = days_from_civil(year, self.month, 1);
        // POSIX counts weekdays from Sunday; monday_index from Monday.
        let first_weekday = (crate::bar::monday_index(first) + 1) % 7;
        let mut day = first + (self.weekday - first_weekday).rem_euclid(7) + (self.week - 1) * 7;
        let (next_year, next_month) = crate::bar::add_months(year, self.month, 1);
        while day >= days_from_civil(next_year, next_month, 1) {
            day -= 7;
        }
        day * DAY + self.time
    }
}

/// Past a zone abbreviation: letters, or `<+03>` in angle brackets.
fn skip_name(text: &str) -> Option<&str> {
    if let Some(rest) = text.strip_prefix('<') {
        return Some(&rest[rest.find('>')? + 1..]);
    }
    let end = text.find(|character: char| !character.is_ascii_alphabetic()).unwrap_or(text.len());
    (end >= 3).then(|| &text[end..])
}

/// `[+-]hh[:mm[:ss]]`: seconds, and the rest of the text.
fn posix_time(text: &str) -> Option<(i64, &str)> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'-' => (-1, &text[1..]),
        b'+' => (1, &text[1..]),
        _ => (1, text),
    };
    let end = rest.find(|character: char| !character.is_ascii_digit() && character != ':').unwrap_or(rest.len());
    let mut seconds = 0;
    for (index, field) in rest[..end].split(':').enumerate() {
        if index > 2 || field.is_empty() {
            return None;
        }
        seconds += field.parse::<i64>().ok()? * [3600, 60, 1][index];
    }
    Some((sign * seconds, &rest[end..]))
}

/// The event on now, for the bar. Of several, the one that started last,
/// such as a meeting within a longer block, then the one ending first.
/// All-day events, and timed ones lasting a day or more, are left to the
/// calendar: they would hold the bar all day.
pub fn current(occurrences: &[Occurrence], now: i64) -> Option<&Occurrence> {
    occurrences
        .iter()
        .filter(|event| !event.all_day && event.end - event.start < DAY && event.start <= now && now < event.end)
        .max_by_key(|event| (event.start, std::cmp::Reverse(event.end)))
}

/// The event to show in the bar when none is on: the next one today that
/// has not started. All-day events are left to the calendar.
pub fn next_today(occurrences: &[Occurrence], now: i64, day_end: i64) -> Option<&Occurrence> {
    occurrences
        .iter()
        .filter(|event| !event.all_day && event.start > now && event.start < day_end)
        .min_by_key(|event| event.start)
}

/// What the calendar lists: what has not ended, from today up to `until`,
/// at most `limit`, all-day events of a day before its timed ones.
pub fn upcoming(occurrences: &[Occurrence], now: i64, until: i64, limit: usize) -> Vec<&Occurrence> {
    let mut list: Vec<&Occurrence> =
        occurrences.iter().filter(|event| event.end.max(event.start + 1) > now && event.start < until).collect();
    list.sort_by_key(|event| (event.start, !event.all_day, event.end));
    list.truncate(limit);
    list
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions")]
mod tests {
    use super::*;

    /// 2026-11-09 00:00 UTC, a Monday.
    const MONDAY: i64 = 20_766 * DAY;

    fn utc_at(year: i32, month: i32, day: i32, hour: i64, minute: i64) -> i64 {
        days_from_civil(year, month, day) * DAY + hour * 3600 + minute * 60
    }

    fn amsterdam() -> TimeZone {
        TimeZone::from_posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
    }

    fn read(text: impl AsRef<[u8]>, from: i64, until: i64) -> Vec<Occurrence> {
        let named = |tzid: &str| -> Option<Box<dyn Zone>> {
            (tzid == "Europe/Amsterdam").then(|| Box::new(amsterdam()) as Box<dyn Zone>)
        };
        occurrences(text.as_ref(), &Zones { local: &Utc, named: &named }, from, until)
    }

    fn calendar(events: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{events}END:VCALENDAR\r\n")
    }

    #[test]
    fn the_test_dates_add_up() {
        assert_eq!(MONDAY, utc_at(2026, 11, 9, 0, 0));
        assert_eq!(crate::bar::monday_index(MONDAY / DAY), 0);
    }

    #[test]
    fn reads_utc_floating_zoned_and_all_day_events() {
        let text = calendar(
            "BEGIN:VEVENT\r\nUID:1\r\nDTSTART:20261109T090000Z\r\nDTEND:20261109T093000Z\r\nSUMMARY:Stand\r\n  up\\, daily\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:2\r\nDTSTART;TZID=Europe/Amsterdam:20261109T140000\r\nDURATION:PT1H\r\nSUMMARY:Review\r\nLOCATION:Room 4\r\n\
             BEGIN:VALARM\r\nACTION:DISPLAY\r\nSUMMARY:not this\r\nEND:VALARM\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:3\r\nDTSTART;VALUE=DATE:20261109\r\nSUMMARY:Holiday\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:4\r\nDTSTART:20261110T080000\r\nSUMMARY:Floating\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:5\r\nDTSTART:20261109T100000Z\r\nSTATUS:CANCELLED\r\nSUMMARY:Gone\r\nEND:VEVENT\r\n",
        );
        let found = read(&text, MONDAY, MONDAY + 7 * DAY);
        let summaries: Vec<_> = found.iter().map(|event| event.summary.as_str()).collect();
        assert_eq!(summaries, ["Holiday", "Stand up, daily", "Review", "Floating"]);
        assert_eq!((found[0].start, found[0].end, found[0].all_day), (MONDAY, MONDAY + DAY, true));
        assert_eq!((found[1].start, found[1].end), (utc_at(2026, 11, 9, 9, 0), utc_at(2026, 11, 9, 9, 30)));
        // 14:00 in Amsterdam is 13:00 UTC in winter.
        assert_eq!((found[2].start, found[2].end), (utc_at(2026, 11, 9, 13, 0), utc_at(2026, 11, 9, 14, 0)));
        assert_eq!(found[2].location, "Room 4");
        assert_eq!(found[3].start, utc_at(2026, 11, 10, 8, 0));
    }

    #[test]
    fn zones_follow_summer_time() {
        let zone = amsterdam();
        assert_eq!(zone.offset_at(utc_at(2026, 7, 1, 12, 0)), 7200);
        assert_eq!(zone.offset_at(utc_at(2026, 12, 1, 12, 0)), 3600);
        // Summer time starts at 01:00 UTC on the last Sunday of March.
        assert_eq!(zone.offset_at(utc_at(2026, 3, 29, 0, 59)), 3600);
        assert_eq!(zone.offset_at(utc_at(2026, 3, 29, 1, 0)), 7200);
        assert_eq!(zone.to_utc(utc_at(2026, 7, 1, 14, 0)), utc_at(2026, 7, 1, 12, 0));
        // 02:30 does not exist that night; 02:30 on 25 October happens twice.
        assert_eq!(zone.to_utc(utc_at(2026, 3, 29, 2, 30)), utc_at(2026, 3, 29, 1, 30));
        assert_eq!(zone.to_utc(utc_at(2026, 10, 25, 2, 30)), utc_at(2026, 10, 25, 0, 30));
        let south = TimeZone::from_posix("<-03>3<-02>,M10.1.0/0,M3.3.0/0").unwrap();
        assert_eq!(south.offset_at(utc_at(2026, 1, 15, 12, 0)), -7200);
        assert_eq!(south.offset_at(utc_at(2026, 6, 15, 12, 0)), -10800);
        assert_eq!(TimeZone::from_posix("<+0530>-5:30").unwrap().offset_at(0), 19_800);
        assert_eq!(TimeZone::from_posix("nonsense"), None);
    }

    #[test]
    fn reads_system_zoneinfo_when_there_is_one() {
        let Some(zone) = TimeZone::named("/mozilla.org/20050126_1/Europe/Amsterdam") else { return };
        assert_eq!(zone.offset_at(utc_at(2026, 7, 1, 12, 0)), 7200);
        assert_eq!(zone.offset_at(utc_at(2040, 1, 1, 12, 0)), 3600);
        assert_eq!(zone.offset_at(utc_at(2040, 7, 1, 12, 0)), 7200);
        assert!(TimeZone::named("../../etc/passwd").is_none());
    }

    #[test]
    fn repeats_daily_weekly_monthly_and_yearly() {
        let text = calendar(
            "BEGIN:VEVENT\r\nUID:d\r\nDTSTART:20261101T090000Z\r\nDTEND:20261101T091500Z\r\nRRULE:FREQ=DAILY;COUNT=10\r\nSUMMARY:Daily\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:w\r\nDTSTART;TZID=Europe/Amsterdam:20261005T100000\r\nDTEND;TZID=Europe/Amsterdam:20261005T110000\r\n\
             RRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261111T235959Z\r\nEXDATE;TZID=Europe/Amsterdam:20261109T100000\r\nSUMMARY:Weekly\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:m\r\nDTSTART;VALUE=DATE:20260131\r\nRRULE:FREQ=MONTHLY\r\nSUMMARY:Monthly\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:y\r\nDTSTART;VALUE=DATE:20001112\r\nRRULE:FREQ=YEARLY\r\nSUMMARY:Birthday\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261101T090000Z\r\nRRULE:FREQ=MONTHLY;BYSETPOS=-1;BYDAY=FR\r\nSUMMARY:Odd\r\nEND:VEVENT\r\n",
        );
        let found = read(&text, MONDAY, MONDAY + 14 * DAY);
        let starts = |summary: &str| -> Vec<i64> {
            found.iter().filter(|event| event.summary == summary).map(|event| event.start).collect()
        };
        // 1 to 10 November: the 9th and 10th are left in the window.
        assert_eq!(starts("Daily"), [utc_at(2026, 11, 9, 9, 0), utc_at(2026, 11, 10, 9, 0)]);
        // Monday the 9th is excluded; Wednesday the 11th is the last. The
        // October start was in summer time, the time stays 10:00 local.
        assert_eq!(starts("Weekly"), [utc_at(2026, 11, 11, 9, 0)]);
        // There is no 30 November.
        assert_eq!(starts("Monthly"), Vec::<i64>::new());
        assert_eq!(starts("Birthday"), [utc_at(2026, 11, 12, 0, 0)]);
        // A rule we cannot follow shows its first occurrence only.
        assert_eq!(starts("Odd"), Vec::<i64>::new());
        let first = read(&text, utc_at(2026, 11, 1, 0, 0), MONDAY);
        assert!(first.iter().any(|event| event.summary == "Odd"));
        let january = read(&text, utc_at(2027, 1, 1, 0, 0), utc_at(2027, 2, 1, 0, 0));
        assert!(january.iter().any(|event| event.summary == "Monthly" && event.start == utc_at(2027, 1, 31, 0, 0)));
    }

    #[test]
    fn a_fold_may_split_a_character() {
        // "Café met Zoë", folded between the two bytes of the é.
        let text = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:f\r\nDTSTART:20261109T100000Z\r\n\
                     SUMMARY:Caf\xc3\r\n \xa9 met Zo\xc3\xab\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let found = read(text, MONDAY, MONDAY + DAY);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].summary, "Café met Zoë");
        assert_eq!(unfold(b"A:\xe2\x82\r\n\t\xac"), ["A:€"]);
    }

    #[test]
    fn an_all_day_event_takes_at_least_its_day() {
        let text = calendar(
            "BEGIN:VEVENT\r\nUID:z\r\nDTSTART;VALUE=DATE:20261110\r\nDTEND;VALUE=DATE:20261110\r\nSUMMARY:Bin day\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:d\r\nDTSTART;VALUE=DATE:20261111\r\nDURATION:PT0S\r\nSUMMARY:No length\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:t\r\nDTSTART:20261110T100000Z\r\nDTEND:20261110T100000Z\r\nSUMMARY:Instant\r\nEND:VEVENT\r\n",
        );
        let found = read(&text, MONDAY + DAY, MONDAY + 3 * DAY);
        let list: Vec<_> = found.iter().map(|event| (event.summary.as_str(), event.start, event.end)).collect();
        assert_eq!(
            list,
            [
                ("Bin day", MONDAY + DAY, MONDAY + 2 * DAY),
                ("Instant", utc_at(2026, 11, 10, 10, 0), utc_at(2026, 11, 10, 10, 0)),
                ("No length", MONDAY + 2 * DAY, MONDAY + 3 * DAY),
            ]
        );
        // It is not shown the day before.
        assert!(read(&text, MONDAY, MONDAY + DAY).is_empty());
    }

    #[test]
    fn weeks_start_where_the_rule_says() {
        // From Tuesday 27 October, every other week on Tuesday and Sunday,
        // in weeks from Sunday: 8 and 10 November, then 22 and 24.
        let event = |wkst: &str| {
            calendar(&format!(
                "BEGIN:VEVENT\r\nUID:w\r\nDTSTART:20261027T180000Z\r\nDTEND:20261027T190000Z\r\n\
                 RRULE:FREQ=WEEKLY;{wkst}INTERVAL=2;BYDAY=TU,SU\r\nSUMMARY:Choir\r\nEND:VEVENT\r\n"
            ))
        };
        let starts = |text: &str| -> Vec<i64> {
            read(text, MONDAY - 7 * DAY, MONDAY + 21 * DAY).iter().map(|event| event.start).collect()
        };
        assert_eq!(
            starts(&event("WKST=SU;")),
            [
                utc_at(2026, 11, 8, 18, 0),
                utc_at(2026, 11, 10, 18, 0),
                utc_at(2026, 11, 22, 18, 0),
                utc_at(2026, 11, 24, 18, 0)
            ]
        );
        // Weeks from Monday, the default: 10 and 15 November, then 24 and 29.
        let monday = [
            utc_at(2026, 11, 10, 18, 0),
            utc_at(2026, 11, 15, 18, 0),
            utc_at(2026, 11, 24, 18, 0),
            utc_at(2026, 11, 29, 18, 0),
        ];
        assert_eq!(starts(&event("")), monday);
        assert_eq!(starts(&event("WKST=MO;")), monday);
        assert!(rule("FREQ=WEEKLY;WKST=XX").is_none());
    }

    #[test]
    fn follows_a_by_month_day_or_month_that_repeats_the_start() {
        let event = |uid: &str, start: &str, rrule: &str| {
            format!(
                "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:{start}\r\nDURATION:PT1H\r\nRRULE:{rrule}\r\nSUMMARY:{uid}\r\nEND:VEVENT\r\n"
            )
        };
        let text = calendar(
            &[
                event("rent", "20261015T100000Z", "FREQ=MONTHLY;BYMONTHDAY=15"),
                event("birthday", "20001112T080000Z", "FREQ=YEARLY;BYMONTH=11;BYMONTHDAY=12"),
                event("month", "20001110T080000Z", "FREQ=YEARLY;BYMONTH=11"),
                // Not the start's own day or month: only the start is shown.
                event("other-day", "20261015T100000Z", "FREQ=MONTHLY;BYMONTHDAY=16"),
                event("other-month", "20001112T080000Z", "FREQ=YEARLY;BYMONTH=12;BYMONTHDAY=12"),
            ]
            .concat(),
        );
        let found = read(&text, MONDAY, MONDAY + 7 * DAY);
        let list: Vec<_> = found.iter().map(|event| (event.summary.as_str(), event.start)).collect();
        assert_eq!(
            list,
            [
                ("month", utc_at(2026, 11, 10, 8, 0)),
                ("birthday", utc_at(2026, 11, 12, 8, 0)),
                ("rent", utc_at(2026, 11, 15, 10, 0))
            ]
        );
        let october = read(&text, utc_at(2026, 10, 15, 0, 0), utc_at(2026, 10, 17, 0, 0));
        assert!(october.iter().any(|event| event.summary == "other-day"));
        // More than one, a negative day, or a part that means more than the
        // start's own day is not followed.
        assert!(rule("FREQ=MONTHLY;BYMONTHDAY=1,15").is_none());
        assert!(rule("FREQ=MONTHLY;BYMONTHDAY=-1").is_none());
        assert!(rule("FREQ=YEARLY;BYMONTHDAY=15").is_none());
        assert!(rule("FREQ=MONTHLY;BYMONTH=11").is_none());
        assert!(rule("FREQ=WEEKLY;BYMONTHDAY=15").is_none());
    }

    #[test]
    fn moved_occurrences_replace_their_originals() {
        let text = calendar(
            "BEGIN:VEVENT\r\nUID:r\r\nDTSTART:20261109T090000Z\r\nDURATION:PT30M\r\nRRULE:FREQ=DAILY;INTERVAL=2\r\nSUMMARY:Sync\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:r\r\nRECURRENCE-ID:20261111T090000Z\r\nDTSTART:20261111T150000Z\r\nDURATION:PT30M\r\nSUMMARY:Sync (moved)\r\nEND:VEVENT\r\n",
        );
        let found = read(&text, MONDAY, MONDAY + 5 * DAY);
        let list: Vec<_> = found.iter().map(|event| (event.start, event.summary.as_str())).collect();
        assert_eq!(
            list,
            [
                (utc_at(2026, 11, 9, 9, 0), "Sync"),
                (utc_at(2026, 11, 11, 15, 0), "Sync (moved)"),
                (utc_at(2026, 11, 13, 9, 0), "Sync")
            ]
        );
    }

    #[test]
    fn reads_values_carefully() {
        assert_eq!(duration("PT1H30M"), Some(5400));
        assert_eq!(duration("P1DT2H"), Some(DAY + 7200));
        assert_eq!(duration("P2W"), Some(14 * DAY));
        assert_eq!(duration("-PT15M"), Some(-900));
        assert_eq!(duration("P1H"), None);
        assert_eq!(duration("PT"), Some(0));
        assert_eq!(stamp("20261332", false, None), None);
        assert_eq!(stamp("20260230T100000", false, None), None);
        assert_eq!(stamp("20261109T250000Z", false, None), None);
        assert_eq!(stamp("20261109T100000+01", false, None), None);
        let (name, params, value) = content_line("DTSTART;TZID=\"Europe/A:b\";X=1:20261109T100000").unwrap();
        assert_eq!((name.as_str(), value.as_str()), ("DTSTART", "20261109T100000"));
        assert_eq!(params["TZID"], "Europe/A:b");
        assert_eq!(unescape("a\\nb\\;c\\\\"), "a\nb;c\\");
        assert_eq!(unfold(b"A:1\r\n 2\r\n\tthree\r\nB:x"), ["A:12three", "B:x"]);
        assert!(rule("FREQ=HOURLY").is_none());
        assert!(rule("FREQ=MONTHLY;BYDAY=MO").is_none());
        assert!(rule("INTERVAL=2").is_none());
        // Unfinished events and broken lines are skipped.
        assert!(
            read("BEGIN:VEVENT\r\nDTSTART:2026\r\nSUMMARY:x\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nnonsense", 0, i64::MAX)
                .is_empty()
        );
    }

    #[test]
    fn the_bar_shows_the_next_event_today() {
        let event = |start: i64, minutes: i64, all_day: bool, summary: &str| Occurrence {
            start,
            end: start + minutes * 60,
            all_day,
            summary: summary.into(),
            location: String::new(),
        };
        let list = [
            event(MONDAY, 24 * 60, true, "Holiday"),
            event(utc_at(2026, 11, 9, 9, 0), 60, false, "Running"),
            event(utc_at(2026, 11, 9, 11, 0), 30, false, "Later"),
            event(utc_at(2026, 11, 9, 10, 30), 30, false, "Next"),
            event(utc_at(2026, 11, 10, 8, 0), 30, false, "Tomorrow"),
        ];
        let now = utc_at(2026, 11, 9, 9, 30);
        let tomorrow = MONDAY + DAY;
        assert_eq!(next_today(&list, now, tomorrow).map(|event| event.summary.as_str()), Some("Next"));
        // Once the last one has started, nothing more today.
        assert_eq!(next_today(&list, utc_at(2026, 11, 9, 11, 0), tomorrow), None);
        let names = |found: Vec<&Occurrence>| found.iter().map(|event| event.summary.clone()).collect::<Vec<_>>();
        assert_eq!(
            names(upcoming(&list, now, tomorrow + DAY, 10)),
            ["Holiday", "Running", "Next", "Later", "Tomorrow"]
        );
        assert_eq!(names(upcoming(&list, now, tomorrow, 2)), ["Holiday", "Running"]);
        assert_eq!(names(upcoming(&list, utc_at(2026, 11, 9, 12, 0), tomorrow + DAY, 10)), ["Holiday", "Tomorrow"]);
    }

    #[test]
    fn the_bar_shows_the_event_on_now() {
        let event = |start: i64, minutes: i64, all_day: bool, summary: &str| Occurrence {
            start,
            end: start + minutes * 60,
            all_day,
            summary: summary.into(),
            location: String::new(),
        };
        let list = [
            event(MONDAY, 24 * 60, true, "Holiday"),
            event(utc_at(2026, 11, 9, 9, 0), 8 * 60, false, "Focus"),
            event(utc_at(2026, 11, 9, 10, 0), 60, false, "Review"),
            event(utc_at(2026, 11, 9, 10, 0), 30, false, "Call"),
            event(utc_at(2026, 11, 9, 10, 30), 30, false, "Standup"),
            event(utc_at(2026, 11, 9, 12, 0), 0, false, "Reminder"),
            event(utc_at(2026, 11, 8, 18, 0), 3 * 24 * 60, false, "Conference"),
            event(utc_at(2026, 11, 9, 22, 0), 8 * 60, false, "Night shift"),
        ];
        let on_at = |hour: i64, minute: i64| {
            current(&list, utc_at(2026, 11, 9, hour, minute)).map(|found| found.summary.as_str())
        };
        // All-day events never count.
        assert_eq!(on_at(8, 0), None);
        assert_eq!(on_at(9, 30), Some("Focus"));
        // The one that started last, then the one ending first.
        assert_eq!(on_at(10, 0), Some("Call"));
        assert_eq!(on_at(10, 29), Some("Call"));
        // At its end the next one takes over, without a gap.
        assert_eq!(on_at(10, 30), Some("Standup"));
        // Then back to the longer block it was in.
        assert_eq!(on_at(11, 0), Some("Focus"));
        // An event without length is never on; it is only ever the next.
        assert_eq!(on_at(12, 0), Some("Focus"));
        // Neither does a timed event lasting days.
        assert_eq!(on_at(17, 0), None);
        // One running past midnight does.
        assert_eq!(on_at(23, 0), Some("Night shift"));
    }
}
