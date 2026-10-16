//! Volume, media keys and brightness: commands, key repeat and the state the
//! on-screen display and the notification panel draw from. The work itself
//! happens in `tileception-media`, off the compositor thread.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::reexports::calloop::{
    ping::make_ping,
    timer::{TimeoutAction, Timer},
    RegistrationToken,
};
use tileception_media::{Change, Event, MediaService, PlayerCommand, Request, Snapshot, Target, Toggle};

use crate::{commands::Command, Tileception};

/// The `[media]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Percent per volume key press.
    pub volume_step: u32,
    /// How far volume may be boosted, in percent (100 to 200).
    pub max_volume: u32,
    /// Percent per brightness key press.
    pub brightness_step: u32,
    /// Show a small card when volume, brightness or playback changes.
    pub osd: bool,
    /// How long that card stays, in milliseconds.
    pub osd_timeout_ms: u64,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self { volume_step: 5, max_volume: 150, brightness_step: 5, osd: true, osd_timeout_ms: 1500 }
    }
}

impl MediaConfig {
    pub fn max_volume(&self) -> u32 {
        self.max_volume.clamp(100, 200)
    }
}

/// A change to a level, as written in a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjust {
    /// Up or down by the given percent, or the configured step.
    Up(Option<u32>),
    Down(Option<u32>),
    Set(u32),
    Mute(Toggle),
}

/// What a level command acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Volume,
    Mic,
    Brightness,
}

/// A media command from a key binding or `tileception msg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Level(Level, Adjust),
    /// Makes the n-th output (or microphone), counting from 1, the default.
    Choose {
        output: bool,
        which: usize,
    },
    Player(PlayerCommand),
    /// Prints what is playing and the levels.
    Status,
}

impl Control {
    /// Whether holding the key down should repeat it.
    pub fn repeats(&self) -> bool {
        matches!(self, Control::Level(_, Adjust::Up(_) | Adjust::Down(_)))
    }
}

/// What the on-screen card shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsdKind {
    Volume,
    Mic,
    Brightness,
    Media,
}

#[derive(Debug, Clone, Copy)]
pub struct Osd {
    pub kind: OsdKind,
    pub shown: Instant,
    pub until: Instant,
}

/// Which slider in the notification panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliderId {
    Volume,
    Mic,
    Brightness,
    App(u32),
}

#[derive(Default)]
pub struct Media {
    service: Option<MediaService>,
    events: Option<mpsc::Receiver<Event>>,
    source: Option<RegistrationToken>,
    pub snapshot: Snapshot,
    pub osd: Option<Osd>,
    /// The key being held and its repeat timer.
    repeat: Option<(u32, RegistrationToken)>,
    /// The panel's list of programs is unfolded.
    pub apps_open: bool,
    /// The panel's list of devices to choose from is unfolded.
    pub devices_open: bool,
    /// A slider being dragged, when its value was last sent, and where it is.
    pub drag: Option<(SliderId, Instant, u32)>,
    /// Scrolling not yet worth a whole step.
    pub scrolled: f64,
    /// Cover art, cropped square, by file and size.
    pub art: std::cell::RefCell<std::collections::HashMap<(std::path::PathBuf, u32), Option<MemoryRenderBuffer>>>,
}

pub fn parse(verb: &str, args: &[&str]) -> Result<Control, String> {
    let level = match verb {
        "volume" => Some(Level::Volume),
        "mic" => Some(Level::Mic),
        "brightness" => Some(Level::Brightness),
        _ => None,
    };
    let percent = |arg: Option<&&str>| -> Result<Option<u32>, String> {
        arg.map(|a| a.trim_end_matches('%').parse::<u32>().map_err(|_| format!("{verb}: {a:?} is not a percentage")))
            .transpose()
    };
    if let Some(level) = level {
        if let (Some(&kind @ ("output" | "input")), Level::Volume | Level::Mic) = (args.first(), level) {
            let which = args
                .get(1)
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n > 0)
                .ok_or(format!("{verb} {kind} needs a device number from `tileception msg volume`"))?;
            return Ok(Control::Choose { output: kind == "output", which });
        }
        let adjust = match args.first().copied() {
            None if verb == "volume" => return Ok(Control::Status),
            Some("up" | "+") => Adjust::Up(percent(args.get(1))?),
            Some("down" | "-") => Adjust::Down(percent(args.get(1))?),
            Some("set") => Adjust::Set(percent(args.get(1))?.ok_or(format!("{verb} set needs a percentage"))?),
            Some("mute") if level != Level::Brightness => Adjust::Mute(match args.get(1).copied() {
                None | Some("toggle") => Toggle::Flip,
                Some("on") => Toggle::On,
                Some("off") => Toggle::Off,
                Some(other) => return Err(format!("{verb} mute takes on, off or toggle, not {other:?}")),
            }),
            Some(n) if n.trim_end_matches('%').parse::<u32>().is_ok() => Adjust::Set(percent(Some(&n))?.unwrap_or(0)),
            _ if level == Level::Brightness => return Err("brightness takes up, down, set or a percentage".into()),
            _ => return Err(format!("{verb} takes up, down, set, mute or a percentage")),
        };
        return Ok(Control::Level(level, adjust));
    }
    let command = match args.first().copied() {
        None | Some("status") => return Ok(Control::Status),
        Some("play-pause" | "toggle") => PlayerCommand::PlayPause,
        Some("play") => PlayerCommand::Play,
        Some("pause") => PlayerCommand::Pause,
        Some("next") => PlayerCommand::Next,
        Some("previous" | "prev") => PlayerCommand::Previous,
        Some("stop") => PlayerCommand::Stop,
        Some(other) => {
            return Err(format!("media takes play-pause, play, pause, next, previous or stop, not {other:?}"))
        }
    };
    Ok(Control::Player(command))
}

