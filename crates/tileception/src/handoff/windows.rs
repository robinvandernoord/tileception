//! Windows on another machine.
//!
//! A window dragged past an edge shared with another machine goes with the
//! pointer. Its program keeps running here, on its owner, which draws it off
//! screen and sends pictures of what changed (see `stream.rs`). The other
//! machine shows those in a tile of its own and sends back pointer and
//! keyboard input and the size its tile has. Its menus and other popups
//! come along in the same pictures, drawn over whatever is next to it.
//! Dragging the window back or the owner reclaiming it brings it home;
//! closing it there closes it.
//!
//! When the connection is lost the window stays where it was: hidden on its
//! owner, an unavailable tile on the other machine. Once the two link again
//! it picks up where it was, unless its owner reclaimed it in the meantime.

use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        input::{AxisSource, ButtonState, KeyState},
        renderer::{
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::GlesRenderer,
        },
    },
    desktop::WindowSurfaceType,
    input::{
        keyboard::{FilterResult, Keycode},
        pointer::{ButtonEvent, CursorImageStatus, MotionEvent},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size, Transform},
};
use tileception_core::{Placement, Rect, WindowId};
use tileception_handoff::{
    Area, Canvas, Message, PeerId, Picture, PointerImage, Resumed, Room, Scroll, Source, unpack_pointer,
};

use super::stream::Stream;
use crate::{
    Tileception,
    focus::KeyboardFocus,
    input::AxisInput,
    render::{OutputElement, physical, physical_point, push_image, solid},
    state::{app_id, window_title},
};

/// evdev key codes are xkb's less eight.
const XKB_OFFSET: u32 = 8;
/// The left mouse button's evdev code.
const BTN_LEFT: u32 = 0x110;

#[derive(Default)]
pub(crate) struct Windows {
    /// This machine's windows shown on another.
    pub(crate) away: HashMap<WindowId, Away>,
    /// Other machines' windows shown here.
    pub(crate) remote: HashMap<WindowId, Remote>,
    /// A window that came with a drag, picked up once the pointer arrives.
    arriving: Option<Arriving>,
    /// The other machine's window under the pointer here.
    hovered: Option<WindowId>,
    /// The other machine's window a button went down on, and how many are
    /// down: it gets the pointer until the last comes up.
    pressed: Option<(WindowId, u32)>,
    /// The other machine's window that has the keyboard here.
    remote_keyboard: Option<WindowId>,
    /// This machine's window that has the keyboard where it is shown.
    away_keyboard: Option<WindowId>,
    /// This machine's window that another machine typed into while this
    /// machine's own input stayed here. It keeps the keyboard until this
    /// machine's own keyboard or pointer is used.
    typing: Option<WindowId>,
    /// This machine's window the pointer is over where it is shown.
    pub(crate) pointed: Option<WindowId>,
    /// What that machine was last told the pointer looks like over it.
    pub(crate) cursor: super::stream::CursorCapture,
    /// The notification about this machine's windows waiting on a machine
    /// that went away, per machine.
    notices: HashMap<PeerId, u32>,
}

pub(crate) struct Away {
    pub(crate) peer: PeerId,
    /// None while the connection to `peer` is lost.
    pub(crate) stream: Option<Stream>,
    /// What `peer` is called, also once it is gone.
    machine: String,
    title: String,
    app_id: String,
    /// The screen around it there, from its top left.
    room: Option<Room>,
}

pub(crate) struct Remote {
    pub(crate) peer: PeerId,
    /// The owner's number for it.
    window: u64,
    pub(crate) title: String,
    app_id: String,
    /// What its owner is called, for when it cannot be reached.
    owner: String,
    canvas: Canvas,
    /// Pixels per logical pixel on the owner.
    scale: f64,
    /// Where the window itself and its popups are in the picture.
    body: Area,
    popups: Vec<Area>,
    buffer: Option<MemoryRenderBuffer>,
    /// The logical size it had when it came, until a picture says otherwise.
    shown: (u32, u32),
    /// The size last asked of its owner, with the screen around it.
    asked: Option<((u32, u32), Room)>,
    /// What the pointer looks like over it; the usual arrow when None.
    cursor: Option<RemoteCursor>,
    /// The connection to its owner was lost; it waits for it to come back.
    lost: bool,
}

impl Remote {
    /// Its size in logical pixels.
    fn size(&self) -> Size<i32, Logical> {
        if self.buffer.is_none() || self.body.width == 0 || self.scale <= 0.0 {
            return (self.shown.0 as i32, self.shown.1 as i32).into();
        }
        (
            (f64::from(self.body.width) / self.scale).round() as i32,
            (f64::from(self.body.height) / self.scale).round() as i32,
        )
            .into()
    }

    /// Forgets its picture: the next one is a whole one.
    fn clear(&mut self) {
        self.size_now();
        self.canvas = Canvas::default();
        self.buffer = None;
        self.popups.clear();
        self.asked = None;
        self.cursor = None;
    }

    /// Keeps its current size for while it has no picture.
    fn size_now(&mut self) {
        let size = self.size();
        self.shown = (size.w.max(1) as u32, size.h.max(1) as u32);
    }

    /// Where its popups are drawn when the window itself is drawn at
    /// `content`, `scale` times its size, and which part of the picture
    /// each shows.
    fn popup_rects(&self, content: &Rect, scale: (f64, f64)) -> Vec<(Rect, Area)> {
        if self.scale <= 0.0 {
            return Vec::new();
        }
        let logical = |pixels: u32, from: u32| (f64::from(pixels) - f64::from(from)) / self.scale;
        self.popups
            .iter()
            .map(|popup| {
                let rect = Rect::new(
                    content.x + logical(popup.left, self.body.left) * scale.0,
                    content.y + logical(popup.top, self.body.top) * scale.1,
                    logical(popup.width, 0) * scale.0,
                    logical(popup.height, 0) * scale.1,
                );
                (rect, *popup)
            })
            .collect()
    }
}

/// What the pointer looks like over another machine's window.
enum RemoteCursor {
    Hidden,
    Picture {
        buffer: MemoryRenderBuffer,
        size: (u32, u32),
        hotspot: (u32, u32),
        /// Pixels per logical pixel on the owner.
        scale: f64,
    },
}

