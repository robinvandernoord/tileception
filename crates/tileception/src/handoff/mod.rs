//! Handoff: one mouse, keyboard, clipboard and windows across Tileception
//! machines.
//!
//! Machines with the same `[handoff] passphrase` find each other and connect
//! (see the tileception-handoff crate). Each user then drags a card naming
//! the other machine to an edge of one of their screens; once both have,
//! the pointer crosses at those edges. Nothing about the arrangement is
//! saved: every new connection asks again. Locking pauses sharing until it
//! is resumed by hand.

mod clipboard;
mod forward;
pub mod pair;
mod prompt;
mod stream;
mod windows;

use std::{collections::BTreeMap, sync::Mutex};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::{RegistrationToken, channel};
use tileception_handoff::{DEFAULT_PORT, Event, Held, Message, PeerId, Service, Settings};
use zeroize::Zeroizing;

pub use clipboard::RemoteClip;

use tileception_notify::Urgency;

use crate::Tileception;

/// `[handoff]` in config.toml.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HandoffConfig {
    pub enabled: bool,
    /// The same on every machine that may share. Nothing listens without one.
    pub passphrase: Option<String>,
    /// What the other machines call this one; the host name by default.
    pub name: Option<String>,
    pub port: u16,
    /// Fixed addresses (`host` or `host:port`) for networks that block mDNS.
    pub peers: Vec<String>,
    /// Announce this machine and look for others on the local network.
    pub discovery: bool,
}

