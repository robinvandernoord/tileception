//! The `timers` plugin: `timer 10m tea` in the launcher starts a countdown
//! and `timer at 15:30 call the dentist` sets an alarm. Each is a card
//! under the launcher with the time left, a progress bar and buttons to
//! pause, add a minute and stop; a notification says when it is time.
//! Timers are kept in the state folder, so they survive the plugin starting
//! again, and ring late when they ended meanwhile.
//!
//! The launcher itself reads plain sentences such as "set a timer for 10
//! minutes" ([`understood`]) and offers a row that calls this plugin, since
//! plugins only get what follows their prefix.

use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Notifier, Plugin, tool};
use crate::plugins::LAUNCHER;

/// The longest timer: a day.
const LONGEST: u64 = 24 * 60 * 60;
/// How many timers there can be at once.
const MOST: usize = 16;
/// How long a timer that rang keeps its card, when not dismissed.
const KEEP: i64 = 10 * 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Timer {
    #[serde(rename = "id")]
    key: u64,
    label: String,
    /// What it counts down from, in seconds, with minutes added since.
    total: u64,
    /// Set for a time of day or as a reminder, instead of counting down a
    /// length: it says when it rings and cannot be paused.
    #[serde(default)]
    alarm: bool,
    phase: Phase,
}

/// Times are seconds since the Unix epoch, so they hold across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    /// Counting down until it rings at this time.
    Running(i64),
    /// Held with this many seconds left.
    Paused(u64),
    /// Rang at this time.
    Rang(i64),
}

impl Timer {
    fn left(&self, now: i64) -> u64 {
        match self.phase {
            Phase::Running(ends) => u64::try_from(ends - now).unwrap_or(0),
            Phase::Paused(left) => left,
            Phase::Rang(_) => 0,
        }
    }

    fn name(&self) -> &str {
        match (self.label.is_empty(), self.alarm) {
            (false, _) => &self.label,
            (true, true) => "Alarm",
            (true, false) => "Timer",
        }
    }

    /// "10 minutes, rings at 14:32", as its card and row say.
    fn detail(&self, now: i64) -> String {
        let length = spoken(self.total);
        match self.phase {
            // Between its end and the next tick it is as good as done.
            Phase::Running(ends) if ends <= now && self.alarm => format!("Rang at {}", clock(ends)),
            Phase::Running(ends) if ends <= now => format!("{length}, done"),
            Phase::Running(ends) if self.alarm => format!("Rings at {}", clock(ends)),
            Phase::Running(ends) => format!("{length}, rings at {}", clock(ends)),
            Phase::Paused(_) => format!("{length}, paused"),
            Phase::Rang(when) if self.alarm => format!("Rang at {}", clock(when)),
            Phase::Rang(when) if now - when < 60 => format!("{length}, done"),
            Phase::Rang(when) => format!("{length}, done at {}", clock(when)),
        }
    }

    /// What its notification and its row say once it rang.
    fn rang(&self) -> String {
        if self.alarm { self.name().to_string() } else { format!("{} is done", self.name()) }
    }
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Timers {
    /// The next timer's id.
    next: u64,
    timers: Vec<Timer>,
}

impl Timers {
    fn start(&mut self, seconds: u64, label: &str, alarm: bool, now: i64) -> Result<&Timer, String> {
        // A local calendar day can exceed 24 hours when clocks go back.
        if seconds == 0 || (!alarm && seconds > LONGEST) {
            return Err("A timer runs from a second to a day".into());
        }
        if self.timers.len() >= MOST {
            return Err(format!("There are {MOST} timers already; stop one first"));
        }
        self.next += 1;
        let ends = now + i64::try_from(seconds).unwrap_or(i64::MAX);
        self.timers.push(Timer {
            key: self.next,
            label: titled(label),
            total: seconds,
            alarm,
            phase: Phase::Running(ends),
        });
        self.timers.last().ok_or_else(|| "The timer was not kept".into())
    }

    /// The timer with this id, or the only one called this.
    fn find(&mut self, wanted: &Value) -> Result<&mut Timer, String> {
        let position = match wanted {
            Value::Number(number) => self.timers.iter().position(|timer| Some(timer.key) == number.as_u64()),
            Value::String(text) => match text.trim().parse::<u64>() {
                Ok(key) => self.timers.iter().position(|timer| timer.key == key),
                Err(_) => self.timers.iter().position(|timer| timer.name().eq_ignore_ascii_case(text.trim())),
            },
            _ => (self.timers.len() == 1).then_some(0),
        };
        position.and_then(|position| self.timers.get_mut(position)).ok_or_else(|| "No such timer".into())
    }

    /// Timers that end now ring; those that rang long ago go. Returns the
    /// ones that rang and whether anything changed.
    fn tick(&mut self, now: i64) -> (Vec<Timer>, bool) {
        let before = self.timers.len();
        self.timers.retain(|timer| !matches!(timer.phase, Phase::Rang(when) if now - when >= KEEP));
        let mut rang = Vec::new();
        for timer in &mut self.timers {
            if let Phase::Running(ends) = timer.phase
                && ends <= now
            {
                timer.phase = Phase::Rang(ends);
                rang.push(timer.clone());
            }
        }
        let changed = !rang.is_empty() || self.timers.len() != before;
        (rang, changed)
    }

    /// Whether time changes what the cards say.
    fn ticking(&self) -> bool {
        self.timers.iter().any(|timer| !matches!(timer.phase, Phase::Paused(_)))
    }

    /// Those that rang first, then the soonest.
    fn sorted(&self, now: i64) -> Vec<&Timer> {
        let mut timers: Vec<&Timer> = self.timers.iter().collect();
        timers.sort_by_key(|timer| (!matches!(timer.phase, Phase::Rang(_)), timer.left(now), timer.key));
        timers
    }

