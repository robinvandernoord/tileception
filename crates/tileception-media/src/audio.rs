//! Volume through the PulseAudio protocol, which PipeWire also serves.
//!
//! Two connections: one only listens for change events, the other sends
//! requests and reads the state back after each change.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pulseaudio::protocol::{self, ChannelVolume, Command, CommandReply, Prop, Props, Volume};

use crate::{changed, lock, Audio, Change, Choice, Device, Event, Notifier, Percent, Shared, Stream, Target};

/// 100%, `PA_VOLUME_NORM`.
const NORM: u64 = 0x10000;

enum Msg {
    Request(Target, Change, Percent),
    /// Make this output (true) or microphone the default.
    Choose(bool, String),
    /// The server reported a change.
    Dirty,
    /// The event connection closed.
    Lost,
    Stop,
}

pub struct Worker {
    messages: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start(shared: Shared, notifier: Notifier) -> Self {
        let (messages, incoming) = mpsc::channel();
        let sender = messages.clone();
        let thread = thread::Builder::new()
            .name("tileception-audio".into())
            .spawn(move || run(&shared, &notifier, &sender, &incoming))
            .map_err(|e| tracing::error!("could not start the audio worker: {e}"))
            .ok();
        Self { messages, thread }
    }

    pub fn send(&self, target: Target, change: Change, max: Percent) {
        let _ = self.messages.send(Msg::Request(target, change, max));
    }

