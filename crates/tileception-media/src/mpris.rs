//! Media players through MPRIS on the session bus.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::message::Type;
use zbus::zvariant::{OwnedValue, Value};
use zbus::MatchRule;

use crate::{lock, Event, Media, Notifier, Player, PlayerCommand, Shared, Status};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

type Properties = HashMap<String, OwnedValue>;

enum Msg {
    Command(PlayerCommand),
    /// A bus name appeared, went away or changed owner.
    Owner {
        name: String,
        owner: Option<String>,
    },
    /// A player, by its unique bus name, reported a change.
    Changed(String),
    /// Cover art finished downloading.
    ArtReady,
    Stop,
}

pub struct Worker {
    messages: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start(shared: Shared, notifier: Notifier, bus_address: Option<String>) -> Self {
        let (messages, incoming) = mpsc::channel();
        let sender = messages.clone();
        let thread = thread::Builder::new()
            .name("tileception-mpris".into())
            .spawn(move || {
                let connect = || match bus_address.as_deref() {
                    Some(address) => zbus::blocking::connection::Builder::address(address)?
                        .method_timeout(Duration::from_secs(2))
                        .build(),
                    None => {
                        zbus::blocking::connection::Builder::session()?.method_timeout(Duration::from_secs(2)).build()
                    }
                };
                let art = {
                    let sender = sender.clone();
                    crate::art::Fetcher::start(crate::art::cache_dir(), move |_| {
                        let _ = sender.send(Msg::ArtReady);
                    })
                };
                match connect() {
                    Ok(connection) => Players::new(connection, shared, notifier, art).run(&sender, &incoming),
                    Err(error) => tracing::warn!("media players unavailable, no session bus: {error}"),
                }
            })
            .map_err(|e| tracing::error!("could not start the media worker: {e}"))
            .ok();
        Self { messages, thread }
    }