    fn cards(&self, now: i64) -> Value {
        json!({ "cards": self.sorted(now).into_iter().map(|timer| card(timer, now)).collect::<Vec<_>>() })
    }

    fn rows(&self, query: &str, now: i64) -> Value {
        let mut rows = Vec::new();
        match parse(query) {
            Some((ask, label)) => rows.push(start_row(ask, &label, Said::Timer, now)),
            None => rows.push(json!({
                "title": if query.trim().is_empty() { "Start a timer" } else { "How long, or at what time?" },
                "detail": "Such as 10m tea, 1h30m, 2:30, or at 15:30 call the dentist",
                "icon": "glyph:clock",
            })),
        }
        for timer in self.sorted(now) {
            let hold = match timer.phase {
                Phase::Running(_) if timer.alarm => None,
                Phase::Running(_) => Some("Pause"),
                Phase::Paused(_) => Some("Resume"),
                Phase::Rang(_) => None,
            };
            let rang = matches!(timer.phase, Phase::Rang(_));
            let mut actions = Vec::new();
            if let Some(hold) = hold {
                actions.push(act(hold, "pause", timer.key, None));
            }
            if !rang {
                actions.push(act("Add a minute", "add", timer.key, Some(60)));
            }
            actions.push(act(if rang { "Dismiss" } else { "Stop" }, "stop", timer.key, None));
            let title =
                if rang { timer.rang() } else { format!("{}: {} left", timer.name(), reading(timer.left(now))) };
            rows.push(
                json!({ "title": title, "detail": timer.detail(now), "icon": "glyph:clock", "actions": actions }),
            );
        }
        json!({ "rows": rows })
    }