    pub fn choose(&self, output: bool, name: String) {
        let _ = self.messages.send(Msg::Choose(output, name));
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.messages.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared, notifier: &Notifier, sender: &mpsc::Sender<Msg>, incoming: &mpsc::Receiver<Msg>) {
    let mut backoff = Duration::from_secs(1);
    let mut reported = false;
    loop {
        match Session::open(sender.clone()) {
            Ok(mut session) => {
                backoff = Duration::from_secs(1);
                reported = false;
                let stopped = session.serve(shared, notifier, incoming);
                session.close();
                if stopped {
                    return;
                }
                tracing::info!("lost the sound server; reconnecting");
            }
            Err(error) if !reported => {
                tracing::warn!("no sound server: {error}");
                reported = true;
            }
            Err(_) => {}
        }
        {
            let mut snapshot = lock(shared);
            if snapshot.audio != Audio::default() {
                snapshot.audio = Audio::default();
                drop(snapshot);
                notifier.send(Event::Audio);
            }
        }
        // Wait before trying again, but stop at once if asked.
        let until = Instant::now() + backoff;
        loop {
            match incoming.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Msg::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

/// One authenticated connection.
struct Connection {
    socket: BufReader<UnixStream>,
    version: u16,
    seq: u32,
}

impl Connection {
    fn open(name: &CStr) -> Result<Self, String> {
        let path = pulseaudio::socket_path_from_env().ok_or("no PulseAudio socket")?;
        let stream = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cookie = pulseaudio::cookie_path_from_env().and_then(|path| std::fs::read(path).ok()).unwrap_or_default();
        let mut connection = Self { socket: BufReader::new(stream), version: protocol::MAX_VERSION, seq: 0 };
        let auth =
            protocol::AuthParams { version: protocol::MAX_VERSION, supports_shm: false, supports_memfd: false, cookie };
        let reply: protocol::AuthReply = connection.request(Command::Auth(auth))?;
        connection.version = protocol::MAX_VERSION.min(reply.version);
        let mut props = Props::new();
        props.set(Prop::ApplicationName, name);
        props.set(Prop::ApplicationId, c"space.tileception.Tileception");
        let _: protocol::SetClientNameReply = connection.request(Command::SetClientName(props))?;
        Ok(connection)
    }

    fn write(&mut self, command: &Command) -> Result<(), String> {
        self.seq = self.seq.wrapping_add(1);
        protocol::write_command_message(self.socket.get_mut(), self.seq, command, self.version)
            .map_err(|e| e.to_string())
    }

    fn request<T: CommandReply>(&mut self, command: Command) -> Result<T, String> {
        self.write(&command)?;
        let (_, reply) =
            protocol::read_reply_message::<T>(&mut self.socket, self.version).map_err(|e| e.to_string())?;
        Ok(reply)
    }

    fn ack(&mut self, command: Command) -> Result<(), String> {
        self.write(&command)?;
        protocol::read_ack_message(&mut self.socket).map(|_| ()).map_err(|e| e.to_string())
    }
}

/// What a request needs that the snapshot leaves out.
#[derive(Default)]
struct Known {
    output: Option<Level>,
    input: Option<Level>,
    streams: HashMap<u32, Level>,
}

#[derive(Clone, Copy)]
struct Level {
    index: u32,
    volume: ChannelVolume,
    muted: bool,
}

struct Session {
    requests: Connection,
    events: UnixStream,
    known: Known,
}

impl Session {
    fn open(sender: mpsc::Sender<Msg>) -> Result<Self, String> {
        let requests = Connection::open(c"Tileception")?;
        // A stuck sound server must not stall this worker (and whoever stops it).
        let timeout = Some(std::time::Duration::from_secs(5));
        let socket = requests.socket.get_ref();
        socket.set_read_timeout(timeout).and_then(|_| socket.set_write_timeout(timeout)).map_err(|e| e.to_string())?;
        let mut listener = Connection::open(c"Tileception events")?;
        let mask = protocol::SubscriptionMask::SINK
            | protocol::SubscriptionMask::SOURCE
            | protocol::SubscriptionMask::SINK_INPUT
            | protocol::SubscriptionMask::SERVER;
        listener.ack(Command::Subscribe(mask))?;
        let events = listener.socket.get_ref().try_clone().map_err(|e| e.to_string())?;
        let version = listener.version;
        thread::Builder::new()
            .name("tileception-audio-events".into())
            .spawn(move || {
                let mut socket = listener.socket;
                loop {
                    match protocol::read_command_message(&mut socket, version) {
                        Ok((_, Command::SubscribeEvent(_))) => {
                            if sender.send(Msg::Dirty).is_err() {
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => {
                            let _ = sender.send(Msg::Lost);
                            return;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { requests, events, known: Known::default() })
    }

    fn close(&self) {
        let _ = self.events.shutdown(Shutdown::Both);
    }

    /// Handles messages until the connection fails (false) or a stop (true).
    fn serve(&mut self, shared: &Shared, notifier: &Notifier, incoming: &mpsc::Receiver<Msg>) -> bool {
        if self.refresh(shared, notifier).is_err() {
            return false;
        }
        loop {
            let Ok(first) = incoming.recv() else { return true };
            let mut dirty = false;
            // Take everything queued, so a burst of events reads the state once.
            for msg in std::iter::once(first).chain(std::iter::from_fn(|| incoming.try_recv().ok())) {
                match msg {
                    Msg::Stop => return true,
                    Msg::Lost => return false,
                    Msg::Dirty => dirty = true,
                    Msg::Request(target, change, max) => {
                        if let Err(error) = self.apply(target, change, max) {
                            tracing::warn!("volume change failed: {error}");
                        }
                        // Read the result back before the next one, so steps
                        // and toggles in a burst build on each other.
                        if self.refresh(shared, notifier).is_err() {
                            return false;
                        }
                    }
                    Msg::Choose(output, name) => {
                        if let Err(error) = self.choose(output, &name) {
                            tracing::warn!("could not switch to {name}: {error}");
                        }
                        dirty = true;
                    }
                }
            }
            if dirty && self.refresh(shared, notifier).is_err() {
                return false;
            }
        }
    }

    fn refresh(&mut self, shared: &Shared, notifier: &Notifier) -> Result<(), String> {
        let server: protocol::ServerInfo = self.requests.request(Command::GetServerInfo)?;
        let sinks: protocol::SinkInfoList = self.requests.request(Command::GetSinkInfoList)?;
        let sources: protocol::SourceInfoList = self.requests.request(Command::GetSourceInfoList)?;
        let inputs: protocol::SinkInputInfoList = self.requests.request(Command::GetSinkInputInfoList)?;

        let mut known = Known::default();
        let mut audio = Audio { connected: true, ..Audio::default() };
        if let Some(sink) = sinks.iter().find(|s| Some(&s.name) == server.default_sink_name.as_ref()) {
            known.output = Some(Level { index: sink.index, volume: sink.cvolume, muted: sink.muted });
            audio.output = Some(device(&sink.name, sink.description.as_deref(), &sink.cvolume, sink.muted));
        }
        let source = sources
            .iter()
            .find(|s| Some(&s.name) == server.default_source_name.as_ref())
            .filter(|s| s.monitor_of_sink_index.is_none());
        if let Some(source) = source {
            known.input = Some(Level { index: source.index, volume: source.cvolume, muted: source.muted });
            audio.input = Some(device(&source.name, source.description.as_deref(), &source.cvolume, source.muted));
        }
        audio.outputs = sinks.iter().map(|s| choice(&s.name, s.description.as_deref())).collect();
        audio.inputs = sources
            .iter()
            .filter(|s| s.monitor_of_sink_index.is_none())
            .map(|s| choice(&s.name, s.description.as_deref()))
            .collect();
        for input in inputs.iter().filter(|i| i.has_volume && i.volume_writable) {
            known.streams.insert(input.index, Level { index: input.index, volume: input.cvolume, muted: input.muted });
            let app = prop(&input.props, Prop::ApplicationName)
                .or_else(|| prop(&input.props, Prop::ApplicationProcessBinary))
                .unwrap_or_else(|| text(&input.name));
            audio.apps.push(Stream {
                index: input.index,
                app,
                icon: prop(&input.props, Prop::ApplicationIconName),
                binary: prop(&input.props, Prop::ApplicationProcessBinary),
                title: prop(&input.props, Prop::MediaName).unwrap_or_else(|| text(&input.name)),
                percent: percent(&input.cvolume),
                muted: input.muted,
            });
        }
        self.known = known;

        let mut snapshot = lock(shared);
        if snapshot.audio != audio {
            snapshot.audio = audio;
            drop(snapshot);
            notifier.send(Event::Audio);
        }
        Ok(())
    }

    fn apply(&mut self, target: Target, change: Change, max: Percent) -> Result<(), String> {
        let level = match target {
            Target::Output => self.known.output.ok_or("no audio output")?,
            Target::Input => self.known.input.ok_or("no microphone")?,
            Target::App(index) => *self.known.streams.get(&index).ok_or("that program stopped playing")?,
        };
        if let Change::Mute(toggle) = change {
            return self.mute(target, level.index, toggle.apply(level.muted));
        }
        // Turning the volume up also unmutes, as it does on most desktops.
        if level.muted && matches!(change, Change::Step(s) if s > 0) {
            self.mute(target, level.index, false)?;
        }
        let current = percent(&level.volume);
        let next = changed(current, change, max);
        if next == current {
            return Ok(());
        }
        let volume = scaled(&level.volume, next);
        let index = level.index;
        self.requests.ack(match target {
            Target::Output => Command::SetSinkVolume(device_volume(index, volume)),
            Target::Input => Command::SetSourceVolume(device_volume(index, volume)),
            Target::App(_) => Command::SetSinkInputVolume(protocol::SetStreamVolumeParams { index, volume }),
        })
    }

    /// Switches the default device and brings the streams along, as most
    /// desktops do, so the change is heard at once.
    fn choose(&mut self, output: bool, name: &str) -> Result<(), String> {
        let name = CString::new(name).map_err(|e| e.to_string())?;
        if output {
            self.requests.ack(Command::SetDefaultSink(name.clone()))?;
            let streams: protocol::SinkInputInfoList = self.requests.request(Command::GetSinkInputInfoList)?;
            for stream in streams {
                if let Err(error) = self.requests.ack(Command::MoveSinkInput(mover(stream.index, &name))) {
                    tracing::debug!("stream {} stays: {error}", stream.index);
                }
            }
        } else {
            self.requests.ack(Command::SetDefaultSource(name.clone()))?;
            let streams: protocol::SourceOutputInfoList = self.requests.request(Command::GetSourceOutputInfoList)?;
            for stream in streams {
                if let Err(error) = self.requests.ack(Command::MoveSourceOutput(mover(stream.index, &name))) {
                    tracing::debug!("recording {} stays: {error}", stream.index);
                }
            }
        }
        Ok(())
    }

    fn mute(&mut self, target: Target, index: u32, mute: bool) -> Result<(), String> {
        self.requests.ack(match target {
            Target::Output => Command::SetSinkMute(device_mute(index, mute)),
            Target::Input => Command::SetSourceMute(device_mute(index, mute)),
            Target::App(_) => Command::SetSinkInputMute(protocol::SetStreamMuteParams { index, mute }),
        })
    }
}

fn mover(index: u32, device: &CStr) -> protocol::MoveStreamParams {
    protocol::MoveStreamParams { index: Some(index), device_index: None, device_name: Some(device.to_owned()) }
}

fn choice(name: &CStr, description: Option<&CStr>) -> Choice {
    Choice { name: text(name), description: description.map(text).unwrap_or_else(|| text(name)) }
}

fn device_mute(index: u32, mute: bool) -> protocol::SetDeviceMuteParams {
    protocol::SetDeviceMuteParams { device_index: Some(index), device_name: None, mute }
}

fn device_volume(index: u32, volume: ChannelVolume) -> protocol::SetDeviceVolumeParams {
    protocol::SetDeviceVolumeParams { device_index: Some(index), device_name: None, volume }
}

fn device(name: &CStr, description: Option<&CStr>, volume: &ChannelVolume, muted: bool) -> Device {
    Device {
        name: text(name),
        description: description.map(text).unwrap_or_else(|| text(name)),
        percent: percent(volume),
        muted,
    }
}

fn text(s: &CStr) -> String {
    s.to_string_lossy().into_owned()
}

fn prop(props: &Props, key: Prop) -> Option<String> {
    let bytes = props.get(key)?;
    let s = CStr::from_bytes_until_nul(bytes).map(text).unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned());
    Some(s).filter(|s| !s.is_empty())
}

/// The loudest channel, as pavucontrol and `wpctl` show it.
fn percent(volume: &ChannelVolume) -> Percent {
    let loudest = volume.channels().iter().map(|v| u64::from(v.as_u32())).max().unwrap_or(0);
    ((loudest * 100 + NORM / 2) / NORM) as Percent
}

/// The same balance between channels, with the loudest at `target` percent.
fn scaled(volume: &ChannelVolume, target: Percent) -> ChannelVolume {
    let want = u64::from(target) * NORM / 100;
    let loudest = volume.channels().iter().map(|v| u64::from(v.as_u32())).max().unwrap_or(0);
    let mut out = ChannelVolume::empty();
    for v in volume.channels() {
        let raw = (u64::from(v.as_u32()) * want).checked_div(loudest).unwrap_or(want);
        out.push(Volume::from_u32_clamped(u32::try_from(raw).unwrap_or(u32::MAX)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(left: u32, right: u32) -> ChannelVolume {
        let mut v = ChannelVolume::empty();
        v.push(Volume::from_u32_clamped(left));
        v.push(Volume::from_u32_clamped(right));
        v
    }

    #[test]
    fn percent_is_linear_on_the_loudest_channel() {
        assert_eq!(percent(&stereo(0x10000, 0x8000)), 100);
        assert_eq!(percent(&stereo(0x8000, 0x8000)), 50);
        assert_eq!(percent(&stereo(0x18000, 0)), 150);
        assert_eq!(percent(&ChannelVolume::empty()), 0);
    }

    #[test]
    fn scaling_keeps_the_balance() {
        let v = scaled(&stereo(0x10000, 0x8000), 50);
        assert_eq!(v.channels()[0].as_u32(), 0x8000);
        assert_eq!(v.channels()[1].as_u32(), 0x4000);
        let v = scaled(&stereo(0, 0), 120);
        assert_eq!(percent(&v), 120);
        assert_eq!(v.channels()[0], v.channels()[1]);
    }
}