impl RemoteCursor {
    fn from_image(image: PointerImage) -> Option<RemoteCursor> {
        match image {
            PointerImage::Default => None,
            PointerImage::Hidden => Some(RemoteCursor::Hidden),
            PointerImage::Picture { width, height, hotspot, scale, pixels } => {
                let pixels = unpack_pointer(width, height, scale, &pixels)?;
                let size = (width as i32, height as i32);
                let buffer =
                    MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, size, 1, Transform::Normal, None);
                Some(RemoteCursor::Picture { buffer, size: (width, height), hotspot, scale })
            }
        }
    }
}

/// An area of a canvas, when it lies within it.
fn within(area: Area, canvas: &Canvas) -> bool {
    area.width > 0
        && area.height > 0
        && area.left.checked_add(area.width).is_some_and(|right| right <= canvas.width)
        && area.top.checked_add(area.height).is_some_and(|bottom| bottom <= canvas.height)
}

/// A window that came with a drag from `peer`, held at `grab` (fractions of
/// its size) with `button` down there.
#[derive(Debug, Clone, Copy)]
struct Arriving {
    peer: PeerId,
    window: WindowId,
    grab: (f64, f64),
    button: u32,
}

impl Windows {
    fn remote_id(&self, peer: PeerId, window: u64) -> Option<WindowId> {
        self.remote.iter().find(|(_, remote)| remote.peer == peer && remote.window == window).map(|(local, _)| *local)
    }

    fn away_id(&self, peer: PeerId, window: u64) -> Option<WindowId> {
        let local = WindowId(window);
        self.away.get(&local).is_some_and(|away| away.peer == peer).then_some(local)
    }

    /// Whether a window is another machine's, shown here.
    pub(crate) fn is_remote(&self, window: WindowId) -> bool {
        self.remote.contains_key(&window)
    }

    pub(crate) fn remote_title(&self, window: WindowId) -> Option<String> {
        self.remote.get(&window).map(|remote| remote.title.clone())
    }

    pub(crate) fn remote_app(&self, window: WindowId) -> Option<String> {
        self.remote.get(&window).map(|remote| remote.app_id.clone())
    }
}

impl Tileception {
    pub(crate) fn window_message(&mut self, peer: PeerId, message: Message) {
        match message {
            // As the machine a window is shown on.
            Message::WindowShown { window, title, app_id, width, height, grab, button } => {
                self.window_shown(peer, window, (title, app_id), (width, height), grab, button);
            }
            Message::WindowInfo { window, title, app_id } => {
                if let Some(remote) = self
                    .handoff
                    .windows
                    .remote_id(peer, window)
                    .and_then(|local| self.handoff.windows.remote.get_mut(&local))
                {
                    remote.title = title;
                    remote.app_id = app_id;
                    self.schedule_render();
                }
            }
            Message::WindowPicture(picture) => self.window_picture(peer, &picture),
            Message::WindowGone { window } => {
                if let Some(local) = self.handoff.windows.remote_id(peer, window) {
                    self.drop_remote(local);
                }
            }
            Message::WindowMove { window } => {
                if let Some(local) = self.handoff.windows.remote_id(peer, window) {
                    self.move_remote(local);
                }
            }
            Message::WindowCursor { window, image } => {
                if let Some(remote) = self
                    .handoff
                    .windows
                    .remote_id(peer, window)
                    .and_then(|local| self.handoff.windows.remote.get_mut(&local))
                {
                    remote.cursor = RemoteCursor::from_image(image);
                    self.schedule_render();
                }
            }
            // As the owner.
            Message::WindowAck { window, serial } => {
                if let Some(away) = self
                    .handoff
                    .windows
                    .away_id(peer, window)
                    .and_then(|local| self.handoff.windows.away.get_mut(&local))
                    && let Some(stream) = away.stream.as_mut()
                {
                    stream.acked(serial);
                    self.schedule_render();
                }
            }
            Message::WindowBack { window, grab, button } => {
                if let Some(local) = self.handoff.windows.away_id(peer, window) {
                    let arriving = (button != 0).then_some(Arriving { peer, window: local, grab, button });
                    self.take_back(local, arriving);
                }
            }
            Message::WindowClose { window } => {
                if let Some(local) = self.handoff.windows.away_id(peer, window) {
                    self.close_window(local);
                }
            }
            Message::WindowsResumed { windows } => self.windows_resumed(peer, windows),
            message => {
                let Some(local) = self.away_input(peer, &message) else { return };
                match message {
                    Message::WindowSize { width, height, room, .. } => self.resize_away(local, width, height, room),
                    Message::WindowPointer { across, down, .. } => self.away_pointer(local, across, down),
                    Message::WindowButton { button, pressed, .. } => self.away_button(button, pressed),
                    Message::WindowScroll { scroll, .. } => self.away_scroll(scroll),
                    Message::WindowLeave { .. } => self.away_leave(),
                    Message::WindowFocus { focused, .. } => self.away_focus(local, focused),
                    Message::WindowKey { key, pressed, .. } => self.away_key(local, key, pressed),
                    _ => {}
                }
            }
        }
    }

    /// The window input from another machine is for, when it may be used.
    fn away_input(&self, peer: PeerId, message: &Message) -> Option<WindowId> {
        let window = match message {
            Message::WindowSize { window, .. }
            | Message::WindowPointer { window, .. }
            | Message::WindowButton { window, .. }
            | Message::WindowScroll { window, .. }
            | Message::WindowLeave { window }
            | Message::WindowFocus { window, .. }
            | Message::WindowKey { window, .. } => *window,
            _ => return None,
        };
        let local = self.handoff.windows.away_id(peer, window)?;
        (self.handoff.active_edge(peer).is_some() && !self.is_locked() && self.windows.contains_key(&local))
            .then_some(local)
    }

    /// Sends a message about another machine's window to its owner.
    fn send_to_owner(&self, local: WindowId, message: impl FnOnce(u64) -> Message) -> bool {
        let Some(remote) = self.handoff.windows.remote.get(&local) else { return false };
        self.handoff.active_edge(remote.peer).is_some() && self.handoff.send(remote.peer, message(remote.window))
    }