    pub fn send(&self, command: PlayerCommand) {
        let _ = self.messages.send(Msg::Command(command));
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

struct Players {
    connection: Connection,
    shared: Shared,
    notifier: Notifier,
    /// Players by well-known name.
    players: BTreeMap<String, Player>,
    /// Unique name to well-known name.
    owners: HashMap<String, String>,
    /// When each player last started playing, as a counter.
    started: HashMap<String, u64>,
    /// The player last controlled from here.
    used: Option<String>,
    clock: u64,
    art: crate::art::Fetcher,
}

impl Players {
    fn new(connection: Connection, shared: Shared, notifier: Notifier, art: crate::art::Fetcher) -> Self {
        Self {
            connection,
            shared,
            notifier,
            players: BTreeMap::new(),
            owners: HashMap::new(),
            started: HashMap::new(),
            used: None,
            clock: 0,
            art,
        }
    }

    fn run(mut self, sender: &mpsc::Sender<Msg>, incoming: &mpsc::Receiver<Msg>) {
        if let Err(error) = self.listen(sender.clone()) {
            tracing::warn!("cannot follow media players: {error}");
            return;
        }
        let names = self
            .call::<_, Vec<String>>(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ListNames",
                &(),
            )
            .unwrap_or_default();
        for name in names.into_iter().filter(|n| is_player(n)) {
            self.add(&name);
        }
        self.publish();

        loop {
            let Ok(first) = incoming.recv() else { return };
            let mut dirty = BTreeSet::new();
            for msg in std::iter::once(first).chain(std::iter::from_fn(|| incoming.try_recv().ok())) {
                match msg {
                    Msg::Stop => return,
                    Msg::Command(command) => self.command(command),
                    Msg::Owner { name, owner: None } => {
                        self.players.remove(&name);
                        self.started.remove(&name);
                        self.owners.retain(|_, n| *n != name);
                    }
                    Msg::Owner { name, owner: Some(_) } => {
                        self.owners.retain(|_, n| *n != name);
                        dirty.insert(name);
                    }
                    Msg::ArtReady => dirty.extend(self.players.keys().cloned()),
                    Msg::Changed(unique) => {
                        if let Some(name) = self.owners.get(&unique) {
                            dirty.insert(name.clone());
                        }
                    }
                }
            }
            for name in dirty {
                self.add(&name);
            }
            self.publish();
        }
    }

    /// Forwards the bus signals that matter onto the worker's queue.
    fn listen(&self, sender: mpsc::Sender<Msg>) -> zbus::Result<()> {
        let rules = [
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .arg0ns(ROOT)?
                .build(),
            MatchRule::builder()
                .msg_type(Type::Signal)
                .path(PATH)?
                .interface(PROPERTIES)?
                .member("PropertiesChanged")?
                .build(),
            MatchRule::builder().msg_type(Type::Signal).path(PATH)?.interface(PLAYER)?.member("Seeked")?.build(),
        ];
        let iterators = rules
            .into_iter()
            .map(|rule| zbus::blocking::MessageIterator::for_match_rule(rule, &self.connection, Some(64)));
        for iterator in iterators {
            let iterator = iterator?;
            let sender = sender.clone();
            thread::Builder::new()
                .name("tileception-mpris-signals".into())
                .spawn(move || {
                    for message in iterator.flatten() {
                        let header = message.header();
                        let msg = if header.member().is_some_and(|m| m.as_str() == "NameOwnerChanged") {
                            let Ok((name, _, owner)) = message.body().deserialize::<(String, String, String)>() else {
                                continue;
                            };
                            if !is_player(&name) {
                                continue;
                            }
                            Msg::Owner { name, owner: Some(owner).filter(|o| !o.is_empty()) }
                        } else if let Some(unique) = header.sender() {
                            Msg::Changed(unique.to_string())
                        } else {
                            continue;
                        };
                        if sender.send(msg).is_err() {
                            return;
                        }
                    }
                })
                .map_err(|e| zbus::Error::Failure(e.to_string()))?;
        }
        Ok(())
    }

    fn call<B, R>(&self, destination: &str, path: &str, interface: &str, method: &str, body: &B) -> zbus::Result<R>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        let reply = self.connection.call_method(Some(destination), path, Some(interface), method, body)?;
        reply.body().deserialize::<R>()
    }

    /// Reads a player's state afresh, adding it if new.
    fn add(&mut self, name: &str) {
        let read = || -> zbus::Result<(String, Properties, Properties)> {
            let owner: String = self.call(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetNameOwner",
                &(name,),
            )?;
            let root: HashMap<String, OwnedValue> = self.call(name, PATH, PROPERTIES, "GetAll", &(ROOT,))?;
            let player: HashMap<String, OwnedValue> = self.call(name, PATH, PROPERTIES, "GetAll", &(PLAYER,))?;
            Ok((owner, root, player))
        };
        let (owner, root, props) = match read() {
            Ok(read) => read,
            Err(error) => {
                tracing::debug!("media player {name} did not answer: {error}");
                self.players.remove(name);
                return;
            }
        };
        let art = &mut self.art;
        let player = parse(name, &root, &props, Instant::now(), &mut |url| art.get(url));
        let was_playing = self.players.get(name).is_some_and(|p| p.status == Status::Playing);
        if player.status == Status::Playing && (!was_playing || !self.started.contains_key(name)) {
            self.clock += 1;
            self.started.insert(name.to_string(), self.clock);
        }
        self.owners.insert(owner, name.to_string());
        self.players.insert(name.to_string(), player);
    }

    fn active(&self) -> Option<&str> {
        let playing = self
            .players
            .iter()
            .filter(|(_, p)| p.status == Status::Playing)
            .max_by_key(|(name, _)| self.started.get(*name).copied().unwrap_or(0))
            .map(|(name, _)| name.as_str());
        playing.or_else(|| self.used.as_deref().filter(|n| self.players.contains_key(*n))).or_else(|| {
            self.players.keys().max_by_key(|name| self.started.get(*name).copied().unwrap_or(0)).map(String::as_str)
        })
    }

    fn command(&mut self, command: PlayerCommand) {
        let Some(name) = self.active().map(str::to_string) else {
            // Nothing to control; the compositor still hears back.
            self.notifier.send(Event::Media);
            return;
        };
        let method = match command {
            PlayerCommand::PlayPause => "PlayPause",
            PlayerCommand::Play => "Play",
            PlayerCommand::Pause => "Pause",
            PlayerCommand::Next => "Next",
            PlayerCommand::Previous => "Previous",
            PlayerCommand::Stop => "Stop",
        };
        if let Err(error) = self.connection.call_method(Some(name.as_str()), PATH, Some(PLAYER), method, &()) {
            tracing::warn!("{name} refused {method}: {error}");
        }
        self.used = Some(name);
    }

    fn publish(&self) {
        let active = self.active();
        let media = Media {
            connected: true,
            active: active.and_then(|a| self.players.keys().position(|n| n == a)),
            players: self.players.values().cloned().collect(),
        };
        let mut snapshot = lock(&self.shared);
        if snapshot.media != media {
            snapshot.media = media;
            drop(snapshot);
            self.notifier.send(Event::Media);
        }
    }
}

/// A player's bus name. playerctld only relays the others, so it would show
/// every player twice.
fn is_player(name: &str) -> bool {
    name.starts_with(PREFIX) && name != "org.mpris.MediaPlayer2.playerctld"
}

fn parse(
    name: &str,
    root: &HashMap<String, OwnedValue>,
    props: &HashMap<String, OwnedValue>,
    now: Instant,
    art: &mut dyn FnMut(&str) -> Option<PathBuf>,
) -> Player {
    let get = |map: &HashMap<String, OwnedValue>, key: &str| map.get(key).map(|v| Value::from(v.clone()));
    let flag = |key: &str| get(props, key).and_then(|v| boolean(&v)).unwrap_or(false);
    let metadata = get(props, "Metadata").map(|v| dict(&v)).unwrap_or_default();
    let meta = |key: &str| metadata.get(key);
    let status = match get(props, "PlaybackStatus").and_then(|v| string(&v)).as_deref() {
        Some("Playing") => Status::Playing,
        Some("Paused") => Status::Paused,
        _ => Status::Stopped,
    };
    let micros = |v: Option<i64>| v.and_then(|us| u64::try_from(us).ok()).map(Duration::from_micros);
    Player {
        bus_name: name.to_string(),
        identity: get(root, "Identity")
            .and_then(|v| string(&v))
            .unwrap_or_else(|| name.trim_start_matches(PREFIX).split('.').next().unwrap_or(name).to_string()),
        desktop_entry: get(root, "DesktopEntry").and_then(|v| string(&v)).filter(|s| !s.is_empty()),
        status,
        title: meta("xesam:title").and_then(string).unwrap_or_default(),
        artist: meta("xesam:artist").map(strings).unwrap_or_default().join(", "),
        album: meta("xesam:album").and_then(string).unwrap_or_default(),
        art: meta("mpris:artUrl").and_then(string).and_then(|url| art(&url)),
        length: micros(meta("mpris:length").and_then(integer)).filter(|d| !d.is_zero()),
        can_next: flag("CanGoNext"),
        can_previous: flag("CanGoPrevious"),
        can_pause: flag("CanPause"),
        can_play: flag("CanPlay"),
        position: micros(get(props, "Position").and_then(|v| integer(&v))).unwrap_or_default(),
        position_at: now,
        rate: get(props, "Rate").and_then(|v| float(&v)).unwrap_or(1.0),
    }
}

fn string(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        Value::Value(inner) => string(inner),
        _ => None,
    }
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items.iter().filter_map(string).collect(),
        Value::Value(inner) => strings(inner),
        other => string(other).into_iter().collect(),
    }
}

