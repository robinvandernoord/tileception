//! Notification sounds, played in the Sound mode: a family (Vibes, Knock
//! or Breath) has a sound for low, normal and critical notifications, and
//! an alarm that loops while a built-in alarm's notification shows. The
//! sounds are built in (see `sounds/README.md`) and played on a thread of
//! their own, which opens the sound device only while something plays.

use std::{
    collections::HashMap,
    sync::mpsc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    #[default]
    Vibes,
    Knock,
    Breath,
}

impl Family {
    pub const ALL: [Family; 3] = [Family::Vibes, Family::Knock, Family::Breath];

    pub fn name(self) -> &'static str {
        match self {
            Family::Vibes => "Vibes",
            Family::Knock => "Knock",
            Family::Breath => "Breath",
        }
    }
}

/// What an app sounds like: a family, or nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppSound {
    Vibes,
    Knock,
    Breath,
    Off,
}

impl AppSound {
    pub fn family(self) -> Option<Family> {
        match self {
            AppSound::Vibes => Some(Family::Vibes),
            AppSound::Knock => Some(Family::Knock),
            AppSound::Breath => Some(Family::Breath),
            AppSound::Off => None,
        }
    }

    /// As written in `[notifications.app_sounds]`.
    pub fn key(self) -> &'static str {
        match self {
            AppSound::Vibes => "vibes",
            AppSound::Knock => "knock",
            AppSound::Breath => "breath",
            AppSound::Off => "off",
        }
    }
}