impl Default for HandoffConfig {
    fn default() -> Self {
        HandoffConfig {
            enabled: false,
            passphrase: None,
            name: None,
            port: DEFAULT_PORT,
            peers: Vec::new(),
            discovery: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    pub fn parse(name: &str) -> Option<Side> {
        match name {
            "left" => Some(Side::Left),
            "right" => Some(Side::Right),
            "top" => Some(Side::Top),
            "bottom" => Some(Side::Bottom),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
            Side::Top => "top",
            Side::Bottom => "bottom",
        }
    }
}

/// One edge of one of this machine's screens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub output: String,
    pub side: Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pause {
    /// This machine locked; its user resumes.
    Ours,
    /// The other machine locked.
    Theirs,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Link {
    /// Waiting for the edges: ours once this user dropped the card, theirs
    /// once the other machine's user did.
    Asking {
        ours: Option<Edge>,
        theirs: bool,
    },
    Linked {
        edge: Edge,
        paused: Option<Pause>,
    },
    /// Someone said no; asked again only after `handoff retry` or when the
    /// other machine's user places this one.
    Dismissed,
}

pub(crate) struct Peer {
    pub name: String,
    pub link: Link,
}

#[derive(Default)]
pub struct Handoff {
    service: Option<Service>,
    token: Option<RegistrationToken>,
    pub(crate) peers: BTreeMap<PeerId, Peer>,
    /// Where this machine's own mouse and keyboard go instead of here.
    pub(crate) forwarding: Option<PeerId>,
    /// The pointer here went back to the machine that was using it: hidden
    /// until it is used here again, so only the machine with input shows one.
    pub(crate) away: bool,
    /// How far the pointer has pushed past a shared edge so far.
    pub(crate) push: f64,
    /// Keys and buttons held, per machine they came from.
    pub(crate) held: Held,
    pub(crate) prompt: prompt::Prompt,
    pub(crate) clipboard: clipboard::Clipboard,
    /// Whether sharing has been paused for a lock that has not been resumed.
    locked: bool,
    /// Windows shown on other machines, and theirs shown here.
    pub(crate) windows: windows::Windows,
    /// The last absolute pointer position, to turn absolute motion into steps.
    pub(crate) absolute: Option<smithay::utils::Point<f64, smithay::utils::Logical>>,
}

impl Handoff {
    pub(crate) fn send(&self, peer: PeerId, message: Message) -> bool {
        self.service.as_ref().is_some_and(|service| service.send(peer, message))
    }

    pub(crate) fn messenger(&self) -> Option<tileception_handoff::Messenger> {
        self.service.as_ref().map(Service::messenger)
    }

    /// The edge where the pointer may cross to this machine, when it may.
    pub(crate) fn active_edge(&self, peer: PeerId) -> Option<&Edge> {
        match &self.peers.get(&peer)?.link {
            Link::Linked { edge, paused: None } => Some(edge),
            _ => None,
        }
    }

    /// The machine linked at this edge, when sharing with it is on.
    pub(crate) fn peer_at(&self, output: &str, side: Side) -> Option<PeerId> {
        self.peers.iter().find_map(|(peer, entry)| match &entry.link {
            Link::Linked { edge, paused: None } if edge.output == output && edge.side == side => Some(*peer),
            _ => None,
        })
    }

    /// Whether an edge is spoken for already, chosen or linked.
    pub(crate) fn edge_taken(&self, output: &str, side: Side) -> bool {
        self.peers.values().any(|entry| match &entry.link {
            Link::Linked { edge, .. } | Link::Asking { ours: Some(edge), .. } => {
                edge.output == output && edge.side == side
            }
            _ => false,
        })
    }

    /// A peer by name (any case) or hex id; the only one when none is given.
    fn find(&self, name: Option<&str>) -> Result<PeerId, String> {
        let Some(name) = name else {
            let mut peers = self.peers.keys();
            return match (peers.next(), peers.next()) {
                (Some(peer), None) => Ok(*peer),
                (None, _) => Err("no machine is connected".into()),
                _ => Err("more than one machine is connected; name one".into()),
            };
        };
        self.peers
            .iter()
            .find(|(peer, entry)| entry.name.eq_ignore_ascii_case(name) || format!("{peer:016x}") == name)
            .map(|(peer, _)| *peer)
            .ok_or_else(|| format!("no connected machine called {name:?}"))
    }
}

/// `handoff ...` commands, for bindings and `tileception msg`.
#[derive(Debug, Clone, PartialEq)]
pub enum HandoffCommand {
    Status,
    /// Places a machine at an edge, as dropping its card there does.
    Edge {
        peer: Option<String>,
        output: String,
        side: Side,
    },
    Dismiss(Option<String>),
    Retry,
    Resume,
    /// Brings this machine's input back from another machine.
    Home,
    /// Brings this machine's windows back from other machines.
    Reclaim,
}

pub fn parse(args: &[&str]) -> Result<HandoffCommand, String> {
    let peer = |name: Option<&&str>| name.map(|name| (*name).to_string());
    match args {
        [] | ["status"] => Ok(HandoffCommand::Status),
        ["edge", rest @ ..] => {
            let (peer, output, side) = match rest {
                [output, side] => (None, output, side),
                [name, output, side] => (Some((*name).to_string()), output, side),
                _ => return Err("handoff edge needs [machine] <output> <left|right|top|bottom>".into()),
            };
            let side = Side::parse(side).ok_or("handoff edge: the side is left, right, top or bottom")?;
            Ok(HandoffCommand::Edge { peer, output: (*output).to_string(), side })
        }
        ["dismiss"] | ["dismiss", _] => Ok(HandoffCommand::Dismiss(peer(args.get(1)))),
        ["retry"] => Ok(HandoffCommand::Retry),
        ["resume"] => Ok(HandoffCommand::Resume),
        ["home"] => Ok(HandoffCommand::Home),
        ["reclaim"] => Ok(HandoffCommand::Reclaim),
        _ => Err("handoff takes status, edge, dismiss, retry, resume, home or reclaim".into()),
    }
}

/// The EFF's large list for passphrases (CC BY 3.0 US, eff.org/dice): 7776
/// words, so each one adds almost 13 bits.
const WORDS: &str = include_str!("words.txt");

/// Three random words to suggest as a passphrase. Guessing is only possible
/// against a live machine (SPAKE2), slowed to a few tries a minute, which
/// makes about 39 bits plenty.
pub fn suggest_passphrase() -> Result<String, String> {
    let words: Vec<&str> = WORDS.lines().filter(|word| !word.is_empty()).collect();
    let count = u32::try_from(words.len()).map_err(|_| "word list too long")?;
    // Rejection sampling keeps every word equally likely.
    let limit = u32::MAX - u32::MAX % count;
    let mut chosen = Vec::new();
    while chosen.len() < 3 {
        let mut bytes = [0; 4];
        getrandom::fill(&mut bytes).map_err(|error| format!("no randomness: {error}"))?;
        let number = u32::from_ne_bytes(bytes);
        if number < limit {
            chosen.push(words[(number % count) as usize]);
        }
    }
    Ok(chosen.join(" "))
}

/// What other machines call this one when the config does not say.
fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Tileception".into())
}

impl Tileception {
    /// Starts (or restarts) sharing as configured.
    pub fn start_handoff(&mut self) {
        self.stop_handoff();
        let config = self.config.file.handoff.clone();
        if !config.enabled {
            return;
        }
        let Some(passphrase) = config.passphrase.filter(|passphrase| !passphrase.trim().is_empty()) else {
            tracing::warn!("handoff: enabled without a passphrase; not sharing");
            return;
        };
        if passphrase.chars().count() < 12 {
            tracing::warn!("handoff: the passphrase is short; several random words are much harder to guess");
        }
        let (sender, receiver) = channel::channel::<Event>();
        let token = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.handoff_event(event);
            }
        });
        let token = match token {
            Ok(token) => token,
            Err(error) => return tracing::error!("handoff: {error}"),
        };
        let sender = Mutex::new(sender);
        let settings = Settings {
            name: config.name.unwrap_or_else(host_name),
            passphrase: Zeroizing::new(passphrase),
            port: config.port,
            peers: config.peers,
            discovery: config.discovery,
        };
        let started = Service::start(settings, move |event| {
            let _ = sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(event);
        });
        match started {
            Ok(service) => {
                tracing::info!("handoff: listening on port {}", service.port());
                self.handoff.service = Some(service);
                self.handoff.token = Some(token);
            }
            Err(error) => {
                tracing::warn!("handoff: cannot listen on port {}: {error}", config.port);
                self.loop_handle.remove(token);
            }
        }
    }

    /// Ends all sharing: input comes home, other machines' input and
    /// clipboards are let go.
    pub fn stop_handoff(&mut self) {
        if self.handoff.forwarding.is_some() {
            self.stop_forwarding(forward::Back::Home);
        }
        let peers: Vec<PeerId> = self.handoff.peers.keys().copied().collect();
        for peer in peers {
            self.forget_input(peer);
            self.clipboard_peer_gone(peer);
            self.windows_peer_gone(peer);
        }
        self.handoff.peers.clear();
        self.handoff.prompt = prompt::Prompt::default();
        self.handoff.service = None;
        if let Some(token) = self.handoff.token.take() {
            self.loop_handle.remove(token);
        }
    }

    /// After a config reload.
    pub fn apply_handoff_config(&mut self, old: &HandoffConfig) {
        if *old != self.config.file.handoff {
            self.start_handoff();
        }
    }

    fn handoff_event(&mut self, event: Event) {
        match event {
            Event::Connected { peer, name, .. } => {
                // A new connection asks again, unless this user said no.
                let entry =
                    self.handoff.peers.entry(peer).or_insert(Peer { name: name.clone(), link: Link::Dismissed });
                entry.name = name;
                if entry.link != Link::Dismissed || !self.handoff.prompt.dismissed.contains(&peer) {
                    entry.link = Link::Asking { ours: None, theirs: false };
                }
            }
            Event::Received { peer, message } => self.handoff_message(peer, message),
            Event::Disconnected { peer } => {
                if self.handoff.forwarding == Some(peer) {
                    self.stop_forwarding(forward::Back::Gone);
                }
                self.forget_input(peer);
                self.clipboard_peer_gone(peer);
                self.windows_peer_gone(peer);
                if !self.handoff.prompt.dismissed.contains(&peer) {
                    self.handoff.peers.remove(&peer);
                }
            }
            Event::Refused { .. } => {}
        }
        self.schedule_render();
    }

    fn handoff_message(&mut self, peer: PeerId, message: Message) {
        let Some(entry) = self.handoff.peers.get_mut(&peer) else { return };
        match message {
            Message::Arranged => match &mut entry.link {
                Link::Asking { ours, theirs } => {
                    *theirs = true;
                    if let Some(edge) = ours.take() {
                        entry.link = Link::Linked { edge, paused: None };
                        self.linked(peer);
                    }
                }
                Link::Dismissed => {
                    // The other user placed this machine: that is asking again.
                    self.handoff.prompt.dismissed.remove(&peer);
                    entry.link = Link::Asking { ours: None, theirs: true };
                }
                Link::Linked { .. } => {}
            },
            Message::Dismissed => {
                entry.link = Link::Dismissed;
                self.handoff.prompt.dismissed.insert(peer);
                self.unlink(peer);
                self.windows_peer_gone(peer);
            }
            Message::Paused => {
                if let Link::Linked { paused, .. } = &mut entry.link
                    && paused.is_none()
                {
                    *paused = Some(Pause::Theirs);
                }
                self.unlink(peer);
            }
            Message::Resumed => {
                if self.handoff.locked {
                    self.handoff.send(peer, Message::Paused);
                } else if let Link::Linked { paused, .. } = &mut entry.link {
                    *paused = None;
                }
            }
            Message::Enter { along } => self.remote_enter(peer, along),
            Message::Return { along } => {
                if self.handoff.forwarding == Some(peer) {
                    self.stop_forwarding(forward::Back::Return(along));
                }
            }
            Message::Leave => {
                self.forget_input(peer);
                self.handoff.away = true;
            }
            Message::Motion { right, down } => self.remote_motion(peer, right, down),
            Message::Button { button, pressed } => self.remote_button(peer, button, pressed),
            Message::Scroll(scroll) => self.remote_scroll(peer, scroll),
            Message::Key { key, pressed } => self.remote_key(peer, key, pressed),
            Message::Selection { target, version, mime_types } => {
                self.remote_selection(peer, target, version, mime_types);
            }
            Message::Fetch { request, target, version, mime_type } => {
                self.serve_fetch(peer, request, target, version, mime_type);
            }
            Message::Data { request, bytes } => self.handoff.clipboard.data(request, bytes),
            Message::DataEnd { request, failed } => self.handoff.clipboard.end(request, failed),
            message => self.window_message(peer, message),
        }
    }

    fn linked(&mut self, peer: PeerId) {
        let Some(entry) = self.handoff.peers.get(&peer) else { return };
        let Link::Linked { edge, .. } = &entry.link else { return };
        let body =
            format!("Move the pointer past the {} edge of {} to use {}.", edge.side.name(), edge.output, entry.name);
        let summary = format!("Sharing with {}", entry.name);
        tracing::info!("handoff: linked with {} at {} {}", entry.name, edge.output, edge.side.name());
        self.notify_locally(&summary, &body, Urgency::Normal);
    }

    /// Stops input from and to a machine, keeping the link itself.
    fn unlink(&mut self, peer: PeerId) {
        if self.handoff.forwarding == Some(peer) {
            self.stop_forwarding(forward::Back::Gone);
        }
        self.forget_input(peer);
    }

    /// Called whenever the session locks or unlocks.
    pub fn handoff_lock_changed(&mut self) {
        let locked = self.is_locked();
        if !locked || self.handoff.locked {
            // Unlocking leaves sharing paused; the card offers to resume.
            return;
        }
        self.handoff.locked = true;
        if self.handoff.forwarding.is_some() {
            self.stop_forwarding(forward::Back::Home);
        }
        let peers: Vec<PeerId> = self.handoff.peers.keys().copied().collect();
        for peer in peers {
            self.forget_input(peer);
            if let Some(Peer { link: Link::Linked { paused, .. }, .. }) = self.handoff.peers.get_mut(&peer) {
                *paused = Some(Pause::Ours);
                self.handoff.send(peer, Message::Paused);
            }
        }
        self.handoff.clipboard.fail_all();
    }

    fn handoff_resume(&mut self) -> Result<(), String> {
        if self.is_locked() {
            return Err("the session is locked".into());
        }
        self.handoff.locked = false;
        let mut resumed = false;
        for (peer, entry) in &mut self.handoff.peers {
            if let Link::Linked { paused: paused @ Some(Pause::Ours), .. } = &mut entry.link {
                *paused = None;
                self.handoff.service.as_ref().map(|service| service.send(*peer, Message::Resumed));
                resumed = true;
            }
        }
        self.schedule_render();
        if resumed { Ok(()) } else { Err("nothing is paused".into()) }
    }

    pub(crate) fn place_peer(&mut self, peer: PeerId, edge: Edge) -> Result<(), String> {
        if self.output_named(&edge.output).is_none() {
            return Err(format!("unknown output {:?}", edge.output));
        }
        if self.handoff.edge_taken(&edge.output, edge.side) {
            return Err("another machine is at that edge".into());
        }
        if !self.outer_edge(&edge.output, edge.side) {
            return Err("another screen of this machine is at that edge".into());
        }
        let entry = self.handoff.peers.get_mut(&peer).ok_or("that machine is gone")?;
        let theirs = match &entry.link {
            Link::Asking { theirs, .. } => *theirs,
            Link::Dismissed => false,
            Link::Linked { .. } => return Err("already sharing with that machine".into()),
        };
        self.handoff.prompt.dismissed.remove(&peer);
        entry.link =
            if theirs { Link::Linked { edge, paused: None } } else { Link::Asking { ours: Some(edge), theirs: false } };
        self.handoff.send(peer, Message::Arranged);
        if theirs {
            self.linked(peer);
        }
        self.schedule_render();
        Ok(())
    }

    pub(crate) fn dismiss_peer(&mut self, peer: PeerId) {
        if let Some(entry) = self.handoff.peers.get_mut(&peer) {
            entry.link = Link::Dismissed;
        }
        self.handoff.prompt.dismissed.insert(peer);
        self.handoff.send(peer, Message::Dismissed);
        self.unlink(peer);
        self.windows_peer_gone(peer);
        self.schedule_render();
    }

    pub fn handoff_command(&mut self, command: HandoffCommand) -> Result<String, String> {
        match command {
            HandoffCommand::Status => return Ok(self.handoff_status()),
            HandoffCommand::Edge { peer, output, side } => {
                let peer = self.handoff.find(peer.as_deref())?;
                self.place_peer(peer, Edge { output, side })?;
            }
            HandoffCommand::Dismiss(name) => {
                let peer = self.handoff.find(name.as_deref())?;
                self.dismiss_peer(peer);
            }
            HandoffCommand::Retry => {
                self.handoff.prompt.dismissed.clear();
                for entry in self.handoff.peers.values_mut() {
                    if entry.link == Link::Dismissed {
                        entry.link = Link::Asking { ours: None, theirs: false };
                    }
                }
                self.schedule_render();
            }
            HandoffCommand::Resume => self.handoff_resume()?,
            HandoffCommand::Home => {
                if self.handoff.forwarding.is_none() {
                    return Err("input is already here".into());
                }
                self.stop_forwarding(forward::Back::Home);
            }
            HandoffCommand::Reclaim => {
                if self.reclaim_windows() == 0 {
                    return Err("no window of this machine is on another".into());
                }
            }
        }
        Ok(String::new())
    }

    fn handoff_status(&self) -> String {
        if self.handoff.service.is_none() {
            return "handoff is off\n".into();
        }
        let mut status = String::new();
        if let Some(service) = &self.handoff.service {
            status.push_str(&format!("listening on port {}\n", service.port()));
        }
        for (peer, entry) in &self.handoff.peers {
            let state = match &entry.link {
                Link::Asking { ours: None, .. } => "asking".to_string(),
                Link::Asking { ours: Some(edge), .. } => {
                    format!("waiting (placed at {} {})", edge.output, edge.side.name())
                }
                Link::Linked { edge, paused } => format!(
                    "linked at {} {}{}",
                    edge.output,
                    edge.side.name(),
                    match paused {
                        None => "",
                        Some(Pause::Ours) => " (paused here)",
                        Some(Pause::Theirs) => " (paused there)",
                    }
                ),
                Link::Dismissed => "dismissed".to_string(),
            };
            let input = if self.handoff.forwarding == Some(*peer) { ", has this machine's input" } else { "" };
            status.push_str(&format!("{} {peer:016x} {state}{input}\n", entry.name));
        }
        for line in self.windows_status() {
            status.push_str(&line);
            status.push('\n');
        }
        status
    }

    /// Whether nothing of this machine lies beyond an edge of a screen.
    pub(crate) fn outer_edge(&self, output: &str, side: Side) -> bool {
        let Some(target) = self.output_named(output) else { return false };
        let Some(rect) = output_bounds(&target) else { return false };
        let beyond = match side {
            Side::Left => tileception_core::Point::new(rect.x - 1.0, rect.y + rect.h / 2.0),
            Side::Right => tileception_core::Point::new(rect.right() + 1.0, rect.y + rect.h / 2.0),
            Side::Top => tileception_core::Point::new(rect.x + rect.w / 2.0, rect.y - 1.0),
            Side::Bottom => tileception_core::Point::new(rect.x + rect.w / 2.0, rect.bottom() + 1.0),
        };
        !self.all_outputs().iter().filter_map(output_bounds).any(|other| other.contains(beyond))
    }
}

