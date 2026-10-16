//! Volume, media player and backlight control, off the compositor thread.
//!
//! Three workers run on their own threads: audio (the PulseAudio protocol,
//! served by PipeWire on most desktops), media players (MPRIS on the session
//! bus) and the display backlight (sysfs, written through logind). Each one
//! keeps its part of a shared [`Snapshot`] current and reports changes as
//! [`Event`]s; each fails on its own, so a missing backlight only disables
//! brightness.

mod art;
mod audio;
mod backlight;
mod mpris;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

pub use backlight::pick_device;

/// Volume and brightness are in percent; 100 is the normal, unamplified volume.
pub type Percent = u32;

/// An audio device: the default output or the default microphone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub description: String,
    pub percent: Percent,
    pub muted: bool,
}

/// A program playing sound.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    pub index: u32,
    pub app: String,
    pub icon: Option<String>,
    pub binary: Option<String>,
    pub title: String,
    pub percent: Percent,
    pub muted: bool,
}

/// A device that could become the default, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Audio {
    /// Whether the sound server answers at all.
    pub connected: bool,
    pub output: Option<Device>,
    pub input: Option<Device>,
    pub apps: Vec<Stream>,
    /// Every output and microphone, to choose the default from.
    pub outputs: Vec<Choice>,
    pub inputs: Vec<Choice>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    #[default]
    Stopped,
}

/// A media player on the session bus.
#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    pub bus_name: String,
    pub identity: String,
    pub desktop_entry: Option<String>,
    pub status: Status,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Cover art, when the player gives a local file.
    pub art: Option<PathBuf>,
    pub length: Option<Duration>,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_pause: bool,
    pub can_play: bool,
    position: Duration,
    position_at: Instant,
    rate: f64,
}