    /// What `tileception msg plugin timers list` prints.
    fn listed(&self, now: i64) -> String {
        if self.timers.is_empty() {
            return "No timers".into();
        }
        self.sorted(now)
            .into_iter()
            .map(|timer| {
                format!("{} {}: {} left ({})", timer.key, timer.name(), reading(timer.left(now)), timer.detail(now))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A launcher action calling one of this plugin's tools for a timer.
fn act(title: &str, name: &str, timer: u64, seconds: Option<u64>) -> Value {
    let mut arguments = json!({ "timer": timer });
    if let Some(seconds) = seconds {
        arguments["seconds"] = json!(seconds);
    }
    json!({ "title": title, "type": "tool", "name": name, "arguments": arguments })
}

/// A timer's card under the launcher.
fn card(timer: &Timer, now: i64) -> Value {
    let tool = |name: &str, seconds: Option<u64>| {
        let action = act("", name, timer.key, seconds);
        json!({ "type": "tool", "name": action["name"], "arguments": action["arguments"] })
    };
    let button = |title: &str, icon: Option<&str>, primary: bool, action: Value| json!({ "title": title, "icon": icon, "primary": primary, "enabled": true, "action": action });
    let left = timer.left(now);
    let more = button("+1 min", None, false, tool("add", Some(60)));
    let stop = button("Stop", Some("glyph:close"), false, tool("stop", None));
    let (tint, buttons) = match timer.phase {
        Phase::Running(_) if timer.alarm => ("accent", vec![more, stop]),
        Phase::Running(_) => {
            ("accent", vec![more, button("Pause", Some("glyph:pause"), true, tool("pause", None)), stop])
        }
        Phase::Paused(_) => {
            ("muted", vec![more, button("Resume", Some("glyph:play"), true, tool("pause", None)), stop])
        }
        Phase::Rang(_) => ("warm", vec![button("Dismiss", Some("glyph:check"), true, tool("stop", None))]),
    };
    let done = timer.total.saturating_sub(left) as f64 / timer.total.max(1) as f64;
    json!({
        "id": timer.key.to_string(),
        "title": timer.name(),
        "detail": timer.detail(now),
        "value": reading(left),
        "icon": "glyph:clock",
        "tint": tint,
        "lit": true,
        "enabled": true,
        "buttons": buttons,
        "progress": { "value": done * 100.0 },
    })
}

/// A label with its first letter as a capital, at most 40 characters.
fn titled(label: &str) -> String {
    let label: String = label.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
    let mut letters = label.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// The time left as a card's reading: "2:41", and from an hour "1h02".
fn reading(seconds: u64) -> String {
    let (hours, minutes, rest) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if hours > 0 { format!("{hours}h{minutes:02}") } else { format!("{minutes}:{rest:02}") }
}

/// A duration in words: "1 hour 30 minutes".
fn spoken(seconds: u64) -> String {
    let parts = [(seconds / 3600, "hour"), (seconds % 3600 / 60, "minute"), (seconds % 60, "second")];
    let words: Vec<String> = parts
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, unit)| format!("{count} {unit}{}", if count == 1 { "" } else { "s" }))
        .collect();
    if words.is_empty() { "0 seconds".into() } else { words.join(" ") }
}

/// The local time of day at a Unix time: "14:32".
fn clock(when: i64) -> String {
    crate::ui::LocalTime::at(when).map_or_else(|| "?".into(), |time| time.clock())
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
}

/// What is asked for: a length of time, or a time of day.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ask {
    Length(u64),
    Time(Clock),
}

/// A time of day as typed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Clock {
    hour: u32,
    minute: u32,
    /// "3" or "3:30" without am or pm: morning or afternoon, whichever
    /// comes first.
    either: bool,
}

impl Clock {
    /// The seconds until the next occurrence on the local calendar.
    fn seconds_from(self, now: i64) -> u64 {
        let hours = if self.either { vec![self.hour % 12, self.hour % 12 + 12] } else { vec![self.hour] };
        hours
            .into_iter()
            .flat_map(|hour| (0..=1).filter_map(move |days| crate::wake::at_clock(now, hour, self.minute, days)))
            .filter_map(|when| when.checked_sub(now).and_then(|wait| u64::try_from(wait).ok()))
            .filter(|wait| *wait > 0)
            .min()
            .unwrap_or(0)
    }
}

impl Ask {
    /// The seconds from now until it rings.
    fn seconds(self, now: i64) -> u64 {
        match self {
            Ask::Length(seconds) => seconds,
            Ask::Time(clock) => clock.seconds_from(now),
        }
    }
}

/// How long or at what time, and what for, from what is typed. The time
/// comes first or last: "10m tea", "tea 10m", "for 1 hour 30 minutes", "at
/// 15:30 call the dentist", "call the dentist at 3", "stretch in 20 min".
/// A bare number is minutes, and "2:30" without "at" is minutes and seconds.
fn parse(query: &str) -> Option<(Ask, String)> {
    let words: Vec<&str> = query.split_whitespace().collect();
    let word = |index: usize| words.get(index).map(|word| word.to_lowercase()).unwrap_or_default();
    let label = |words: &[&str]| {
        let filler =
            |first: &&str| ["to", "for", "about", "that"].iter().any(|filler| first.eq_ignore_ascii_case(filler));
        words[usize::from(words.first().is_some_and(filler))..].join(" ")
    };
    // The time first.
    match word(0).as_str() {
        "for" | "in" => {
            let (seconds, used) = duration(&words[1..])?;
            return Some((Ask::Length(seconds), label(&words[1 + used..])));
        }
        "at" => {
            let (clock, used, _) = time_of_day(&words[1..])?;
            return Some((Ask::Time(clock), label(&words[1 + used..])));
        }
        _ => {}
    }
    if let Some((clock, used, true)) = time_of_day(&words) {
        return Some((Ask::Time(clock), label(&words[used..])));
    }
    if let Some((seconds, used)) = duration(&words) {
        return Some((Ask::Length(seconds), label(&words[used..])));
    }
    // The time last, after what it is for.
    (1..words.len()).find_map(|from| {
        let rest = &words[from..];
        let ends = |used: usize| from + used == words.len();
        match word(from).as_str() {
            "for" | "in" => {
                let (seconds, used) = duration(&rest[1..])?;
                ends(1 + used).then(|| (Ask::Length(seconds), label(&words[..from])))
            }
            "at" => {
                let (clock, used, _) = time_of_day(&rest[1..])?;
                ends(1 + used).then(|| (Ask::Time(clock), label(&words[..from])))
            }
            _ => {
                let (seconds, used) = duration(rest)?;
                ends(used).then(|| (Ask::Length(seconds), label(&words[..from])))
            }
        }
    })
}

/// The time of day at the start of the words, how many words it takes and
/// whether it says am or pm: "15:30", "3", "3pm", "3:30 pm", "noon".
fn time_of_day(words: &[&str]) -> Option<(Clock, usize, bool)> {
    let first = words.first()?.to_lowercase();
    let exact = |hour: u32| Some((Clock { hour, minute: 0, either: false }, 1, true));
    match first.as_str() {
        "noon" | "midday" => return exact(12),
        "midnight" => return exact(0),
        _ => {}
    }
    let half = |word: &str| match word {
        "am" | "a.m." => Some(false),
        "pm" | "p.m." => Some(true),
        _ => None,
    };
    let (digits, mut afternoon) = match first.strip_suffix("am").or_else(|| first.strip_suffix("pm")) {
        Some(digits) => (digits.to_string(), Some(first.ends_with("pm"))),
        None => (first.clone(), None),
    };
    let mut used = 1;
    if afternoon.is_none()
        && let Some(next) = words.get(1).and_then(|next| half(&next.to_lowercase()))
    {
        afternoon = Some(next);
        used = 2;
    }
    let (hour, minute) = match digits.split_once(':') {
        Some((hour, minute)) if minute.len() == 2 => (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?),
        Some(_) => return None,
        None => (digits.parse::<u32>().ok()?, 0),
    };
    if digits.len() > 5 || hour > 23 || minute > 59 {
        return None;
    }
    let clock = match afternoon {
        Some(afternoon) if (1..=12).contains(&hour) => {
            Clock { hour: hour % 12 + if afternoon { 12 } else { 0 }, minute, either: false }
        }
        Some(_) => return None,
        // "03:00" and "15:00" are on the 24-hour clock; "3" may be either half.
        None => Clock { hour, minute, either: (1..=12).contains(&hour) && !digits.starts_with('0') },
    };
    if words.get(used).is_some_and(|next| next.eq_ignore_ascii_case("o'clock")) {
        used += 1;
    }
    Some((clock, used, afternoon.is_some()))
}

/// What the words asked for call it, which the row repeats.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Said {
    Timer,
    Alarm,
    Reminder,
}

/// The launcher row that starts what is asked for.
fn start_row(ask: Ask, label: &str, said: Said, now: i64) -> Value {
    let label = titled(label);
    let seconds = ask.seconds(now);
    let ends = now + i64::try_from(seconds).unwrap_or(0);
    let named = |rest: String| if label.is_empty() { rest } else { format!("{label}, {}", lowered(&rest)) };
    let (title, detail, arguments) = match ask {
        Ask::Length(_) if said != Said::Timer => (
            format!("{} in {}", if said == Said::Reminder { "Remind me" } else { "Set an alarm" }, spoken(seconds)),
            named(format!("Rings at {}", clock(ends))),
            json!({ "duration": format!("{seconds}s"), "label": label, "reminder": true }),
        ),
        Ask::Length(_) => (
            format!("Start a timer of {}", spoken(seconds)),
            named(format!("Rings at {}", clock(ends))),
            json!({ "duration": format!("{seconds}s"), "label": label }),
        ),
        Ask::Time(_) => (
            format!("{} {}", if said == Said::Reminder { "Remind me at" } else { "Set an alarm for" }, clock(ends)),
            // Whole minutes are enough for something hours away.
            named(format!("In {}", spoken(if seconds >= 60 { seconds.div_ceil(60) * 60 } else { seconds }))),
            json!({ "at": clock(ends), "label": label }),
        ),
    };
    json!({
        "title": title,
        "detail": detail,
        "icon": "glyph:clock",
        "actions": [{ "title": "Start", "type": "tool", "name": "start", "arguments": arguments }],
    })
}

/// A sentence with its first letter small, to follow a name and a comma.
fn lowered(text: &str) -> String {
    let mut letters = text.chars();
    letters.next().map_or_else(String::new, |first| first.to_lowercase().chain(letters).collect())
}

/// The row for a plain sentence typed in the launcher, without this
/// plugin's prefix: "set a timer for 10 minutes", "timer for 5 min tea",
/// "10 minute timer", "set an alarm at 7", "remind me at 3 to call the
/// dentist", "remind me to stretch in 20 minutes", "wake me up at 6:30".
/// None for anything else, so searching apps is never in the way.
pub(crate) fn understood(text: &str, now: i64) -> Option<crate::plugins::Row> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|word| word.to_lowercase()).collect();
    let word_is = |index: usize, among: &[&str]| lower.get(index).is_some_and(|word| among.contains(&word.as_str()));
    let mut next = usize::from(word_is(0, &["please"]));
    let (rest, said, wake): (&[&str], Said, bool) = if word_is(next, &["remind"]) && word_is(next + 1, &["me"]) {
        (&words[next + 2..], Said::Reminder, false)
    } else if word_is(next, &["wake"]) && word_is(next + 1, &["me"]) {
        (&words[next + 2 + usize::from(word_is(next + 2, &["up"]))..], Said::Alarm, true)
    } else {
        let verb = word_is(next, &["set", "start", "create", "add", "plan", "make", "new", "schedule"]);
        next += usize::from(verb);
        next += usize::from(word_is(next, &["a", "an", "the", "my"]));
        if word_is(next, &["timer", "alarm", "reminder", "countdown"]) {
            // Without a verb it is this plugin's prefix, or a search, unless
            // a word such as "for" follows.
            if !verb && !word_is(next + 1, &["for", "of", "at", "in"]) {
                return None;
            }
            let said = match lower[next].as_str() {
                "alarm" => Said::Alarm,
                "reminder" => Said::Reminder,
                _ => Said::Timer,
            };
            (&words[next + 1..], said, false)
        } else {
            // "10 minute timer for tea".
            let noun = lower.iter().position(|word| word == "timer")?;
            let (seconds, used) = duration(&words[..noun])?;
            if used != noun {
                return None;
            }
            let label = words[noun + 1..].join(" ");
            let label = label.strip_prefix("for ").unwrap_or(&label);
            return serde_json::from_value(start_row(Ask::Length(seconds), label, Said::Timer, now)).ok();
        }
    };
    let mut rest = rest.to_vec();
    if rest.first().is_some_and(|first| first.eq_ignore_ascii_case("of")) {
        rest[0] = "for";
    }
    let (ask, label) = parse(&rest.join(" "))?;
    let label = if wake && label.is_empty() { "Wake up".to_string() } else { label };
    serde_json::from_value(start_row(ask, &label, said, now)).ok()
}

