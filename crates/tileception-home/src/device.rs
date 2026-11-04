//! One device's connection, on its own thread: connect, negotiate a
//! session on 3.4 or 3.5, ask for the state, then keep the connection open with
//! heartbeats and pass on what the device reports. A change is shown at
//! once and confirmed by the device's report; without one it is asked
//! for again.

use std::{
    io::{ErrorKind, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Map, Value, json};

use crate::{
    Reach, Shared,
    model::{Change, Model, data_points},
    protocol::{Codec, Frame, Side, Version, command},
    store::DeviceEntry,
};

/// Tuya devices listen here.
pub const PORT: u16 = 6668;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a read waits before the loop looks at requests again.
const POLL: Duration = Duration::from_millis(200);
const HEARTBEAT: Duration = Duration::from_secs(10);
/// Silence this long means the connection is gone.
const SILENCE: Duration = Duration::from_secs(30);
/// A change not confirmed this soon is asked for again.
const CONFIRM: Duration = Duration::from_secs(3);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A connection's frames, over a socket.
pub(crate) struct Link {
    stream: TcpStream,
    codec: Codec,
    buffer: Vec<u8>,
}

impl Link {
    pub(crate) fn connect(address: SocketAddr, version: Version, key: [u8; 16]) -> Result<Link, String> {
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(|err| err.to_string())?;
        let _ = stream.set_nodelay(true);
        let mut link = Link { stream, codec: Codec::new(version, Side::Client, key), buffer: Vec::new() };
        if matches!(version, Version::V34 | Version::V35) {
            let (nonce, start) = link.codec.session_start();
            link.write(&start)?;
            let deadline = Instant::now() + CONNECT_TIMEOUT;
            let reply = loop {
                match link.read(POLL)? {
                    Some(frame) if frame.command == command::SESSION_REPLY => break frame,
                    Some(_) => {}
                    None if Instant::now() > deadline => return Err("no handshake reply".into()),
                    None => {}
                }
            };
            let (finish, session) = link.codec.session_finish(&nonce, &reply.payload).map_err(|err| err.to_string())?;
            link.write(&finish)?;
            link.codec.set_session_key(session);
        }
        Ok(link)
    }

    pub(crate) fn send(&mut self, command: u32, payload: &[u8]) -> Result<(), String> {
        let message = self.codec.encode(command, payload);
        self.write(&message)
    }

    fn write(&mut self, message: &[u8]) -> Result<(), String> {
        self.stream.write_all(message).map_err(|err| err.to_string())
    }

    /// The next frame, or `None` after waiting `timeout` for one.
    pub(crate) fn read(&mut self, timeout: Duration) -> Result<Option<Frame>, String> {
        loop {
            match self.codec.decode(&self.buffer) {
                Ok(Some((frame, used))) => {
                    self.buffer.drain(..used);
                    return Ok(Some(frame));
                }
                Ok(None) => {}
                Err(err) => return Err(err.to_string()),
            }
            self.stream.set_read_timeout(Some(timeout)).map_err(|err| err.to_string())?;
            let mut chunk = [0; 2048];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("connection closed".into()),
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(None),
                Err(err) => return Err(err.to_string()),
            }
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// The JSON a command carries, by version.
pub(crate) struct Payloads<'a> {
    pub device_id: &'a str,
    pub version: Version,
    /// Devices with 22-character ids that ask for their state with
    /// CONTROL_NEW and a list of data points.
    pub device22: bool,
}

impl Payloads<'_> {
    pub(crate) fn query(&self, model_ids: &[String]) -> (u32, Vec<u8>) {
        let device = self.device_id;
        let time = now_secs().to_string();
        match self.version {
            Version::V34 | Version::V35 => (command::QUERY_NEW, b"{}".to_vec()),
            Version::V33 if self.device22 => {
                let dps: Map<String, Value> = model_ids.iter().map(|point| (point.clone(), Value::Null)).collect();
                let body = json!({"devId": device, "uid": device, "t": time, "dps": dps});
                (command::CONTROL_NEW, body.to_string().into_bytes())
            }
            Version::V33 => {
                let body = json!({"gwId": device, "devId": device, "uid": device, "t": time});
                (command::QUERY, body.to_string().into_bytes())
            }
        }
    }

    pub(crate) fn control(&self, dps: &Map<String, Value>) -> (u32, Vec<u8>) {
        let device = self.device_id;
        match self.version {
            Version::V34 | Version::V35 => {
                let body = json!({"protocol": 5, "t": now_secs(), "data": {"dps": dps}});
                (command::CONTROL_NEW, body.to_string().into_bytes())
            }
            Version::V33 => {
                let body = json!({"devId": device, "uid": device, "t": now_secs().to_string(), "dps": dps});
                (command::CONTROL, body.to_string().into_bytes())
            }
        }
    }

    pub(crate) fn heartbeat(&self) -> (u32, Vec<u8>) {
        let device = self.device_id;
        (command::HEARTBEAT, json!({"gwId": device, "devId": device}).to_string().into_bytes())
    }
}

/// The data points in a status payload: `{"dps": ...}`, or on 3.4 and 3.5
/// `{"data": {"dps": ...}}`.
pub(crate) fn reported_dps(payload: &[u8]) -> Option<Map<String, Value>> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let dps = value.get("dps").or_else(|| value.get("data").and_then(|data| data.get("dps")))?;
    dps.as_object().cloned()
}