impl Tileception {
    /// Starts the workers. `bus` overrides the session bus, for tests.
    pub fn start_media(&mut self, bus: Option<String>) {
        let result = (|| {
            let (ping, source) = make_ping().map_err(|e| e.to_string())?;
            let (service, events) = MediaService::start(bus, move || ping.ping());
            let token = self
                .loop_handle
                .insert_source(source, |_, _, state| state.media_events())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((service, events, token))
        })();
        match result {
            Ok((service, events, token)) => {
                self.media.source = Some(token);
                self.media.snapshot = service.snapshot();
                self.media.service = Some(service);
                self.media.events = Some(events);
            }
            Err(error) => tracing::error!("could not start media controls: {error}"),
        }
    }

    /// Stops the workers; the controls disappear from the panel.
    pub fn stop_media(&mut self) {
        if let Some(token) = self.media.source.take() {
            self.loop_handle.remove(token);
        }
        self.stop_media_repeat();
        self.media.service = None;
        self.media.events = None;
        self.media.snapshot = Snapshot::default();
        self.media.osd = None;
        self.schedule_render();
    }

    fn media_events(&mut self) {
        let Some(events) = &self.media.events else { return };
        if events.try_iter().count() == 0 {
            return;
        }
        if let Some(service) = &self.media.service {
            self.media.snapshot = service.snapshot();
        }
        self.schedule_render();
    }

    pub fn media_request(&mut self, request: Request) {
        if let Some(service) = &self.media.service {
            service.send(request);
        }
    }

    /// Changes a level from the panel or a key.
    pub fn adjust(&mut self, level: Level, adjust: Adjust) {
        let config = &self.config.file.media;
        let step = |n: Option<u32>, default: u32| n.unwrap_or(default).max(1) as i32;
        let (volume_step, brightness_step, max) = (config.volume_step, config.brightness_step, config.max_volume());
        let change = |default: u32| match adjust {
            Adjust::Up(n) => Change::Step(step(n, default)),
            Adjust::Down(n) => Change::Step(-step(n, default)),
            Adjust::Set(n) => Change::Set(n),
            Adjust::Mute(toggle) => Change::Mute(toggle),
        };
        let request = match level {
            Level::Volume => Request::Volume { target: Target::Output, change: change(volume_step), max },
            // Microphones are not boosted.
            Level::Mic => Request::Volume { target: Target::Input, change: change(volume_step), max: 100 },
            Level::Brightness => Request::Brightness(change(brightness_step)),
        };
        self.media_request(request);
    }

    pub fn media_command(&mut self, control: Control) -> Result<String, String> {
        if !self.shell().media() {
            return Err("volume and media controls are switched off in [shell]".into());
        }
        match control {
            Control::Status => return Ok(self.media_status()),
            Control::Level(level, adjust) => {
                self.adjust(level, adjust);
                self.show_osd(match level {
                    Level::Volume => OsdKind::Volume,
                    Level::Mic => OsdKind::Mic,
                    Level::Brightness => OsdKind::Brightness,
                });
            }
            Control::Player(command) => {
                self.media_request(Request::Player(command));
                self.show_osd(OsdKind::Media);
            }
            Control::Choose { output, which } => {
                let audio = &self.media.snapshot.audio;
                let list = if output { &audio.outputs } else { &audio.inputs };
                let choice = list.get(which - 1).ok_or(format!("there is no device {which}"))?;
                let name = choice.name.clone();
                self.media_request(Request::Default { output, name });
                self.show_osd(if output { OsdKind::Volume } else { OsdKind::Mic });
            }
        }
        Ok("ok\n".into())
    }

    pub fn show_osd(&mut self, kind: OsdKind) {
        let config = &self.config.file.media;
        if !config.osd {
            return;
        }
        let now = Instant::now();
        // Pressing again keeps the card up rather than fading it in anew.
        let shown = match self.media.osd {
            Some(osd) if osd.kind == kind && osd.until > now => osd.shown,
            _ => now,
        };
        self.media.osd = Some(Osd { kind, shown, until: now + Duration::from_millis(config.osd_timeout_ms) });
        self.schedule_render();
    }

