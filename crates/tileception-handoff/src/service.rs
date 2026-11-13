//! Connections to other machines, off the compositor thread.
//!
//! A listener takes connections, a dialler keeps trying the machines it knows
//! of (from mDNS and from the config), and every secured connection gets a
//! reader and a writer thread. When two machines dial each other at once, the
//! connection dialled by the machine with the lower id is the one kept.
//!
//! The service also keeps which machines are trusted (see `trust`): it pins
//! machines that paired with the passphrase, tells connected machines when
//! this one gets a new key, and passes forgotten keys on to them.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io,
    net::{IpAddr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use snow::TransportState;
use zeroize::Zeroizing;

use crate::{
    secure,
    trust::{self, Forgotten, Key, Keys, Standing, Trust, Trusted},
    wire::{Frame, Message, PROTOCOL},
};

pub type PeerId = u64;

/// The port both machines listen on unless the config says otherwise.
pub const DEFAULT_PORT: u16 = 24850;

const SERVICE_TYPE: &str = "_tileception._tcp.local.";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(2);
/// Silence for this long means the other machine is gone (or asleep).
const SILENCE: Duration = Duration::from_secs(7);
const DIAL_EVERY: Duration = Duration::from_secs(3);
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const LONGEST_BACKOFF: Duration = Duration::from_secs(300);
/// Failed handshakes allowed per minute, all callers together. Each one is a
/// guess at the passphrase.
const FAILURES_PER_MINUTE: usize = 6;

pub struct Settings {
    /// What the other machines call this one.
    pub name: String,
    pub passphrase: Zeroizing<String>,
    pub port: u16,
    /// Fixed addresses (`host` or `host:port`) for networks without mDNS.
    pub peers: Vec<String>,
    /// Announce and look for machines with mDNS.
    pub discovery: bool,
    /// This machine's id, kept between runs so others recognise it after a
    /// restart; a fresh one when not given.
    pub machine: Option<PeerId>,
    /// Where this machine's static key is kept (readable by its owner
    /// only); a fresh key each run when not given.
    pub key_file: Option<PathBuf>,
    /// Where the trusted machines and forgotten keys are kept; nothing is
    /// remembered between runs when not given.
    pub trust_file: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Event {
    /// A machine with the same passphrase is connected.
    Connected {
        peer: PeerId,
        name: String,
        address: SocketAddr,
    },
    Received {
        peer: PeerId,
        message: Message,
    },
    Disconnected {
        peer: PeerId,
    },
    /// A handshake failed: a different passphrase, or a stranger.
    Refused {
        address: SocketAddr,
    },
    /// A trusted machine was forgotten: here, or by `from`, a connected
    /// machine that passed it on. It is disconnected if it was connected.
    Forgotten {
        peer: PeerId,
        name: String,
        from: Option<String>,
    },
    /// A machine with the same passphrase runs a version that speaks
    /// another protocol, so the two cannot share.
    Incompatible {
        address: SocketAddr,
        name: String,
        protocol: u32,
    },
}

pub struct Service {
    shared: Arc<Shared>,
    mdns: Option<mdns_sd::ServiceDaemon>,
}

type Notify = Box<dyn Fn(Event) + Send + Sync>;

struct Shared {
    machine: PeerId,
    name: String,
    passphrase: Zeroizing<String>,
    port: u16,
    keys: Mutex<Keys>,
    key_file: Option<PathBuf>,
    trust: Mutex<Trust>,
    notify: Notify,
    stopping: AtomicBool,
    serials: AtomicU64,
    links: Mutex<HashMap<PeerId, Link>>,
    candidates: Mutex<BTreeMap<String, Candidate>>,
    /// Wakes the dialler early when mDNS finds someone.
    dial_now: Condvar,
    failures: Mutex<VecDeque<Instant>>,
}

struct Link {
    serial: u64,
    /// Which machine dialled; decides which of two parallel links stays.
    dialler: PeerId,
    /// The other machine's static key, and what it calls itself.
    key: Key,
    name: String,
    stream: TcpStream,
    outbox: Arc<Outbox>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.outbox.close();
    }
}

/// Encoded frames waiting for a connection's writer. Control traffic has
/// priority; pictures and transfers take turns below it. Only worker threads
/// may wait for bulk queue space. A full control queue disconnects instead
/// of blocking the compositor or silently losing a key release.
#[derive(Default)]
struct Outbox {
    queues: Mutex<Queues>,
    ready: Condvar,
    space: Condvar,
}

#[derive(Default)]
struct Queues {
    urgent: Queue,
    pictures: Queue,
    transfers: Queue,
    picture_next: bool,
    closed: bool,
}

const CONTROL_BYTES: usize = 256 * 1024;
const BULK_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct Queue {
    frames: VecDeque<Box<[u8]>>,
    bytes: usize,
}

impl Queue {
    fn cost(frame: &[u8]) -> usize {
        // Include a minimum charge for queue nodes and tiny messages.
        frame.len().max(64)
    }

    fn push(&mut self, frame: Box<[u8]>) {
        self.bytes += Self::cost(&frame);
        self.frames.push_back(frame);
    }

    fn pop(&mut self) -> Option<Box<[u8]>> {
        let frame = self.frames.pop_front()?;
        self.bytes -= Self::cost(&frame);
        Some(frame)
    }
}

impl Outbox {
    fn push(&self, frame: Frame, bulk: bool) -> bool {
        let picture = matches!(&frame, Frame::App(Message::WindowPicture(_)));
        let Ok(bytes) = crate::wire::encode(&frame) else { return false };
        drop(frame);
        if bytes.len() + secure::TAG > secure::MAX_MESSAGE {
            return false;
        }
        let bytes = bytes.into_boxed_slice();
        let cost = Queue::cost(&bytes);
        let mut queues = lock(&self.queues);
        loop {
            if queues.closed {
                return false;
            }
            let queue = if !bulk {
                &mut queues.urgent
            } else if picture {
                &mut queues.pictures
            } else {
                &mut queues.transfers
            };
            let limit = if bulk { BULK_BYTES } else { CONTROL_BYTES };
            if queue.bytes + cost <= limit {
                queue.push(bytes);
                self.ready.notify_one();
                return true;
            }
            if !bulk {
                return false;
            }
            queues = self.space.wait(queues).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn close(&self) {
        lock(&self.queues).closed = true;
        self.ready.notify_all();
        self.space.notify_all();
    }

    /// The next frame to write, a ping after `quiet` without one, or nothing
    /// once the connection is done.
    fn next(&self, quiet: Duration) -> Option<Box<[u8]>> {
        let mut queues = lock(&self.queues);
        let since = Instant::now();
        loop {
            if queues.closed {
                return None;
            }
            let frame = queues.urgent.pop().or_else(|| {
                let frame = if queues.picture_next {
                    queues.pictures.pop().or_else(|| queues.transfers.pop())
                } else {
                    queues.transfers.pop().or_else(|| queues.pictures.pop())
                };
                queues.picture_next = !queues.picture_next;
                frame
            });
            if let Some(frame) = frame {
                self.space.notify_all();
                return Some(frame);
            }
            let left = quiet.saturating_sub(since.elapsed());
            if left.is_zero() {
                return crate::wire::encode(&Frame::Ping).ok().map(Vec::into_boxed_slice);
            }
            queues =
                self.ready.wait_timeout(queues, left).map_or_else(|error| error.into_inner().0, |(guard, _)| guard);
        }
    }
}

#[derive(Clone)]
struct Candidate {
    /// From mDNS, or learnt from the first connection to a configured address.
    peer: Option<PeerId>,
    /// From the config: dialled whichever machine has the lower id.
    configured: bool,
    addresses: Vec<SocketAddr>,
    host: Option<String>,
    next_try: Instant,
    backoff: Duration,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Service {
    /// Starts listening, announcing and dialling. `notify` is called from the
    /// service's threads.
    pub fn start(settings: Settings, notify: impl Fn(Event) + Send + Sync + 'static) -> io::Result<Service> {
        let listener = TcpListener::bind((IpAddr::V6(Ipv6Addr::UNSPECIFIED), settings.port))
            .or_else(|_| TcpListener::bind(("0.0.0.0", settings.port)))?;
        let port = listener.local_addr()?.port();
        let keys = Keys::load_or_create(settings.key_file.as_deref())?;
        let shared = Arc::new(Shared {
            machine: settings.machine.unwrap_or_else(random_id),
            name: settings.name,
            passphrase: settings.passphrase,
            port,
            keys: Mutex::new(keys),
            key_file: settings.key_file,
            trust: Mutex::new(Trust::load(settings.trust_file)),
            notify: Box::new(notify),
            stopping: AtomicBool::new(false),
            serials: AtomicU64::new(0),
            links: Mutex::default(),
            candidates: Mutex::default(),
            dial_now: Condvar::new(),
            failures: Mutex::default(),
        });
        {
            let mut candidates = lock(&shared.candidates);
            for peer in settings.peers {
                let host =
                    if peer.contains(':') && !peer.ends_with(']') { peer.clone() } else { format!("{peer}:{port}") };
                let mut candidate = Candidate::new(None, Vec::new(), Some(host));
                candidate.configured = true;
                candidates.insert(format!("config {peer}"), candidate);
            }
        }
        spawn("tileception-handoff-listen", {
            let shared = shared.clone();
            move || shared.listen(&listener)
        })?;
        spawn("tileception-handoff-dial", {
            let shared = shared.clone();
            move || shared.dial_loop()
        })?;
        let mdns = if settings.discovery {
            match shared.clone().announce() {
                Ok(daemon) => Some(daemon),
                Err(error) => {
                    tracing::warn!("handoff: mDNS discovery is off: {error}");
                    None
                }
            }
        } else {
            None
        };
        Ok(Service { shared, mdns })
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }

    /// Queues control traffic without waiting; false when disconnected or
    /// overloaded (the connection is then shut down to release held input).
    pub fn send(&self, peer: PeerId, message: Message) -> bool {
        self.shared.send(peer, message, false)
    }

    /// A handle for sending from other threads.
    pub fn messenger(&self) -> Messenger {
        Messenger(self.shared.clone())
    }

    /// This machine's public static key.
    pub fn key(&self) -> Key {
        lock(&self.shared.keys).public
    }

    /// The machines that connect with their key alone, whether connected
    /// now or not.
    pub fn trusted(&self) -> Vec<Trusted> {
        lock(&self.shared.trust).trusted().to_vec()
    }

    /// The keys refused even with the passphrase, oldest first.
    pub fn forgotten(&self) -> Vec<Forgotten> {
        lock(&self.shared.trust).forgotten().to_vec()
    }

    /// Stops trusting a key, for good: the machine is disconnected, refused
    /// from now on even with the passphrase, and connected machines are told
    /// to refuse it too. A `Forgotten` event follows when it was trusted.
    pub fn forget(&self, key: Key) {
        self.shared.forget(vec![(key, String::new())], None);
    }

    /// Makes a new static key for this machine and tells the connected
    /// machines, which then trust it in place of the old one and refuse the
    /// old one. Machines not connected now need the passphrase once.
    pub fn new_key(&self) -> io::Result<Key> {
        let keys = Keys::generate()?;
        if let Some(path) = &self.shared.key_file {
            keys.save(path)?;
        }
        let key = keys.public;
        *lock(&self.shared.keys) = keys;
        for link in lock(&self.shared.links).values() {
            link.outbox.push(Frame::NewKey { key }, false);
        }
        tracing::info!("handoff: this machine has a new key, {}", trust::fingerprint(&key));
        Ok(key)
    }
}

/// Sends messages from any thread, for as long as the service runs.
#[derive(Clone)]
pub struct Messenger(Arc<Shared>);

impl Messenger {
    pub fn send(&self, peer: PeerId, message: Message) -> bool {
        !self.0.stopping() && self.0.send(peer, message, false)
    }

    /// Queues bulk data, waiting for bounded queue space. Call only from a
    /// worker thread, never the compositor. Pictures and transfers get
    /// separate budgets and alternate when no control traffic is waiting.
    pub fn send_bulk(&self, peer: PeerId, message: Message) -> bool {
        !self.0.stopping() && self.0.send(peer, message, true)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.dial_now.notify_all();
        for (_, link) in lock(&self.shared.links).drain() {
            let _ = link.stream.shutdown(Shutdown::Both);
        }
        // Wakes the listener so it notices.
        let _ = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], self.shared.port)), HANDSHAKE_TIMEOUT);
        if let Some(mdns) = self.mdns.take() {
            let _ = mdns.shutdown();
        }
    }
}

