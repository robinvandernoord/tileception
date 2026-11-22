//! The three notification modes: Sound (banners and sound), Silent (banners
//! only, the default) and Do not disturb (neither, though critical banners
//! such as alarms still show). Each lasts until it is switched off, until a
//! time, or until Tileception restarts. A mode with an end goes back to the
//! one before it once the time comes. The mode is kept in the state folder,
//! so it survives a restart unless it was chosen "until restart".

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::{Tileception, sound, ui::glyph, wake};

/// `[notifications]`: sounds, and the times the mode menu offers.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    /// The family of sounds in the Sound mode.
    pub sounds: sound::Family,
    /// How loud, in percent.
    pub volume: u8,
    /// Whether the built-in alarms ring in the Silent mode too.
    pub alarms_in_silent: bool,
    /// The time "Until tomorrow morning" ends, such as "09:00".
    pub morning: String,
    /// The time "Until this evening" ends, such as "18:00".
    pub evening: String,
    /// Another family, or "off", by desktop entry or app name.
    pub app_sounds: BTreeMap<String, sound::AppSound>,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        NotificationsConfig {
            sounds: sound::Family::default(),
            volume: 70,
            alarms_in_silent: false,
            morning: "09:00".into(),
            evening: "18:00".into(),
            app_sounds: BTreeMap::new(),
        }
    }
}

impl NotificationsConfig {
    pub fn morning(&self) -> (u32, u32) {
        wake::clock(self.morning.trim()).unwrap_or((9, 0))
    }

    pub fn evening(&self) -> (u32, u32) {
        wake::clock(self.evening.trim()).unwrap_or((18, 0))
    }

    /// The sound an app gets: its exception, by desktop entry or else by
    /// name, or the family.
    pub fn app_sound(&self, apps: &[&str]) -> sound::AppSound {
        apps.iter()
            .filter(|app| !app.is_empty())
            .find_map(|app| self.app_sounds.get(*app).copied())
            .unwrap_or(self.sounds.into())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Sound,
    #[default]
    Silent,
    #[serde(rename = "dnd")]
    DoNotDisturb,
}

impl Mode {
    /// In the order the switch shows them.
    pub const ALL: [Mode; 3] = [Mode::Sound, Mode::Silent, Mode::DoNotDisturb];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Sound => "Sound",
            Mode::Silent => "Silent",
            Mode::DoNotDisturb => "Do not disturb",
        }
    }

    /// What the mode does, under the switch.
    pub fn summary(self) -> &'static str {
        match self {
            Mode::Sound => "Banners and sound",
            Mode::Silent => "Banners, no sound",
            Mode::DoNotDisturb => "No banners, no sound",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Mode::Sound => glyph::BELL,
            Mode::Silent => glyph::BELL_OFF,
            Mode::DoNotDisturb => glyph::MOON,
        }
    }

    /// The one after it on the switch, round to the first.
    pub fn next(self) -> Mode {
        match self {
            Mode::Sound => Mode::Silent,
            Mode::Silent => Mode::DoNotDisturb,
            Mode::DoNotDisturb => Mode::Sound,
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "sound" => Some(Mode::Sound),
            "silent" => Some(Mode::Silent),
            "dnd" | "do-not-disturb" => Some(Mode::DoNotDisturb),
            _ => None,
        }
    }
}

/// When a mode ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// When another mode is chosen, also after a restart.
    Off,
    /// When Tileception starts again.
    Restart,
    /// At this time, in seconds since 1970.
    Time(i64),
}

/// How long a mode is asked for, before it is worked out from the clock: a
/// preset, a typed time or a command's argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Until {
    Off,
    Restart,
    /// This many minutes from now.
    After(u32),
    /// The next time the clock shows this, or that time tomorrow.
    Clock {
        hour: u32,
        minute: u32,
        tomorrow: bool,
    },
    /// The next `[notifications] morning`.
    Morning,
    /// This evening's `[notifications] evening`, or tomorrow's.
    Evening {
        tomorrow: bool,
    },
}

