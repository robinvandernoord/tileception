//! The `timers` plugin: `timer 10m tea` in the launcher starts a countdown.
//! Each timer is a card under the launcher with the time left, a progress
//! bar and buttons to pause, add a minute and stop; a notification says
//! when it is done. Timers are kept in the state folder, so they survive
//! the plugin starting again, and ring late when they ended meanwhile.

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
        if self.label.is_empty() { "Timer" } else { &self.label }
    }

    /// "10 minutes, rings at 14:32", as its card and row say.
    fn detail(&self, now: i64) -> String {
        match self.phase {
            // Between its end and the next tick it is as good as done.
            Phase::Running(ends) if ends <= now => format!("{}, done", spoken(self.total)),
            Phase::Running(ends) => format!("{}, rings at {}", spoken(self.total), clock(ends)),
            Phase::Paused(_) => format!("{}, paused", spoken(self.total)),
            Phase::Rang(when) if now - when < 60 => format!("{}, done", spoken(self.total)),
            Phase::Rang(when) => format!("{}, done at {}", spoken(self.total), clock(when)),
        }
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
    fn start(&mut self, seconds: u64, label: &str, now: i64) -> Result<&Timer, String> {
        if !(1..=LONGEST).contains(&seconds) {
            return Err("A timer runs from a second to a day".into());
        }
        if self.timers.len() >= MOST {
            return Err(format!("There are {MOST} timers already; stop one first"));
        }
        self.next += 1;
        let ends = now + i64::try_from(seconds).unwrap_or(i64::MAX);
        self.timers.push(Timer { key: self.next, label: titled(label), total: seconds, phase: Phase::Running(ends) });
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
            Some((seconds, label)) => {
                let label = titled(&label);
                let ends = now + i64::try_from(seconds).unwrap_or(0);
                let detail = if label.is_empty() {
                    format!("Rings at {}", clock(ends))
                } else {
                    format!("{label}, rings at {}", clock(ends))
                };
                rows.push(json!({
                    "title": format!("Start a timer of {}", spoken(seconds)),
                    "detail": detail,
                    "icon": "glyph:clock",
                    "actions": [{ "title": "Start", "type": "tool", "name": "start",
                                  "arguments": { "duration": format!("{seconds}s"), "label": label } }],
                }));
            }
            None => rows.push(json!({
                "title": if query.trim().is_empty() { "Start a timer" } else { "How long?" },
                "detail": "Type how long and what for, such as 10m tea, 1h30m or 2:30",
                "icon": "glyph:clock",
            })),
        }
        for timer in self.sorted(now) {
            let hold = match timer.phase {
                Phase::Running(_) => Some("Pause"),
                Phase::Paused(_) => Some("Resume"),
                Phase::Rang(_) => None,
            };
            let mut actions = Vec::new();
            if let Some(hold) = hold {
                actions.push(act(hold, "pause", timer.key, None));
                actions.push(act("Add a minute", "add", timer.key, Some(60)));
            }
            actions.push(act(if hold.is_some() { "Stop" } else { "Dismiss" }, "stop", timer.key, None));
            let title = match timer.phase {
                Phase::Rang(_) => format!("{} is done", timer.name()),
                _ => format!("{}: {} left", timer.name(), reading(timer.left(now))),
            };
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
    let (tint, buttons) = match timer.phase {
        Phase::Running(_) => (
            "accent",
            vec![
                button("+1 min", None, false, tool("add", Some(60))),
                button("Pause", Some("glyph:pause"), true, tool("pause", None)),
                button("Stop", Some("glyph:close"), false, tool("stop", None)),
            ],
        ),
        Phase::Paused(_) => (
            "muted",
            vec![
                button("+1 min", None, false, tool("add", Some(60))),
                button("Resume", Some("glyph:play"), true, tool("pause", None)),
                button("Stop", Some("glyph:close"), false, tool("stop", None)),
            ],
        ),
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

/// How long and what for, from what is typed: the duration comes first or
/// last ("10m tea", "tea 10m", "for 1 hour 30 minutes"), a bare number is
/// minutes, and "2:30" is minutes and seconds.
fn parse(query: &str) -> Option<(u64, String)> {
    let mut words: Vec<&str> = query.split_whitespace().collect();
    if words.first().is_some_and(|word| word.eq_ignore_ascii_case("for")) {
        words.remove(0);
    }
    if let Some((seconds, used)) = duration(&words) {
        return Some((seconds, words[used..].join(" ")));
    }
    (1..words.len()).find_map(|from| {
        let (seconds, used) = duration(&words[from..])?;
        (from + used == words.len()).then(|| (seconds, words[..from].join(" ")))
    })
}

/// The duration at the start of the words, and how many words it takes.
fn duration(words: &[&str]) -> Option<(u64, usize)> {
    let first = words.first()?;
    if first.contains(':') {
        let parts: Vec<u64> = first.split(':').map(str::parse).collect::<Result<_, _>>().ok()?;
        let seconds = match parts[..] {
            [minutes, seconds] if seconds < 60 => minutes * 60 + seconds,
            [hours, minutes, seconds] if minutes < 60 && seconds < 60 => hours * 3600 + minutes * 60 + seconds,
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
        *bus = zbus::blocking::Connection::session().map_err(|error| eprintln!("timers: no session bus: {error}")).ok();
    }
    let Some(session) = bus.as_ref() else { return };
    let hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> =
        std::collections::HashMap::from([("urgency", zbus::zvariant::Value::U8(2))]);
    let summary = format!("{} is done", timer.name());
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
            { "name": "start", "description": "Start a timer",
              "inputSchema": schema(json!({
                  "duration": { "type": "string", "description": "How long, such as 10m, 1h30m, 90s or 2:30" },
                  "label": { "type": "string", "description": "What it is for" } }), &["duration"]) },
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
                let asked = arguments["duration"].as_str().unwrap_or_default();
                let (seconds, rest) = parse(asked).ok_or("Say how long, such as 10m, 1h30m, 90s or 2:30")?;
                let label = arguments["label"].as_str().map_or(rest, String::from);
                let timer = timers.start(seconds, &label, now)?;
                format!("{}: {}", timer.name(), timer.detail(now))
            }
            "pause" => {
                let timer = timers.find(&arguments["timer"])?;
                timer.phase = match timer.phase {
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
        let parsed = |query: &str| parse(query).map(|(seconds, label)| (seconds, label.to_string()));
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
        let key = timers.start(600, "tea", 1000).unwrap().key;
        assert!(timers.start(0, "", 1000).is_err());
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
        timers.start(60, "", 1000).unwrap();
        let (rang, _) = timers.tick(9000);
        assert_eq!(rang[0].phase, Phase::Rang(1060));
        let kept: Timers = serde_json::from_slice(&serde_json::to_vec(&timers).unwrap()).unwrap();
        assert_eq!(kept, timers);
    }

    #[test]
    fn cards_and_rows_are_what_the_launcher_reads() {
        let mut timers = Timers::default();
        timers.start(600, "tea", 1000).unwrap();
        timers.start(60, "", 1000).unwrap();
        timers.timers[0].phase = Phase::Paused(450);
        timers.start(30, "eggs", 1000).unwrap();
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