impl Player {
    /// The playback position now, carried forward from the last report.
    pub fn position(&self, now: Instant) -> Duration {
        if self.status != Status::Playing {
            return self.position;
        }
        // Players report their own rate; keep it sane so the sum stays finite.
        let rate = if self.rate.is_finite() { self.rate.clamp(0.0, 64.0) } else { 1.0 };
        let ran = now.saturating_duration_since(self.position_at).as_secs_f64() * rate;
        let at = self.position.saturating_add(Duration::try_from_secs_f64(ran).unwrap_or_default());
        self.length.map_or(at, |length| at.min(length))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    /// Whether the session bus could be reached.
    pub connected: bool,
    pub players: Vec<Player>,
    /// The player that media keys control.
    pub active: Option<usize>,
}

impl Media {
    pub fn active(&self) -> Option<&Player> {
        self.active.and_then(|i| self.players.get(i))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backlight {
    pub device: String,
    pub percent: Percent,
}

/// Everything the workers know, as last reported.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub audio: Audio,
    pub media: Media,
    /// `None` when no adjustable backlight was found.
    pub backlight: Option<Backlight>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Audio,
    Media,
    Backlight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    On,
    Off,
    Flip,
}

impl Toggle {
    pub fn apply(self, current: bool) -> bool {
        match self {
            Toggle::On => true,
            Toggle::Off => false,
            Toggle::Flip => !current,
        }
    }
}

/// A change to a volume or brightness level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Up or down by this many percent, landing on multiples of it.
    Step(i32),
    Set(Percent),
    Mute(Toggle),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Output,
    Input,
    /// A program's stream, by [`Stream::index`].
    App(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerCommand {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `max` caps how far a volume may go above 100%.
    Volume {
        target: Target,
        change: Change,
        max: Percent,
    },
    /// Makes a device the default and moves what plays (or records) to it.
    /// `output` picks speakers over microphones.
    Default {
        output: bool,
        name: String,
    },
    Player(PlayerCommand),
    Brightness(Change),
}

/// The level after a change, clamped to `0..=max`. Steps land on multiples of
/// the step, so 37 up by 5 gives 40.
pub fn changed(current: Percent, change: Change, max: Percent) -> Percent {
    match change {
        Change::Set(value) => value.min(max),
        Change::Mute(_) => current,
        Change::Step(0) => current,
        Change::Step(step) => {
            let size = step.unsigned_abs();
            let next =
                if step > 0 { (current / size + 1) * size } else { current.div_ceil(size).saturating_sub(1) * size };
            // Never step up past a level that was already above the cap.
            if step > 0 && current >= max { current } else { next.min(max) }
        }
    }
}

type Shared = Arc<Mutex<Snapshot>>;

fn lock(shared: &Mutex<Snapshot>) -> MutexGuard<'_, Snapshot> {
    // A worker that panicked mid-update leaves a usable snapshot behind.
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Reports a change from a worker.
#[derive(Clone)]
struct Notifier {
    events: mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Notifier {
    fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            (self.wake)();
        }
    }
}

/// Owns the workers. Dropping it stops them.
pub struct MediaService {
    shared: Shared,
    audio: audio::Worker,
    mpris: mpris::Worker,
    backlight: backlight::Worker,
}

impl MediaService {
    /// Starts the workers. `bus_address` overrides the session bus, for tests.
    pub fn start(
        bus_address: Option<String>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> (Self, mpsc::Receiver<Event>) {
        let (events, receiver) = mpsc::channel();
        let notifier = Notifier { events, wake: Arc::new(wake) };
        let shared: Shared = Arc::default();
        let audio = audio::Worker::start(shared.clone(), notifier.clone());
        let mpris = mpris::Worker::start(shared.clone(), notifier.clone(), bus_address);
        let backlight = backlight::Worker::start(shared.clone(), notifier);
        (Self { shared, audio, mpris, backlight }, receiver)
    }

    pub fn snapshot(&self) -> Snapshot {
        lock(&self.shared).clone()
    }

    pub fn send(&self, request: Request) {
        match request {
            Request::Volume { target, change, max } => self.audio.send(target, change, max),
            Request::Default { output, name } => self.audio.choose(output, name),
            Request::Player(command) => self.mpris.send(command),
            Request::Brightness(change) => self.backlight.send(change),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_land_on_multiples() {
        assert_eq!(changed(37, Change::Step(5), 150), 40);
        assert_eq!(changed(40, Change::Step(5), 150), 45);
        assert_eq!(changed(37, Change::Step(-5), 150), 35);
        assert_eq!(changed(35, Change::Step(-5), 150), 30);
        assert_eq!(changed(3, Change::Step(-5), 150), 0);
        assert_eq!(changed(0, Change::Step(-5), 150), 0);
    }

    #[test]
    fn levels_respect_the_cap() {
        assert_eq!(changed(98, Change::Step(5), 100), 100);
        assert_eq!(changed(100, Change::Step(5), 100), 100);
        assert_eq!(changed(148, Change::Step(5), 150), 150);
        assert_eq!(changed(200, Change::Set(180), 150), 150);
        // Set elsewhere above the cap: up stays, down comes back in steps.
        assert_eq!(changed(160, Change::Step(5), 150), 160);
        assert_eq!(changed(160, Change::Step(-5), 150), 150);
    }

    #[test]
    fn position_runs_while_playing() {
        let start = Instant::now();
        let mut player = Player {
            bus_name: String::new(),
            identity: String::new(),
            desktop_entry: None,
            status: Status::Playing,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            art: None,
            length: Some(Duration::from_secs(10)),
            can_next: false,
            can_previous: false,
            can_pause: false,
            can_play: false,
            position: Duration::from_secs(2),
            position_at: start,
            rate: 1.0,
        };
        assert_eq!(player.position(start + Duration::from_secs(3)), Duration::from_secs(5));
        assert_eq!(player.position(start + Duration::from_secs(30)), Duration::from_secs(10));
        player.status = Status::Paused;
        assert_eq!(player.position(start + Duration::from_secs(3)), Duration::from_secs(2));
    }
}