impl Candidate {
    fn new(peer: Option<PeerId>, addresses: Vec<SocketAddr>, host: Option<String>) -> Self {
        Candidate { peer, configured: false, addresses, host, next_try: Instant::now(), backoff: FIRST_BACKOFF }
    }

    fn addresses(&self) -> Vec<SocketAddr> {
        let mut addresses = self.addresses.clone();
        if let Some(host) = &self.host {
            match host.to_socket_addrs() {
                Ok(resolved) => addresses.extend(resolved),
                Err(error) => tracing::debug!("handoff: cannot resolve {host}: {error}"),
            }
        }
        addresses.sort_by_key(|address| (address.is_ipv6(), *address));
        addresses.dedup();
        addresses
    }
}

impl Shared {
    fn send(&self, peer: PeerId, message: Message, bulk: bool) -> bool {
        let link = lock(&self.links).get(&peer).map(|link| (link.serial, link.outbox.clone()));
        let Some((serial, outbox)) = link else { return false };
        if outbox.push(Frame::App(message), bulk) {
            return true;
        }
        outbox.close();
        // Never shut down a replacement connection after an old worker wakes.
        if let Some(link) = lock(&self.links).get(&peer).filter(|link| link.serial == serial) {
            let _ = link.stream.shutdown(Shutdown::Both);
        }
        false
    }

    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn listen(self: Arc<Self>, listener: &TcpListener) {
        for stream in listener.incoming() {
            if self.stopping() {
                return;
            }
            let Ok(stream) = stream else { continue };
            let shared = self.clone();
            let _ = spawn("tileception-handoff-accept", move || {
                let address = stream.peer_addr().ok();
                // Trusted machines still connect while someone guesses.
                let may_pair = !shared.limited();
                if let Err(error) = shared.clone().secure(stream, false, may_pair)
                    && let Some(address) = address
                {
                    shared.failed(address, &error);
                }
            });
        }
    }