fn integer(v: &Value) -> Option<i64> {
    match v {
        Value::I64(n) => Some(*n),
        Value::U64(n) => i64::try_from(*n).ok(),
        Value::I32(n) => Some(i64::from(*n)),
        Value::U32(n) => Some(i64::from(*n)),
        Value::F64(n) => Some(*n as i64),
        Value::Value(inner) => integer(inner),
        _ => None,
    }
}

fn float(v: &Value) -> Option<f64> {
    match v {
        Value::F64(n) => Some(*n),
        Value::Value(inner) => float(inner),
        other => integer(other).map(|n| n as f64),
    }
}

fn boolean(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Value(inner) => boolean(inner),
        _ => None,
    }
}

fn dict<'a>(v: &Value<'a>) -> HashMap<String, Value<'a>> {
    match v {
        Value::Dict(d) => d.iter().filter_map(|(k, v)| Some((string(k)?, v.try_clone().ok()?))).collect(),
        Value::Value(inner) => dict(inner),
        _ => HashMap::new(),
    }
}

/// A `file://` URL as a path.
pub(crate) fn local_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).and_then(|b| hex(*b)), bytes.get(i + 2).and_then(|b| hex(*b))) {
            (b'%', Some(hi), Some(lo)) => {
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            (b, ..) => {
                out.push(b);
                i += 1;
            }
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_urls() {
        assert_eq!(local_path("file:///tmp/My%20Cover.jpg"), Some(PathBuf::from("/tmp/My Cover.jpg")));
        assert_eq!(local_path("https://i.scdn.co/image/abc"), None);
        assert_eq!(local_path("file:///a%2"), Some(PathBuf::from("/a%2")));
    }
}
