//! The pointer and keyboard crossing between machines.
//!
//! This machine's input goes to another one once the pointer pushes past
//! the edge linked to it, until that machine sends it back (its pointer
//! crossed the same edge the other way) or Ctrl+Super+Escape brings it home.
//! Input from another machine is used as if it were local, except that it
//! is never passed on to a third machine.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, InputBackend, InputEvent, KeyState, PointerAxisEvent,
        PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, Keycode, keysyms},
        pointer::MotionEvent,
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};
use tileception_handoff::{Message, PeerId, Scroll, Source};

use super::{Edge, Side, output_bounds};
use crate::{Tileception, input::AxisInput};

/// evdev key codes are xkb's less eight.
const XKB_OFFSET: u32 = 8;
/// How far the pointer pushes past a shared edge before it crosses, so a
/// small nudge right after arriving does not send input straight back.
const RESIST: f64 = 8.0;

/// How this machine's input comes back.
pub(crate) enum Back {
    /// The other machine's pointer crossed back here, at this point.
    Return(f64),
    /// The return-home shortcut, a lock or switching sharing off.
    Home,
    /// The other machine went away or stopped sharing.
    Gone,
}

impl Tileception {
    pub(crate) fn input_time(&self) -> u32 {
        (self.now() * 1000.0) as u32
    }

    /// Local input while it belongs to another machine. True when it was
    /// sent there (or dropped) instead of being used here.
    pub(crate) fn forward_input<I: InputBackend>(&mut self, event: &InputEvent<I>) -> bool {
        if self.handoff.forwarding.is_none() {
            return false;
        }
        let message = match event {
            InputEvent::PointerMotion { event } => {
                let delta = event.delta();
                Message::Motion { right: delta.x, down: delta.y }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let screen = self.desktop_bounds();
                let position = event.position_transformed((screen.w as i32, screen.h as i32).into());
                let Some(previous) = self.handoff.absolute.replace(position) else { return true };
                Message::Motion { right: position.x - previous.x, down: position.y - previous.y }
            }
            InputEvent::PointerButton { event } => {
                Message::Button { button: event.button_code(), pressed: event.state() == ButtonState::Pressed }
            }
            InputEvent::PointerAxis { event } => Message::Scroll(Scroll {
                finger: event.source() == AxisSource::Finger,
                horizontal: event.amount(Axis::Horizontal),
                vertical: event.amount(Axis::Vertical),
                horizontal_v120: event.amount_v120(Axis::Horizontal),
                vertical_v120: event.amount_v120(Axis::Vertical),
            }),
            // Touching this machine's screen is using this machine.
            InputEvent::TouchDown { .. } => {
                self.stop_forwarding(Back::Home);
                return false;
            }
            InputEvent::GestureSwipeBegin { .. }
            | InputEvent::GestureSwipeUpdate { .. }
            | InputEvent::GestureSwipeEnd { .. }
            | InputEvent::GesturePinchBegin { .. }
            | InputEvent::GesturePinchUpdate { .. }
            | InputEvent::GesturePinchEnd { .. }
            | InputEvent::GestureHoldBegin { .. }
            | InputEvent::GestureHoldEnd { .. } => return true,
            _ => return false,
        };
        self.forward_message(message)
    }

    /// Sends one piece of local input to the machine that has it, if any.
    pub(crate) fn forward_message(&mut self, message: Message) -> bool {
        let Some(peer) = self.handoff.forwarding else { return false };
        if !self.handoff.send(peer, message) {
            self.stop_forwarding(Back::Gone);
        }
        true
    }

    /// A local key while input belongs to another machine. The keyboard here
    /// still tracks it, so modifiers are right when input comes back.
    pub(crate) fn forward_key(&mut self, peer: PeerId, code: Keycode, key_state: KeyState, time: u32) {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let pressed = key_state == KeyState::Pressed;
        let serial = SERIAL_COUNTER.next_serial();
        let home = keyboard.input::<bool, _>(self, code, key_state, serial, time, |_, mods, handle| {
            let home = pressed
                && mods.logo
                && mods.ctrl
                && !mods.alt
                && !mods.shift
                && handle.modified_sym().raw() == keysyms::KEY_Escape;
            FilterResult::Intercept(home)
        });
        if home == Some(true) {
            self.stop_forwarding(Back::Home);
            return;
        }
        let key = code.raw().saturating_sub(XKB_OFFSET);
        if !self.handoff.send(peer, Message::Key { key, pressed }) {
            self.stop_forwarding(Back::Gone);
        }
    }

