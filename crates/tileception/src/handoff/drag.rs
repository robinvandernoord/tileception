//! Drag and drop across machines.
//!
//! The same goes for windows shown on another machine: a drag a program
//! starts in one of this machine's windows shown elsewhere, with the pointer
//! there, is carried on there; and a drag from there over one of this
//! machine's windows is carried on here, inside that window, moved by the
//! pointer there.
//!
//! A program's drag that reaches a shared edge goes along with the pointer.
//! The program here keeps dragging, with nothing under it, while the other
//! machine carries the drag on as its own, offering the same types and
//! showing the same card by the pointer. Dropped there on a program that
//! wants it, that machine fetches the data from here, as a paste does.
//! Files are copied over first (see `files`), and the program gets those
//! copies. Once the drag is over there, it is let go of here too; dragged
//! back, it carries on here.

use std::os::{fd::OwnedFd, unix::net::UnixStream};

use smithay::{
    backend::input::ButtonState,
    input::pointer::{ButtonEvent, GrabStartData, MotionEvent},
    reexports::wayland_server::protocol::wl_data_device_manager::DndAction,
    utils::SERIAL_COUNTER,
    wayland::selection::data_device::{SourceMetadata, start_dnd},
};
use tileception_core::WindowId;
use tileception_handoff::{DragKind, DragPreview, Message, PeerId, Source};

use super::{
    clipboard::{STALL, send_out},
    files::{FileList, offered_types},
};
use crate::{Tileception, drag::Card};

#[derive(Default)]
pub(crate) struct Drags {
    /// A drag of a program here that went to another machine.
    outgoing: Option<Outgoing>,
    /// Another machine's drag, arrived ahead of its pointer.
    pending: Option<Incoming>,
    /// Another machine's drag, carried on here.
    incoming: Option<Incoming>,
    next: u64,
    /// The other machine's window here whose owner refused this machine's
    /// drag; not asked again until the pointer leaves it.
    refused: Option<WindowId>,
}

struct Outgoing {
    peer: PeerId,
    drag: u64,
    button: u32,
    /// Whose button holds the drag here.
    holder: Source,
    /// Over the other machine's window shown here (this machine's number
    /// for it): the pointer stays here.
    into: Option<WindowId>,
    /// Whether the other machine knows what it carries.
    preview_sent: bool,
}

struct Incoming {
    peer: PeerId,
    drag: u64,
    button: u32,
    mime_types: Vec<String>,
    card: Card,
    /// Whose button holds the drag here, once it is.
    holder: Source,
    /// Dropped: it ends when the program it was dropped on is done with it.
    dropped: bool,
    /// Inside this machine's window shown on the other machine, moved by
    /// the pointer there.
    into: Option<WindowId>,
}

impl Drags {
    /// Whether this machine's drag went to another machine with the pointer.
    pub(crate) fn sent_away(&self) -> bool {
        self.outgoing.as_ref().is_some_and(|outgoing| outgoing.into.is_none())
    }

    /// Whether a program here drags for another machine, which carries it.
    pub(crate) fn holding_for_elsewhere(&self) -> bool {
        self.outgoing.is_some()
    }

    /// Whether a program here drags for `peer`, which carries it with its
    /// pointer (not inside one of its windows shown here).
    pub(crate) fn holding_for(&self, peer: PeerId) -> bool {
        self.outgoing.as_ref().is_some_and(|outgoing| outgoing.peer == peer && outgoing.into.is_none())
    }

    /// Whether another machine's drag is carried on here.
    pub(crate) fn carrying(&self) -> bool {
        self.incoming.is_some()
    }

    /// The card of another machine's drag carried on here, by the pointer.
    pub(crate) fn incoming_card(&self) -> Option<&Card> {
        self.incoming
            .as_ref()
            .filter(|incoming| !incoming.dropped && incoming.into.is_none())
            .map(|incoming| &incoming.card)
    }
}