    /// Whether too many handshakes failed lately to try another.
    fn limited(&self) -> bool {
        let mut failures = lock(&self.failures);
        let now = Instant::now();
        while failures.front().is_some_and(|failure| now.duration_since(*failure) > Duration::from_secs(60)) {
            failures.pop_front();
        }
        failures.len() >= FAILURES_PER_MINUTE
    }

    fn failed(&self, address: SocketAddr, error: &secure::Error) {
        if matches!(error, secure::Error::Incompatible) {
            // Not a guess at the passphrase: it was right, or never asked.
            tracing::warn!("handoff: {address} runs {error}");
        } else if matches!(error, secure::Error::Forgotten | secure::Error::Limited) {
            tracing::warn!("handoff: {address} {error}");
        } else if matches!(error, secure::Error::Refused) {
            lock(&self.failures).push_back(Instant::now());
            tracing::warn!("handoff: {address} {error}");
            (self.notify)(Event::Refused { address });
        } else {
            tracing::debug!("handoff: {address}: {error}");
        }
    }

    fn dial_loop(self: Arc<Self>) {
        while !self.stopping() {
            let due: Vec<(String, Candidate)> = {
                let links = lock(&self.links);
                let now = Instant::now();
                lock(&self.candidates)
                    .iter()
                    .filter(|(_, candidate)| candidate.next_try <= now)
                    .filter(|(_, candidate)| match candidate.peer {
                        Some(peer) if links.contains_key(&peer) => false,
                        // Of two machines that found each other, the lower id dials.
                        Some(peer) => candidate.configured || self.machine < peer,
                        None => true,
                    })
                    .map(|(key, candidate)| (key.clone(), candidate.clone()))
                    .collect()
            };
            for (key, candidate) in due {
                // DNS may block for seconds. Neither the connections mutex
                // used by compositor sends nor the candidates mutex is held.
                let addresses = candidate.addresses();
                let reached = self.dial(&addresses);
                let mut candidates = lock(&self.candidates);
                if let Some(candidate) = candidates.get_mut(&key) {
                    if reached == Some(self.machine) {
                        // Ourselves, through a configured address.
                        candidate.next_try = Instant::now() + LONGEST_BACKOFF;
                    } else if let Some(peer) = reached {
                        candidate.peer = Some(peer);
                        candidate.backoff = FIRST_BACKOFF;
                        candidate.next_try = Instant::now() + DIAL_EVERY;
                    } else {
                        candidate.next_try = Instant::now() + candidate.backoff;
                        candidate.backoff = (candidate.backoff * 2).min(LONGEST_BACKOFF);
                    }
                }
            }
            let candidates = lock(&self.candidates);
            let _ = self.dial_now.wait_timeout(candidates, DIAL_EVERY);
        }
    }

