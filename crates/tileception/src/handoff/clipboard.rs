//! The clipboard and the primary selection across machines.
//!
//! Nothing is announced when something is copied: when the pointer leaves
//! for another machine, or input goes into one of its windows shown here,
//! that machine is told what this one offers (only the types, and only when
//! it changed since). Machines showing this machine's windows are told at
//! once, as copying in one of them there is meant for pasting there. The contents travel when someone
//! pastes there. A selection that came from another machine is never offered
//! back or passed on. Copied files are pasted as copies (see `files`).

use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    sync::mpsc,
    time::Duration,
};

use smithay::wayland::selection::{
    SelectionTarget,
    data_device::{
        clear_data_device_selection, current_data_device_selection_userdata, request_data_device_client_selection,
        set_data_device_selection,
    },
    primary_selection::{
        clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
        set_primary_selection,
    },
};
use tileception_handoff::{CHUNK, Message, PeerId, Target};

use super::files::{FileList, offered_types};
use crate::{Tileception, launcher::Clip};

/// The most one paste carries.
const LIMIT: usize = 64 * 1024 * 1024;
/// A transfer that stalls this long is given up.
pub(crate) const STALL: Duration = Duration::from_secs(10);

/// A selection offered by another machine, as this machine's programs see it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteClip {
    pub peer: PeerId,
    pub target: Target,
    pub version: u64,
    pub mime_types: Vec<String>,
}

enum Owner {
    /// A program (or Tileception) here; no types means it was cleared.
    Here {
        version: u64,
        mime_types: Vec<String>,
    },
    Remote(PeerId),
}

#[derive(Default)]
pub struct Clipboard {
    owners: BTreeMap<Target, Owner>,
    /// The version of each selection last offered to each machine.
    offered: BTreeMap<(PeerId, Target), u64>,
    version: u64,
    request: u64,
    /// Pastes here waiting for another machine's data.
    incoming: HashMap<u64, (PeerId, mpsc::Sender<Vec<u8>>)>,
}

impl Clipboard {
    pub(crate) fn data(&mut self, request: u64, bytes: Vec<u8>) {
        let delivered = self.incoming.get(&request).is_some_and(|(_, sender)| sender.send(bytes).is_ok());
        if !delivered {
            self.incoming.remove(&request);
        }
    }

    /// Dropping the sender closes the paste, complete or not.
    pub(crate) fn end(&mut self, request: u64, failed: bool) {
        if failed {
            tracing::debug!("handoff: paste {request} failed on the other machine");
        }
        self.incoming.remove(&request);
    }

    pub(crate) fn fail_all(&mut self) {
        self.incoming.clear();
    }

    /// A number for a request to another machine, unique among this one's.
    pub(crate) fn next_request(&mut self) -> u64 {
        self.request += 1;
        self.request
    }
}