impl Tileception {
    pub(crate) fn drag_message(&mut self, peer: PeerId, message: Message) {
        match message {
            Message::DragStart { drag, mime_types, button, preview, into } => {
                if self.handoff.active_edge(peer).is_none() || self.is_locked() {
                    self.handoff.send(peer, Message::DragEnd { drag });
                    return;
                }
                let into_here = into.and_then(|window| self.handoff.windows.away_id(peer, window));
                if into.is_some() && (into_here.is_none() || self.handoff.drags.incoming.is_some()) {
                    self.handoff.send(peer, Message::DragEnd { drag });
                    return;
                }
                let mime_types = offered_types(mime_types);
                let card = Card::new(preview);
                self.handoff.drags.pending = Some(Incoming {
                    peer,
                    drag,
                    button,
                    mime_types,
                    card,
                    holder: Source::Peer(peer),
                    dropped: false,
                    into: into_here,
                });
                if into_here.is_some() {
                    // Into one of this machine's windows there: moved by
                    // that machine's messages, not by its held input here.
                    self.start_incoming_drag(peer, Source::Peer(peer), false);
                } else if self.handoff.windows.let_go_of_pressed(peer) {
                    // Started in one of that machine's windows here, with
                    // this machine's own button: the drag is this pointer's.
                    self.start_incoming_drag(peer, Source::Local, true);
                }
            }
            Message::DragUpdate { drag, preview } => {
                let drags = &mut self.handoff.drags;
                for incoming in [drags.pending.as_mut(), drags.incoming.as_mut()].into_iter().flatten() {
                    if incoming.peer == peer && incoming.drag == drag {
                        incoming.card = Card::new(preview.clone());
                    }
                }
            }
            Message::DragFetch { request, drag, mime_type } => self.serve_drag(peer, request, drag, &mime_type),
            Message::DragFiles { request, drag } => self.serve_drag_files(peer, request, drag),
            Message::DragFile { request, path, directory } => self.handoff.files.entry(request, path, directory),
            Message::DragLink { request, uri } => self.handoff.files.link(request, uri),
            Message::DragEnd { drag } => {
                let drags = &mut self.handoff.drags;
                if drags.pending.as_ref().is_some_and(|pending| pending.peer == peer && pending.drag == drag) {
                    drags.pending = None;
                }
                if drags.incoming.as_ref().is_some_and(|incoming| incoming.peer == peer && incoming.drag == drag) {
                    self.cancel_incoming_drag();
                }
                let ours = self.handoff.drags.outgoing.as_ref().is_some_and(|out| out.peer == peer && out.drag == drag);
                if ours {
                    let outgoing = self.handoff.drags.outgoing.as_ref().expect("checked above");
                    let still_held = self.handoff.held.buttons(Source::Local).contains(&outgoing.button);
                    if let Some(window) = outgoing.into.filter(|_| still_held) {
                        // Refused, or dropped nowhere inside that window
                        // with the button still down here: it goes on here.
                        self.handoff.drags.outgoing = None;
                        self.handoff.drags.refused = Some(window);
                    } else {
                        self.let_go_of_outgoing();
                    }
                }
            }
            _ => {}
        }
        self.schedule_render();
    }

    /// Whether the drag a button of `source` holds may cross to `peer`: a
    /// program's drag here, or another machine's going back to it.
    pub(crate) fn may_carry_drag(&self, peer: PeerId, source: Source) -> bool {
        if let Some(incoming) = &self.handoff.drags.incoming {
            return incoming.peer == peer && incoming.holder == source && !incoming.dropped;
        }
        self.drag.as_ref().is_some_and(|drag| drag.source.is_some()) && self.handoff.drags.outgoing.is_none()
    }

    /// The pointer crosses to `peer` with a drag held by `source`: it goes
    /// along, or back to the machine it came from.
    pub(crate) fn carry_drag(&mut self, peer: PeerId, source: Source) {
        if self.handoff.drags.incoming.as_ref().is_some_and(|incoming| incoming.peer == peer) {
            // Back to its own machine, which still holds it.
            self.cancel_incoming_drag();
            return;
        }
        if !self.handoff.held.holds_button(source) || !self.may_carry_drag(peer, source) {
            return;
        }
        let Some(button) = self.handoff.held.buttons(source).first().copied() else { return };
        self.send_drag(peer, button, source, None);
    }