impl Until {
    /// Reads a typed end: "45m", "1h30", "18:00", "6pm", "9", "tomorrow 9",
    /// "evening", "tomorrow", "restart" (or the start of it).
    pub fn parse(text: &str) -> Option<Until> {
        let text = text.trim().to_ascii_lowercase();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return None;
        }
        if text.len() >= 2 && "restart".starts_with(&text) {
            return Some(Until::Restart);
        }
        if let Some(rest) = text.strip_prefix('+').or_else(|| text.strip_prefix("in ")) {
            return wake::duration(rest.trim()).map(Until::After);
        }
        let (tomorrow, rest) = match ["tomorrow", "tmrw", "tom"].iter().find_map(|word| text.strip_prefix(word)) {
            Some(rest) if rest.is_empty() || rest.starts_with(' ') => (true, rest.trim()),
            _ => (false, text.as_str()),
        };
        let rest = rest.strip_prefix("at ").unwrap_or(rest).trim();
        match rest {
            // Only "tomorrow" leaves nothing.
            "" | "morning" => return Some(Until::Morning),
            "evening" | "tonight" => return Some(Until::Evening { tomorrow }),
            _ => {}
        }
        // A time of day has a colon or a.m./p.m.; letters for hours or
        // minutes make a length of time.
        let half_day = ["am", "pm", "a.m.", "p.m.", "a", "p"].iter().any(|suffix| rest.ends_with(suffix));
        if !half_day && !rest.contains(':') && rest.contains(['h', 'm']) {
            return if tomorrow { None } else { wake::duration(rest).map(Until::After) };
        }
        wake::clock(rest).map(|(hour, minute)| Until::Clock { hour, minute, tomorrow })
    }

    /// When it ends, from `now`.
    pub fn resolve(self, now: i64, config: &NotificationsConfig) -> Option<End> {
        let next = |(hour, minute): (u32, u32), tomorrow: bool| {
            let today = wake::at_clock(now, hour, minute, 0)?;
            if today > now && !tomorrow { Some(today) } else { wake::at_clock(now, hour, minute, 1) }
        };
        let when = match self {
            Until::Off => return Some(End::Off),
            Until::Restart => return Some(End::Restart),
            Until::After(minutes) => now + i64::from(minutes) * 60,
            Until::Clock { hour, minute, tomorrow } => next((hour, minute), tomorrow)?,
            Until::Morning => next(config.morning(), false)?,
            Until::Evening { tomorrow } => next(config.evening(), tomorrow)?,
        };
        Some(End::Time(when))
    }
}

/// The mode, when it ends, and what comes back then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeState {
    pub mode: Mode,
    pub end: End,
    /// The last mode chosen without an end.
    pub before: Mode,
}

impl Default for ModeState {
    fn default() -> Self {
        ModeState { mode: Mode::default(), end: End::Off, before: Mode::default() }
    }
}

impl ModeState {
    /// Switches to `mode` until `end`. One with an end goes back to the last
    /// mode without, also when another with an end came between.
    pub fn set(&mut self, mode: Mode, end: End) {
        let settled = if self.end == End::Off { self.mode } else { self.before };
        *self = ModeState { mode, end, before: if end == End::Off { mode } else { settled } };
    }

    /// Keeps the mode without its end.
    pub fn clear_end(&mut self) {
        self.set(self.mode, End::Off);
    }

    /// Ends the mode if its time has come; whether it did.
    pub fn expire(&mut self, now: i64) -> bool {
        match self.end {
            End::Time(when) if when <= now => {
                *self = ModeState { mode: self.before, end: End::Off, before: self.before };
                true
            }
            _ => false,
        }
    }

    /// Do not disturb switched off: back to the mode before it, or Silent.
    pub fn without_dnd(&self) -> Mode {
        if self.mode != Mode::DoNotDisturb {
            self.mode
        } else if self.before != Mode::DoNotDisturb {
            self.before
        } else {
            Mode::Silent
        }
    }
}

/// The state file as written.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    mode: Mode,
    /// When a mode with an end ends, in seconds since 1970.
    #[serde(skip_serializing_if = "Option::is_none")]
    until: Option<i64>,
    before: Mode,
}

