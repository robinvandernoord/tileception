//! Keyboard shortcuts and pointer handling.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Device as _, Event, GestureBeginEvent, GestureEndEvent,
        GesturePinchUpdateEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
        PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, TouchEvent,
    },
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};
use tileception_core::{Axis as LayoutAxis, Dir, Node, Side, WindowId};
use tileception_handoff::Source;

use crate::{
    commands::Command,
    config::{Mods, TAP, WHEEL_DOWN, WHEEL_LEFT, WHEEL_RIGHT, WHEEL_UP},
    key_repeat::Repeat,
    state::{GapDrag, MoveDrag, PendingPress, ResizeDrag, Tileception},
};

pub(crate) const BTN_LEFT: u32 = 0x110;
pub(crate) const BTN_RIGHT: u32 = 0x111;
/// Pixels the pointer must travel before a press becomes a drag.
pub(crate) const DRAG_THRESHOLD: f64 = 6.0;
/// Largest share of the screen a dragged window's picture takes.
const DRAG_PICTURE: f64 = 0.3;

/// A compositor command bound to a key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Quit,
    /// Starts the compositor again, as installed now, in the same session.
    Restart,
    Spawn,
    Close,
    Overview,
    ExitOverview,
    StepIn,
    StepOut,
    Neighbour(Dir),
    Move(Dir),
    Back,
    Forward,
    Maximize,
    ToggleDesktop,
    ToggleFloat,
    NextTab,
    PrevTab,
    SaveView(usize),
    RecallView(usize),
    /// Switch to virtual terminal n.
    #[allow(clippy::min_ident_chars, reason = "preserve the existing enum variant")]
    Vt(i32),
    /// Toggle fullscreen for the focused window.
    Fullscreen,
    /// Step in toward the window under the pointer.
    StepInPointer,
    /// Focus the next (true) or previous window.
    Cycle(bool),
    /// Stack the focused tile's row, or lay its column side by side.
    ToggleSplit,
    /// Grow the focused window by pixels (negative shrinks).
    Resize(f64, f64),
    /// Lock the screen with the lock command.
    Lock,
    /// Open the power menu.
    PowerMenu,
    Launcher,
    Notifications,
    DoNotDisturb,
    Group,
    /// Take the focused window out of the layout without closing it.
    Minimize,
    /// Put a minimized window back where it was: this one, or the last.
    Restore(Option<WindowId>),
}

impl Tileception {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        let touchscreen = match &event {
            InputEvent::TouchDown { event } => Some(event.device().name()),
            InputEvent::TouchMotion { event } => Some(event.device().name()),
            InputEvent::TouchUp { event } => Some(event.device().name()),
            InputEvent::TouchCancel { event } => Some(event.device().name()),
            InputEvent::TouchFrame { event } => Some(event.device().name()),
            _ => None,
        };
        if let Some(mapping) = touchscreen.as_ref().and_then(|name| self.config.file.touchscreens.get(name)) {
            // An explicitly mapped touchscreen never falls through to another
            // screen when its display is disconnected.
            let Some(output) = self.output_named(&mapping.output) else { return };
            self.activate_output(&output);
        }
        // The pointer, hover states and whatever the input changes.
        self.schedule_render();
        if matches!(
            &event,
            InputEvent::Keyboard { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerMotion { .. }
                | InputEvent::PointerButton { .. }
                | InputEvent::PointerAxis { .. }
                | InputEvent::GestureSwipeBegin { .. }
                | InputEvent::GesturePinchBegin { .. }
                | InputEvent::GestureHoldBegin { .. }
                | InputEvent::TouchDown { .. }
        ) && self.idle_activity()
        {
            return;
        }
        if self.forward_input(&event) {
            return;
        }
        if matches!(
            event,
            InputEvent::Keyboard { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerMotion { .. }
                | InputEvent::PointerButton { .. }
        ) {
            self.used_here();
        }
        if self.handoff.away
            && matches!(
                event,
                InputEvent::PointerMotionAbsolute { .. }
                    | InputEvent::PointerMotion { .. }
                    | InputEvent::PointerButton { .. }
                    | InputEvent::PointerAxis { .. }
                    | InputEvent::TouchDown { .. }
            )
        {
            // This machine's own pointer is used here again.
            self.handoff.away = false;
            self.schedule_render();
        }
        match event {
            InputEvent::Keyboard { event } => self.on_key::<I>(event),
            InputEvent::PointerMotionAbsolute { event } => {
                if self.output.is_none() {
                    return;
                }
                let screen = self.desktop_bounds();
                let pos = event.position_transformed((screen.w as i32, screen.h as i32).into());
                let origin = self.output.as_ref().map(|output| output.current_location().to_f64()).unwrap_or_default();
                self.handoff.absolute = Some(pos);
                let pos = pos + Point::from((screen.x, screen.y)) - origin;
                if self.handoff_cross(Source::Local, pos) {
                    return;
                }
                let pos = self.desktop_motion(pos);
                self.on_motion(pos, event.time_msec());
                self.activate_constraint_under();
            }
            InputEvent::PointerMotion { event } => {
                let pos = self.pointer + event.delta();
                // A locked or confined pointer may not move; raw motion is reported either way.
                if let Some(pos) = self.constrained_motion(event.delta(), event.delta_unaccel(), event.time(), pos) {
                    if self.handoff_cross(Source::Local, pos) {
                        return;
                    }
                    let pos = self.desktop_motion(pos);
                    self.on_motion(pos, event.time_msec());
                    self.activate_constraint_under();
                }
            }
            InputEvent::PointerButton { event } => {
                // Only the first press and the last release when another
                // machine holds the same button.
                if self.handoff.held.button(Source::Local, event.button_code(), event.state() == ButtonState::Pressed) {
                    self.on_button(event.button_code(), event.state(), event.time_msec())
                }
            }
            InputEvent::PointerAxis { event } => self.on_axis(&AxisInput::from_event::<I>(&event)),
            InputEvent::GestureSwipeBegin { event } => {
                self.gesture_begin(false, event.fingers(), None, event.time_msec())
            }
            InputEvent::GestureSwipeUpdate { event } => self.gesture_update(event.delta(), 1.0, 0.0, event.time_msec()),
            InputEvent::GestureSwipeEnd { event } => self.gesture_end(event.cancelled(), event.time_msec()),
            InputEvent::GesturePinchBegin { event } => {
                self.gesture_begin(true, event.fingers(), None, event.time_msec())
            }
            InputEvent::GesturePinchUpdate { event } => {
                self.gesture_update(event.delta(), event.scale(), event.rotation(), event.time_msec())
            }
            InputEvent::GesturePinchEnd { event } => self.gesture_end(event.cancelled(), event.time_msec()),
            InputEvent::GestureHoldBegin { event } => {
                self.gesture_hold(Some(event.fingers()), false, event.time_msec())
            }
            InputEvent::GestureHoldEnd { event } => self.gesture_hold(None, event.cancelled(), event.time_msec()),
            InputEvent::TouchDown { event } => {
                if self.output.is_none() {
                    return;
                }
                let screen = self.output_rect();
                let pos = event.position_transformed((screen.w as i32, screen.h as i32).into());
                self.touch_down(event.slot(), pos, event.time_msec());
            }
            InputEvent::TouchMotion { event } => {
                if self.output.is_none() {
                    return;
                }
                let screen = self.output_rect();
                let pos = event.position_transformed((screen.w as i32, screen.h as i32).into());
                self.touch_motion(event.slot(), pos, event.time_msec());
            }
            InputEvent::TouchUp { event } => self.touch_up(event.slot(), event.time_msec()),
            InputEvent::TouchCancel { event } => self.touch_cancel(event.time_msec()),
            InputEvent::TouchFrame { .. } => self.touch_frame(),
            _ => {}
        }
    }

    fn on_key<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        self.handle_key(event.key_code(), event.state(), Event::time_msec(&event));
    }