    /// Pointer motion past the edge of the active screen, towards a linked
    /// machine. True when the pointer went there, or back where it came from.
    pub(crate) fn handoff_cross(&mut self, source: Source, position: Point<f64, Logical>) -> bool {
        if self.handoff.peers.is_empty() || self.is_locked() {
            return false;
        }
        let Some(output) = self.output.clone() else { return false };
        let screen = self.output_rect();
        let side = if position.x < screen.x {
            Side::Left
        } else if position.x >= screen.right() {
            Side::Right
        } else if position.y < screen.y {
            Side::Top
        } else if position.y >= screen.bottom() {
            Side::Bottom
        } else {
            self.handoff.push = 0.0;
            return false;
        };
        // Another screen of this machine there comes first.
        let global = position + output.current_location().to_f64();
        let global = tileception_core::Point::new(global.x, global.y);
        if self
            .all_outputs()
            .iter()
            .filter(|other| **other != output)
            .filter_map(output_bounds)
            .any(|rect| rect.contains(global))
        {
            return false;
        }
        let Some(peer) = self.handoff.peer_at(&output.name(), side) else { return false };
        // A drag stays on its machine, unless it drags a window or a
        // program's data.
        if self.move_drag.is_none() && self.handoff.held.holds_button(source) && !self.may_carry_drag(peer, source) {
            return false;
        }
        let crossing = match source {
            Source::Local => self.handoff.forwarding.is_none(),
            // Never passed on to a third machine.
            Source::Peer(from) => from == peer,
        };
        if !crossing {
            return false;
        }
        let beyond = match side {
            Side::Left => screen.x - position.x,
            Side::Right => position.x - screen.right(),
            Side::Top => screen.y - position.y,
            Side::Bottom => position.y - screen.bottom(),
        };
        // Short of it, the pointer stays at the edge.
        self.handoff.push += beyond.max(1.0);
        if self.handoff.push < RESIST {
            return false;
        }
        self.handoff.push = 0.0;
        let along = match side {
            Side::Left | Side::Right => (position.y - screen.y) / screen.h,
            Side::Top | Side::Bottom => (position.x - screen.x) / screen.w,
        }
        .clamp(0.0, 1.0);
        // A dragged window goes along, ahead of the pointer.
        if !self.carry_window(peer, source) {
            return false;
        }
        self.carry_drag(peer, source);
        match source {
            Source::Local => self.start_forwarding(peer, along),
            Source::Peer(from) => {
                self.share_selections(from);
                self.handoff.send(from, Message::Return { along });
                self.forget_input(from);
                self.handoff.away = true;
                self.schedule_render();
            }
        }
        true
    }