/// The duration at the start of the words, and how many words it takes.
fn duration(words: &[&str]) -> Option<(u64, usize)> {
    let first = words.first()?;
    if first.contains(':') {
        let parts: Vec<u64> = first.split(':').map(str::parse).collect::<Result<_, _>>().ok()?;
        let seconds = match parts[..] {
            [minutes, seconds] if seconds < 60 => minutes.checked_mul(60)?.checked_add(seconds)?,
            [hours, minutes, seconds] if minutes < 60 && seconds < 60 => {
                hours.checked_mul(3600)?.checked_add(minutes.checked_mul(60)?)?.checked_add(seconds)?
            }
            _ => return None,
        };
        return (1..=LONGEST).contains(&seconds).then_some((seconds, 1));
    }
    let mut total = 0.0;
    let mut used = 0;
    while let Some(word) = words.get(used) {
        let word = word.to_lowercase();
        let Some(pieces) = pieces(&word) else { break };
        let mut sum = 0.0;
        let mut also = 0;
        for (index, (number, unit)) in pieces.iter().enumerate() {
            let scale = if unit.is_empty() && index + 1 == pieces.len() {
                // The unit is the next word, or this is a bare number of minutes.
                match words.get(used + 1).and_then(|next| scale(&next.to_lowercase())) {
                    Some(scale) => {
                        also = 1;
                        scale
                    }
                    None if used == 0 && pieces.len() == 1 => 60.0,
                    None => return finished(total, used),
                }
            } else {
                match scale(unit) {
                    Some(scale) => scale,
                    None => return finished(total, used),
                }
            };
            sum += number * scale;
        }
        total += sum;
        used += 1 + also;
        if pieces.last().is_some_and(|(_, unit)| unit.is_empty()) && also == 0 {
            break;
        }
    }
    finished(total, used)
}