    /// Tries each address until one connects, returning the machine reached.
    fn dial(self: &Arc<Self>, addresses: &[SocketAddr]) -> Option<PeerId> {
        for address in addresses {
            if self.stopping() {
                return None;
            }
            let Ok(stream) = TcpStream::connect_timeout(address, HANDSHAKE_TIMEOUT) else { continue };
            return match self.clone().secure(stream, true, true) {
                Ok(peer) => Some(peer),
                Err(error) => {
                    self.failed(*address, &error);
                    None
                }
            };
        }
        None
    }

    /// Handshake, hello, then the connection's own threads. Returns the
    /// machine at the other end, which may turn out to be this one.
    fn secure(
        self: Arc<Self>,
        mut stream: TcpStream,
        initiator: bool,
        may_pair: bool,
    ) -> Result<PeerId, secure::Error> {
        let address = stream.peer_addr()?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let keys = lock(&self.keys).clone();
        let credentials = secure::Credentials {
            keys: &keys,
            passphrase: &self.passphrase,
            // Not ourselves through a configured address: that is found out
            // below, after the passphrase.
            standing: |key: &Key| lock(&self.trust).standing(key),
            may_pair,
        };
        let secured = secure::handshake(&mut stream, &credentials, initiator)?;
        drop(keys);
        let (mut transport, key, paired) = (secured.transport, secured.key, secured.paired);
        let hello = Frame::Hello { protocol: PROTOCOL, machine: self.machine, name: self.name.clone() };
        secure::write_frame(&mut stream, &secure::seal(&mut transport, &hello)?)?;
        let Frame::Hello { protocol, machine: peer, name } =
            secure::open(&mut transport, &secure::read_frame(&mut stream)?)?
        else {
            return Err(secure::Error::Refused);
        };
        if protocol != PROTOCOL {
            (self.notify)(Event::Incompatible { address, name, protocol });
            return Err(secure::Error::Incompatible);
        }
        if peer == self.machine {
            return Ok(peer);
        }
        let name = trust::clean_name(&name);
        {
            let mut trust = lock(&self.trust);
            // Forgotten while the handshake ran.
            if trust.standing(&key) == Standing::Refused {
                return Err(secure::Error::Forgotten);
            }
            if trust.pin(Trusted { key, machine: peer, name: name.clone() }) {
                trust.save();
                if paired {
                    tracing::info!("handoff: {name} paired with the passphrase, key {}", trust::fingerprint(&key));
                }
            }
        }
        stream.set_read_timeout(Some(SILENCE))?;
        let dialler = if initiator { self.machine } else { peer };
        let serial = self.serials.fetch_add(1, Ordering::SeqCst);
        let outbox = Arc::new(Outbox::default());
        let forgotten: Vec<(Key, String)> =
            lock(&self.trust).forgotten().iter().map(|forgotten| (forgotten.key, forgotten.name.clone())).collect();
        if !forgotten.is_empty() {
            outbox.push(Frame::Forgotten { keys: forgotten }, false);
        }
        let link =
            Link { serial, dialler, key, name: name.clone(), stream: stream.try_clone()?, outbox: outbox.clone() };
        // Whether this is news, and whether a connection it replaces died.
        let (fresh, died) = {
            let mut links = lock(&self.links);
            match links.get(&peer) {
                // The lower id's connection wins over the other one's. A new
                // one from the same dialler means it saw the old one die.
                Some(existing) if existing.dialler == self.machine.min(peer) && dialler != existing.dialler => {
                    drop(links);
                    let _ = stream.shutdown(Shutdown::Both);
                    return Ok(peer);
                }
                Some(existing) => {
                    let died = existing.dialler == dialler;
                    let _ = existing.stream.shutdown(Shutdown::Both);
                    links.insert(peer, link);
                    (died, died)
                }
                None => {
                    links.insert(peer, link);
                    (true, false)
                }
            }
        };
        let transport = Arc::new(Mutex::new(transport));
        let writer_stream = stream.try_clone()?;
        spawn("tileception-handoff-write", {
            let transport = transport.clone();
            move || write_loop(writer_stream, &transport, &outbox)
        })?;
        if died {
            tracing::info!("handoff: {name} reconnected");
            (self.notify)(Event::Disconnected { peer });
        }
        if fresh {
            tracing::info!("handoff: connected to {name} at {address}");
            (self.notify)(Event::Connected { peer, name, address });
        }
        let shared = self.clone();
        spawn("tileception-handoff-read", move || shared.read_loop(stream, &transport, peer, serial))?;
        Ok(peer)
    }