    /// The window being dragged past an edge goes to the machine there, or
    /// home if it came from there. False when it may not go.
    pub(crate) fn carry_window(&mut self, peer: PeerId, source: Source) -> bool {
        let Some(drag) = self.move_drag else { return true };
        let window = drag.window;
        if let Some(remote) = self.handoff.windows.remote.get(&window) {
            // Another machine's window goes nowhere but home.
            if remote.peer != peer {
                return false;
            }
        } else if !self.windows.contains_key(&window) {
            return false;
        }
        self.move_drag = None;
        // The drag's button goes over with it: let go of it here.
        let buttons = self.handoff.held.release_buttons(source);
        let button = buttons.first().copied().unwrap_or(0);
        self.release_buttons_here(&buttons);
        if let Some(remote) = self.handoff.windows.remote.get(&window) {
            let message = Message::WindowBack { window: remote.window, grab: drag.grab, button };
            self.handoff.send(peer, message);
            self.drop_remote(window);
            return true;
        }
        self.send_window(peer, window, drag.grab, button)
    }

    /// Tells programs here that buttons went up.
    fn release_buttons_here(&mut self, buttons: &[u32]) {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let time = self.input_time();
        for button in buttons {
            let event = ButtonEvent {
                button: *button,
                state: ButtonState::Released,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            };
            pointer.button(self, &event);
        }
        pointer.frame(self);
    }

    /// Shows one of this machine's windows on another.
    fn send_window(&mut self, peer: PeerId, window: WindowId, grab: (f64, f64), button: u32) -> bool {
        if self.fullscreen == Some(window) {
            self.set_fullscreen(window, false);
        }
        let Some(placed) = self.windows.get(&window).cloned() else { return false };
        let Some(messenger) = self.handoff.messenger() else { return false };
        let geometry = placed.geometry();
        let (title, app) = (window_title(&placed), app_id(&placed));
        let message = Message::WindowShown {
            window: window.0,
            title: title.clone(),
            app_id: app.clone(),
            width: geometry.size.w.max(1) as u32,
            height: geometry.size.h.max(1) as u32,
            grab,
            button,
        };
        if !self.handoff.send(peer, message) {
            return false;
        }
        let Some(stream) = Stream::start(peer, window.0, messenger) else {
            self.handoff.send(peer, Message::WindowGone { window: window.0 });
            return false;
        };
        tracing::info!(window = window.0, "handoff: window shown on {peer:016x}");
        let machine = self.handoff.peers.get(&peer).map(|entry| entry.name.clone()).unwrap_or_default();
        let away = Away { peer, stream: Some(stream), machine, title, app_id: app, room: None };
        self.handoff.windows.away.insert(window, away);
        self.selection.windows.remove(&window);
        let now = self.now();
        self.world.remove_window(window, now);
        placed.set_activated(false);
        self.apply_layout();
        self.persist_layout();
        true
    }

    /// Brings one of this machine's windows back from another machine.
    fn take_back(&mut self, window: WindowId, arriving: Option<Arriving>) {
        if self.handoff.windows.away.remove(&window).is_none() {
            return;
        }
        let windows = &mut self.handoff.windows;
        if windows.away_keyboard == Some(window) {
            windows.away_keyboard = None;
        }
        if windows.typing == Some(window) {
            windows.typing = None;
        }
        if windows.pointed == Some(window) {
            windows.pointed = None;
        }
        if !self.windows.contains_key(&window) {
            return;
        }
        tracing::info!(window = window.0, "handoff: window back");
        let now = self.now();
        self.world.add_window(window, Placement::Dock, now);
        self.world.focus_window(window, now);
        self.handoff.windows.arriving = arriving;
        self.apply_layout();
        self.force_keyboard_focus_sync();
        self.persist_layout();
        self.schedule_render();
    }

    /// `handoff reclaim`: every window shown elsewhere comes home, or every
    /// one shown on `machine` (by name, any case, or hex id).
    pub(crate) fn reclaim_windows(&mut self, machine: Option<&str>) -> usize {
        let on = |away: &Away| {
            machine.is_none_or(|machine| {
                away.machine.eq_ignore_ascii_case(machine) || format!("{:016x}", away.peer) == machine
            })
        };
        let away: Vec<(WindowId, PeerId)> = self
            .handoff
            .windows
            .away
            .iter()
            .filter(|(_, away)| on(away))
            .map(|(window, away)| (*window, away.peer))
            .collect();
        for (window, peer) in &away {
            self.handoff.send(*peer, Message::WindowGone { window: window.0 });
            self.take_back(*window, None);
        }
        let peers: std::collections::BTreeSet<PeerId> = away.iter().map(|(_, peer)| *peer).collect();
        for peer in peers {
            self.settle_notice(peer);
        }
        away.len()
    }

    /// Closes the notification about windows waiting on `peer` once none do.
    fn settle_notice(&mut self, peer: PeerId) {
        let waiting = self.handoff.windows.away.values().any(|away| away.peer == peer && away.stream.is_none());
        if !waiting && let Some(notice) = self.handoff.windows.notices.remove(&peer) {
            self.close_local_notification(notice);
        }
    }