fn finished(total: f64, used: usize) -> Option<(u64, usize)> {
    let seconds = total.round() as u64;
    (used > 0 && (1..=LONGEST).contains(&seconds)).then_some((seconds, used))
}

/// A word such as "1h30m" or "10" as numbers with their units; none when
/// it is anything else.
fn pieces(word: &str) -> Option<Vec<(f64, &str)>> {
    let mut pieces = Vec::new();
    let mut rest = word;
    while !rest.is_empty() {
        let digits = rest.find(|letter: char| !letter.is_ascii_digit() && letter != '.').unwrap_or(rest.len());
        let number: f64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let letters = rest.find(|letter: char| !letter.is_ascii_alphabetic()).unwrap_or(rest.len());
        pieces.push((number, &rest[..letters]));
        rest = &rest[letters..];
        if letters == 0 && !rest.is_empty() {
            return None;
        }
    }
    (!pieces.is_empty()).then_some(pieces)
}

/// The seconds in a unit.
fn scale(unit: &str) -> Option<f64> {
    Some(match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
        "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
        _ => return None,
    })
}

/// What the request loop and the ticking thread share.
struct Shared {
    timers: Mutex<Timers>,
    /// Woken when a timer starts ticking.
    wake: Condvar,
    /// Where the timers are kept; none with `--no-save`.
    path: Option<PathBuf>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Timers> {
        self.timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn save(&self, timers: &Timers) {
        let Some(path) = &self.path else { return };
        let written = serde_json::to_vec(timers).map_err(|error| error.to_string()).and_then(|bytes| {
            let scratch = path.with_extension("json.new");
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
            }
            std::fs::write(&scratch, bytes)
                .and_then(|()| std::fs::rename(&scratch, path))
                .map_err(|error| error.to_string())
        });
        if let Err(error) = written {
            eprintln!("timers: could not save {}: {error}", path.display());
        }
    }
}

pub struct TimersPlugin {
    shared: Arc<Shared>,
    notifier: Option<Notifier>,
}

impl TimersPlugin {
    /// `--no-save` keeps the timers in memory only, as nested and headless
    /// instances do, which share the desktop's state folder.
    pub fn new(args: &[String]) -> Self {
        let path = (!args.iter().any(|arg| arg == "--no-save")).then(state_file).flatten();
        let timers = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { shared: Arc::new(Shared { timers: Mutex::new(timers), wake: Condvar::new(), path }), notifier: None }
    }

    /// The timers changed: saves them and has the cards read again.
    fn changed(&self, timers: &Timers) {
        self.shared.save(timers);
        self.shared.wake.notify_all();
        if let Some(notifier) = &self.notifier {
            notifier.changed(LAUNCHER);
        }
    }
}

fn state_file() -> Option<PathBuf> {
    let root = std::env::var_os("XDG_STATE_HOME")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state")))?;
    Some(root.join("tileception/timers.json"))
}

/// Once a second while a timer counts down or shows that it rang: rings
/// those that end and has the cards read again.
fn tick(shared: &Shared, notifier: &Notifier) {
    let mut bus = None;
    loop {
        let mut timers = shared
            .wake
            .wait_while(shared.lock(), |timers| !timers.ticking())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (rang, changed) = timers.tick(unix_now());
        if changed {
            shared.save(&timers);
        }
        drop(timers);
        notifier.changed(LAUNCHER);
        for timer in rang {
            ring(&mut bus, &timer);
        }
        // Until the next whole second, so the cards change with the clock.
        let part = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.subsec_millis());
        std::thread::sleep(Duration::from_millis(u64::from(1000 - part.min(999)) + 5));
    }
}

/// Says a timer is done, as a notification that stays until dismissed.
fn ring(bus: &mut Option<zbus::blocking::Connection>, timer: &Timer) {
    if bus.is_none() {
        let connection = match std::env::var("TILECEPTION_NOTIFICATION_BUS") {
            Ok(address) => {
                zbus::blocking::connection::Builder::address(address.as_str()).and_then(|builder| builder.build())
            }
            Err(_) => zbus::blocking::Connection::session(),
        };
        *bus = connection.map_err(|error| eprintln!("timers: no notification bus: {error}")).ok();
    }
    let Some(session) = bus.as_ref() else { return };
    let hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> =
        std::collections::HashMap::from([("urgency", zbus::zvariant::Value::U8(2))]);
    let summary = timer.rang();
    let body = timer.detail(unix_now());
    let actions: &[&str] = &[];
    let sent = session.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &("Timers", 0u32, "alarm-symbolic", summary.as_str(), body.as_str(), actions, hints, 0i32),
    );
    if let Err(error) = sent {
        eprintln!("timers: could not notify: {error}");
        *bus = None;
    }
}