    fn start_forwarding(&mut self, peer: PeerId, along: f64) {
        self.handoff.forwarding = Some(peer);
        self.handoff.absolute = None;
        // Whatever that machine still holds here would never be let go.
        self.forget_input(peer);
        // Offered before the pointer arrives, so pasting there works at once.
        self.share_selections(peer);
        if !self.handoff.send(peer, Message::Enter { along }) {
            self.handoff.forwarding = None;
            return;
        }
        tracing::info!("handoff: input goes to {peer:016x}");
        // Programs here lose the pointer and the keyboard until it is back.
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let time = self.input_time();
        pointer.motion(self, None, &MotionEvent { location: self.pointer, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    pub(crate) fn stop_forwarding(&mut self, back: Back) {
        let Some(peer) = self.handoff.forwarding.take() else { return };
        tracing::info!("handoff: input is back from {peer:016x}");
        match back {
            Back::Home => {
                self.handoff.send(peer, Message::Leave);
            }
            Back::Return(along) => {
                if let Some(edge) = self.handoff.active_edge(peer).cloned() {
                    self.place_at_edge(&edge, along);
                }
                // The button of a window or data dragged along is held here.
                self.pick_up_arriving(peer, Source::Local);
                self.pick_up_drag(peer, Source::Local);
            }
            Back::Gone => {}
        }
        self.force_keyboard_focus_sync();
        let time = self.input_time();
        self.on_motion(self.pointer, time);
        self.schedule_render();
    }

    /// Puts the pointer just inside an edge, at a fraction along it.
    fn place_at_edge(&mut self, edge: &Edge, along: f64) -> bool {
        let Some(output) = self.output_named(&edge.output) else { return false };
        self.activate_output(&output);
        let screen = self.output_rect();
        let along = along.clamp(0.0, 1.0);
        let across = (screen.x + along * screen.w).min(screen.right() - 1.0);
        let down = (screen.y + along * screen.h).min(screen.bottom() - 1.0);
        self.pointer = match edge.side {
            Side::Left => (screen.x + 1.0, down),
            Side::Right => (screen.right() - 2.0, down),
            Side::Top => (across, screen.y + 1.0),
            Side::Bottom => (across, screen.bottom() - 2.0),
        }
        .into();
        true
    }

    /// Whether input from this machine may be used here now. Waking the
    /// screen takes the input, as it does locally.
    fn remote_input_allowed(&mut self, peer: PeerId) -> bool {
        self.handoff.active_edge(peer).is_some() && !self.is_locked() && !self.idle_activity()
    }

    pub(crate) fn remote_enter(&mut self, peer: PeerId, along: f64) {
        // The pointer is here now: this machine's own input stops going
        // elsewhere, or the cursor and keyboard focus would stay hidden.
        if let Some(target) = self.handoff.forwarding {
            if target != peer {
                self.handoff.send(target, Message::Leave);
            }
            self.handoff.forwarding = None;
            tracing::info!("handoff: input is back from {target:016x}, {peer:016x} took the pointer");
            self.force_keyboard_focus_sync();
        }
        let edge = self.handoff.active_edge(peer).cloned();
        let placed = match edge {
            Some(edge) if !self.is_locked() => {
                self.idle_activity();
                self.place_at_edge(&edge, along)
            }
            _ => false,
        };
        if !placed {
            // Nowhere to arrive: straight back.
            self.handoff.send(peer, Message::Return { along });
            return;
        }
        self.handoff.away = false;
        // A window or data dragged along is held by the button down there.
        self.pick_up_arriving(peer, Source::Peer(peer));
        self.pick_up_drag(peer, Source::Peer(peer));
        let time = self.input_time();
        self.on_motion(self.pointer, time);
        self.schedule_render();
    }

    pub(crate) fn remote_motion(&mut self, peer: PeerId, right: f64, down: f64) {
        if !self.remote_input_allowed(peer) {
            return;
        }
        let delta: Point<f64, Logical> = (right, down).into();
        let target = self.pointer + delta;
        let utime = (self.now() * 1_000_000.0) as u64;
        let Some(position) = self.constrained_motion(delta, delta, utime, target) else { return };
        if self.handoff_cross(Source::Peer(peer), position) {
            return;
        }
        let position = self.desktop_motion(position);
        let time = self.input_time();
        self.on_motion(position, time);
        self.activate_constraint_under();
        self.schedule_render();
    }

    pub(crate) fn remote_button(&mut self, peer: PeerId, button: u32, pressed: bool) {
        // A release always goes through, or the button would stay down.
        if pressed && !self.remote_input_allowed(peer) {
            return;
        }
        if !self.handoff.held.button(Source::Peer(peer), button, pressed) {
            return;
        }
        let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
        let time = self.input_time();
        self.on_button(button, state, time);
        self.schedule_render();
    }

    pub(crate) fn remote_key(&mut self, peer: PeerId, key: u32, pressed: bool) {
        // A release always goes through, or the key would stay down.
        if pressed && !self.remote_input_allowed(peer) {
            return;
        }
        if !self.handoff.held.key(Source::Peer(peer), key, pressed) {
            return;
        }
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        let time = self.input_time();
        self.deliver_key(Keycode::new(key + XKB_OFFSET), state, time);
        self.schedule_render();
    }

    pub(crate) fn remote_scroll(&mut self, peer: PeerId, scroll: Scroll) {
        if !self.remote_input_allowed(peer) {
            return;
        }
        let input = AxisInput {
            source: if scroll.finger { AxisSource::Finger } else { AxisSource::Wheel },
            time: self.input_time(),
            horizontal: scroll.horizontal,
            vertical: scroll.vertical,
            horizontal_v120: scroll.horizontal_v120,
            vertical_v120: scroll.vertical_v120,
        };
        self.on_axis(&input);
        self.schedule_render();
    }

    /// Lets go of everything another machine still holds here.
    pub(crate) fn forget_input(&mut self, peer: PeerId) {
        self.release_away_keys(peer);
        let released = self.handoff.held.release(Source::Peer(peer));
        let time = self.input_time();
        for key in released.keys {
            self.deliver_key(Keycode::new(key + XKB_OFFSET), KeyState::Released, time);
        }
        for button in released.buttons {
            self.on_button(button, ButtonState::Released, time);
        }
    }
}