    /// The connection to a machine was lost. This machine's windows there
    /// stay hidden until it links again or they are reclaimed; its windows
    /// here become unavailable tiles until then.
    pub(crate) fn windows_peer_lost(&mut self, peer: PeerId) {
        let windows = &mut self.handoff.windows;
        let mut stranded = 0;
        for (window, away) in &mut windows.away {
            if away.peer != peer {
                continue;
            }
            away.stream = None;
            stranded += 1;
            if windows.pointed == Some(*window) {
                windows.pointed = None;
            }
            if windows.away_keyboard == Some(*window) {
                windows.away_keyboard = None;
            }
            if windows.typing == Some(*window) {
                windows.typing = None;
            }
        }
        if windows.arriving.is_some_and(|arriving| arriving.peer == peer) {
            windows.arriving = None;
        }
        let mut lost = Vec::new();
        for (window, remote) in &mut windows.remote {
            if remote.peer == peer {
                remote.lost = true;
                remote.clear();
                lost.push(*window);
            }
        }
        for window in lost {
            if windows.hovered == Some(window) {
                windows.hovered = None;
            }
            if windows.pressed.is_some_and(|(pressed, _)| pressed == window) {
                windows.pressed = None;
            }
            if windows.remote_keyboard == Some(window) {
                windows.remote_keyboard = None;
            }
        }
        if stranded > 0 {
            let name =
                self.handoff.peers.get(&peer).map_or_else(|| "the other machine".into(), |entry| entry.name.clone());
            let summary = if stranded == 1 {
                format!("A window is still on {name}")
            } else {
                format!("{stranded} windows are still on {name}")
            };
            let body = format!(
                "The connection to {name} was lost. Bring them back here, or keep them there: they go \
                 on there when {name} links again, and handoff reclaim brings them back later."
            );
            tracing::info!("handoff: {stranded} windows wait for {name}");
            let reclaim = crate::handoff::HandoffCommand::Reclaim(Some(format!("{peer:016x}")));
            let buttons = vec![("Bring back", Some(crate::commands::Command::Handoff(reclaim))), ("Keep there", None)];
            let notice = self.notify_locally_with(&summary, &body, tileception_notify::Urgency::Normal, buttons);
            if let Some(previous) = self.handoff.windows.notices.insert(peer, notice) {
                self.close_local_notification(previous);
            }
        }
        self.schedule_render();
    }

    /// The machines linked: windows shown on `peer` before the connection
    /// was lost go on there.
    pub(crate) fn resume_windows(&mut self, peer: PeerId) {
        let Some(messenger) = self.handoff.messenger() else { return };
        let mut resumed = Vec::new();
        let mut failed = Vec::new();
        for (window, away) in &mut self.handoff.windows.away {
            if away.peer != peer || away.stream.is_some() {
                continue;
            }
            match Stream::start(peer, window.0, messenger.clone()) {
                Some(stream) => {
                    away.stream = Some(stream);
                    resumed.push(Resumed { window: window.0, title: away.title.clone(), app_id: away.app_id.clone() });
                }
                None => failed.push(*window),
            }
        }
        for window in failed {
            self.take_back(window, None);
        }
        if !resumed.is_empty() {
            tracing::info!("handoff: {} windows resume on {peer:016x}", resumed.len());
        }
        // Always sent: it also tells the other machine which of this one's
        // windows it was waiting for are gone.
        self.handoff.send(peer, Message::WindowsResumed { windows: resumed });
        self.settle_notice(peer);
    }

    /// The other machine's windows that go on here after a lost connection.
    fn windows_resumed(&mut self, peer: PeerId, windows: Vec<Resumed>) {
        let mut waiting: Vec<WindowId> = self
            .handoff
            .windows
            .remote
            .iter()
            .filter(|(_, remote)| remote.peer == peer && remote.lost)
            .map(|(window, _)| *window)
            .collect();
        for resumed in windows {
            let Some(local) = self.handoff.windows.remote_id(peer, resumed.window) else {
                // Closed here while it was away: it goes home.
                self.handoff.send(peer, Message::WindowBack { window: resumed.window, grab: (0.5, 0.5), button: 0 });
                continue;
            };
            waiting.retain(|window| *window != local);
            if let Some(remote) = self.handoff.windows.remote.get_mut(&local) {
                remote.lost = false;
                remote.title = resumed.title;
                remote.app_id = resumed.app_id;
                remote.clear();
            }
        }
        // Reclaimed by their owner in the meantime.
        for window in waiting {
            self.drop_remote(window);
        }
        self.apply_layout();
        self.schedule_render();
    }

    /// A machine went away or stopped sharing: its windows here go, and this
    /// machine's windows there come home.
    pub(crate) fn windows_peer_gone(&mut self, peer: PeerId) {
        let away: Vec<WindowId> =
            self.handoff.windows.away.iter().filter(|(_, away)| away.peer == peer).map(|(window, _)| *window).collect();
        for window in away {
            self.take_back(window, None);
        }
        self.settle_notice(peer);
        let remote: Vec<WindowId> = self
            .handoff
            .windows
            .remote
            .iter()
            .filter(|(_, remote)| remote.peer == peer)
            .map(|(window, _)| *window)
            .collect();
        for window in remote {
            self.drop_remote(window);
        }
        if self.handoff.windows.arriving.is_some_and(|arriving| arriving.peer == peer) {
            self.handoff.windows.arriving = None;
        }
    }

    /// One of this machine's windows closed.
    pub(crate) fn window_forgotten(&mut self, window: WindowId) {
        if let Some(away) = self.handoff.windows.away.remove(&window) {
            self.handoff.send(away.peer, Message::WindowGone { window: window.0 });
        }
        let windows = &mut self.handoff.windows;
        if windows.away_keyboard == Some(window) {
            windows.away_keyboard = None;
        }
        if windows.typing == Some(window) {
            windows.typing = None;
        }
        if windows.pointed == Some(window) {
            windows.pointed = None;
        }
    }

    /// Another machine's window comes here, with the pointer dragging it.
    fn window_shown(
        &mut self,
        peer: PeerId,
        window: u64,
        (title, app_id): (String, String),
        shown: (u32, u32),
        grab: (f64, f64),
        button: u32,
    ) {
        if self.handoff.active_edge(peer).is_none() || self.is_locked() {
            // Not now: it goes straight back.
            self.handoff.send(peer, Message::WindowBack { window, grab, button: 0 });
            return;
        }
        if let Some(stale) = self.handoff.windows.remote_id(peer, window) {
            self.drop_remote(stale);
        }
        let local = self.next_id();
        tracing::info!(window = local.0, app = app_id, "handoff: window from {peer:016x}");
        let owner = self.handoff.peers.get(&peer).map(|entry| entry.name.clone()).unwrap_or_default();
        let remote = Remote {
            peer,
            window,
            title,
            app_id,
            owner,
            canvas: Canvas::default(),
            scale: 1.0,
            body: Area::default(),
            popups: Vec::new(),
            buffer: None,
            shown: (shown.0.clamp(1, 16384), shown.1.clamp(1, 16384)),
            asked: None,
            cursor: None,
            lost: false,
        };
        self.handoff.windows.remote.insert(local, remote);
        let now = self.now();
        self.world.add_window(local, Placement::Dock, now);
        self.world.focus_window(local, now);
        self.handoff.windows.arriving = (button != 0).then_some(Arriving { peer, window: local, grab, button });
        self.apply_layout();
    }