impl Plugin for TimersPlugin {
    fn tools(&self) -> Value {
        let timer = json!({ "description": "The timer's id or name; the only timer when left out" });
        let schema = |properties: Value, required: &[&str]| json!({ "type": "object", "properties": properties, "required": required });
        json!([
            tool("launcher_info", "Launcher metadata", &[]),
            tool("launcher_search", "A timer to start, and the timers there are", &["prefix", "query"]),
            { "name": "start", "description": "Start a timer for a length of time, or an alarm at a time of day",
              "inputSchema": schema(json!({
                  "duration": { "type": "string", "description": "How long, such as 10m, 1h30m, 90s or 2:30" },
                  "at": { "type": "string", "description": "Instead: a time of day, such as 15:30, 3pm or noon" },
                  "label": { "type": "string", "description": "What it is for" },
                  "reminder": { "type": "boolean", "description": "Ring with the label alone, as a reminder" } }), &[]) },
            { "name": "pause", "description": "Pause a running timer, or resume a paused one",
              "inputSchema": schema(json!({ "timer": timer }), &[]) },
            { "name": "add", "description": "Give a timer more time",
              "inputSchema": schema(json!({ "timer": timer, "seconds": { "type": "integer", "description": "60 when left out" } }), &[]) },
            { "name": "stop", "description": "Stop a timer, or dismiss one that is done",
              "inputSchema": schema(json!({ "timer": timer }), &[]) },
            tool("list", "The timers and the time they have left", &[]),
        ])
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String> {
        let now = unix_now();
        let mut timers = self.shared.lock();
        let text = match name {
            "launcher_info" => {
                return Ok(json!({
                    "name": "Timers",
                    "description": "Count down and say when the time is up",
                    "icon": "glyph:clock",
                    "prefixes": ["timer"],
                }));
            }
            "launcher_search" => return Ok(timers.rows(arguments["query"].as_str().unwrap_or_default(), now)),
            "list" => return Ok(json!({ "text": timers.listed(now) })),
            "start" => {
                let asked = match (arguments["duration"].as_str(), arguments["at"].as_str()) {
                    (Some(length), _) => length.to_string(),
                    (None, Some(time)) => format!("at {time}"),
                    (None, None) => String::new(),
                };
                let (ask, rest) =
                    parse(&asked).ok_or("Say how long, such as 10m, 1h30m or 2:30, or at what time, such as 15:30")?;
                let label = arguments["label"].as_str().map_or(rest, String::from);
                let alarm = matches!(ask, Ask::Time(_)) || arguments["reminder"].as_bool() == Some(true);
                let timer = timers.start(ask.seconds(now), &label, alarm, now)?;
                format!("{}: {}", timer.name(), timer.detail(now))
            }
            "pause" => {
                let timer = timers.find(&arguments["timer"])?;
                timer.phase = match timer.phase {
                    Phase::Running(_) if timer.alarm => {
                        return Err(format!("{} is set for a time; stop it instead", timer.name()));
                    }
                    Phase::Running(_) => Phase::Paused(timer.left(now)),
                    Phase::Paused(left) => Phase::Running(now + i64::try_from(left).unwrap_or(0)),
                    Phase::Rang(_) => return Err(format!("{} is done", timer.name())),
                };
                format!("{}: {}", timer.name(), timer.detail(now))
            }
            "add" => {
                let more = arguments["seconds"].as_u64().unwrap_or(60);
                let timer = timers.find(&arguments["timer"])?;
                let left = timer.left(now) + more;
                if left > LONGEST {
                    return Err("A timer runs for a day at most".into());
                }
                timer.phase = match timer.phase {
                    Phase::Running(_) => Phase::Running(now + i64::try_from(left).unwrap_or(0)),
                    Phase::Paused(_) => Phase::Paused(left),
                    Phase::Rang(_) => return Err(format!("{} is done", timer.name())),
                };
                timer.total += more;
                format!("{}: {}", timer.name(), timer.detail(now))
            }
            "stop" => {
                let key = timers.find(&arguments["timer"])?.key;
                timers.timers.retain(|timer| timer.key != key);
                "Stopped".into()
            }
            _ => return Err(format!("Unknown tool {name}")),
        };
        self.changed(&timers);
        Ok(json!({ "text": text }))
    }

    fn resources(&self) -> Vec<(&'static str, &'static str)> {
        vec![(LAUNCHER, "Timers under the launcher")]
    }

    fn read(&mut self, uri: &str) -> Result<Value, String> {
        match uri {
            LAUNCHER => Ok(self.shared.lock().cards(unix_now())),
            _ => Err(format!("no resource {uri}")),
        }
    }

    fn start(&mut self, notifier: Notifier) {
        self.notifier = Some(notifier.clone());
        let shared = self.shared.clone();
        if let Err(error) = std::thread::Builder::new().name("timers".into()).spawn(move || tick(&shared, &notifier)) {
            eprintln!("timers: could not start counting: {error}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;

    #[test]
    fn reads_how_long_and_what_for() {
        let parsed = |query: &str| match parse(query) {
            Some((Ask::Length(seconds), label)) => Some((seconds, label)),
            _ => None,
        };
        assert_eq!(parsed("10m tea"), Some((600, "tea".into())));
        assert_eq!(parsed("10 minutes green tea"), Some((600, "green tea".into())));
        assert_eq!(parsed("1h30m"), Some((5400, String::new())));
        assert_eq!(parsed("1h 30m pasta"), Some((5400, "pasta".into())));
        assert_eq!(parsed("1 hour 30 minutes"), Some((5400, String::new())));
        assert_eq!(parsed("for 90s"), Some((90, String::new())));
        assert_eq!(parsed("1.5h"), Some((5400, String::new())));
        assert_eq!(parsed("2:30 eggs"), Some((150, "eggs".into())));
        assert_eq!(parsed("1:00:05"), Some((3605, String::new())));
        // A bare number is minutes, and the duration may come last.
        assert_eq!(parsed("5"), Some((300, String::new())));
        assert_eq!(parsed("5 tea"), Some((300, "tea".into())));
        assert_eq!(parsed("tea 5m"), Some((300, "tea".into())));
        assert_eq!(parsed("3 eggs 6m"), Some((180, "eggs 6m".into())));
        for nothing in ["", "tea", "10x", "0m", "25h", "2:75", "m"] {
            assert_eq!(parsed(nothing), None, "{nothing:?}");
        }
    }

    #[test]
    fn reads_times_of_day() {
        let time = |query: &str| match parse(query) {
            Some((Ask::Time(clock), label)) => Some((clock.hour, clock.minute, clock.either, label)),
            _ => None,
        };
        assert_eq!(time("at 15:30 call the dentist"), Some((15, 30, false, "call the dentist".into())));
        assert_eq!(time("at 3 to call the dentist"), Some((3, 0, true, "call the dentist".into())));
        assert_eq!(time("call the dentist at 3"), Some((3, 0, true, "call the dentist".into())));
        assert_eq!(time("at 03:00"), Some((3, 0, false, String::new())));
        assert_eq!(time("3pm tea"), Some((15, 0, false, "tea".into())));
        assert_eq!(time("at 12 am"), Some((0, 0, false, String::new())));
        assert_eq!(time("at 7:15 PM standup"), Some((19, 15, false, "standup".into())));
        assert_eq!(time("at noon lunch"), Some((12, 0, false, "lunch".into())));
        assert_eq!(time("at 3 o'clock"), Some((3, 0, true, String::new())));
        // Without "at", "2:30" is a length and "3" is minutes.
        assert_eq!(time("2:30"), None);
        assert_eq!(time("3 tea"), None);
        for nothing in ["at", "at tea", "at 25", "at 3:5", "at 13pm", "at 9:99"] {
            assert_eq!(parse(nothing), None, "{nothing:?}");
        }
        // A length after what it is for, with "in".
        assert_eq!(parse("stretch in 20 min"), Some((Ask::Length(1200), "stretch".into())));

        let three = Clock { hour: 3, minute: 0, either: true };
        let local = |hour, minute, second| crate::wake::at_clock(1000, hour, minute, 0).unwrap() + second;
        assert_eq!(three.seconds_from(local(14, 0, 0)), 3600, "the afternoon comes first");
        assert_eq!(three.seconds_from(local(16, 0, 0)), 11 * 3600, "then the night");
        assert_eq!(three.seconds_from(local(3, 0, 0)), 12 * 3600, "never now");
        let exact = Clock { hour: 15, minute: 30, either: false };
        assert_eq!(exact.seconds_from(local(15, 29, 30)), 30);
        assert_eq!(exact.seconds_from(local(15, 30, 0)), 24 * 3600);
    }

    #[test]
    fn understands_plain_sentences() {
        let start = |text: &str| {
            let row = understood(text, 1000)?;
            let crate::plugins::ActionKind::Tool { name, arguments } = row.actions[0].kind.clone() else { return None };
            assert_eq!(name, "start");
            Some((row.title, arguments))
        };
        let timer = |seconds: u64, label: &str| json!({ "duration": format!("{seconds}s"), "label": label });
        let said = |text: &str| start(text).map(|(_, arguments)| arguments);
        assert_eq!(said("set a timer for 10 minutes"), Some(timer(600, "")));
        assert_eq!(said("Set a timer of 10 minutes for tea"), Some(timer(600, "Tea")));
        assert_eq!(said("timer for 5 min tea"), Some(timer(300, "Tea")));
        assert_eq!(said("please start a countdown 90s"), Some(timer(90, "")));
        assert_eq!(said("10 minute timer"), Some(timer(600, "")));
        assert_eq!(said("5 min timer for eggs"), Some(timer(300, "Eggs")));
        assert_eq!(
            said("remind me to stretch in 20 minutes"),
            Some(json!({ "duration": "1200s", "label": "Stretch", "reminder": true }))
        );
        assert_eq!(start("remind me in 1 hour").map(|(title, _)| title), Some("Remind me in 1 hour".into()));
        // A time of day goes to the plugin as the 24-hour time it resolves to.
        let (title, arguments) = start("remind me at 15:30 to call the dentist").unwrap();
        assert_eq!(title, "Remind me at 15:30");
        assert_eq!(arguments, json!({ "at": "15:30", "label": "Call the dentist" }));
        assert_eq!(
            start("plan an alarm at 07:05"),
            Some(("Set an alarm for 07:05".into(), json!({ "at": "07:05", "label": "" })))
        );
        assert_eq!(
            start("set an alarm in 10 minutes"),
            Some(("Set an alarm in 10 minutes".into(), json!({ "duration": "600s", "label": "", "reminder": true })))
        );
        assert_eq!(said("wake me up at 06:30"), Some(json!({ "at": "06:30", "label": "Wake up" })));
        // Searches and this plugin's own prefix are left alone.
        for other in ["timer", "timer 10m", "alarm", "set", "set a timer", "remind me", "firefox", "10 minutes", "time"]
        {
            assert!(understood(other, 1000).is_none(), "{other:?}");
        }
    }

    #[test]
    fn alarms_say_when_and_cannot_be_paused() {
        let mut plugin = TimersPlugin::new(&["--no-save".into()]);
        plugin.call("start", &json!({ "at": "15:30", "label": "call the dentist" })).unwrap();
        plugin.call("start", &json!({ "duration": "20m", "label": "stretch", "reminder": true })).unwrap();
        assert!(plugin.call("start", &json!({})).is_err());
        assert!(plugin.call("pause", &json!({ "timer": 1 })).is_err());
        plugin.call("add", &json!({ "timer": 2 })).unwrap();
        let timers = plugin.shared.lock();
        let alarm = &timers.timers[0];
        assert!(alarm.alarm && timers.timers[1].alarm);
        assert!((1..=24 * 3600).contains(&alarm.total));
        assert!(alarm.detail(unix_now()).starts_with("Rings at 15:30"));
        assert_eq!(alarm.rang(), "Call the dentist");
        let launcher: crate::panel::block::Launcher = serde_json::from_value(timers.cards(unix_now())).unwrap();
        assert!(
            launcher
                .cards
                .iter()
                .all(|card| card.buttons.len() == 2 && !card.buttons.iter().any(|button| button.primary))
        );
        let rows = crate::plugins::parse_rows(timers.rows("", unix_now())).unwrap();
        assert_eq!(rows[1].actions.len(), 2);
    }

    #[test]
    fn says_durations_and_readings() {
        assert_eq!(spoken(600), "10 minutes");
        assert_eq!(spoken(5400), "1 hour 30 minutes");
        assert_eq!(spoken(61), "1 minute 1 second");
        assert_eq!(reading(161), "2:41");
        assert_eq!(reading(5), "0:05");
        assert_eq!(reading(3725), "1h02");
        assert_eq!(titled("  green   tea "), "Green tea");
    }

    #[test]
    fn counts_down_pauses_and_rings() {
        let mut timers = Timers::default();
        let key = timers.start(600, "tea", false, 1000).unwrap().key;
        assert!(timers.start(0, "", false, 1000).is_err());
        assert_eq!(timers.timers[0].left(1100), 500);
        assert_eq!(timers.tick(1100), (Vec::new(), false));

        // Paused, time stands still and nothing needs redrawing.
        let timer = timers.find(&json!(key)).unwrap();
        timer.phase = Phase::Paused(timer.left(1100));
        assert!(!timers.ticking());
        assert_eq!(timers.timers[0].left(5000), 500);
        assert!(timers.find(&json!("TEA")).is_ok() && timers.find(&json!("coffee")).is_err());
        timers.timers[0].phase = Phase::Running(5000 + 500);

        let (rang, changed) = timers.tick(5500);
        assert!(changed);
        assert_eq!(rang.len(), 1);
        assert_eq!(timers.timers[0].phase, Phase::Rang(5500));
        // It rings once, keeps its card for a while and then goes.
        assert_eq!(timers.tick(5501), (Vec::new(), false));
        assert_eq!(timers.tick(5500 + KEEP), (Vec::new(), true));
        assert!(timers.timers.is_empty());
    }

    #[test]
    fn a_timer_that_ended_meanwhile_rang_then() {
        let mut timers = Timers::default();
        timers.start(60, "", false, 1000).unwrap();
        let (rang, _) = timers.tick(9000);
        assert_eq!(rang[0].phase, Phase::Rang(1060));
        let kept: Timers = serde_json::from_slice(&serde_json::to_vec(&timers).unwrap()).unwrap();
        assert_eq!(kept, timers);
    }

    #[test]
    fn cards_and_rows_are_what_the_launcher_reads() {
        let mut timers = Timers::default();
        timers.start(600, "tea", false, 1000).unwrap();
        timers.start(60, "", false, 1000).unwrap();
        timers.timers[0].phase = Phase::Paused(450);
        timers.start(30, "eggs", false, 1000).unwrap();
        timers.tick(1030);

        let launcher: crate::panel::block::Launcher = serde_json::from_value(timers.cards(1030)).unwrap();
        let titles: Vec<&str> = launcher.cards.iter().map(|card| card.title.as_str()).collect();
        assert_eq!(titles, ["Eggs", "Timer", "Tea"]);
        let [eggs, timer, tea] = &launcher.cards[..] else { panic!("three cards") };
        assert_eq!((eggs.value.as_str(), eggs.buttons.len()), ("0:00", 1));
        assert!((eggs.progress.unwrap().value - 100.0).abs() < 1e-9);
        assert_eq!((timer.value.as_str(), timer.buttons.len()), ("0:30", 3));
        assert!((timer.progress.unwrap().value - 50.0).abs() < 1e-9);
        assert_eq!(tea.value, "7:30");
        assert_eq!(tea.buttons[1].icon.as_deref(), Some("glyph:play"));
        assert!(
            launcher
                .cards
                .iter()
                .flat_map(|card| &card.buttons)
                .all(|button| button.enabled && button.action.is_some())
        );

        let rows = crate::plugins::parse_rows(timers.rows("5m", 1030)).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].title, "Start a timer of 5 minutes");
        assert_eq!(
            rows[0].actions[0].kind,
            crate::plugins::ActionKind::Tool {
                name: "start".into(),
                arguments: json!({ "duration": "300s", "label": "" })
            }
        );
        assert_eq!(rows[1].title, "Eggs is done");
        assert_eq!(rows[2].actions.len(), 3);
        // Nothing usable typed: a hint without an action.
        assert!(crate::plugins::parse_rows(timers.rows("soon", 1030)).unwrap()[0].actions.is_empty());
    }

    #[test]
    fn tools_start_change_and_stop_timers() {
        let mut plugin = TimersPlugin::new(&["--no-save".into()]);
        assert!(plugin.shared.path.is_none());
        assert!(plugin.call("start", &json!({ "duration": "soon" })).is_err());
        plugin.call("start", &json!({ "duration": "10m tea" })).unwrap();
        plugin.call("start", &json!({ "duration": "5m", "label": "eggs" })).unwrap();
        assert!(plugin.call("pause", &json!({})).is_err(), "which of the two is not said");
        plugin.call("pause", &json!({ "timer": "tea" })).unwrap();
        plugin.call("add", &json!({ "timer": "tea" })).unwrap();
        {
            let timers = plugin.shared.lock();
            assert_eq!(timers.timers[0].total, 660);
            assert!(matches!(timers.timers[0].phase, Phase::Paused(left) if (659..=660).contains(&left)));
        }
        plugin.call("stop", &json!({ "timer": 2 })).unwrap();
        let listed = plugin.call("list", &json!({})).unwrap();
        assert!(listed["text"].as_str().unwrap().starts_with("1 Tea: "));
        assert_eq!(plugin.read(LAUNCHER).unwrap()["cards"].as_array().unwrap().len(), 1);
    }
}