    fn read_loop(&self, mut stream: TcpStream, transport: &Mutex<TransportState>, peer: PeerId, serial: u64) {
        loop {
            let frame = secure::read_frame(&mut stream).and_then(|sealed| secure::open(&mut lock(transport), &sealed));
            match frame {
                Ok(Frame::App(message)) => (self.notify)(Event::Received { peer, message }),
                Ok(Frame::Ping) => {}
                Ok(Frame::NewKey { key }) => self.new_key_of(peer, serial, key),
                Ok(Frame::Forgotten { keys }) => {
                    let from = lock(&self.links).get(&peer).map(|link| link.name.clone());
                    self.forget(keys, from.map(|name| (peer, name)));
                }
                Ok(Frame::Hello { .. } | Frame::Trust { .. } | Frame::Pake(_) | Frame::Confirm(_)) | Err(_) => break,
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
        let mut links = lock(&self.links);
        if links.get(&peer).is_some_and(|link| link.serial == serial) {
            links.remove(&peer);
            drop(links);
            tracing::info!("handoff: disconnected from {peer:016x}");
            if !self.stopping() {
                (self.notify)(Event::Disconnected { peer });
            }
            self.dial_now.notify_all();
        }
    }

    /// A connected machine moved to a new key.
    fn new_key_of(&self, peer: PeerId, serial: u64, key: Key) {
        let own = lock(&self.keys).public;
        let mut links = lock(&self.links);
        let Some(link) = links.get_mut(&peer).filter(|link| link.serial == serial) else { return };
        let old = link.key;
        let mut trust = lock(&self.trust);
        if key == own || !trust.replace(&old, key) {
            tracing::warn!("handoff: {} offered a new key that cannot be taken", link.name);
            return;
        }
        trust.save();
        link.key = key;
        tracing::info!("handoff: {} has a new key, {}", link.name, trust::fingerprint(&key));
        let name = link.name.clone();
        // The old key is refused here now; others learn that too.
        for (other, link) in links.iter() {
            if *other != peer {
                link.outbox.push(Frame::Forgotten { keys: vec![(old, name.clone())] }, false);
            }
        }
    }

    /// Refuses keys from now on, disconnecting machines that use them and
    /// telling the other connected machines. `from` is the machine that
    /// passed them on, when not this one's user.
    fn forget(&self, keys: Vec<(Key, String)>, from: Option<(PeerId, String)>) {
        let own = lock(&self.keys).public;
        let mut fresh = Vec::new();
        let mut forgotten = Vec::new();
        {
            let mut trust = lock(&self.trust);
            for (key, name) in keys {
                if key == own {
                    continue;
                }
                if let Some(was) = trust.forget(key, &name) {
                    let name = was.as_ref().map_or(name, |was| was.name.clone());
                    fresh.push((key, trust::clean_name(&name)));
                    forgotten.extend(was);
                }
            }
            if fresh.is_empty() {
                return;
            }
            trust.save();
        }
        {
            let links = lock(&self.links);
            for (peer, link) in links.iter() {
                if fresh.iter().any(|(key, _)| *key == link.key) {
                    let _ = link.stream.shutdown(Shutdown::Both);
                } else if from.as_ref().is_none_or(|(sender, _)| sender != peer) {
                    link.outbox.push(Frame::Forgotten { keys: fresh.clone() }, false);
                }
            }
        }
        let from = from.map(|(_, name)| name);
        for was in forgotten {
            match &from {
                Some(sender) => tracing::info!("handoff: {sender} forgot {}; it is refused here too", was.name),
                None => tracing::info!("handoff: forgot {}", was.name),
            }
            (self.notify)(Event::Forgotten { peer: was.machine, name: was.name, from: from.clone() });
        }
    }

    fn announce(self: Arc<Self>) -> Result<mdns_sd::ServiceDaemon, mdns_sd::Error> {
        let daemon = mdns_sd::ServiceDaemon::new()?;
        let machine = format!("{:016x}", self.machine);
        let host = format!("tileception-{machine}.local.");
        let properties = [("id", machine.as_str())];
        let info = mdns_sd::ServiceInfo::new(SERVICE_TYPE, &machine, &host, "", self.port, &properties[..])?
            .enable_addr_auto();
        daemon.register(info)?;
        let events = daemon.browse(SERVICE_TYPE)?;
        let shared = self;
        let started = spawn("tileception-handoff-mdns", move || {
            while let Ok(event) = events.recv() {
                shared.discovered(event);
            }
        });
        if let Err(error) = started {
            tracing::warn!("handoff: cannot watch mDNS: {error}");
        }
        Ok(daemon)
    }

    fn discovered(&self, event: mdns_sd::ServiceEvent) {
        match event {
            mdns_sd::ServiceEvent::ServiceResolved(service) => {
                let Some(peer) =
                    service.get_property_val_str("id").and_then(|machine| PeerId::from_str_radix(machine, 16).ok())
                else {
                    return;
                };
                if peer == self.machine {
                    return;
                }
                // Link-local IPv6 addresses need an interface scope, which is
                // not kept here.
                let addresses: Vec<SocketAddr> = service
                    .get_addresses()
                    .iter()
                    .map(|address| address.to_ip_addr())
                    .filter(|address| !matches!(address, IpAddr::V6(ipv6) if ipv6.is_unicast_link_local()))
                    .map(|address| SocketAddr::new(address, service.get_port()))
                    .collect();
                tracing::debug!("handoff: found {peer:016x} at {addresses:?}");
                lock(&self.candidates).insert(service.fullname.clone(), Candidate::new(Some(peer), addresses, None));
                self.dial_now.notify_all();
            }
            mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                lock(&self.candidates).remove(&fullname);
            }
            _ => {}
        }
    }
}

fn write_loop(mut stream: TcpStream, transport: &Mutex<TransportState>, outbox: &Outbox) {
    while let Some(frame) = outbox.next(PING_EVERY) {
        // Socket writes can block. Release the cipher lock first so the
        // reader can keep receiving input and acknowledgements meanwhile.
        let sealed = secure::seal_bytes(&mut lock(transport), &frame);
        let written = sealed.and_then(|sealed| secure::write_frame(&mut stream, &sealed));
        if written.is_err() {
            break;
        }
    }
    outbox.close();
    let _ = stream.shutdown(Shutdown::Both);
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new().name(name.into()).spawn(work).map(drop)
}

/// A fresh id: it only has to tell machines apart.
pub fn random_id() -> PeerId {
    let mut bytes = [0; 8];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        return nanos.as_nanos() as u64 ^ u64::from(std::process::id());
    }
    u64::from_ne_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn bulk_frames_wait_for_everything_else() {
        let outbox = Outbox::default();
        outbox.push(Frame::App(Message::WindowGone { window: 1 }), true);
        outbox.push(Frame::App(Message::Leave), false);
        let quiet = Duration::from_millis(10);
        let next = || outbox.next(quiet).map(|bytes| crate::wire::decode(&bytes).expect("valid frame"));
        assert_eq!(next(), Some(Frame::App(Message::Leave)));
        assert_eq!(next(), Some(Frame::App(Message::WindowGone { window: 1 })));
        assert_eq!(next(), Some(Frame::Ping));
        outbox.close();
        assert_eq!(outbox.next(quiet), None);
        assert!(!outbox.push(Frame::Ping, false));
    }