    /// Another machine's window asked to be moved: dragged here like a
    /// window here whose title bar is dragged. Its owner's program saw the
    /// press; it gets the release now, as the drag takes over.
    fn move_remote(&mut self, window: WindowId) {
        let Some((pressed, _)) = self.handoff.windows.pressed else { return };
        if pressed != window || self.move_drag.is_some() || self.world.in_overview() {
            return;
        }
        self.handoff.windows.pressed = None;
        self.send_to_owner(window, |owned| Message::WindowButton { window: owned, button: BTN_LEFT, pressed: false });
        let now = self.now();
        self.world.focus_window(window, now);
        self.apply_layout();
        self.start_move_drag(window, self.pointer);
    }

    /// A program here asked to move its window, which is shown on another
    /// machine: that machine drags it.
    pub(crate) fn move_away(&mut self, window: WindowId) -> bool {
        let Some(away) = self.handoff.windows.away.get(&window) else { return false };
        self.handoff.send(away.peer, Message::WindowMove { window: window.0 });
        true
    }

    /// The pointer arrived from `peer` (Enter, or the other machine handing
    /// this one's own pointer back): a window that came with it is dragged
    /// on. `source` is whoever holds the drag's button now.
    pub(crate) fn pick_up_arriving(&mut self, peer: PeerId, source: Source) {
        let Some(arriving) = self.handoff.windows.arriving.take() else { return };
        if arriving.peer != peer || !self.world.contains_window(arriving.window) || self.move_drag.is_some() {
            return;
        }
        self.handoff.held.button(source, arriving.button, true);
        self.start_move_drag(arriving.window, self.pointer);
        if let Some(drag) = self.move_drag.as_mut() {
            drag.grab = arriving.grab;
        }
    }

    fn window_picture(&mut self, peer: PeerId, picture: &Picture) {
        // Paused sharing takes no pictures, not even ones already on the way.
        if self.handoff.active_edge(peer).is_none() || self.is_locked() {
            return;
        }
        let Some(local) = self.handoff.windows.remote_id(peer, picture.window) else { return };
        let Some(remote) = self.handoff.windows.remote.get_mut(&local) else { return };
        if remote.lost {
            return;
        }
        if !remote.canvas.paint(picture) {
            tracing::warn!("handoff: a picture of window {local:?} does not fit; dropped");
            return;
        }
        if !picture.last {
            return;
        }
        let canvas = &remote.canvas;
        let whole = Area { left: 0, top: 0, width: canvas.width, height: canvas.height };
        remote.body = if within(picture.body, canvas) { picture.body } else { whole };
        remote.popups = picture.popups.iter().copied().filter(|popup| within(*popup, canvas)).collect();
        let size = (canvas.width as i32, canvas.height as i32);
        remote.buffer =
            Some(MemoryRenderBuffer::from_slice(&canvas.pixels, Fourcc::Abgr8888, size, 1, Transform::Normal, None));
        if picture.scale.is_finite() && picture.scale > 0.0 {
            remote.scale = picture.scale;
        }
        self.handoff.send(peer, Message::WindowAck { window: picture.window, serial: picture.serial });
        self.schedule_render();
    }

    /// Another machine's window stops being shown here.
    fn drop_remote(&mut self, window: WindowId) {
        if let Some(output) = self.output_for_window(window)
            && self.output.as_ref() != Some(&output)
        {
            self.with_output(&output, |state| state.drop_remote(window));
            return;
        }
        if self.handoff.windows.remote.remove(&window).is_none() {
            return;
        }
        let windows = &mut self.handoff.windows;
        if windows.hovered == Some(window) {
            windows.hovered = None;
        }
        if windows.pressed.is_some_and(|(pressed, _)| pressed == window) {
            windows.pressed = None;
        }
        if windows.remote_keyboard == Some(window) {
            windows.remote_keyboard = None;
        }
        if windows.arriving.is_some_and(|arriving| arriving.window == window) {
            windows.arriving = None;
        }
        if self.move_drag.is_some_and(|drag| drag.window == window) {
            self.move_drag = None;
        }
        if self.resize_drag.is_some_and(|drag| drag.window == window) {
            self.resize_drag = None;
        }
        self.selection.windows.remove(&window);
        let now = self.now();
        self.world.remove_window(window, now);
        if self.keyboard_focus == Some(window) {
            self.keyboard_focus = None;
        }
        self.apply_layout();
        self.schedule_render();
    }

    /// The size and placement of a window, here or another machine's.
    pub(crate) fn window_geometry(&self, window: WindowId) -> Option<Rectangle<i32, Logical>> {
        if let Some(placed) = self.windows.get(&window) {
            return Some(placed.geometry());
        }
        self.handoff.windows.remote.get(&window).map(|remote| Rectangle::new((0, 0).into(), remote.size()))
    }

    /// Asks the owner of another machine's window to fit the place it has.
    pub(crate) fn ask_remote_size(&mut self, window: WindowId, size: Size<i32, Logical>, room: Room) {
        let linked = self
            .handoff
            .windows
            .remote
            .get(&window)
            .is_some_and(|remote| self.handoff.active_edge(remote.peer).is_some());
        let Some(remote) = self.handoff.windows.remote.get_mut(&window) else { return };
        let wanted = (size.w.max(1) as u32, size.h.max(1) as u32);
        if remote.asked == Some((wanted, room)) || !linked {
            return;
        }
        remote.asked = Some((wanted, room));
        let (peer, owned) = (remote.peer, remote.window);
        self.handoff.send(peer, Message::WindowSize { window: owned, width: wanted.0, height: wanted.1, room });
    }

    /// Closing another machine's window asks its owner to close it. One
    /// whose owner cannot be reached just goes; the owner takes it back
    /// when they link again.
    pub(crate) fn close_remote(&self, window: WindowId) -> bool {
        let Some(remote) = self.handoff.windows.remote.get(&window) else { return false };
        if remote.lost {
            self.loop_handle.insert_idle(move |state| state.drop_remote(window));
        } else {
            self.handoff.send(remote.peer, Message::WindowClose { window: remote.window });
        }
        true
    }