    /// Hands this machine's program's drag, held by `button` of `holder`, to
    /// `peer`: with the pointer, or over `into` (that machine's window shown
    /// here, and its number for it).
    fn send_drag(&mut self, peer: PeerId, button: u32, holder: Source, into: Option<(WindowId, u64)>) -> bool {
        let Some(drag) = self.drag.as_mut().filter(|drag| drag.source.is_some()) else { return false };
        let known = drag.preview().cloned();
        let preview_sent = known.is_some();
        let preview = known.unwrap_or_else(|| DragPreview {
            label: "Dragging".into(),
            count: 1,
            kind: DragKind::Other,
            thumbnail: None,
        });
        let mime_types = drag.mime_types.clone();
        let drags = &mut self.handoff.drags;
        drags.next += 1;
        let drag_id = drags.next;
        let message =
            Message::DragStart { drag: drag_id, mime_types, button, preview, into: into.map(|(_, owned)| owned) };
        if !self.handoff.send(peer, message) {
            return false;
        }
        // Another machine's button would be let go of as its input goes;
        // this one lets go of it when the drag is over there.
        if matches!(holder, Source::Peer(_)) {
            self.handoff.held.button(holder, button, false);
        }
        let into = into.map(|(here, _)| here);
        self.handoff.drags.outgoing = Some(Outgoing { peer, drag: drag_id, button, holder, into, preview_sent });
        tracing::info!("handoff: a drag goes to {peer:016x}");
        // Nothing here is under it any more.
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let time = self.input_time();
        let location = pointer.current_location();
        pointer.motion(self, None, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
        true
    }

    /// Tells the machine carrying this machine's drag what it carries, once
    /// that is known. Called once a frame.
    pub fn send_drag_preview(&mut self) {
        let Some(outgoing) = self.handoff.drags.outgoing.as_ref().filter(|outgoing| !outgoing.preview_sent) else {
            return;
        };
        let (peer, drag) = (outgoing.peer, outgoing.drag);
        let Some(preview) = self.drag.as_mut().and_then(|local| local.preview().cloned()) else { return };
        self.handoff.send(peer, Message::DragUpdate { drag, preview });
        if let Some(outgoing) = self.handoff.drags.outgoing.as_mut() {
            outgoing.preview_sent = true;
        }
    }

    /// A program here started a drag with another machine's pointer, in one
    /// of this machine's windows shown there: that machine carries it on.
    pub(crate) fn drag_out_of_away(&mut self) {
        let Some((peer, button)) = self.handoff.windows.dragging_from_away() else { return };
        if self.handoff.active_edge(peer).is_some() && !self.is_locked() {
            self.send_drag(peer, button, Source::Peer(peer), None);
        }
    }

    /// The pointer here moved over `target` (another machine's window, or
    /// nothing) while a program here drags: the drag goes into that window,
    /// carried on by its owner, or comes back out of one.
    pub(crate) fn drag_over_remote(&mut self, target: Option<(WindowId, PeerId, u64)>) {
        let current = self.handoff.drags.outgoing.as_ref().and_then(|outgoing| outgoing.into);
        if current.is_some() && current != target.map(|(window, ..)| window) {
            // Out of it again: the owner there was told by WindowLeave.
            self.handoff.drags.outgoing = None;
        }
        let drags = &mut self.handoff.drags;
        if drags.refused.is_some() && drags.refused != target.map(|(window, ..)| window) {
            drags.refused = None;
        }
        let Some((window, peer, owned)) = target else { return };
        if self.handoff.drags.refused == Some(window) {
            return;
        }
        let local = self.drag.as_ref().is_some_and(|drag| drag.source.is_some());
        if !local || self.handoff.drags.outgoing.is_some() || self.handoff.drags.incoming.is_some() {
            return;
        }
        let Some(button) = self.handoff.held.buttons(Source::Local).first().copied() else { return };
        self.send_drag(peer, button, Source::Local, Some((window, owned)));
    }

    /// The pointer of the machine dragging into one of this machine's
    /// windows left it: that machine takes the drag back.
    pub(crate) fn drag_left_window(&mut self, peer: PeerId) {
        if self.handoff.drags.incoming.as_ref().is_some_and(|incoming| incoming.peer == peer && incoming.into.is_some())
        {
            self.cancel_incoming_drag();
        }
    }

    /// The pointer arrived from `peer`, its buttons now held by `source`:
    /// a drag that came with it is carried on here, or one of this
    /// machine's own that went there is back.
    pub(crate) fn pick_up_drag(&mut self, peer: PeerId, source: Source) {
        if let Some(outgoing) = self.handoff.drags.outgoing.take_if(|outgoing| outgoing.peer == peer) {
            if matches!(outgoing.holder, Source::Peer(_)) {
                self.handoff.held.button(source, outgoing.button, true);
            }
            tracing::info!("handoff: a drag is back from {peer:016x}");
            return;
        }
        self.start_incoming_drag(peer, source, true);
    }

    /// Carries on the drag from `peer` that arrived ahead, held by `source`
    /// (and its button marked held here when `hold`).
    fn start_incoming_drag(&mut self, peer: PeerId, source: Source, hold: bool) {
        let Some(mut incoming) = self.handoff.drags.pending.take_if(|pending| pending.peer == peer) else { return };
        incoming.holder = source;
        if hold {
            self.handoff.held.button(source, incoming.button, true);
        }
        let metadata = SourceMetadata { mime_types: incoming.mime_types.clone(), dnd_action: DndAction::Copy };
        let location = self.seat.get_pointer().map_or(self.pointer, |pointer| pointer.current_location());
        let start = GrabStartData { focus: None, button: incoming.button, location };
        tracing::info!("handoff: a drag from {peer:016x} goes on here");
        self.handoff.drags.incoming = Some(incoming);
        let (display, seat) = (self.display_handle.clone(), self.seat.clone());
        start_dnd(&display, &seat, self, SERIAL_COUNTER.next_serial(), Some(start), None, metadata);
    }

    /// Another machine's drag stops here without a drop: the pointer took
    /// it back there, or its program there gave it up.
    fn cancel_incoming_drag(&mut self) {
        let Some(incoming) = self.handoff.drags.incoming.take() else { return };
        self.handoff.held.button(incoming.holder, incoming.button, false);
        if incoming.dropped {
            return;
        }
        // Nothing under it, so letting go drops it nowhere.
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let time = self.input_time();
        let location = pointer.current_location();
        pointer.motion(self, None, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), time);
        pointer.frame(self);
    }