    fn settings(passphrase: &str, peers: Vec<String>) -> Settings {
        Settings {
            name: "test".into(),
            passphrase: Zeroizing::new(passphrase.into()),
            port: 0,
            peers,
            discovery: false,
            machine: None,
            key_file: None,
            trust_file: None,
        }
    }

    fn start(passphrase: &str, peers: Vec<String>) -> (Service, mpsc::Receiver<Event>) {
        start_with(settings(passphrase, peers))
    }

    fn start_with(settings: Settings) -> (Service, mpsc::Receiver<Event>) {
        let (sender, receiver) = mpsc::channel();
        let sender = Mutex::new(sender);
        let service = Service::start(settings, move |event| {
            let _ = lock(&sender).send(event);
        })
        .expect("starts");
        (service, receiver)
    }

    fn next(events: &mpsc::Receiver<Event>) -> Event {
        events.recv_timeout(Duration::from_secs(10)).expect("an event")
    }

    #[test]
    fn machines_with_the_same_passphrase_connect_and_talk() {
        let (first, first_events) = start("same", Vec::new());
        let (second, second_events) = start("same", vec![format!("127.0.0.1:{}", first.port())]);
        let Event::Connected { peer: second_id, .. } = next(&first_events) else { panic!("first did not connect") };
        let Event::Connected { peer: first_id, .. } = next(&second_events) else { panic!("second did not connect") };
        assert!(second.send(first_id, Message::Arranged));
        assert!(
            matches!(next(&first_events), Event::Received { peer, message: Message::Arranged } if peer == second_id)
        );
        drop(second);
        assert!(matches!(next(&first_events), Event::Disconnected { peer } if peer == second_id));
    }