impl From<Family> for AppSound {
    fn from(family: Family) -> Self {
        match family {
            Family::Vibes => AppSound::Vibes,
            Family::Knock => AppSound::Knock,
            Family::Breath => AppSound::Breath,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Clip {
    Low,
    Normal,
    Critical,
    /// Loops.
    Alarm,
}

impl Clip {
    pub fn for_urgency(urgency: tileception_notify::Urgency) -> Clip {
        match urgency {
            tileception_notify::Urgency::Low => Clip::Low,
            tileception_notify::Urgency::Normal => Clip::Normal,
            tileception_notify::Urgency::Critical => Clip::Critical,
        }
    }
}

fn ogg(family: Family, clip: Clip) -> &'static [u8] {
    match (family, clip) {
        (Family::Vibes, Clip::Low) => include_bytes!("../sounds/vibes_low.ogg"),
        (Family::Vibes, Clip::Normal) => include_bytes!("../sounds/vibes_notify.ogg"),
        (Family::Vibes, Clip::Critical) => include_bytes!("../sounds/vibes_critical.ogg"),
        (Family::Vibes, Clip::Alarm) => include_bytes!("../sounds/vibes_alarm.ogg"),
        (Family::Knock, Clip::Low) => include_bytes!("../sounds/knock_low.ogg"),
        (Family::Knock, Clip::Normal) => include_bytes!("../sounds/knock_notify.ogg"),
        (Family::Knock, Clip::Critical) => include_bytes!("../sounds/knock_critical.ogg"),
        (Family::Knock, Clip::Alarm) => include_bytes!("../sounds/knock_alarm.ogg"),
        (Family::Breath, Clip::Low) => include_bytes!("../sounds/breath_low.ogg"),
        (Family::Breath, Clip::Normal) => include_bytes!("../sounds/breath_notify.ogg"),
        (Family::Breath, Clip::Critical) => include_bytes!("../sounds/breath_critical.ogg"),
        (Family::Breath, Clip::Alarm) => include_bytes!("../sounds/breath_alarm.ogg"),
    }
}

/// The longest an alarm rings without being dismissed.
const RING_FOR: Duration = Duration::from_secs(120);

/// How long the sound device stays open after the last sound, so sounds
/// close together do not each open it.
const KEEP_OPEN: Duration = Duration::from_secs(10);

enum Command {
    Play {
        family: Family,
        clip: Clip,
        volume: f32,
    },
    /// Loops the family's alarm until stopped, as `key`.
    Ring {
        key: u32,
        family: Family,
        volume: f32,
    },
    /// Plays a sound once, in place of the last preview.
    Preview {
        family: Family,
        clip: Clip,
        volume: f32,
    },
    Stop(u32),
    StopAll,
}

/// The key previews play under: ids of notifications never reach it.
const PREVIEW: u32 = u32::MAX;

/// The way to the sound thread, which starts with the first sound.
#[derive(Default)]
pub struct Sounds {
    sender: Option<mpsc::Sender<Command>>,
}

impl Sounds {
    /// Plays one sound; `volume` goes from 0 to 1.
    pub fn play(&mut self, family: Family, clip: Clip, volume: f32) {
        self.send(Command::Play { family, clip, volume });
    }

    /// Loops the family's alarm until [`Sounds::stop`] with the same key.
    pub fn ring(&mut self, key: u32, family: Family, volume: f32) {
        self.send(Command::Ring { key, family, volume });
    }

    /// Plays a sound once to hear it, whatever the mode, stopping the last
    /// preview. An alarm plays one round.
    pub fn preview(&mut self, family: Family, clip: Clip, volume: f32) {
        self.send(Command::Preview { family, clip, volume });
    }

    pub fn stop_preview(&mut self) {
        self.stop(PREVIEW);
    }

    pub fn stop(&mut self, key: u32) {
        if self.sender.is_some() {
            self.send(Command::Stop(key));
        }
    }

    pub fn stop_all(&mut self) {
        if self.sender.is_some() {
            self.send(Command::StopAll);
        }
    }

    fn send(&mut self, command: Command) {
        let command = match &self.sender {
            Some(sender) => match sender.send(command) {
                Ok(()) => return,
                // The thread ended (it should not); a new one takes over.
                Err(mpsc::SendError(command)) => command,
            },
            None => command,
        };
        let (sender, receiver) = mpsc::channel();
        let started = std::thread::Builder::new().name("tileception-sound".into()).spawn(move || run(receiver));
        if let Err(error) = started {
            self.sender = None;
            return tracing::warn!("could not start playing sounds: {error}");
        }
        let _ = sender.send(command);
        self.sender = Some(sender);
    }
}

/// The sound thread: plays what it is sent, and closes the sound device
/// once nothing has played for a while.
fn run(receiver: mpsc::Receiver<Command>) {
    let mut device: Option<rodio::MixerDeviceSink> = None;
    let mut playing: Vec<(Option<u32>, rodio::Player)> = Vec::new();
    let mut decoded: HashMap<(Family, Clip), Option<rodio::buffer::SamplesBuffer>> = HashMap::new();
    let mut quiet_since = Instant::now();
    loop {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(Command::Stop(key)) => playing.retain(|(ringing, _)| *ringing != Some(key)),
            Ok(Command::StopAll) => playing.clear(),
            Ok(command @ (Command::Play { .. } | Command::Ring { .. } | Command::Preview { .. })) => {
                let (key, family, clip, volume) = match command {
                    Command::Play { family, clip, volume } => (None, family, clip, volume),
                    Command::Ring { key, family, volume } => (Some(key), family, Clip::Alarm, volume),
                    Command::Preview { family, clip, volume } => (Some(PREVIEW), family, clip, volume),
                    Command::Stop(_) | Command::StopAll => unreachable!("matched above"),
                };
                if key == Some(PREVIEW) {
                    playing.retain(|(playing_key, _)| *playing_key != key);
                } else if key.is_some() && playing.iter().any(|(ringing, _)| *ringing == key) {
                    continue;
                }
                let Some(samples) = decoded.entry((family, clip)).or_insert_with(|| decode(family, clip)).clone()
                else {
                    continue;
                };
                if device.is_none() {
                    device = match rodio::DeviceSinkBuilder::open_default_sink() {
                        Ok(mut opened) => {
                            opened.log_on_drop(false);
                            Some(opened)
                        }
                        Err(error) => {
                            tracing::warn!("could not open the sound device: {error}");
                            continue;
                        }
                    };
                }
                let Some(opened) = &device else { continue };
                let player = rodio::Player::connect_new(opened.mixer());
                player.set_volume(volume.clamp(0.0, 1.0));
                if key.is_some() && key != Some(PREVIEW) {
                    use rodio::Source;
                    player.append(samples.repeat_infinite().take_duration(RING_FOR));
                } else {
                    player.append(samples);
                }
                playing.push((key, player));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        playing.retain(|(_, player)| !player.empty());
        if !playing.is_empty() {
            quiet_since = Instant::now();
        } else if device.is_some() && quiet_since.elapsed() >= KEEP_OPEN {
            device = None;
        }
    }
}

/// How long a sound plays (an alarm: one round).
pub fn length(family: Family, clip: Clip) -> Duration {
    use rodio::Source;
    static LENGTHS: std::sync::Mutex<Option<HashMap<(Family, Clip), Duration>>> = std::sync::Mutex::new(None);
    let mut lengths = LENGTHS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    *lengths.get_or_insert_with(HashMap::new).entry((family, clip)).or_insert_with(|| {
        decode(family, clip).and_then(|samples| samples.total_duration()).unwrap_or(Duration::from_secs(1))
    })
}

/// The sound, decoded once and kept.
fn decode(family: Family, clip: Clip) -> Option<rodio::buffer::SamplesBuffer> {
    use rodio::Source;
    match rodio::Decoder::new(std::io::Cursor::new(ogg(family, clip))) {
        Ok(decoder) => {
            let (channels, rate) = (decoder.channels(), decoder.sample_rate());
            Some(rodio::buffer::SamplesBuffer::new(channels, rate, decoder.collect::<Vec<_>>()))
        }
        Err(error) => {
            tracing::warn!("could not read the {family:?} {clip:?} sound: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_decodes() {
        for family in Family::ALL {
            for clip in [Clip::Low, Clip::Normal, Clip::Critical, Clip::Alarm] {
                let samples = decode(family, clip).unwrap_or_else(|| panic!("{family:?} {clip:?}"));
                use rodio::Source;
                let length = samples.total_duration().expect("a buffer knows its length");
                assert!(length > Duration::from_millis(100), "{family:?} {clip:?} {length:?}");
            }
        }
    }
}