/// What a worker needs to find its device.
pub(crate) struct Target {
    /// The device as imported: its id, model and data point mapping.
    pub entry: DeviceEntry,
    pub model: Model,
    pub key: [u8; 16],
    pub slot: usize,
}

pub(crate) struct Worker {
    pub requests: mpsc::Sender<Change>,
    pub _thread: Option<std::thread::JoinHandle<()>>,
}

pub(crate) fn start(target: Target, shared: Shared, stop: Arc<AtomicBool>) -> Worker {
    let (requests, receiver) = mpsc::channel();
    let name = format!("home-{}", target.slot);
    let thread = std::thread::Builder::new().name(name).spawn(move || run(target, shared, receiver, stop)).ok();
    Worker { requests, _thread: thread }
}

fn run(target: Target, shared: Shared, requests: mpsc::Receiver<Change>, stop: Arc<AtomicBool>) {
    let mut failures = 0u32;
    let mut down_since: Option<SystemTime> = None;
    let mut flags = Flags { device22: target.entry.device_id.len() == 22, answered: false };
    // With no version known, cycle the guesses until one answers.
    let guesses = [Version::V33, Version::V35, Version::V34];
    let mut guess = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let Some((host, version)) = shared.address(&target.entry) else {
            shared.set_reach(
                target.slot,
                Reach::Unreachable {
                    since: *down_since.get_or_insert_with(SystemTime::now),
                    why: "not found on the network".into(),
                },
            );
            idle(&requests, &stop, Duration::from_secs(5));
            continue;
        };
        let version = version.unwrap_or_else(|| {
            let version = guesses[guess % guesses.len()];
            guess += 1;
            version
        });
        let port = target.entry.port.unwrap_or(PORT);
        let address = SocketAddr::new(host, port);
        flags.answered = false;
        let outcome = Link::connect(address, version, target.key)
            .and_then(|link| session(&target, &shared, link, version, &mut flags, (&requests, &stop)));
        if flags.answered {
            failures = 0;
            down_since = None;
            guess = guess.saturating_sub(1);
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let why = match outcome {
            Ok(()) => "disconnected".to_string(),
            Err(why) => why,
        };
        tracing::debug!("home: {} ({address}): {why}", target.entry.name);
        failures = failures.saturating_add(1);
        let since = *down_since.get_or_insert_with(SystemTime::now);
        shared.set_reach(target.slot, Reach::Unreachable { since, why });
        let backoff = Duration::from_secs(1u64 << failures.min(6)).min(MAX_BACKOFF);
        idle(&requests, &stop, backoff);
    }
}

/// Waits, dropping requests a device that is not there cannot take.
fn idle(requests: &mpsc::Receiver<Change>, stop: &AtomicBool, duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until && !stop.load(Ordering::Relaxed) {
        let _ = requests.recv_timeout(POLL);
    }
}

/// What a worker learns about its device across connections.
struct Flags {
    /// The device asks for its state the "device22" way.
    device22: bool,
    /// The current connection got an answer.
    answered: bool,
}

fn session(
    target: &Target,
    shared: &Shared,
    mut link: Link,
    version: Version,
    flags: &mut Flags,
    (requests, stop): (&mpsc::Receiver<Change>, &AtomicBool),
) -> Result<(), String> {
    let ids: Vec<String> = target.entry.mapping.keys().cloned().collect();
    let ids = if ids.is_empty() { vec!["1".to_string()] } else { ids };
    let mut payloads = Payloads { device_id: &target.entry.device_id, version, device22: flags.device22 };
    let (query, body) = payloads.query(&ids);
    link.send(query, &body)?;
    let mut heard = Instant::now();
    let mut beat = Instant::now();
    let started = Instant::now();
    let mut pending: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        while let Ok(change) = requests.try_recv() {
            let Some(dps) = data_points(&target.model, &shared.state(target.slot), &change) else { continue };
            let (control, body) = payloads.control(&dps);
            link.send(control, &body)?;
            shared.apply(target.slot, &target.model, &dps, true);
            pending = Some(Instant::now());
        }
        let now = Instant::now();
        if now.duration_since(beat) >= HEARTBEAT {
            let (heartbeat, body) = payloads.heartbeat();
            link.send(heartbeat, &body)?;
            beat = now;
        }
        if now.duration_since(heard) >= SILENCE {
            return Err("no reply".into());
        }
        if !flags.answered && now.duration_since(started) >= CONFIRM * 2 {
            return Err("no reply to the status query (wrong version or key?)".into());
        }
        if pending.is_some_and(|since| now.duration_since(since) >= CONFIRM) {
            let (query, body) = payloads.query(&ids);
            link.send(query, &body)?;
            pending = None;
            shared.settle(target.slot);
        }
        let Some(frame) = link.read(POLL)? else { continue };
        heard = Instant::now();
        if frame.payload.windows(12).any(|window| window == b"data unvalid")
            && version == Version::V33
            && !flags.device22
        {
            flags.device22 = true;
            payloads.device22 = true;
            let (query, body) = payloads.query(&ids);
            link.send(query, &body)?;
            continue;
        }
        let Some(dps) = reported_dps(&frame.payload) else { continue };
        if !flags.answered {
            flags.answered = true;
            shared.set_reach(target.slot, Reach::Online);
        }
        pending = None;
        shared.apply(target.slot, &target.model, &dps, false);
    }
    Ok(())
}