/// Sends what `reader` gives to another machine as the answer to its
/// `request`, from a thread of its own. False when that could not start.
pub(crate) fn send_out(
    messenger: tileception_handoff::Messenger,
    peer: PeerId,
    request: u64,
    mut reader: impl Read + Send + 'static,
) -> bool {
    let started = std::thread::Builder::new().name("tileception-handoff-copy".into()).spawn(move || {
        let mut total = 0;
        let failed = loop {
            let mut chunk = vec![0; CHUNK];
            match reader.read(&mut chunk) {
                Ok(0) => break false,
                Ok(length) => {
                    total += length;
                    chunk.truncate(length);
                    if total > LIMIT || !messenger.send(peer, Message::Data { request, bytes: chunk }) {
                        break true;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break true,
            }
        };
        messenger.send(peer, Message::DataEnd { request, failed });
    });
    started.is_ok()
}

fn selection_target(target: Target) -> SelectionTarget {
    match target {
        Target::Clipboard => SelectionTarget::Clipboard,
        Target::Primary => SelectionTarget::Primary,
    }
}

fn target_of(selection: SelectionTarget) -> Target {
    match selection {
        SelectionTarget::Clipboard => Target::Clipboard,
        SelectionTarget::Primary => Target::Primary,
    }
}

impl Tileception {
    /// Something here copied or cleared a selection.
    pub fn selection_offered(&mut self, selection: SelectionTarget, mime_types: Option<Vec<String>>) {
        let clipboard = &mut self.handoff.clipboard;
        clipboard.version += 1;
        clipboard.owners.insert(
            target_of(selection),
            Owner::Here { version: clipboard.version, mime_types: mime_types.unwrap_or_default() },
        );
        if !self.is_locked() {
            for peer in self.handoff.windows.shown_on() {
                if self.handoff.active_edge(peer).is_some() {
                    self.share_selections(peer);
                }
            }
        }
    }

    /// Tells a machine what this one offers, as input goes there.
    pub(crate) fn share_selections(&mut self, peer: PeerId) {
        for target in [Target::Clipboard, Target::Primary] {
            let Some(Owner::Here { version, mime_types }) = self.handoff.clipboard.owners.get(&target) else {
                continue;
            };
            if self.handoff.clipboard.offered.get(&(peer, target)) == Some(version) {
                continue;
            }
            let message = Message::Selection { target, version: *version, mime_types: mime_types.clone() };
            if self.handoff.send(peer, message) {
                self.handoff.clipboard.offered.insert((peer, target), *version);
            }
        }
    }

    pub(crate) fn remote_selection(&mut self, peer: PeerId, target: Target, version: u64, mime_types: Vec<String>) {
        if self.handoff.active_edge(peer).is_none() {
            return;
        }
        let selection = selection_target(target);
        if mime_types.is_empty() {
            if matches!(self.handoff.clipboard.owners.get(&target), Some(Owner::Remote(owner)) if *owner == peer) {
                self.clear_remote_selection(target);
            }
            return;
        }
        let mime_types = offered_types(mime_types);
        let clip = Some(Clip::Remote(RemoteClip { peer, target, version, mime_types: mime_types.clone() }));
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.display_handle, &self.seat, mime_types.clone(), clip);
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.display_handle, &self.seat, mime_types.clone(), clip)
            }
        }
        if let Some(xwm) = self.xwm.as_mut()
            && let Err(error) = xwm.new_selection(selection, Some(mime_types))
        {
            tracing::warn!("handoff: could not offer the selection to X11: {error:?}");
        }
        self.handoff.clipboard.owners.insert(target, Owner::Remote(peer));
    }

    fn clear_remote_selection(&mut self, target: Target) {
        match target {
            Target::Clipboard => clear_data_device_selection(&self.display_handle, &self.seat),
            Target::Primary => clear_primary_selection(&self.display_handle, &self.seat),
        }
        if let Some(xwm) = self.xwm.as_mut() {
            let _ = xwm.new_selection(selection_target(target), None);
        }
        self.handoff.clipboard.owners.remove(&target);
    }

    /// A machine went away: what it offered can no longer be pasted.
    pub(crate) fn clipboard_peer_gone(&mut self, peer: PeerId) {
        for target in [Target::Clipboard, Target::Primary] {
            if matches!(self.handoff.clipboard.owners.get(&target), Some(Owner::Remote(owner)) if *owner == peer) {
                self.clear_remote_selection(target);
            }
        }
        let clipboard = &mut self.handoff.clipboard;
        clipboard.offered.retain(|(offered_to, _), _| *offered_to != peer);
        clipboard.incoming.retain(|_, (from, _)| *from != peer);
    }

    /// A program here pastes what another machine offered.
    pub fn handoff_fetch(&mut self, clip: &RemoteClip, mime_type: String, pipe: OwnedFd) {
        if self.handoff.active_edge(clip.peer).is_none() || self.is_locked() {
            return;
        }
        let (target, version) = (clip.target, clip.version);
        tracing::info!("handoff: a program here pastes {mime_type} from {:016x}", clip.peer);
        if let Some(list) = FileList::wanted(&mime_type, &clip.mime_types) {
            let message = |request| Message::FetchFiles { request, target, version };
            self.receive_files_into(clip.peer, pipe, list, message);
        } else {
            let message = |request| Message::Fetch { request, target, version, mime_type };
            self.receive_into(clip.peer, pipe, message);
        }
    }

    /// Asks another machine for data, written to `pipe` as it arrives.
    pub(crate) fn receive_into(&mut self, peer: PeerId, pipe: OwnedFd, message: impl FnOnce(u64) -> Message) {
        let request = self.handoff.clipboard.next_request();
        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        let started = std::thread::Builder::new().name("tileception-handoff-paste".into()).spawn(move || {
            let mut file = std::fs::File::from(pipe);
            let mut written = 0;
            while let Ok(bytes) = receiver.recv_timeout(STALL) {
                written += bytes.len();
                if written > LIMIT || file.write_all(&bytes).is_err() {
                    break;
                }
            }
        });
        if started.is_err() {
            return;
        }
        self.handoff.clipboard.incoming.insert(request, (peer, sender));
        if !self.handoff.send(peer, message(request)) {
            self.handoff.clipboard.incoming.remove(&request);
        }
    }

    /// Another machine pastes what this one offered.
    pub(crate) fn serve_fetch(&mut self, peer: PeerId, request: u64, target: Target, version: u64, mime_type: String) {
        let offered = matches!(
            self.handoff.clipboard.owners.get(&target),
            Some(Owner::Here { version: current, mime_types }) if *current == version && mime_types.contains(&mime_type)
        );
        let messenger = self.handoff.messenger();
        let reader = if offered && self.handoff.active_edge(peer).is_some() && !self.is_locked() {
            self.read_selection(selection_target(target), mime_type)
        } else {
            None
        };
        let (Some(reader), Some(messenger)) = (reader, messenger) else {
            self.handoff.send(peer, Message::DataEnd { request, failed: true });
            return;
        };
        if !send_out(messenger, peer, request, reader) {
            self.handoff.send(peer, Message::DataEnd { request, failed: true });
        }
    }

    /// Another machine pastes the files this one copied.
    pub(crate) fn serve_fetch_files(&mut self, peer: PeerId, request: u64, target: Target, version: u64) {
        let offered = matches!(
            self.handoff.clipboard.owners.get(&target),
            Some(Owner::Here { version: current, mime_types })
                if *current == version && mime_types.iter().any(|mime| mime == "text/uri-list")
        );
        let list = if offered && self.handoff.active_edge(peer).is_some() && !self.is_locked() {
            self.read_selection(selection_target(target), "text/uri-list".into())
        } else {
            None
        };
        self.serve_files(peer, request, list);
    }

    /// Asks whoever owns a selection here to write it out, returning the
    /// end to read from.
    fn read_selection(&mut self, selection: SelectionTarget, mime_type: String) -> Option<UnixStream> {
        let (reader, writer) = UnixStream::pair().ok()?;
        reader.set_read_timeout(Some(STALL)).ok()?;
        let owned = match selection {
            SelectionTarget::Clipboard => {
                current_data_device_selection_userdata(&self.seat).and_then(|data| data.clone())
            }
            SelectionTarget::Primary => current_primary_selection_userdata(&self.seat).and_then(|data| data.clone()),
        };
        if let Some(clip) = owned {
            clip.send(writer.into());
            return Some(reader);
        }
        let pipe: OwnedFd = writer.try_clone().ok()?.into();
        let requested = match selection {
            SelectionTarget::Clipboard => {
                request_data_device_client_selection(&self.seat, mime_type.clone(), pipe).is_ok()
            }
            SelectionTarget::Primary => request_primary_client_selection(&self.seat, mime_type.clone(), pipe).is_ok(),
        };
        if !requested {
            // Owned by an X11 program.
            let xwm = self.xwm.as_mut()?;
            xwm.send_selection(selection, mime_type, writer.into(), self.loop_handle.clone()).ok()?;
        }
        Some(reader)
    }
}