    /// A key, from a keyboard or from `tileception msg key` in tests.
    pub(crate) fn handle_key(&mut self, code: smithay::input::keyboard::Keycode, key_state: KeyState, time: u32) {
        let visible =
            self.handoff.held.key(Source::Local, code.raw().saturating_sub(8), key_state == KeyState::Pressed);
        if let Some(peer) = self.handoff.forwarding {
            self.forward_key(peer, code, key_state, time);
        } else if visible {
            self.deliver_key(code, key_state, time);
        }
    }

    /// A key for this machine, from here or from another machine.
    pub(crate) fn deliver_key(&mut self, code: smithay::input::keyboard::Keycode, key_state: KeyState, time: u32) {
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let pressed = key_state == KeyState::Pressed;
        let keycode = code.raw();
        let mut modifier = false;
        let commands = keyboard.input::<Vec<Command>, _>(self, code, key_state, serial, time, |state, mods, handle| {
            let sym = handle.modified_sym().raw();
            let raw_sym = handle.raw_syms().first().map(|keysym| keysym.raw()).unwrap_or(sym);
            modifier = crate::config::modifier_of(raw_sym).is_some();
            if state.is_locked() {
                state.tap_armed = None;
                state.tap_fired.clear();
                if let Some(command @ Command::Action(Action::Vt(_))) =
                    builtin(Mods::from_state(mods), sym, raw_sym, false)
                    && pressed
                {
                    return FilterResult::Intercept(vec![command]);
                }
                // Volume, brightness and media keys work on the lock screen.
                if pressed {
                    let held = Mods::from_state(mods);
                    let media = state.config.lookup(held, raw_sym).or_else(|| state.config.lookup(held, sym)).filter(
                        |matched| {
                            !matched.is_empty() && matched.iter().all(|command| matches!(command, Command::Media(_)))
                        },
                    );
                    if let Some(commands) = media {
                        return FilterResult::Intercept(commands.to_vec());
                    }
                }
                // The built-in lock screen and the greeter are the same tiles.
                if state.draws_lock_screen() {
                    let (held, caps_lock, layout) =
                        (Mods::from_state(mods), mods.caps_lock, mods.serialized.layout_effective);
                    state.greeter_key(sym, held, caps_lock, layout, pressed);
                    if pressed && crate::key_repeat::edits(sym, held) {
                        state.greeter_repeat = Some(Repeat::Greeter { sym, mods: held, caps_lock, layout });
                    }
                    return FilterResult::Intercept(Vec::new());
                }
                return FilterResult::Forward;
            }
            // Applications' global shortcuts, and the question for their keys,
            // which takes every key press but modifiers.
            let trigger = crate::global_shortcuts::Trigger::new(Mods::from_state(mods), raw_sym);
            let modifier = crate::config::modifier_of(raw_sym).is_some() || handle.modified_sym().is_modifier_key();
            if state.asking_for_shortcut() && pressed && !modifier {
                let held = Mods::from_state(mods);
                let taken = builtin(held, sym, raw_sym, false).is_some()
                    || state.config.lookup(held, raw_sym).or_else(|| state.config.lookup(held, sym)).is_some();
                state.shortcut_keys_pressed(trigger, types(sym), taken);
                return FilterResult::Intercept(Vec::new());
            }
            if state.portal_shortcut_key(keycode, trigger, pressed) {
                return FilterResult::Intercept(Vec::new());
            }
            if let Some(tap) = state.track_tap(pressed, raw_sym, Mods::from_state(mods)) {
                // The release still goes to the client, so it does not
                // think the modifier is stuck; the binding runs after.
                state.tap_fired = state.config.lookup(tap, TAP).map(<[Command]>::to_vec).unwrap_or_default();
            }
            if !pressed {
                return FilterResult::Forward;
            }
            let mut held = Mods::from_state(mods);
            // Unmodified, so Super+Shift+1 is "1" and not "!".
            let raw = handle.raw_syms().first().map(|keysym| keysym.raw()).unwrap_or(sym);
            if let Some(command) =
                builtin(held, sym, raw, state.world.in_overview() && !state.launcher.open && state.power_menu.is_none())
            {
                return FilterResult::Intercept(vec![command]);
            }
            if state.power_menu.is_some() {
                // The menu takes every key while it is open.
                return FilterResult::Intercept(vec![Command::PowerMenuKey(sym)]);
            }
            // A layer surface with the keyboard (slurp) gets it over the panels.
            let panels = state.layer_focus.is_none();
            if panels && sym == keysyms::KEY_Escape && held == Mods::default() && state.close_tray() {
                return FilterResult::Intercept(Vec::new());
            }
            if panels && state.duck.typing() {
                // The duck's question, like the doctor's field: Super
                // shortcuts and keys that do not type still work.
                let mut bound = held;
                if state.nested && bound.alt && !bound.logo {
                    bound.alt = false;
                    bound.logo = true;
                }
                let commands = state.config.lookup(bound, raw).or_else(|| state.config.lookup(bound, sym));
                if bound.logo {
                    return FilterResult::Intercept(commands.map(<[Command]>::to_vec).unwrap_or_default());
                }
                if let Some(commands) = commands.filter(|matched| !matched.is_empty() && !types(sym)) {
                    return FilterResult::Intercept(commands.to_vec());
                }
                return FilterResult::Intercept(vec![Command::DuckKey(sym, Mods::from_state(mods))]);
            }
            if panels && state.launcher.open && state.launcher.home {
                // The welcome screen is the desktop: shortcuts still work,
                // and Esc closes the notification panel first.
                if sym == keysyms::KEY_Escape && held == Mods::default() && state.notifications.open {
                    state.notifications.open = false;
                    return FilterResult::Intercept(Vec::new());
                }
                let mut bound = held;
                if state.nested && bound.alt && !bound.logo {
                    bound.alt = false;
                    bound.logo = true;
                }
                if let Some(commands) = state.config.lookup(bound, raw).or_else(|| state.config.lookup(bound, sym)) {
                    return FilterResult::Intercept(commands.to_vec());
                }
            }
            if panels && state.launcher.open {
                return FilterResult::Intercept(vec![Command::LauncherKey(sym, Mods::from_state(mods))]);
            }
            if panels && state.doctor.open && state.doctor.focused {
                // Typing goes to the panel; Super shortcuts and keys that do
                // not type (Print, volume, media) still work.
                let mut bound = held;
                if state.nested && bound.alt && !bound.logo {
                    bound.alt = false;
                    bound.logo = true;
                }
                let commands = state.config.lookup(bound, raw).or_else(|| state.config.lookup(bound, sym));
                if bound.logo {
                    return FilterResult::Intercept(commands.map(<[Command]>::to_vec).unwrap_or_default());
                }
                if let Some(commands) = commands.filter(|matched| !matched.is_empty() && !types(sym)) {
                    return FilterResult::Intercept(commands.to_vec());
                }
                return FilterResult::Intercept(vec![Command::DoctorKey(sym, Mods::from_state(mods))]);
            }
            if sym == keysyms::KEY_Escape && held == Mods::default() && state.close_calendar() {
                return FilterResult::Intercept(Vec::new());
            }
            if sym == keysyms::KEY_Escape
                && held == Mods::default()
                && (state.notifications.open || !state.selection.windows.is_empty())
            {
                state.notifications.open = false;
                state.selection.windows.clear();
                return FilterResult::Intercept(Vec::new());
            }
            if state.nested && held.alt && !held.logo {
                // The host desktop keeps Super for itself; Alt stands in.
                held.alt = false;
                held.logo = true;
            }
            match state.config.lookup(held, raw).or_else(|| state.config.lookup(held, sym)) {
                Some(commands) => FilterResult::Intercept(commands.to_vec()),
                None => FilterResult::Forward,
            }
        });
        if commands.is_none() {
            self.key_to_owner(code, key_state);
        }
        let tapped = std::mem::take(&mut self.tap_fired);
        let commands = commands.unwrap_or_default();
        let greeter_repeat = self.greeter_repeat.take();
        if !pressed {
            self.key_released(keycode, modifier);
        } else if let Some(repeat) = greeter_repeat.or_else(|| Repeat::for_commands(&commands)) {
            self.start_key_repeat(keycode, repeat);
        } else {
            self.stop_key_repeat();
        }
        for command in commands.into_iter().chain(tapped) {
            if let Err(err) = self.execute(command) {
                tracing::warn!("{err}");
            }
        }
    }