    fn media_status(&self) -> String {
        let s = &self.media.snapshot;
        let mut out = String::new();
        let level = |d: &Option<tileception_media::Device>| match d {
            Some(d) => format!("{}%{} ({})", d.percent, if d.muted { " muted" } else { "" }, d.description),
            None => "none".into(),
        };
        if s.audio.connected {
            out += &format!("volume: {}\nmic: {}\n", level(&s.audio.output), level(&s.audio.input));
            let current =
                |d: &Option<tileception_media::Device>, name: &str| d.as_ref().is_some_and(|d| d.name == name);
            for (i, c) in s.audio.outputs.iter().enumerate() {
                let mark = if current(&s.audio.output, &c.name) { "*" } else { " " };
                out += &format!("output {}{mark} {}\n", i + 1, c.description);
            }
            for (i, c) in s.audio.inputs.iter().enumerate() {
                let mark = if current(&s.audio.input, &c.name) { "*" } else { " " };
                out += &format!("input {}{mark} {}\n", i + 1, c.description);
            }
            for app in &s.audio.apps {
                out += &format!(
                    "app {}: {} {}%{} ({})\n",
                    app.index,
                    app.app,
                    app.percent,
                    if app.muted { " muted" } else { "" },
                    app.title
                );
            }
        } else {
            out += "volume: no sound server\n";
        }
        match &s.backlight {
            Some(b) => out += &format!("brightness: {}% ({})\n", b.percent, b.device),
            None => out += "brightness: none\n",
        }
        match s.media.active() {
            Some(p) => {
                out += &format!("player: {} {:?}: {}", p.identity, p.status, p.title);
                if !p.artist.is_empty() {
                    out += &format!(" by {}", p.artist);
                }
                out += "\n";
            }
            None => out += "player: none\n",
        }
        out
    }

    /// Repeats a held volume or brightness key, at the keyboard's repeat rate.
    pub fn start_media_repeat(&mut self, keycode: u32, commands: Vec<Command>) {
        self.stop_media_repeat();
        let k = &self.config.file.keyboard;
        let delay = Duration::from_millis(k.repeat_delay.unwrap_or(400).max(1) as u64);
        let interval = Duration::from_secs_f64(1.0 / f64::from(k.repeat_rate.unwrap_or(30).max(1)));
        let token = self.loop_handle.insert_source(Timer::from_duration(delay), move |_, _, state| {
            for command in &commands {
                if let Err(error) = state.execute(command.clone()) {
                    tracing::warn!("{error}");
                }
            }
            TimeoutAction::ToDuration(interval)
        });
        match token {
            Ok(token) => self.media.repeat = Some((keycode, token)),
            Err(error) => tracing::warn!("could not repeat the key: {error}"),
        }
    }

    pub fn stop_media_repeat(&mut self) {
        if let Some((_, token)) = self.media.repeat.take() {
            self.loop_handle.remove(token);
        }
    }

    pub fn media_key_released(&mut self, keycode: u32) {
        if self.media.repeat.is_some_and(|(held, _)| held == keycode) {
            self.stop_media_repeat();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_media_commands() {
        assert_eq!(parse("volume", &["up"]), Ok(Control::Level(Level::Volume, Adjust::Up(None))));
        assert_eq!(parse("volume", &["down", "10"]), Ok(Control::Level(Level::Volume, Adjust::Down(Some(10)))));
        assert_eq!(parse("volume", &["120%"]), Ok(Control::Level(Level::Volume, Adjust::Set(120))));
        assert_eq!(parse("volume", &["set", "40"]), Ok(Control::Level(Level::Volume, Adjust::Set(40))));
        assert_eq!(parse("volume", &["mute"]), Ok(Control::Level(Level::Volume, Adjust::Mute(Toggle::Flip))));
        assert_eq!(parse("mic", &["mute", "on"]), Ok(Control::Level(Level::Mic, Adjust::Mute(Toggle::On))));
        assert_eq!(parse("volume", &[]), Ok(Control::Status));
        assert_eq!(parse("brightness", &["up"]), Ok(Control::Level(Level::Brightness, Adjust::Up(None))));
        assert!(parse("brightness", &["mute"]).is_err());
        assert!(parse("volume", &["loud"]).is_err());
        assert_eq!(parse("media", &["play-pause"]), Ok(Control::Player(PlayerCommand::PlayPause)));
        assert_eq!(parse("media", &["prev"]), Ok(Control::Player(PlayerCommand::Previous)));
        assert_eq!(parse("media", &[]), Ok(Control::Status));
        assert!(parse("media", &["rewind"]).is_err());
        assert_eq!(parse("volume", &["output", "2"]), Ok(Control::Choose { output: true, which: 2 }));
        assert_eq!(parse("mic", &["input", "1"]), Ok(Control::Choose { output: false, which: 1 }));
        assert!(parse("volume", &["output", "0"]).is_err());
        assert!(parse("brightness", &["output", "1"]).is_err());
        assert!(Control::Level(Level::Volume, Adjust::Up(None)).repeats());
        assert!(!Control::Level(Level::Volume, Adjust::Mute(Toggle::Flip)).repeats());
    }
}
