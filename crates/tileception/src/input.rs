//! Keyboard shortcuts and pointer handling.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, keysyms},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};
use tileception_core::{Axis as LayoutAxis, Dir, Node, Side, WindowId};

use crate::{
    commands::Command,
    config::{Mods, TAP, WHEEL_DOWN, WHEEL_LEFT, WHEEL_RIGHT, WHEEL_UP},
    state::{GapDrag, MoveDrag, PendingPress, ResizeDrag, Tileception},
};

pub(crate) const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
/// Pixels the pointer must travel before a press becomes a drag.
pub(crate) const DRAG_THRESHOLD: f64 = 6.0;
/// Largest share of the screen a dragged window's picture takes.
const DRAG_PICTURE: f64 = 0.3;

/// A compositor command bound to a key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Quit,
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
        if matches!(
            &event,
            InputEvent::Keyboard { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerMotion { .. }
                | InputEvent::PointerButton { .. }
                | InputEvent::PointerAxis { .. }
        ) && self.idle_activity()
        {
            return;
        }
        match event {
            InputEvent::Keyboard { event } => self.on_key::<I>(event),
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output) = self.output.as_ref() else { return };
                let size = output.current_mode().map(|m| m.size.to_logical(1)).unwrap_or_default();
                let pos = event.position_transformed(size);
                self.on_motion(pos, event.time_msec());
                self.activate_constraint_under();
            }
            InputEvent::PointerMotion { event } => {
                let screen = self.output_rect();
                let mut pos = self.pointer + event.delta();
                pos.x = pos.x.clamp(screen.x, screen.right() - 1.0);
                pos.y = pos.y.clamp(screen.y, screen.bottom() - 1.0);
                // A locked or confined pointer may not move; raw motion is reported either way.
                if let Some(pos) = self.constrained_motion(event.delta(), event.delta_unaccel(), event.time(), pos) {
                    self.on_motion(pos, event.time_msec());
                    self.activate_constraint_under();
                }
            }
            InputEvent::PointerButton { event } => {
                self.on_button(event.button_code(), event.state(), event.time_msec())
            }
            InputEvent::PointerAxis { event } => self.on_axis::<I>(&event),
            _ => {}
        }
    }

    fn on_key<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) {
        self.handle_key(event.key_code(), event.state(), Event::time_msec(&event));
    }

    /// A key, from a keyboard or from `tileception msg key` in tests.
    pub(crate) fn handle_key(&mut self, code: smithay::input::keyboard::Keycode, key_state: KeyState, time: u32) {
        let serial = SERIAL_COUNTER.next_serial();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let pressed = key_state == KeyState::Pressed;
        let keycode = code.raw();
        let commands = keyboard.input::<Vec<Command>, _>(self, code, key_state, serial, time, |state, mods, handle| {
            let sym = handle.modified_sym().raw();
            let raw_sym = handle.raw_syms().first().map(|s| s.raw()).unwrap_or(sym);
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
                    let media = state
                        .config
                        .lookup(held, raw_sym)
                        .or_else(|| state.config.lookup(held, sym))
                        .filter(|c| !c.is_empty() && c.iter().all(|c| matches!(c, Command::Media(_))));
                    if let Some(commands) = media {
                        return FilterResult::Intercept(commands.to_vec());
                    }
                }
                if matches!(state.lock, crate::lock::LockState::BuiltIn) {
                    if pressed {
                        state.builtin_lock_key(sym, mods.ctrl, mods.caps_lock);
                    }
                    return FilterResult::Intercept(Vec::new());
                }
                return FilterResult::Forward;
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
            let raw = handle.raw_syms().first().map(|s| s.raw()).unwrap_or(sym);
            if let Some(command) =
                builtin(held, sym, raw, state.world.in_overview() && !state.launcher.open && state.power_menu.is_none())
            {
                return FilterResult::Intercept(vec![command]);
            }
            if state.power_menu.is_some() {
                // The menu takes every key while it is open.
                return FilterResult::Intercept(vec![Command::PowerMenuKey(sym)]);
            }
            if state.launcher.open && state.launcher.home {
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
            if state.launcher.open {
                return FilterResult::Intercept(vec![Command::LauncherKey(sym, Mods::from_state(mods))]);
            }
            if state.doctor.open && state.doctor.focused {
                // Typing goes to the panel; Super shortcuts still work.
                let mut bound = held;
                if state.nested && bound.alt && !bound.logo {
                    bound.alt = false;
                    bound.logo = true;
                }
                let commands = state.config.lookup(bound, raw).or_else(|| state.config.lookup(bound, sym));
                if bound.logo {
                    return FilterResult::Intercept(commands.map(<[Command]>::to_vec).unwrap_or_default());
                }
                // Volume, brightness and media keys are not typing either.
                if let Some(media) =
                    commands.filter(|c| !c.is_empty() && c.iter().all(|c| matches!(c, Command::Media(_))))
                {
                    return FilterResult::Intercept(media.to_vec());
                }
                return FilterResult::Intercept(vec![Command::DoctorKey(sym, Mods::from_state(mods))]);
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
        let tapped = std::mem::take(&mut self.tap_fired);
        let commands = commands.unwrap_or_default();
        if !pressed {
            self.media_key_released(keycode);
        } else if !commands.is_empty() && commands.iter().all(|c| matches!(c, Command::Media(m) if m.repeats())) {
            self.start_media_repeat(keycode, commands.clone());
        } else {
            self.stop_media_repeat();
        }
        for command in commands.into_iter().chain(tapped) {
            if let Err(err) = self.execute(command) {
                tracing::warn!("{err}");
            }
        }
    }

    /// Whether the launcher gets the pointer here: all of it when opened on
    /// purpose; as the welcome screen only over itself and not under the
    /// notification panel.
    fn launcher_takes(&self, at: tileception_core::Point) -> bool {
        !self.launcher.home || (self.home_contains(at) && !self.notifications_contain(at))
    }

    /// Follows a modifier pressed on its own. Returns the modifier when it is
    /// let go without anything else happening in between.
    fn track_tap(&mut self, pressed: bool, sym: u32, held: Mods) -> Option<Mods> {
        let this = crate::config::modifier_of(sym);
        match (pressed, this) {
            // Held now means held including this key: only it may be down.
            (true, Some(m)) if held == m || held == Mods::default() => self.tap_armed = Some(m),
            (true, _) => self.tap_armed = None,
            (false, Some(m)) if self.tap_armed == Some(m) => {
                self.tap_armed = None;
                return Some(m);
            }
            (false, _) => {}
        }
        None
    }

    pub fn run(&mut self, action: Action) {
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
        if let (Some(id), false) = (self.fullscreen, keeps_fullscreen) {
            self.set_fullscreen(id, false);
        }
        tracing::debug!(?action, "action");
        match action {
            Action::Quit => self.loop_signal.stop(),
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
                let p = tileception_core::Point::new(self.pointer.x, self.pointer.y);
                self.world.step_in(Some(p), now);
            }
            Action::Cycle(forward) => self.world.cycle(forward, now),
            Action::ToggleSplit => self.world.toggle_split(now),
            Action::Resize(dw, dh) => {
                self.world.resize_focused(dw, dh, now);
            }
            Action::StepOut => self.world.step_out(now),
            Action::Neighbour(dir) => self.world.neighbour(dir, now),
            Action::Move(dir) => self.world.move_focused(dir, now),
            Action::Back => self.world.back(now),
            Action::Forward => self.world.forward(now),
            Action::Maximize => self.world.maximize(now),
            Action::ToggleDesktop => self.world.toggle_desktop(now),
            Action::ToggleFloat => {
                if let Some(f) = self.world.focus() {
                    self.world.toggle_float(f, now);
                }
            }
            Action::NextTab => self.world.cycle_tab(1),
            Action::PrevTab => self.world.cycle_tab(-1),
            Action::SaveView(n) => self.world.save_view(n),
            Action::RecallView(n) => {
                self.world.recall_view(n, now);
            }
            Action::Vt(n) => self.change_vt(n),
            Action::Lock => {
                self.lock_session(false);
            }
            Action::PowerMenu => self.shell_power_menu(),
            Action::Launcher => self.shell_launcher(),
            Action::Notifications => self.shell_notifications(),
            Action::DoNotDisturb => self.toggle_dnd(),
            Action::Group => {}
            Action::Minimize => {
                if let Some(id) = self.world.focus() {
                    self.minimize_window(id);
                }
            }
            Action::Restore(id) => {
                match id {
                    Some(id) => self.world.restore(id, now),
                    None => self.world.restore_last(now),
                };
            }
            Action::Fullscreen => {
                if let Some(id) = self.world.focus() {
                    let on = self.fullscreen != Some(id);
                    self.set_fullscreen(id, on);
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
        if matches!(self.lock, crate::lock::LockState::BuiltIn) {
            return;
        }
        let at = tileception_core::Point::new(pos.x, pos.y);
        if !self.is_locked() && self.launcher.open && self.launcher_takes(at) {
            self.launcher_motion(at);
            return;
        }
        if self.power_menu.is_some() {
            if let (Some(i), Some(menu)) = (self.power_action_at(pos.x, pos.y), self.power_menu.as_mut()) {
                menu.selected = i;
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
        if self.doctor_contains(at) {
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
            let moved = (pos.x - press.at.x).hypot(pos.y - press.at.y);
            if let (Some(window), true) = (press.window, moved > DRAG_THRESHOLD) {
                self.pending_press = None;
                self.start_move_drag(window, press.at);
            }
        }
        if let Some(mut drag) = self.resize_drag {
            let (dx, dy) = (pos.x - drag.last.x, pos.y - drag.last.y);
            self.world.resize_window(drag.window, drag.side_x, dx, drag.side_y, dy, now);
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
                if let Some(r) = self.world.float_rect(drag.window) {
                    let moved =
                        tileception_core::Rect::new(r.x + pos.x - drag.last.x, r.y + pos.y - drag.last.y, r.w, r.h);
                    self.world.set_float_rect(drag.window, moved);
                    self.apply_layout();
                }
                drag.last = pos;
            } else {
                let p = tileception_core::Point::new(pos.x, pos.y);
                drag.target = self.world.drop_target(p, drag.window, self.world.view(now));
            }
            self.move_drag = Some(drag);
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            pointer.frame(self);
            return;
        }
        let under = self.surface_under(pos);
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        pointer.motion(self, under, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
    }

    /// A client asked to be moved from its own title bar.
    pub fn begin_title_drag(&mut self, window: WindowId) {
        if self.move_drag.is_none() && !self.world.in_overview() {
            self.world.focus_window(window, self.now());
            self.apply_layout();
            let at = self.pointer;
            self.start_move_drag(window, at);
        }
    }

    /// Picks a window up so it follows the pointer as a picture.
    pub(crate) fn start_move_drag(&mut self, window: WindowId, at: Point<f64, Logical>) {
        let now = self.now();
        let screen = self.world.screen();
        let shown = self.drawn_windows(now).into_iter().find(|d| d.id == window).map(|d| d.content);
        let (grab, size) = match shown {
            Some(c) if c.w > 0.0 && c.h > 0.0 => {
                let grab = (((at.x - c.x) / c.w).clamp(0.0, 1.0), ((at.y - c.y) / c.h).clamp(0.0, 1.0));
                let k = (DRAG_PICTURE * screen.w / c.w).min(DRAG_PICTURE * screen.h / c.h).min(1.0);
                (grab, (c.w * k, c.h * k))
            }
            // A tab that is not on show: a picture of a typical size.
            _ => ((0.5, 0.1), (DRAG_PICTURE * screen.w, DRAG_PICTURE * screen.h)),
        };
        let free = self.world.tree().is_floating(window) && !self.world.in_overview();
        self.move_drag = Some(MoveDrag { window, grab, size, target: None, free, last: at });
    }

    pub(crate) fn on_button(&mut self, button: u32, state: ButtonState, time: u32) {
        self.tap_armed = None;
        // A held slider lets go wherever the button comes up.
        if state == ButtonState::Released && button == BTN_LEFT && self.controls_release() {
            return;
        }
        if matches!(self.lock, crate::lock::LockState::BuiltIn) {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if self.is_locked() {
            pointer.button(self, &ButtonEvent { button, state, serial, time });
            pointer.frame(self);
            return;
        }
        let at = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        if self.launcher.open && state == ButtonState::Released && button == BTN_LEFT {
            self.launcher_release();
        }
        if self.launcher.open && self.launcher_takes(at) {
            if state == ButtonState::Pressed && button == BTN_LEFT {
                self.launcher_click(at);
            }
            return;
        }
        if self.power_menu.is_some() {
            // A click picks a button; a click anywhere else closes the menu.
            if state == ButtonState::Pressed {
                match self.power_action_at(self.pointer.x, self.pointer.y) {
                    Some(i) => self.run_power_action(crate::power::PowerAction::ALL[i]),
                    None => self.close_power_menu(),
                }
            }
            return;
        }
        let now = self.now();
        let p = tileception_core::Point::new(self.pointer.x, self.pointer.y);
        if state == ButtonState::Released && button == BTN_LEFT && self.controls_release() {
            return;
        }
        if state == ButtonState::Pressed && button == BTN_LEFT && self.doctor_press(p) {
            return;
        }
        if self.doctor_contains(p) {
            return;
        }
        if self.notifications_contain(p) {
            if state == ButtonState::Pressed && button == BTN_LEFT {
                self.notification_click(p);
            }
            return;
        }
        if self.move_drag.is_none() && self.strip_button(button, state == ButtonState::Pressed) {
            return;
        }

        if state == ButtonState::Released {
            if let Some(anchor) = self.selection.anchor.take() {
                if (p.x - anchor.x).hypot(p.y - anchor.y) < DRAG_THRESHOLD {
                    // A click, not a box: keep the selection as it was before the press,
                    // toggling the window under a Super+Shift click.
                    self.selection.windows = std::mem::take(&mut self.selection.additive);
                    let mods = self.seat.get_keyboard().expect("seat has a keyboard").modifier_state();
                    if mods.shift && mods.logo {
                        let at = self.tab_at(anchor, now).or_else(|| {
                            self.drawn_windows(now).into_iter().find(|d| d.content.contains(anchor)).map(|d| d.id)
                        });
                        if let Some(window) = at {
                            self.toggle_selection(window);
                        }
                        return;
                    }
                }
                if self.world.in_overview() && (p.x - anchor.x).hypot(p.y - anchor.y) < DRAG_THRESHOLD {
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
                    let at = tileception_core::Point::new(press.at.x, press.at.y);
                    self.world.click_in_overview(at, now);
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
            if let Some(d) = self.drawn_windows(now).into_iter().find(|d| d.content.contains(p)) {
                self.world.focus_window(d.id, now);
                self.apply_layout();
                let c = d.content.center();
                let side_x = if p.x < c.x { Side::Left } else { Side::Right };
                let side_y = if p.y < c.y { Side::Top } else { Side::Bottom };
                self.resize_drag = Some(ResizeDrag { window: d.id, side_x, side_y, last: self.pointer });
                return;
            }
        }
        if state == ButtonState::Pressed && button == BTN_LEFT && !pointer.is_grabbed() && !over_layer {
            let drawn = self.drawn_windows(now);
            let under = drawn.iter().find(|d| d.content.contains(p)).copied();
            let mods = self.seat.get_keyboard().expect("seat has a keyboard").modifier_state();
            let tab = self.tab_at(p, now);
            let overview = self.world.in_overview();
            // Super+Shift: a click toggles the window, a drag selects with a box.
            // In Overview a drag from empty space also selects with a box.
            if (mods.shift && mods.logo) || (overview && under.is_none() && tab.is_none()) {
                self.selection.anchor = Some(p);
                self.selection.additive = if mods.shift { self.selection.windows.clone() } else { Default::default() };
                return;
            }
            // Shift+click selects tabs and previews; on a live window it belongs
            // to the program, for example to extend a text selection.
            if mods.shift
                && (tab.is_some() || under.is_some_and(|d| !d.live))
                && let Some(window) = tab.or(under.map(|d| d.id))
            {
                self.toggle_selection(window);
                return;
            }
            if !mods.shift {
                self.selection.windows.clear();
            }
            if overview {
                // Dragging a window in Overview moves it; a click zooms in.
                self.pending_press = Some(PendingPress { at: self.pointer, window: under.map(|d| d.id) });
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
            if let Some(tab) = self.tab_at(p, now) {
                self.world.focus_window(tab, now);
                self.apply_layout();
                self.pending_press = Some(PendingPress { at: self.pointer, window: Some(tab) });
                return;
            }
            let Some(d) = under else {
                pointer.button(self, &ButtonEvent { button, state, serial, time });
                pointer.frame(self);
                return;
            };
            if self.world.focus() != Some(d.id) {
                self.world.focus_window(d.id, now);
                self.apply_layout();
            }
            if drag_held {
                self.start_move_drag(d.id, self.pointer);
                return;
            }
            if !d.live {
                // A click on a preview only focuses it.
                return;
            }
        }
        pointer.button(self, &ButtonEvent { button, state, serial, time });
        pointer.frame(self);
    }

    /// The window whose tab is under a point.
    fn tab_at(&self, p: tileception_core::Point, now: f64) -> Option<WindowId> {
        let strip = self.world.tab_strips(self.world.view(now)).into_iter().find(|s| s.rect.contains(p))?;
        let i = ((p.x - strip.rect.x) / strip.rect.w * strip.views.len() as f64) as usize;
        strip.views.get(i.min(strip.views.len() - 1)).copied()
    }

    fn on_axis<I: InputBackend>(&mut self, event: &I::PointerAxisEvent) {
        if matches!(self.lock, crate::lock::LockState::BuiltIn) {
            return;
        }
        if self.launcher.open
            && !self.is_locked()
            && self.launcher_takes(tileception_core::Point::new(self.pointer.x, self.pointer.y))
        {
            if let Some(value) = event.amount(Axis::Vertical).or_else(|| event.amount_v120(Axis::Vertical))
                && value != 0.0
            {
                self.launcher_step(if value > 0.0 { 1 } else { -1 });
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
            let p = tileception_core::Point::new(self.pointer.x, self.pointer.y);
            let discrete = event.amount_v120(Axis::Vertical).is_some() && event.source() == AxisSource::Wheel;
            let amount = event.amount(Axis::Vertical).or_else(|| event.amount_v120(Axis::Vertical)).unwrap_or(0.0);
            if amount != 0.0 && self.controls_scroll(p, amount, discrete) {
                return;
            }
            if amount != 0.0 {
                self.notification_scroll(if amount > 0.0 { 1 } else { -1 });
            }
            return;
        }
        self.tap_armed = None;
        let source = event.source();
        if !self.is_locked() && self.wheel_binding::<I>(event) {
            return;
        }
        let amount = |axis| event.amount(axis).unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
        let horizontal = amount(Axis::Horizontal);
        let vertical = amount(Axis::Vertical);
        let mut frame = AxisFrame::new(event.time_msec()).source(source);
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
            if let Some(d) = event.amount_v120(Axis::Horizontal) {
                frame = frame.v120(Axis::Horizontal, d as i32);
            }
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
            if let Some(d) = event.amount_v120(Axis::Vertical) {
                frame = frame.v120(Axis::Vertical, d as i32);
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
    fn wheel_binding<I: InputBackend>(&mut self, event: &I::PointerAxisEvent) -> bool {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let mut held = Mods::from_state(&keyboard.modifier_state());
        if held == Mods::default() {
            return false;
        }
        if self.nested && held.alt && !held.logo {
            held.alt = false;
            held.logo = true;
        }
        let bound = |k| self.config.lookup(held, k).is_some();
        if ![WHEEL_UP, WHEEL_DOWN, WHEEL_LEFT, WHEEL_RIGHT].into_iter().any(bound) {
            return false;
        }
        // One notch of a wheel is 120; smooth scrolling adds up to a step.
        let notch = |axis| {
            event.amount_v120(axis).map(|v| v / 120.0).unwrap_or_else(|| event.amount(axis).unwrap_or(0.0) / 50.0)
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
                if let Some((s, loc)) = hit {
                    return Some((s, loc.to_f64()));
                }
            }
        }
        let p = tileception_core::Point::new(pos.x, pos.y);
        let now = self.now();
        let drawn = self.drawn_windows(now);
        // Menus may reach outside their window's tile.
        for d in drawn.iter().filter(|d| d.live) {
            let Some(window) = self.windows.get(&d.id) else { continue };
            let origin: Point<f64, Logical> = (d.origin.x.round(), d.origin.y.round()).into();
            if let Some((s, loc)) = window.surface_under(pos - origin, smithay::desktop::WindowSurfaceType::POPUP) {
                return Some((s, loc.to_f64() + origin));
            }
        }
        let d = drawn.iter().find(|d| d.content.contains(p))?;
        if !d.live {
            return None;
        }
        let window = self.windows.get(&d.id)?;
        let origin: Point<f64, Logical> = (d.origin.x.round(), d.origin.y.round()).into();
        window
            .surface_under(pos - origin, smithay::desktop::WindowSurfaceType::ALL)
            .map(|(s, loc)| (s, loc.to_f64() + origin))
    }

    /// The split boundary under a point, if any, for dragging gaps.
    fn gap_at(&self, pos: Point<f64, Logical>) -> Option<GapDrag> {
        let view = self.world.target_view();
        let tree = self.world.tree();
        let reach = self.world.settings.gap / 2.0 + 4.0;
        for split in tree.subtree(tree.root()) {
            let Node::Split { axis, children, .. } = tree.node(split) else { continue };
            for i in 0..children.len().saturating_sub(1) {
                let (Some(a), Some(_)) =
                    (self.world.node_rect(children[i], view), self.world.node_rect(children[i + 1], view))
                else {
                    continue;
                };
                let hit = match axis {
                    LayoutAxis::Horizontal => (pos.x - a.right()).abs() <= reach && pos.y >= a.y && pos.y < a.bottom(),
                    LayoutAxis::Vertical => (pos.y - a.bottom()).abs() <= reach && pos.x >= a.x && pos.x < a.right(),
                };
                if hit {
                    return Some(GapDrag { split, index: i, last: pos, horizontal: *axis == LayoutAxis::Horizontal });
                }
            }
        }
        None
    }
}

/// Keys that work whatever the config says.
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