    /// Whether the launcher gets the pointer here: all of it when opened on
    /// purpose; as the welcome screen only over itself and not under the
    /// notification panel. Never under an overlay layer surface.
    pub(crate) fn launcher_takes(&self, point: tileception_core::Point) -> bool {
        if self.overlay_under(point) {
            return false;
        }
        !self.launcher.home || (self.home_contains(point) && !self.notifications_contain(point))
    }

    /// Follows a modifier pressed on its own. Returns the modifier when it is
    /// let go without anything else happening in between.
    fn track_tap(&mut self, pressed: bool, sym: u32, held: Mods) -> Option<Mods> {
        let this = crate::config::modifier_of(sym);
        match (pressed, this) {
            // Held now means held including this key: only it may be down.
            (true, Some(modifier)) if held == modifier || held == Mods::default() => self.tap_armed = Some(modifier),
            (true, _) => self.tap_armed = None,
            (false, Some(modifier)) if self.tap_armed == Some(modifier) => {
                self.tap_armed = None;
                return Some(modifier);
            }
            (false, _) => {}
        }
        None
    }

    pub fn run(&mut self, action: Action) {
        if let Action::Restore(Some(window)) = action
            && let Some(output) = self.output_for_window(window)
        {
            self.activate_output(&output);
        }
        if self.selection_action(action) {
            if action != Action::Close {
                self.persist_layout();
            }
            return;
        }
        let now = self.now();
        // Navigating leaves fullscreen; closing, spawning and VT switches do not.
        let keeps_fullscreen = matches!(
            action,
            Action::Close
                | Action::Quit
                | Action::Restart
                | Action::Spawn
                | Action::Vt(_)
                | Action::Fullscreen
                | Action::Resize(..)
                | Action::Lock
                | Action::PowerMenu
                | Action::Launcher
                | Action::Notifications
                | Action::DoNotDisturb
        );
        if let (Some(window_id), false) = (self.fullscreen, keeps_fullscreen) {
            self.set_fullscreen(window_id, false);
        }
        tracing::debug!(?action, "action");
        match action {
            Action::Quit => self.loop_signal.stop(),
            Action::Restart => {
                self.restart = true;
                self.loop_signal.stop();
            }
            Action::Spawn => {
                let terminal = self.terminal.clone();
                let mut parts = terminal.split_whitespace();
                if let Some(cmd) = parts.next() {
                    let args: Vec<String> = parts.map(String::from).collect();
                    self.spawn(cmd, &args);
                }
            }
            Action::Close => self.close_focused(),
            Action::Overview => self.world.toggle_overview(now),
            Action::ExitOverview => self.world.exit_overview(now),
            Action::StepIn => self.world.step_in(None, now),
            Action::StepInPointer => {
                let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
                self.world.step_in(Some(point), now);
            }
            Action::Cycle(forward) => self.world.cycle(forward, now),
            Action::ToggleSplit => self.world.toggle_split(now),
            Action::Resize(width_delta, height_delta) => {
                self.world.resize_focused(width_delta, height_delta, now);
            }
            Action::StepOut => self.world.step_out(now),
            Action::Neighbour(dir) => self.world.neighbour(dir, now),
            Action::Move(dir) => self.world.move_focused(dir, now),
            Action::Back => self.world.back(now),
            Action::Forward => self.world.forward(now),
            Action::Maximize => self.world.maximize(now),
            Action::ToggleDesktop => self.world.toggle_desktop(now),
            Action::ToggleFloat => {
                if let Some(focused) = self.world.focus() {
                    self.world.toggle_float(focused, now);
                }
            }
            Action::NextTab => self.world.cycle_tab(1),
            Action::PrevTab => self.world.cycle_tab(-1),
            Action::SaveView(slot) => self.world.save_view(slot),
            Action::RecallView(slot) => {
                self.world.recall_view(slot, now);
            }
            Action::Vt(vt_number) => self.change_vt(vt_number),
            Action::Lock => {
                self.lock_session(false);
            }
            Action::PowerMenu => self.shell_power_menu(),
            Action::Launcher => self.shell_launcher(),
            Action::Notifications => self.shell_notifications(),
            Action::DoNotDisturb => self.toggle_dnd(),
            Action::Group => {}
            Action::Minimize => {
                if let Some(window_id) = self.world.focus() {
                    self.minimize_window(window_id);
                }
            }
            Action::Restore(target) => {
                match target {
                    Some(window_id) => self.world.restore(window_id, now),
                    None => self.world.restore_last(now),
                };
            }
            Action::Fullscreen => {
                if let Some(window_id) = self.world.focus() {
                    let enable = self.fullscreen != Some(window_id);
                    self.set_fullscreen(window_id, enable);
                }
            }
        }
        self.apply_layout();
        if matches!(
            action,
            Action::Move(_)
                | Action::Resize(..)
                | Action::ToggleFloat
                | Action::ToggleSplit
                | Action::SaveView(_)
                | Action::RecallView(_)
                | Action::NextTab
                | Action::PrevTab
                | Action::ToggleDesktop
                | Action::Minimize
                | Action::Restore(_)
        ) {
            self.persist_layout();
        }
    }