    /// Another machine's window as drawn in its place: its picture, cut to
    /// `content` when it is too big for it, or a plain tile before the first
    /// picture or while sharing with its owner is paused. One whose owner
    /// cannot be reached says so.
    pub(crate) fn remote_elements(
        &self,
        renderer: &mut GlesRenderer,
        window: WindowId,
        content: &Rect,
        shown: (f64, f64),
        alpha: f32,
    ) -> Vec<OutputElement> {
        let Some(remote) = self.handoff.windows.remote.get(&window) else { return Vec::new() };
        let plain = || vec![solid(content, self.theme.surface.alpha(alpha).0, self.scale)];
        if remote.lost {
            let mut elements = Vec::new();
            let theme = &self.theme;
            let owner = if remote.owner.is_empty() { "its machine" } else { remote.owner.as_str() };
            let label = format!("Waiting for {owner}");
            let image = self.text.borrow_mut().line(
                &label,
                &theme.font,
                theme.font_size,
                theme.muted,
                content.w as f32 - 16.0,
                false,
            );
            if let Some(image) = image.filter(|image| image.height < content.h) {
                let position =
                    (content.x + (content.w - image.width) / 2.0, content.y + (content.h - image.height) / 2.0);
                push_image(&mut elements, renderer, &image.buffer, position, self.scale);
            }
            elements.extend(plain());
            return elements;
        }
        let paused = self.handoff.active_edge(remote.peer).is_none();
        let Some(buffer) = remote.buffer.as_ref().filter(|_| !paused) else { return plain() };
        let area = physical(content, self.scale);
        let location = physical_point((content.x, content.y), self.scale).to_f64();
        // The part of the picture shown: the window itself, unless cut off.
        let body = remote.body;
        let source = Rectangle::<f64, Logical>::new(
            (f64::from(body.left), f64::from(body.top)).into(),
            ((shown.0 * remote.scale).min(f64::from(body.width)), (shown.1 * remote.scale).min(f64::from(body.height)))
                .into(),
        );
        let size: Size<i32, Logical> = (area.size.w, area.size.h).into();
        let alpha = (alpha < 1.0).then_some(alpha);
        match MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location,
            buffer,
            alpha,
            Some(source),
            Some(size),
            Kind::Unspecified,
        ) {
            Ok(element) => vec![OutputElement::Memory(element)],
            Err(_) => plain(),
        }
    }

    /// Another machine's window's menus and other popups, drawn above every
    /// window wherever they reach.
    pub(crate) fn remote_popup_elements(
        &self,
        renderer: &mut GlesRenderer,
        window: WindowId,
        content: &Rect,
        scale: (f64, f64),
    ) -> Vec<OutputElement> {
        let Some(remote) = self.handoff.windows.remote.get(&window) else { return Vec::new() };
        if remote.lost || self.handoff.active_edge(remote.peer).is_none() {
            return Vec::new();
        }
        let Some(buffer) = remote.buffer.as_ref() else { return Vec::new() };
        remote
            .popup_rects(content, scale)
            .into_iter()
            .rev()
            .filter_map(|(rect, popup)| {
                let area = physical(&rect, self.scale);
                let location = physical_point((rect.x, rect.y), self.scale).to_f64();
                let source = Rectangle::<f64, Logical>::new(
                    (f64::from(popup.left), f64::from(popup.top)).into(),
                    (f64::from(popup.width), f64::from(popup.height)).into(),
                );
                let size: Size<i32, Logical> = (area.size.w, area.size.h).into();
                MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location,
                    buffer,
                    None,
                    Some(source),
                    Some(size),
                    Kind::Unspecified,
                )
                .ok()
                .map(OutputElement::Memory)
            })
            .collect()
    }

    /// The pointer as the program of the other machine's window under it
    /// set it, when it set one; None for the usual arrow.
    pub(crate) fn remote_cursor_elements(&self, renderer: &mut GlesRenderer) -> Option<Vec<OutputElement>> {
        let windows = &self.handoff.windows;
        let window = windows.pressed.map(|(window, _)| window).or(windows.hovered)?;
        let remote = windows.remote.get(&window).filter(|remote| !remote.lost)?;
        self.handoff.active_edge(remote.peer)?;
        match remote.cursor.as_ref()? {
            RemoteCursor::Hidden => Some(Vec::new()),
            RemoteCursor::Picture { buffer, size, hotspot, scale } => {
                // From the owner's pixels to this screen's.
                let ratio = self.scale / scale;
                let position = (
                    (self.pointer.x * self.scale - f64::from(hotspot.0) * ratio).round(),
                    (self.pointer.y * self.scale - f64::from(hotspot.1) * ratio).round(),
                );
                let shown: Size<i32, Logical> =
                    ((f64::from(size.0) * ratio).round() as i32, (f64::from(size.1) * ratio).round() as i32).into();
                let element = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    position,
                    buffer,
                    None,
                    None,
                    Some(shown),
                    Kind::Cursor,
                )
                .ok()?;
                Some(vec![OutputElement::Memory(element)])
            }
        }
    }

    /// The other machine's window under a point, if it is on top there: its
    /// popups first, as they are drawn above every window.
    fn remote_under(&self, pos: Point<f64, Logical>) -> Option<WindowId> {
        use smithay::wayland::shell::wlr_layer::Layer;
        if self.handoff.windows.remote.is_empty()
            || self.layer_surface_under(pos, &[Layer::Overlay, Layer::Top]).is_some()
        {
            return None;
        }
        let point = tileception_core::Point::new(pos.x, pos.y);
        let drawn = self.drawn_windows(self.now());
        let in_popup = drawn.iter().find(|drawn| {
            self.handoff.windows.remote.get(&drawn.window_id).is_some_and(|remote| {
                !remote.lost
                    && remote.popup_rects(&drawn.content, drawn.scale).iter().any(|(rect, _)| rect.contains(point))
            })
        });
        if let Some(drawn) = in_popup {
            return Some(drawn.window_id);
        }
        let shown = drawn.into_iter().find(|drawn| drawn.content.contains(point))?;
        self.handoff.windows.remote.get(&shown.window_id).filter(|remote| !remote.lost).map(|_| shown.window_id)
    }

    /// A point in another machine's window, in its logical pixels.
    fn remote_point(&self, window: WindowId, pos: Point<f64, Logical>) -> Option<(f64, f64)> {
        let drawn = self.drawn_windows(self.now()).into_iter().find(|drawn| drawn.window_id == window)?;
        let (scale_x, scale_y) = drawn.scale;
        (scale_x > 0.0 && scale_y > 0.0)
            .then(|| ((pos.x - drawn.content.x) / scale_x, (pos.y - drawn.content.y) / scale_y))
    }

    /// Pointer motion here over another machine's window goes to its owner.
    /// True when it did.
    pub(crate) fn pointer_to_owner(&mut self, pos: Point<f64, Logical>, time: u32) -> bool {
        let target = self.handoff.windows.pressed.map(|(window, _)| window).or_else(|| self.remote_under(pos));
        if self.handoff.windows.hovered != target {
            if let Some(left) = self.handoff.windows.hovered.take() {
                self.send_to_owner(left, |window| Message::WindowLeave { window });
            }
            self.handoff.windows.hovered = target;
        }
        let Some(window) = target else { return false };
        if let Some((across, down)) = self.remote_point(window, pos) {
            self.send_to_owner(window, |owned| Message::WindowPointer { window: owned, across, down });
        }
        // Programs here lose the pointer; ours is the plain arrow.
        self.cursor_status = CursorImageStatus::default_named();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
        true
    }

    /// A button over another machine's window goes to its owner, and so does
    /// its release wherever the pointer is by then. True when it did.
    pub(crate) fn button_to_owner(&mut self, button: u32, state: ButtonState) -> bool {
        let pressed = state == ButtonState::Pressed;
        let windows = &mut self.handoff.windows;
        let target = match (windows.pressed, pressed) {
            (Some((window, count)), true) => {
                windows.pressed = Some((window, count + 1));
                window
            }
            (Some((window, count)), false) => {
                windows.pressed = (count > 1).then_some((window, count - 1));
                window
            }
            (None, true) => {
                let Some(window) = windows.hovered else { return false };
                windows.pressed = Some((window, 1));
                window
            }
            (None, false) => {
                let Some(window) = windows.hovered else { return false };
                window
            }
        };
        self.send_to_owner(target, |window| Message::WindowButton { window, button, pressed });
        true
    }

    /// Scrolling over another machine's window goes to its owner.
    pub(crate) fn scroll_to_owner(&mut self, event: &AxisInput) -> bool {
        let Some(window) = self.handoff.windows.hovered else { return false };
        let scroll = Scroll {
            finger: event.source == AxisSource::Finger,
            horizontal: event.horizontal,
            vertical: event.vertical,
            horizontal_v120: event.horizontal_v120,
            vertical_v120: event.vertical_v120,
        };
        self.send_to_owner(window, |owned| Message::WindowScroll { window: owned, scroll });
        true
    }

    /// A key that no binding took, for another machine's window when it has
    /// the keyboard here.
    pub(crate) fn key_to_owner(&mut self, code: Keycode, key_state: KeyState) {
        let Some(window) = self.handoff.windows.remote_keyboard else { return };
        let key = code.raw().saturating_sub(XKB_OFFSET);
        let pressed = key_state == KeyState::Pressed;
        self.send_to_owner(window, |owned| Message::WindowKey { window: owned, key, pressed });
    }

    /// Keeps the owner of the window with the keyboard here informed.
    pub(crate) fn set_remote_keyboard(&mut self, window: Option<WindowId>) {
        let previous = self.handoff.windows.remote_keyboard;
        if previous == window {
            return;
        }
        self.handoff.windows.remote_keyboard = window;
        if let Some(previous) = previous {
            self.send_to_owner(previous, |owned| Message::WindowFocus { window: owned, focused: false });
        }
        if let Some(window) = window {
            self.send_to_owner(window, |owned| Message::WindowFocus { window: owned, focused: true });
        }
    }

    /// What has this machine's keyboard for another machine: one of this
    /// machine's windows shown there, while it has the keyboard there and
    /// this machine's own input goes there too, or it was just typed into.
    pub(crate) fn away_keyboard_target(&self) -> Option<KeyboardFocus> {
        let windows = &self.handoff.windows;
        let window = windows.typing.or_else(|| windows.away_keyboard.filter(|_| self.handoff.forwarding.is_some()))?;
        self.windows.get(&window).and_then(crate::state::focus_target)
    }

    /// This machine's own keyboard or pointer was used here: a window typed
    /// into from elsewhere lets go of the keyboard.
    pub(crate) fn used_here(&mut self) {
        self.handoff.windows.pointed = None;
        if self.handoff.windows.typing.take().is_some() {
            self.force_keyboard_focus_sync();
        }
    }

    /// The screen around one of this machine's windows shown elsewhere.
    pub(crate) fn away_room(&self, window: WindowId) -> Option<Room> {
        self.handoff.windows.away.get(&window).and_then(|away| away.room)
    }

    fn resize_away(&mut self, window: WindowId, width: u32, height: u32, room: Room) {
        if let Some(away) = self.handoff.windows.away.get_mut(&window) {
            away.room = (room.width > 0 && room.height > 0).then_some(room);
        }
        let Some(placed) = self.windows.get(&window) else { return };
        let size: Size<i32, Logical> = (width.clamp(1, 16384) as i32, height.clamp(1, 16384) as i32).into();
        if let Some(toplevel) = placed.toplevel() {
            toplevel.with_pending_state(|pending| pending.size = Some(size));
            if toplevel.is_initial_configure_sent() {
                toplevel.send_pending_configure();
            }
        } else if let Some(x11) = placed.x11_surface() {
            let _ = x11.configure(Rectangle::new((0, 0).into(), size));
        }
    }

    fn away_pointer(&mut self, window: WindowId, across: f64, down: f64) {
        let Some(placed) = self.windows.get(&window).cloned() else { return };
        self.handoff.windows.pointed = Some(window);
        let geometry = placed.geometry();
        let location: Point<f64, Logical> = geometry.loc.to_f64() + Point::from((across, down));
        let focus =
            placed.surface_under(location, WindowSurfaceType::ALL).map(|(surface, offset)| (surface, offset.to_f64()));
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let time = self.input_time();
        pointer.motion(self, focus, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    fn away_button(&mut self, button: u32, pressed: bool) {
        let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let time = self.input_time();
        pointer.button(self, &ButtonEvent { button, state, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    fn away_scroll(&mut self, scroll: Scroll) {
        let input = AxisInput {
            source: if scroll.finger { AxisSource::Finger } else { AxisSource::Wheel },
            time: self.input_time(),
            horizontal: scroll.horizontal,
            vertical: scroll.vertical,
            horizontal_v120: scroll.horizontal_v120,
            vertical_v120: scroll.vertical_v120,
        };
        self.deliver_axis(&input);
    }

    fn away_leave(&mut self) {
        self.handoff.windows.pointed = None;
        let time = self.input_time();
        if self.handoff.forwarding.is_some() {
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            let location = pointer.current_location();
            pointer.motion(self, None, &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
        } else {
            self.on_motion(self.pointer, time);
        }
    }

    fn away_focus(&mut self, window: WindowId, focused: bool) {
        let windows = &mut self.handoff.windows;
        if focused {
            windows.away_keyboard = Some(window);
        } else if windows.away_keyboard == Some(window) {
            windows.away_keyboard = None;
        }
        if let Some(placed) = self.windows.get(&window) {
            placed.set_activated(focused);
            // Clicking anything else there closes the window's menus, as
            // clicking outside them here would.
            if !focused && let Some(toplevel) = placed.toplevel() {
                let surface = toplevel.wl_surface();
                let popups: Vec<_> =
                    smithay::desktop::PopupManager::popups_for_surface(surface).map(|(popup, _)| popup).collect();
                for popup in popups.iter().rev() {
                    let _ = smithay::desktop::PopupManager::dismiss_popup(surface, popup);
                }
            }
            if let Some(toplevel) = placed.toplevel()
                && toplevel.is_initial_configure_sent()
            {
                toplevel.send_pending_configure();
            }
        }
        self.force_keyboard_focus_sync();
    }

    /// A key for one of this machine's windows from where it is shown. When
    /// this machine's own keyboard pressed it, the keyboard here already
    /// knows (it was forwarded); then the window only needs telling.
    fn away_key(&mut self, window: WindowId, key: u32, pressed: bool) {
        let target = self.windows.get(&window).and_then(crate::state::focus_target);
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        if target.is_some() && keyboard.current_focus() != target {
            self.handoff.windows.typing = Some(window);
            self.force_keyboard_focus_sync();
        }
        let code = Keycode::new(key + XKB_OFFSET);
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time();
        if keyboard.pressed_keys().contains(&code) == pressed {
            keyboard.input_forward(self, code, state, serial, time, true);
        } else {
            keyboard.input::<(), _>(self, code, state, serial, time, |_, _, _| FilterResult::Forward);
        }
    }

    /// Keeps titles and apps of windows shown elsewhere up to date there.
    pub(crate) fn update_away_info(&mut self, window: WindowId) {
        let Some(placed) = self.windows.get(&window) else { return };
        let (title, app) = (window_title(placed), app_id(placed));
        let Some(away) = self.handoff.windows.away.get_mut(&window) else { return };
        if away.title == title && away.app_id == app {
            return;
        }
        away.title.clone_from(&title);
        away.app_id.clone_from(&app);
        let peer = away.peer;
        self.handoff.send(peer, Message::WindowInfo { window: window.0, title, app_id: app });
    }

    /// Lines for `handoff` status about windows on other machines.
    pub(crate) fn windows_status(&self) -> Vec<String> {
        let name = |peer: PeerId| {
            self.handoff.peers.get(&peer).map_or_else(|| format!("{peer:016x}"), |entry| entry.name.clone())
        };
        let mut lines: Vec<String> = self
            .handoff
            .windows
            .away
            .iter()
            .map(|(window, away)| {
                let waiting = if away.stream.is_none() { " (waiting for it to link again)" } else { "" };
                let machine = if away.machine.is_empty() { name(away.peer) } else { away.machine.clone() };
                format!("window {} {:?} is on {machine}{waiting}", window.0, away.title)
            })
            .collect();
        lines.extend(self.handoff.windows.remote.iter().map(|(window, remote)| {
            let waiting = if remote.lost { " (unavailable)" } else { "" };
            format!("window {} {:?} is from {}{waiting}", window.0, remote.title, remote.owner)
        }));
        lines.sort();
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(body: Area, popups: Vec<Area>, scale: f64) -> Remote {
        Remote {
            peer: 1,
            window: 2,
            title: String::new(),
            app_id: String::new(),
            owner: "laptop".into(),
            canvas: Canvas::default(),
            scale,
            body,
            popups,
            buffer: None,
            shown: (10, 10),
            asked: None,
            cursor: None,
            lost: false,
        }
    }

    #[test]
    fn popups_are_drawn_beside_the_window_as_zoomed() {
        // A 2x picture: the window starts 100 pixels in, a menu at the left
        // edge below it.
        let body = Area { left: 100, top: 0, width: 800, height: 600 };
        let menu = Area { left: 0, top: 560, width: 400, height: 200 };
        let remote = remote(body, vec![menu], 2.0);
        let content = Rect::new(1000.0, 200.0, 200.0, 150.0);
        let rects = remote.popup_rects(&content, (0.5, 0.5));
        assert_eq!(rects, vec![(Rect::new(975.0, 340.0, 100.0, 50.0), menu)]);
    }

    #[test]
    fn areas_must_lie_within_the_canvas() {
        let canvas = Canvas { width: 100, height: 50, pixels: Vec::new() };
        assert!(within(Area { left: 0, top: 0, width: 100, height: 50 }, &canvas));
        assert!(!within(Area { left: 1, top: 0, width: 100, height: 50 }, &canvas));
        assert!(!within(Area { left: 0, top: 0, width: 0, height: 50 }, &canvas));
        assert!(!within(Area { left: u32::MAX, top: 0, width: 2, height: 1 }, &canvas));
    }
}