impl Saved {
    /// What a restart finds: a mode "until restart" is over by then.
    fn from_state(state: &ModeState) -> Saved {
        match state.end {
            End::Off => Saved { mode: state.mode, until: None, before: state.mode },
            End::Restart => Saved { mode: state.before, until: None, before: state.before },
            End::Time(when) => Saved { mode: state.mode, until: Some(when), before: state.before },
        }
    }

    fn into_state(self, now: i64) -> ModeState {
        let end = self.until.map_or(End::Off, End::Time);
        let before = if end == End::Off { self.mode } else { self.before };
        let mut state = ModeState { mode: self.mode, end, before };
        state.expire(now);
        state
    }
}

fn state_file() -> Option<PathBuf> {
    let root = std::env::var_os("XDG_STATE_HOME")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state")))?;
    Some(root.join("tileception/notification-mode.toml"))
}

pub fn load(now: i64) -> ModeState {
    let Some(path) = state_file() else { return ModeState::default() };
    let Ok(text) = std::fs::read_to_string(&path) else { return ModeState::default() };
    match toml::from_str::<Saved>(&text) {
        Ok(saved) => saved.into_state(now),
        Err(error) => {
            tracing::warn!("could not read {}: {error}", path.display());
            ModeState::default()
        }
    }
}

fn save(state: &ModeState) {
    let Some(path) = state_file() else { return };
    let result = (|| {
        let text = toml::to_string(&Saved::from_state(state)).map_err(|error| error.to_string())?;
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
        }
        let partial = path.with_extension("toml.part");
        std::fs::write(&partial, text).map_err(|error| error.to_string())?;
        std::fs::rename(&partial, &path).map_err(|error| error.to_string())
    })();
    if let Err(error) = result {
        tracing::warn!("could not save the notification mode: {error}");
    }
}

/// "until 18:00", "until tomorrow 09:00", "until restart"; nothing without an end.
pub fn describe(end: End, now: i64) -> String {
    match end {
        End::Off => String::new(),
        End::Restart => "until restart".into(),
        End::Time(when) => {
            let (Some(then), Some(today)) = (wake::local(when), wake::local(now)) else { return String::new() };
            let days = if then.tm_year == today.tm_year {
                then.tm_yday - today.tm_yday
            } else {
                // Over New Year: tomorrow when it is less than two days on.
                if when - now < 2 * 24 * 3600 { 1 } else { 2 }
            };
            let clock = format!("{:02}:{:02}", then.tm_hour, then.tm_min);
            match days {
                0 => format!("until {clock}"),
                1 => format!("until tomorrow {clock}"),
                _ => {
                    const MONTHS: [&str; 12] =
                        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
                    let month = MONTHS[then.tm_mon.clamp(0, 11) as usize];
                    format!("until {} {month} {clock}", then.tm_mday)
                }
            }
        }
    }
}

/// "45 min", "3 h", "3 h 28 min".
pub fn left(seconds: i64) -> String {
    let minutes = (seconds.max(0) + 59) / 60;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// A row of the menu under a mode's arrow.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub until: Until,
    pub label: String,
    /// The time it ends, at the right, for those that end at a time.
    pub clock: Option<String>,
}

/// The menu's rows, then "Other time..." after them.
pub fn presets(now: i64, config: &NotificationsConfig) -> Vec<Preset> {
    let clock_of = |until: Until| match until.resolve(now, config) {
        Some(End::Time(when)) => wake::clock_at(when).map(|(hour, minute)| format!("{hour:02}:{minute:02}")),
        _ => None,
    };
    let same_day = |until: Until| match until.resolve(now, config) {
        Some(End::Time(when)) => {
            wake::local(when).zip(wake::local(now)).is_some_and(|(then, today)| then.tm_yday == today.tm_yday)
        }
        _ => false,
    };
    let evening = Until::Evening { tomorrow: false };
    let rows = [
        (Until::Off, "Until you turn it off".to_string()),
        (Until::After(60), "1 hour".to_string()),
        (evening, if same_day(evening) { "Until this evening" } else { "Until tomorrow evening" }.to_string()),
        (
            Until::Morning,
            if same_day(Until::Morning) { "Until this morning" } else { "Until tomorrow morning" }.to_string(),
        ),
        (Until::Restart, "Until restart".to_string()),
    ];
    rows.into_iter().map(|(until, label)| Preset { until, label, clock: clock_of(until) }).collect()
}