    pub(crate) fn on_motion(&mut self, pos: Point<f64, Logical>, time: u32) {
        self.pointer = pos;
        if self.draws_lock_screen() {
            self.greeter_motion(pos.x, pos.y);
            return;
        }
        let point = tileception_core::Point::new(pos.x, pos.y);
        if !self.is_locked() && self.power_menu.is_none() && self.handoff_prompt_motion(pos) {
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if !self.is_locked() && self.power_menu.is_none() && self.tray.open && self.bar_motion(pos, time) {
            return;
        }
        if !self.is_locked() && self.launcher.open && self.launcher_takes(point) {
            self.launcher_motion(point);
            return;
        }
        if self.power_menu.is_some() {
            if let (Some(index), Some(menu)) = (self.power_action_at(pos.x, pos.y), self.power_menu.as_mut()) {
                menu.selected = index;
            }
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if self.is_locked() {
            let under = self.lock_surface_under(pos);
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, under, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if self.media.drag.is_some() {
            // A held slider follows the pointer even outside the panel.
            self.controls_drag(pos.x);
            return;
        }
        if self.duck_motion(point) || self.doctor_drag(point) {
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if self.doctor_contains(point) {
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if self.notifications_contain(tileception_core::Point::new(pos.x, pos.y)) {
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if self.bar_motion(pos, time) {
            return;
        }
        if self.strip_motion(pos, time) {
            return;
        }
        let now = self.now();
        if self.selection.anchor.is_some() {
            self.selection_motion(tileception_core::Point::new(pos.x, pos.y));
            return;
        }
        if let Some(mut drag) = self.gap_drag {
            let delta = if drag.horizontal { pos.x - drag.last.x } else { pos.y - drag.last.y };
            if self.world.resize_gap(drag.split, drag.index, delta, now) {
                drag.last = pos;
                self.gap_drag = Some(drag);
                self.apply_layout();
            }
            return;
        }
        if let Some(press) = self.pending_press {
            let moved = (pos.x - press.position.x).hypot(pos.y - press.position.y);
            if let (Some(window), true) = (press.window, moved > DRAG_THRESHOLD) {
                self.pending_press = None;
                self.start_move_drag(window, press.position);
            }
        }
        if let Some(mut drag) = self.resize_drag {
            let (delta_x, delta_y) = (pos.x - drag.last.x, pos.y - drag.last.y);
            self.world.resize_window(drag.window, drag.side_x, delta_x, drag.side_y, delta_y, now);
            drag.last = pos;
            self.resize_drag = Some(drag);
            self.apply_layout();
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if let Some(mut drag) = self.move_drag {
            if drag.free {
                // Floating windows move freely, as themselves.
                if let Some(rect) = self.world.float_rect(drag.window) {
                    let moved = tileception_core::Rect::new(
                        rect.x + pos.x - drag.last.x,
                        rect.y + pos.y - drag.last.y,
                        rect.w,
                        rect.h,
                    );
                    self.world.set_float_rect(drag.window, moved);
                    self.apply_layout();
                }
                drag.last = pos;
            } else {
                let drop_point = tileception_core::Point::new(pos.x, pos.y);
                drag.target = self.world.drop_target(drop_point, drag.window, self.world.view(now));
            }
            self.move_drag = Some(drag);
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        if self.pointer_to_owner(pos, time) {
            return;
        }
        let under = self.surface_under(pos);
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        pointer.motion(self, under, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    /// A client asked to be moved from its own title bar.
    pub fn begin_title_drag(&mut self, window: WindowId) {
        if self.move_away(window) {
            return;
        }
        if self.move_drag.is_none() && !self.world.in_overview() {
            self.world.focus_window(window, self.now());
            self.apply_layout();
            let position = self.pointer;
            self.start_move_drag(window, position);
        }
    }

    /// Picks a window up so it follows the pointer as a picture.
    pub(crate) fn start_move_drag(&mut self, window: WindowId, position: Point<f64, Logical>) {
        let now = self.now();
        let screen = self.world.screen();
        let shown =
            self.drawn_windows(now).into_iter().find(|drawn| drawn.window_id == window).map(|drawn| drawn.content);
        let (grab, size) = match shown {
            Some(content) if content.w > 0.0 && content.h > 0.0 => {
                let grab = (
                    ((position.x - content.x) / content.w).clamp(0.0, 1.0),
                    ((position.y - content.y) / content.h).clamp(0.0, 1.0),
                );
                let scale = (DRAG_PICTURE * screen.w / content.w).min(DRAG_PICTURE * screen.h / content.h).min(1.0);
                (grab, (content.w * scale, content.h * scale))
            }
            // A tab that is not on show: a picture of a typical size.
            _ => ((0.5, 0.1), (DRAG_PICTURE * screen.w, DRAG_PICTURE * screen.h)),
        };
        let free = self.world.tree().is_floating(window) && !self.world.in_overview();
        self.move_drag = Some(MoveDrag { window, grab, size, target: None, free, last: position });
    }

    pub(crate) fn on_button(&mut self, button: u32, state: ButtonState, time: u32) {
        self.tap_armed = None;
        // A held slider lets go wherever the button comes up.
        if state == ButtonState::Released && button == BTN_LEFT && self.controls_release() {
            return;
        }
        if self.draws_lock_screen() {
            if button == BTN_LEFT && state == ButtonState::Pressed {
                self.greeter_click(self.pointer.x, self.pointer.y);
            }
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if self.is_locked() {
            pointer.button(self, &ButtonEvent { button, state, serial, time });
            pointer.frame(self);
            return;
        }
        let pointer_at = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        if self.power_menu.is_none() && self.handoff_prompt_button(button, state == ButtonState::Pressed) {
            return;
        }
        if self.power_menu.is_none()
            && (self.tray.open || state == ButtonState::Released)
            && self.bar_button(button, state == ButtonState::Pressed)
        {
            return;
        }
        if self.launcher.open && state == ButtonState::Released && button == BTN_LEFT {
            self.launcher_release();
        }
        if self.launcher.open && self.launcher_takes(pointer_at) {
            if state == ButtonState::Pressed && button == BTN_LEFT {
                self.launcher_click(pointer_at);
            }
            return;
        }
        if self.power_menu.is_some() {
            // A click picks a button; a click anywhere else closes the menu.
            if state == ButtonState::Pressed {
                match self.power_action_at(self.pointer.x, self.pointer.y) {
                    Some(index) => self.activate_power_item(index),
                    None => self.close_power_menu(),
                }
            }
            return;
        }
        let now = self.now();
        let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        if state == ButtonState::Released && button == BTN_LEFT && self.controls_release() {
            return;
        }
        // A held duck lets go wherever the button comes up.
        if state == ButtonState::Released && self.duck_pressed() && self.duck_button(button == BTN_LEFT, false, point) {
            return;
        }
        if state == ButtonState::Released && button == BTN_LEFT && self.doctor_release() {
            return;
        }
        if state == ButtonState::Pressed && button == BTN_LEFT && self.doctor_press(point) {
            return;
        }
        if self.doctor_contains(point) {
            return;
        }
        if self.notifications_contain(point) {
            if state == ButtonState::Pressed && button == BTN_LEFT {
                self.notification_click(point);
            }
            return;
        }
        if self.move_drag.is_none() && self.duck_button(button == BTN_LEFT, state == ButtonState::Pressed, point) {
            return;
        }
        if self.move_drag.is_none() && self.bar_button(button, state == ButtonState::Pressed) {
            return;
        }
        if self.move_drag.is_none() && self.strip_button(button, state == ButtonState::Pressed) {
            return;
        }

        if state == ButtonState::Released {
            if let Some(anchor) = self.selection.anchor.take() {
                if (point.x - anchor.x).hypot(point.y - anchor.y) < DRAG_THRESHOLD {
                    // A click, not a box: keep the selection as it was before the press,
                    // toggling the window under a Super+Shift click.
                    self.selection.windows = std::mem::take(&mut self.selection.additive);
                    let mods = self.seat.get_keyboard().expect("seat has a keyboard").modifier_state();
                    if mods.shift && mods.logo {
                        let toggled = self.tab_at(anchor, now).or_else(|| {
                            self.drawn_windows(now)
                                .into_iter()
                                .find(|drawn| drawn.content.contains(anchor))
                                .map(|drawn| drawn.window_id)
                        });
                        if let Some(window) = toggled {
                            self.toggle_selection(window);
                        }
                        return;
                    }
                }
                if self.world.in_overview() && (point.x - anchor.x).hypot(point.y - anchor.y) < DRAG_THRESHOLD {
                    self.world.click_in_overview(anchor, now);
                    self.apply_layout();
                }
                return;
            }
            if self.gap_drag.take().is_some() || self.resize_drag.take().is_some() {
                self.persist_layout();
                return;
            }
            if let Some(drag) = self.move_drag.take() {
                if let Some(target) = drag.target {
                    self.world.apply_drop(drag.window, target, now);
                }
                self.apply_layout();
                self.persist_layout();
                // A title bar drag began with a press the client saw; close it.
                pointer.button(self, &ButtonEvent { button, state, serial, time });
                pointer.frame(self);
                return;
            }
            if let Some(press) = self.pending_press.take() {
                if self.world.in_overview() {
                    let press_point = tileception_core::Point::new(press.position.x, press.position.y);
                    self.world.click_in_overview(press_point, now);
                    self.apply_layout();
                }
                return;
            }
        }

        let over_layer = {
            use smithay::wayland::shell::wlr_layer::Layer;
            self.layer_surface_under(self.pointer, &[Layer::Overlay, Layer::Top]).is_some()
        };
        let drag_held = {
            let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
            Mods::from_state(&keyboard.modifier_state()).has(self.config.drag_mod)
        };
        if state == ButtonState::Pressed && button == BTN_RIGHT && drag_held && !pointer.is_grabbed() && !over_layer {
            // Modifier + right drag resizes from the nearest corner.
            if let Some(shown) = self.drawn_windows(now).into_iter().find(|candidate| candidate.content.contains(point))
            {
                self.world.focus_window(shown.window_id, now);
                self.apply_layout();
                let centre = shown.content.center();
                let side_x = if point.x < centre.x { Side::Left } else { Side::Right };
                let side_y = if point.y < centre.y { Side::Top } else { Side::Bottom };
                self.resize_drag = Some(ResizeDrag { window: shown.window_id, side_x, side_y, last: self.pointer });
                return;
            }
        }
        if state == ButtonState::Pressed && button == BTN_LEFT && !pointer.is_grabbed() && !over_layer {
            let drawn = self.drawn_windows(now);
            let under = drawn.iter().find(|candidate| candidate.content.contains(point)).copied();
            let mods = self.seat.get_keyboard().expect("seat has a keyboard").modifier_state();
            let tab = self.tab_at(point, now);
            let overview = self.world.in_overview();
            // Super+Shift: a click toggles the window, a drag selects with a box.
            // In Overview a drag from empty space also selects with a box.
            if (mods.shift && mods.logo) || (overview && under.is_none() && tab.is_none()) {
                self.selection.anchor = Some(point);
                self.selection.additive = if mods.shift { self.selection.windows.clone() } else { Default::default() };
                return;
            }
            // Shift+click selects tabs and previews; on a live window it belongs
            // to the program, for example to extend a text selection.
            if mods.shift
                && (tab.is_some() || under.is_some_and(|candidate| !candidate.live))
                && let Some(window) = tab.or(under.map(|drawn| drawn.window_id))
            {
                self.toggle_selection(window);
                return;
            }
            if !mods.shift {
                self.selection.windows.clear();
            }
            if overview {
                // Dragging a window in Overview moves it; a click zooms in.
                self.pending_press =
                    Some(PendingPress { position: self.pointer, window: under.map(|drawn| drawn.window_id) });
                return;
            }

            // A click during an animation ends it.
            if self.world.is_animating(now) {
                self.world.finish_animation();
            }
            if let Some(drag) = self.gap_at(self.pointer) {
                self.gap_drag = Some(drag);
                return;
            }
            if let Some(tab) = self.tab_at(point, now) {
                self.world.focus_window(tab, now);
                self.apply_layout();
                self.pending_press = Some(PendingPress { position: self.pointer, window: Some(tab) });
                return;
            }
            let Some(shown) = under else {
                pointer.button(self, &ButtonEvent { button, state, serial, time });
                pointer.frame(self);
                return;
            };
            if self.world.focus() != Some(shown.window_id) {
                self.world.focus_window(shown.window_id, now);
                self.apply_layout();
            }
            if drag_held {
                self.start_move_drag(shown.window_id, self.pointer);
                return;
            }
            if !shown.live {
                // A click on a preview only focuses it.
                return;
            }
        }
        if self.button_to_owner(button, state) {
            return;
        }
        pointer.button(self, &ButtonEvent { button, state, serial, time });
        pointer.frame(self);
    }

    /// The window whose tab is under a point.
    pub(crate) fn tab_at(&self, point: tileception_core::Point, now: f64) -> Option<WindowId> {
        let strip =
            self.world.tab_strips(self.world.view(now)).into_iter().find(|tab_strip| tab_strip.rect.contains(point))?;
        let index = ((point.x - strip.rect.x) / strip.rect.w * strip.views.len() as f64) as usize;
        strip.views.get(index.min(strip.views.len() - 1)).copied()
    }

    pub(crate) fn on_axis(&mut self, event: &AxisInput) {
        if self.draws_lock_screen() {
            return;
        }
        if !self.is_locked() && self.power_menu.is_none() && self.tray.open {
            let notches = event
                .amount_v120(Axis::Vertical)
                .map(|v120| v120 / 120.0)
                .or_else(|| event.amount(Axis::Vertical).map(|amount| amount / 50.0))
                .unwrap_or(0.0);
            self.bar_scroll(notches);
            return;
        }
        if !self.is_locked() && self.power_menu.is_none() {
            let notches = event
                .amount_v120(Axis::Vertical)
                .or_else(|| event.amount_v120(Axis::Horizontal))
                .map(|v120| v120 / 120.0)
                .or_else(|| {
                    event.amount(Axis::Vertical).or_else(|| event.amount(Axis::Horizontal)).map(|amount| amount / 50.0)
                })
                .unwrap_or(0.0);
            if self.strip_scroll(notches) {
                return;
            }
        }
        if self.launcher.open
            && !self.is_locked()
            && self.launcher_takes(tileception_core::Point::new(self.pointer.x, self.pointer.y))
        {
            if let Some(value) = event.amount(Axis::Vertical).or_else(|| event.amount_v120(Axis::Vertical))
                && value != 0.0
            {
                self.launcher_scroll(if value > 0.0 { 1 } else { -1 });
            }
            return;
        }
        if !self.is_locked() && self.doctor_contains(tileception_core::Point::new(self.pointer.x, self.pointer.y)) {
            let amount = event.amount(Axis::Vertical).or_else(|| event.amount_v120(Axis::Vertical)).unwrap_or(0.0);
            if amount != 0.0 {
                self.doctor_scroll(if amount > 0.0 { 1.0 } else { -1.0 });
            }
            return;
        }
        if !self.is_locked()
            && self.notifications.open
            && self.notifications_contain(tileception_core::Point::new(self.pointer.x, self.pointer.y))
        {
            let point = tileception_core::Point::new(self.pointer.x, self.pointer.y);
            let discrete = event.amount_v120(Axis::Vertical).is_some() && event.source() == AxisSource::Wheel;
            let amount = event.amount(Axis::Vertical).or_else(|| event.amount_v120(Axis::Vertical)).unwrap_or(0.0);
            if amount != 0.0 && self.controls_scroll(point, amount, discrete) {
                return;
            }
            if amount != 0.0 {
                self.notification_scroll(if amount > 0.0 { 1 } else { -1 });
            }
            return;
        }
        if !self.is_locked() && self.bar_takes(tileception_core::Point::new(self.pointer.x, self.pointer.y)) {
            // In notches: a wheel step is one, smooth scrolling adds up to one.
            let notches = event
                .amount_v120(Axis::Vertical)
                .map(|v120| v120 / 120.0)
                .or_else(|| event.amount(Axis::Vertical).map(|amount| amount / 50.0))
                .unwrap_or(0.0);
            self.bar_scroll(notches);
            return;
        }
        self.tap_armed = None;
        if !self.is_locked() && self.wheel_binding(event) {
            return;
        }
        if self.scroll_to_owner(event) {
            return;
        }
        self.deliver_axis(event);
    }

    /// Scrolling for the program under the pointer.
    pub(crate) fn deliver_axis(&mut self, event: &AxisInput) {
        let source = event.source();
        let amount = |axis| event.amount(axis).unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
        let horizontal = amount(Axis::Horizontal);
        let vertical = amount(Axis::Vertical);
        let mut frame = AxisFrame::new(event.time_msec()).source(source);
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
            if let Some(v120) = event.amount_v120(Axis::Horizontal) {
                frame = frame.v120(Axis::Horizontal, v120 as i32);
            }
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
            if let Some(v120) = event.amount_v120(Axis::Vertical) {
                frame = frame.v120(Axis::Vertical, v120 as i32);
            }
        }
        if source == AxisSource::Finger {
            if event.amount(Axis::Horizontal) == Some(0.0) {
                frame = frame.stop(Axis::Horizontal);
            }
            if event.amount(Axis::Vertical) == Some(0.0) {
                frame = frame.stop(Axis::Vertical);
            }
        }
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Runs a binding like `Super+WheelUp`. Returns whether the scroll was
    /// taken: scrolling with a bound modifier never reaches the client.
    fn wheel_binding(&mut self, event: &AxisInput) -> bool {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let mut held = Mods::from_state(&keyboard.modifier_state());
        if held == Mods::default() {
            return false;
        }
        if self.nested && held.alt && !held.logo {
            held.alt = false;
            held.logo = true;
        }
        let bound = |keysym| self.config.lookup(held, keysym).is_some();
        if ![WHEEL_UP, WHEEL_DOWN, WHEEL_LEFT, WHEEL_RIGHT].into_iter().any(bound) {
            return false;
        }
        // Natural scrolling turns the touchpad around for content, not for
        // bindings: Super with two fingers up still steps in.
        let sign =
            if self.config.file.touchpad.natural_scroll && event.source() == AxisSource::Finger { -1.0 } else { 1.0 };
        // One notch of a wheel is 120; smooth scrolling adds up to a step.
        let notch = |axis| {
            sign * event
                .amount_v120(axis)
                .map(|v120| v120 / 120.0)
                .unwrap_or_else(|| event.amount(axis).unwrap_or(0.0) / 50.0)
        };
        self.wheel_accum.0 += notch(Axis::Horizontal);
        self.wheel_accum.1 += notch(Axis::Vertical);
        let mut keys = Vec::new();
        while self.wheel_accum.1 <= -1.0 {
            self.wheel_accum.1 += 1.0;
            keys.push(WHEEL_UP);
        }
        while self.wheel_accum.1 >= 1.0 {
            self.wheel_accum.1 -= 1.0;
            keys.push(WHEEL_DOWN);
        }
        while self.wheel_accum.0 <= -1.0 {
            self.wheel_accum.0 += 1.0;
            keys.push(WHEEL_LEFT);
        }
        while self.wheel_accum.0 >= 1.0 {
            self.wheel_accum.0 -= 1.0;
            keys.push(WHEEL_RIGHT);
        }
        for key in keys {
            for command in self.config.lookup(held, key).map(<[Command]>::to_vec).unwrap_or_default() {
                if let Err(err) = self.execute(command) {
                    tracing::warn!("{err}");
                }
            }
        }
        true
    }

    /// The client surface under a point, for live windows only.
    pub(crate) fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        use smithay::wayland::shell::wlr_layer::Layer;
        if let Some(hit) = self.layer_surface_under(pos, &[Layer::Overlay, Layer::Top]) {
            return Some(hit);
        }
        if let Some(hit) = self.window_surface_under(pos) {
            return Some(hit);
        }
        self.layer_surface_under(pos, &[Layer::Bottom, Layer::Background])
    }

    fn window_surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        for overlay in self.x11_overlays.iter().rev() {
            let geo = overlay.geometry();
            if !geo.to_f64().contains(pos) {
                continue;
            }
            if let Some(surface) = overlay.wl_surface() {
                let hit = smithay::desktop::utils::under_from_surface_tree(
                    &surface,
                    pos,
                    geo.loc,
                    smithay::desktop::WindowSurfaceType::ALL,
                );
                if let Some((hit_surface, loc)) = hit {
                    return Some((hit_surface, loc.to_f64()));
                }
            }
        }
        let point = tileception_core::Point::new(pos.x, pos.y);
        let now = self.now();
        let drawn = self.drawn_windows(now);
        // Menus may reach outside their window's tile.
        for shown in drawn.iter().filter(|candidate| candidate.live) {
            let Some(window) = self.windows.get(&shown.window_id) else { continue };
            let origin: Point<f64, Logical> = (shown.origin.x.round(), shown.origin.y.round()).into();
            if let Some((hit_surface, loc)) =
                window.surface_under(pos - origin, smithay::desktop::WindowSurfaceType::POPUP)
            {
                return Some((hit_surface, loc.to_f64() + origin));
            }
        }
        let shown = drawn.iter().find(|candidate| candidate.content.contains(point))?;
        if !shown.live {
            return None;
        }
        let window = self.windows.get(&shown.window_id)?;
        let origin: Point<f64, Logical> = (shown.origin.x.round(), shown.origin.y.round()).into();
        window
            .surface_under(pos - origin, smithay::desktop::WindowSurfaceType::ALL)
            .map(|(hit_surface, loc)| (hit_surface, loc.to_f64() + origin))
    }

    /// The split boundary under a point, if any, for dragging gaps.
    pub(crate) fn gap_at(&self, pos: Point<f64, Logical>) -> Option<GapDrag> {
        let view = self.world.target_view();
        let tree = self.world.tree();
        let reach = self.world.settings.gap / 2.0 + 4.0;
        for split in tree.subtree(tree.root()) {
            let Node::Split { axis, children, .. } = tree.node(split) else { continue };
            for index in 0..children.len().saturating_sub(1) {
                let (Some(first_rect), Some(_)) =
                    (self.world.node_rect(children[index], view), self.world.node_rect(children[index + 1], view))
                else {
                    continue;
                };
                let hit = match axis {
                    LayoutAxis::Horizontal => {
                        (pos.x - first_rect.right()).abs() <= reach
                            && pos.y >= first_rect.y
                            && pos.y < first_rect.bottom()
                    }
                    LayoutAxis::Vertical => {
                        (pos.y - first_rect.bottom()).abs() <= reach
                            && pos.x >= first_rect.x
                            && pos.x < first_rect.right()
                    }
                };
                if hit {
                    return Some(GapDrag { split, index, last: pos, horizontal: *axis == LayoutAxis::Horizontal });
                }
            }
        }
        None
    }
}

/// Keys that work whatever the config says.
/// One scroll step, from a local pointer or from another machine.
pub(crate) struct AxisInput {
    pub source: AxisSource,
    pub time: u32,
    pub horizontal: Option<f64>,
    pub vertical: Option<f64>,
    pub horizontal_v120: Option<f64>,
    pub vertical_v120: Option<f64>,
}

impl AxisInput {
    fn from_event<I: InputBackend>(event: &I::PointerAxisEvent) -> AxisInput {
        AxisInput {
            source: event.source(),
            time: event.time_msec(),
            horizontal: event.amount(Axis::Horizontal),
            vertical: event.amount(Axis::Vertical),
            horizontal_v120: event.amount_v120(Axis::Horizontal),
            vertical_v120: event.amount_v120(Axis::Vertical),
        }
    }

    fn source(&self) -> AxisSource {
        self.source
    }

    fn time_msec(&self) -> u32 {
        self.time
    }

    fn amount(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Horizontal => self.horizontal,
            Axis::Vertical => self.vertical,
        }
    }

    fn amount_v120(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Horizontal => self.horizontal_v120,
            Axis::Vertical => self.vertical_v120,
        }
    }
}

fn builtin(held: Mods, sym: u32, raw: u32, in_overview: bool) -> Option<Command> {
    let plain = held == Mods::default();
    if in_overview && plain && sym == keysyms::KEY_Escape {
        return Some(Command::Action(Action::ExitOverview));
    }
    if in_overview && plain && sym == keysyms::KEY_Return {
        return Some(Command::Action(Action::StepIn));
    }
    if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&sym) {
        return Some(Command::Action(Action::Vt((sym - keysyms::KEY_XF86Switch_VT_1 + 1) as i32)));
    }
    if held.ctrl && held.alt {
        if (keysyms::KEY_F1..=keysyms::KEY_F12).contains(&raw) {
            return Some(Command::Action(Action::Vt((raw - keysyms::KEY_F1 + 1) as i32)));
        }
        if raw == keysyms::KEY_BackSpace || sym == keysyms::KEY_Terminate_Server {
            return Some(Command::Action(Action::Quit));
        }
    }
    None
}

/// Whether a key is one the Doctor's text field uses: text, or editing and
/// moving around in it. Everything else may run its shortcut.
fn types(sym: u32) -> bool {
    use keysyms::*;
    let editing = [
        KEY_Return,
        KEY_KP_Enter,
        KEY_BackSpace,
        KEY_Delete,
        KEY_Tab,
        KEY_ISO_Left_Tab,
        KEY_Escape,
        KEY_Left,
        KEY_Right,
        KEY_Up,
        KEY_Down,
        KEY_Home,
        KEY_End,
        KEY_Page_Up,
        KEY_Page_Down,
        KEY_Insert,
    ];
    editing.contains(&sym)
        || smithay::input::keyboard::Keysym::new(sym).key_char().is_some_and(|character| !character.is_control())
}
