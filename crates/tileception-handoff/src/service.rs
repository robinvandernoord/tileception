//! Connections to other machines, off the compositor thread.
//!
//! A listener takes connections, a dialler keeps trying the machines it knows
//! of (from mDNS and from the config), and every secured connection gets a
//! reader and a writer thread. When two machines dial each other at once, the
//! connection dialled by the machine with the lower id is the one kept.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io,
    net::{IpAddr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
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
    stream: TcpStream,
    outbox: Arc<Outbox>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.outbox.close();
    }
}

/// Frames waiting for a connection's writer. Bulk frames (window pictures)
/// go only when nothing else waits, so input never queues behind them.
#[derive(Default)]
struct Outbox {
    queues: Mutex<Queues>,
    ready: Condvar,
}

#[derive(Default)]
struct Queues {
    urgent: VecDeque<Frame>,
    bulk: VecDeque<Frame>,
    closed: bool,
}

impl Outbox {
    fn push(&self, frame: Frame, bulk: bool) -> bool {
        let mut queues = lock(&self.queues);
        if queues.closed {
            return false;
        }
        if bulk {
            queues.bulk.push_back(frame);
        } else {
            queues.urgent.push_back(frame);
        }
        self.ready.notify_one();
        true
    }

    fn close(&self) {
        lock(&self.queues).closed = true;
        self.ready.notify_all();
    }

    /// The next frame to write, a ping after `quiet` without one, or nothing
    /// once the connection is done.
    fn next(&self, quiet: Duration) -> Option<Frame> {
        let mut queues = lock(&self.queues);
        let since = Instant::now();
        loop {
            if queues.closed {
                return None;
            }
            if let Some(frame) = queues.urgent.pop_front().or_else(|| queues.bulk.pop_front()) {
                return Some(frame);
            }
            let left = quiet.saturating_sub(since.elapsed());
            if left.is_zero() {
                return Some(Frame::Ping);
            }
            queues =
                self.ready.wait_timeout(queues, left).map_or_else(|error| error.into_inner().0, |(guard, _)| guard);
        }
    }
}

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
        let shared = Arc::new(Shared {
            machine: settings.machine.unwrap_or_else(random_id),
            name: settings.name,
            passphrase: settings.passphrase,
            port,
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

    /// Queues a message; false when that machine is not connected.
    pub fn send(&self, peer: PeerId, message: Message) -> bool {
        self.shared.send(peer, message, false)
    }

    /// A handle for sending from other threads.
    pub fn messenger(&self) -> Messenger {
        Messenger(self.shared.clone())
    }
}

/// Sends messages from any thread, for as long as the service runs.
#[derive(Clone)]
pub struct Messenger(Arc<Shared>);

impl Messenger {
    pub fn send(&self, peer: PeerId, message: Message) -> bool {
        !self.0.stopping() && self.0.send(peer, message, false)
    }

    /// Queues a message behind everything else waiting, for large data such
    /// as window pictures.
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
        let outbox = lock(&self.links).get(&peer).map(|link| link.outbox.clone());
        outbox.is_some_and(|outbox| outbox.push(Frame::App(message), bulk))
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
            if self.limited() {
                continue;
            }
            let shared = self.clone();
            let _ = spawn("tileception-handoff-accept", move || {
                let address = stream.peer_addr().ok();
                if let Err(error) = shared.clone().secure(stream, false)
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
            // Not a guess at the passphrase: it was right.
            tracing::warn!("handoff: {address} runs {error}");
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
            let due: Vec<(String, Vec<SocketAddr>)> = {
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
                    .map(|(key, candidate)| (key.clone(), candidate.addresses()))
                    .collect()
            };
            for (key, addresses) in due {
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
            return match self.clone().secure(stream, true) {
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
    fn secure(self: Arc<Self>, mut stream: TcpStream, initiator: bool) -> Result<PeerId, secure::Error> {
        let address = stream.peer_addr()?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let mut transport = secure::handshake(&mut stream, &self.passphrase, initiator)?;
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
        stream.set_read_timeout(Some(SILENCE))?;
        let dialler = if initiator { self.machine } else { peer };
        let serial = self.serials.fetch_add(1, Ordering::SeqCst);
        let outbox = Arc::new(Outbox::default());
        let link = Link { serial, dialler, stream: stream.try_clone()?, outbox: outbox.clone() };
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
                Ok(Frame::Hello { .. }) | Err(_) => break,
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
        let written =
            secure::seal(&mut lock(transport), &frame).and_then(|sealed| secure::write_frame(&mut stream, &sealed));
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
        assert_eq!(outbox.next(quiet), Some(Frame::App(Message::Leave)));
        assert_eq!(outbox.next(quiet), Some(Frame::App(Message::WindowGone { window: 1 })));
        assert_eq!(outbox.next(quiet), Some(Frame::Ping));
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
        }
    }

    fn start(passphrase: &str, peers: Vec<String>) -> (Service, mpsc::Receiver<Event>) {
        let (sender, receiver) = mpsc::channel();
        let sender = Mutex::new(sender);
        let service = Service::start(settings(passphrase, peers), move |event| {
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
}