    /// The drag that went elsewhere is over there: the program here lets go
    /// of it, over nothing.
    fn let_go_of_outgoing(&mut self) {
        let Some(outgoing) = self.handoff.drags.outgoing.take() else { return };
        self.handoff.windows.forget_away_button();
        if outgoing.holder == Source::Local {
            self.handoff.held.button(Source::Local, outgoing.button, false);
        }
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let time = self.input_time();
        // Over nothing, so letting go cancels it here instead of dropping
        // it on a window here.
        let location = pointer.current_location();
        pointer.motion(self, None, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
        tracing::info!("handoff: letting go of a drag carried on by {:016x}", outgoing.peer);
        let event = ButtonEvent {
            button: outgoing.button,
            state: ButtonState::Released,
            serial: SERIAL_COUNTER.next_serial(),
            time,
        };
        pointer.button(self, &event);
        pointer.frame(self);
    }

    /// A program's drag here ended: if it was carried on elsewhere, it is
    /// over there too.
    pub(crate) fn handoff_drag_ended(&mut self) {
        if let Some(outgoing) = self.handoff.drags.outgoing.take() {
            self.handoff.send(outgoing.peer, Message::DragEnd { drag: outgoing.drag });
        }
    }

    /// Another machine's drag was let go of here.
    pub fn handoff_drag_dropped(&mut self) {
        if let Some(incoming) = self.handoff.drags.incoming.as_mut() {
            incoming.dropped = true;
        }
        self.schedule_render();
    }

    /// The program it was dropped on is done with it, or nothing took it.
    pub fn handoff_drag_finished(&mut self) {
        if let Some(incoming) = self.handoff.drags.incoming.take() {
            self.handoff.held.button(incoming.holder, incoming.button, false);
            self.handoff.send(incoming.peer, Message::DragEnd { drag: incoming.drag });
        }
        self.schedule_render();
    }

    /// A program here wants what another machine's drag carries.
    pub fn handoff_drag_send(&mut self, mime_type: String, pipe: OwnedFd) {
        let Some(incoming) = &self.handoff.drags.incoming else {
            tracing::warn!("handoff: a program wants {mime_type} of a drag that is over");
            return;
        };
        let (peer, drag) = (incoming.peer, incoming.drag);
        let files = FileList::wanted(&mime_type, &incoming.mime_types);
        if self.handoff.active_edge(peer).is_none() || self.is_locked() {
            tracing::warn!("handoff: a program wants {mime_type} of a drag while sharing is paused");
            return;
        }
        tracing::info!("handoff: a program here wants {mime_type} of a drag from {peer:016x}");
        if let Some(list) = files {
            self.receive_files_into(peer, pipe, list, |request| Message::DragFiles { request, drag });
        } else {
            self.receive_into(peer, pipe, |request| Message::DragFetch { request, drag, mime_type });
        }
    }

    /// The drags of a machine that went away or stopped sharing end.
    pub(crate) fn drags_peer_gone(&mut self, peer: PeerId) {
        let drags = &mut self.handoff.drags;
        if drags.pending.as_ref().is_some_and(|pending| pending.peer == peer) {
            drags.pending = None;
        }
        self.handoff.files.forget(peer);
        if drags.incoming.as_ref().is_some_and(|incoming| incoming.peer == peer) {
            self.cancel_incoming_drag();
        }
        if self.handoff.drags.outgoing.as_ref().is_some_and(|outgoing| outgoing.peer == peer) {
            self.let_go_of_outgoing();
        }
    }

    /// The source of this machine's drag that went to `peer` as `drag`.
    fn outgoing_source(
        &self,
        peer: PeerId,
        drag: u64,
    ) -> Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource> {
        let outgoing = self.handoff.drags.outgoing.as_ref()?;
        if outgoing.peer != peer
            || outgoing.drag != drag
            || self.handoff.active_edge(peer).is_none()
            || self.is_locked()
        {
            return None;
        }
        self.drag.as_ref()?.source.clone()
    }

    /// Another machine wants this machine's dragged data in one type.
    fn serve_drag(&mut self, peer: PeerId, request: u64, drag: u64, mime_type: &str) {
        let offered = self.drag.as_ref().is_some_and(|local| local.mime_types.iter().any(|mime| mime == mime_type));
        let source = self.outgoing_source(peer, drag).filter(|_| offered);
        let reader = source.and_then(|source| ask_source(&source, mime_type));
        let messenger = self.handoff.messenger();
        let (Some(reader), Some(messenger)) = (reader, messenger) else {
            self.handoff.send(peer, Message::DataEnd { request, failed: true });
            return;
        };
        if !send_out(messenger, peer, request, reader) {
            self.handoff.send(peer, Message::DataEnd { request, failed: true });
        }
    }

    /// Another machine wants this machine's dragged files themselves.
    fn serve_drag_files(&mut self, peer: PeerId, request: u64, drag: u64) {
        let offered =
            self.drag.as_ref().is_some_and(|local| local.mime_types.iter().any(|mime| mime == "text/uri-list"));
        let source = self.outgoing_source(peer, drag).filter(|_| offered);
        let list = source.and_then(|source| ask_source(&source, "text/uri-list"));
        self.serve_files(peer, request, list);
    }
}

/// Asks a dragging program for its data in one type, returning the end to
/// read it from.
fn ask_source(
    source: &smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource,
    mime_type: &str,
) -> Option<UnixStream> {
    let (reader, writer) = UnixStream::pair().ok()?;
    reader.set_read_timeout(Some(STALL)).ok()?;
    source.send(mime_type.to_string(), std::os::fd::AsFd::as_fd(&writer));
    Some(reader)
}