/// How often a mode's end is checked: it ends within this much of its time.
const CHECK: Duration = Duration::from_secs(15);

impl Tileception {
    /// Reads the mode kept from the last session and starts checking for
    /// its end. Called once the service runs.
    pub(crate) fn start_notification_mode(&mut self) {
        self.notifications.mode = load(wake::now());
        save(&self.notifications.mode);
        self.apply_notification_mode();
        if self.notifications.mode_timer.is_none() {
            let timer = self.loop_handle.insert_source(Timer::from_duration(CHECK), |_, _, state| {
                state.check_notification_mode();
                TimeoutAction::ToDuration(CHECK)
            });
            match timer {
                Ok(token) => self.notifications.mode_timer = Some(token),
                Err(error) => tracing::error!("could not check the notification mode's end: {error}"),
            }
        }
    }

    pub(crate) fn stop_notification_mode(&mut self) {
        if let Some(token) = self.notifications.mode_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    pub fn notification_mode(&self) -> ModeState {
        self.notifications.mode
    }

    /// Switches the mode, for as long as `until` says.
    pub fn set_notification_mode(&mut self, mode: Mode, until: Until) {
        let Some(end) = until.resolve(wake::now(), &self.config.file.notifications) else {
            return tracing::warn!("could not work out when {until:?} is");
        };
        self.notifications.mode.set(mode, end);
        self.mode_changed();
    }

    /// The panel's x beside the end time: the mode stays, without an end.
    pub fn clear_notification_mode_end(&mut self) {
        self.notifications.mode.clear_end();
        self.mode_changed();
    }

    /// D in the panel and `notification-mode next`: the next mode, without an end.
    pub fn next_notification_mode(&mut self) {
        let next = self.notifications.mode.mode.next();
        self.set_notification_mode(next, Until::Off);
    }

    pub fn toggle_dnd(&mut self) {
        let dnd = self.notifications.mode.mode == Mode::DoNotDisturb;
        self.set_dnd(!dnd);
    }

    /// `do-not-disturb on` or `off`: off goes back to the mode before it.
    pub fn set_dnd(&mut self, enabled: bool) {
        let mode = if enabled { Mode::DoNotDisturb } else { self.notifications.mode.without_dnd() };
        self.set_notification_mode(mode, Until::Off);
    }

    fn check_notification_mode(&mut self) {
        if self.notifications.mode.expire(wake::now()) {
            self.mode_changed();
        }
    }

    fn mode_changed(&mut self) {
        save(&self.notifications.mode);
        self.apply_notification_mode();
        // What may still sound: everything in Sound, alarms in Silent if allowed.
        let silenced = match self.notifications.mode.mode {
            Mode::Sound => false,
            Mode::Silent => !self.config.file.notifications.alarms_in_silent,
            Mode::DoNotDisturb => true,
        };
        if silenced {
            self.notifications.sounds.stop_all();
        }
        self.schedule_render();
    }

    /// Tells the service whether banners are held back.
    fn apply_notification_mode(&mut self) {
        let dnd = self.notifications.mode.mode == Mode::DoNotDisturb;
        if let Some(service) = &self.notifications.service {
            service.send(tileception_notify::Request::SetDoNotDisturb(dnd));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_typed_ends() {
        assert_eq!(Until::parse("45m"), Some(Until::After(45)));
        assert_eq!(Until::parse("45 min"), Some(Until::After(45)));
        assert_eq!(Until::parse("2h"), Some(Until::After(120)));
        assert_eq!(Until::parse("1h30"), Some(Until::After(90)));
        assert_eq!(Until::parse("in 90"), Some(Until::After(90)));
        assert_eq!(Until::parse("18:00"), Some(Until::Clock { hour: 18, minute: 0, tomorrow: false }));
        assert_eq!(Until::parse("6pm"), Some(Until::Clock { hour: 18, minute: 0, tomorrow: false }));
        assert_eq!(Until::parse("9"), Some(Until::Clock { hour: 9, minute: 0, tomorrow: false }));
        assert_eq!(Until::parse("Tomorrow 9"), Some(Until::Clock { hour: 9, minute: 0, tomorrow: true }));
        assert_eq!(Until::parse("tomorrow"), Some(Until::Morning));
        assert_eq!(Until::parse("morning"), Some(Until::Morning));
        assert_eq!(Until::parse("tonight"), Some(Until::Evening { tomorrow: false }));
        assert_eq!(Until::parse("tomorrow evening"), Some(Until::Evening { tomorrow: true }));
        assert_eq!(Until::parse("re"), Some(Until::Restart));
        assert_eq!(Until::parse("restart"), Some(Until::Restart));
        for nonsense in ["", "r", "soon", "25:00", "tomorrow 45m", "tomorrowland"] {
            assert_eq!(Until::parse(nonsense), None, "{nonsense}");
        }
    }

    #[test]
    fn resolves_ends() {
        let config = NotificationsConfig::default();
        let now = wake::at_clock(1_800_000_000, 14, 30, 0).expect("local time");
        assert_eq!(Until::After(60).resolve(now, &config), Some(End::Time(now + 3600)));
        assert_eq!(Until::Evening { tomorrow: false }.resolve(now, &config), Some(End::Time(now + 12_600)));
        let morning = wake::at_clock(now, 9, 0, 1).expect("local time");
        assert_eq!(Until::Morning.resolve(now, &config), Some(End::Time(morning)));
        // A time that has passed today is tomorrow's.
        let noon = wake::at_clock(now, 12, 0, 1).expect("local time");
        assert_eq!(Until::Clock { hour: 12, minute: 0, tomorrow: false }.resolve(now, &config), Some(End::Time(noon)));
        assert_eq!(Until::Restart.resolve(now, &config), Some(End::Restart));
    }

    #[test]
    fn timed_modes_go_back() {
        let mut state = ModeState::default();
        state.set(Mode::Sound, End::Off);
        state.set(Mode::DoNotDisturb, End::Time(100));
        state.set(Mode::Silent, End::Time(200));
        assert_eq!(state.before, Mode::Sound);
        assert!(!state.expire(199));
        assert!(state.expire(200));
        assert_eq!(state, ModeState { mode: Mode::Sound, end: End::Off, before: Mode::Sound });
        state.set(Mode::DoNotDisturb, End::Time(300));
        state.clear_end();
        assert_eq!(state, ModeState { mode: Mode::DoNotDisturb, end: End::Off, before: Mode::DoNotDisturb });
        assert_eq!(state.without_dnd(), Mode::Silent);
    }

    #[test]
    fn restarts_find_the_right_mode() {
        let mut state = ModeState::default();
        state.set(Mode::Sound, End::Off);
        state.set(Mode::DoNotDisturb, End::Restart);
        let saved = toml::to_string(&Saved::from_state(&state)).expect("saved");
        let read: Saved = toml::from_str(&saved).expect("read");
        assert_eq!(read.into_state(0), ModeState { mode: Mode::Sound, end: End::Off, before: Mode::Sound });

        state.set(Mode::DoNotDisturb, End::Time(500));
        let read: Saved = toml::from_str(&toml::to_string(&Saved::from_state(&state)).expect("saved")).expect("read");
        assert_eq!(read.into_state(400), state);
        let read: Saved = toml::from_str(&toml::to_string(&Saved::from_state(&state)).expect("saved")).expect("read");
        assert_eq!(read.into_state(500).mode, Mode::Sound);
    }

    #[test]
    fn says_how_long() {
        assert_eq!(left(45 * 60), "45 min");
        assert_eq!(left(3 * 3600), "3 h");
        assert_eq!(left(3 * 3600 + 28 * 60), "3 h 28 min");
        assert_eq!(describe(End::Restart, 0), "until restart");
        assert_eq!(describe(End::Off, 0), "");
    }
}