    #[test]
    fn different_passphrases_are_refused() {
        let (first, first_events) = start("one", Vec::new());
        let (_second, second_events) = start("two", vec![format!("127.0.0.1:{}", first.port())]);
        assert!(matches!(next(&first_events), Event::Refused { .. }));
        assert!(matches!(next(&second_events), Event::Refused { .. }));
    }

    /// A machine that keeps its key and trust in `directory`.
    fn kept(directory: &std::path::Path, name: &str, passphrase: &str, peers: Vec<String>) -> Settings {
        Settings {
            name: name.into(),
            machine: Some(match name {
                "first" => 1,
                "second" => 2,
                _ => 3,
            }),
            key_file: Some(directory.join(format!("{name}.key"))),
            trust_file: Some(directory.join(format!("{name}.trust"))),
            ..settings(passphrase, peers)
        }
    }

    fn connected(events: &mpsc::Receiver<Event>) -> PeerId {
        loop {
            if let Event::Connected { peer, .. } = next(events) {
                return peer;
            }
        }
    }

    fn scratch() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tileception-handoff-trust-{:016x}", random_id()))
    }

    #[test]
    fn paired_machines_reconnect_after_the_passphrase_changed() {
        let directory = scratch();
        let (first, first_events) = start_with(kept(&directory, "first", "old words", Vec::new()));
        let address = vec![format!("127.0.0.1:{}", first.port())];
        let (second, second_events) = start_with(kept(&directory, "second", "old words", address.clone()));
        assert_eq!(connected(&first_events), 2);
        assert_eq!(connected(&second_events), 1);
        assert_eq!(first.trusted().iter().map(|trusted| trusted.key).collect::<Vec<_>>(), [second.key()]);
        drop((first, second));

        let (first, first_events) = start_with(kept(&directory, "first", "new words", Vec::new()));
        let address = vec![format!("127.0.0.1:{}", first.port())];
        let (_second, second_events) = start_with(kept(&directory, "second", "other words", address));
        assert_eq!(connected(&first_events), 2);
        assert_eq!(connected(&second_events), 1);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn forgotten_machines_stay_out() {
        let directory = scratch();
        let (first, first_events) = start_with(kept(&directory, "first", "same", Vec::new()));
        let address = vec![format!("127.0.0.1:{}", first.port())];
        let (second, second_events) = start_with(kept(&directory, "second", "same", address));
        connected(&first_events);
        connected(&second_events);
        first.forget(second.key());
        let events = [next(&first_events), next(&first_events)];
        assert!(events.iter().any(|event| matches!(event, Event::Forgotten { peer: 2, from: None, .. })));
        assert!(events.iter().any(|event| matches!(event, Event::Disconnected { peer: 2 })));
        assert!(first.trusted().is_empty());
        assert_eq!(first.forgotten().iter().map(|forgotten| forgotten.key).collect::<Vec<_>>(), [second.key()]);
        drop(first);
        // Still refused after a restart, passphrase and all.
        let (first, first_events) = start_with(kept(&directory, "first", "same", Vec::new()));
        drop(second);
        let address = vec![format!("127.0.0.1:{}", first.port())];
        let (_second, _second_events) = start_with(kept(&directory, "second", "same", address));
        std::thread::sleep(Duration::from_secs(2));
        assert!(first_events.try_iter().all(|event| !matches!(event, Event::Connected { .. })));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_new_key_is_taken_over_and_forgetting_is_passed_on() {
        let directory = scratch();
        let (first, first_events) = start_with(kept(&directory, "first", "same", Vec::new()));
        let address = vec![format!("127.0.0.1:{}", first.port())];
        let (second, second_events) = start_with(kept(&directory, "second", "same", address.clone()));
        let (third, third_events) = start_with(kept(&directory, "third", "same", address));
        let mut seen = vec![connected(&first_events), connected(&first_events)];
        seen.sort_unstable();
        assert_eq!(seen, [2, 3]);
        connected(&second_events);
        connected(&third_events);

        let old = second.key();
        let new = second.new_key().expect("a new key");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !first.trusted().iter().any(|trusted| trusted.key == new) {
            assert!(Instant::now() < deadline, "the new key was not taken over");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(first.forgotten().iter().any(|forgotten| forgotten.key == old));

        // The third machine learns from the first that the second's old key
        // is refused, and that the second is forgotten.
        first.forget(new);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !third.forgotten().iter().any(|forgotten| forgotten.key == old)
            || !third.forgotten().iter().any(|forgotten| forgotten.key == new)
        {
            assert!(Instant::now() < deadline, "forgetting was not passed on");
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(directory);
    }
}