/// A screen in the shared desktop's logical coordinates.
pub(crate) fn output_bounds(output: &smithay::output::Output) -> Option<tileception_core::Rect> {
    let mode = output.current_mode()?;
    let origin = output.current_location();
    let size = output
        .current_transform()
        .transform_size(mode.size.to_f64().to_logical(output.current_scale().fractional_scale()));
    Some(tileception_core::Rect::new(f64::from(origin.x), f64::from(origin.y), size.w, size.h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_are_three_listed_words() {
        assert_eq!(WORDS.lines().count(), 7776);
        let phrase = suggest_passphrase().expect("randomness");
        let words: Vec<&str> = phrase.split(' ').collect();
        assert_eq!(words.len(), 3);
        assert!(words.iter().all(|word| WORDS.lines().any(|listed| listed == *word)));
    }

    #[test]
    fn commands_parse() {
        assert_eq!(parse(&[]), Ok(HandoffCommand::Status));
        assert_eq!(
            parse(&["edge", "HDMI-A-1", "left"]),
            Ok(HandoffCommand::Edge { peer: None, output: "HDMI-A-1".into(), side: Side::Left })
        );
        assert_eq!(
            parse(&["edge", "laptop", "eDP-1", "top"]),
            Ok(HandoffCommand::Edge { peer: Some("laptop".into()), output: "eDP-1".into(), side: Side::Top })
        );
        assert!(parse(&["edge", "eDP-1", "middle"]).is_err());
        assert_eq!(parse(&["dismiss", "laptop"]), Ok(HandoffCommand::Dismiss(Some("laptop".into()))));
        assert_eq!(parse(&["home"]), Ok(HandoffCommand::Home));
        assert_eq!(parse(&["reclaim"]), Ok(HandoffCommand::Reclaim));
    }
}
