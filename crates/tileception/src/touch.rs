//! The touchscreen. A touch that starts on a program's window goes to that
//! program as touch (wl_touch), with every finger that joins it; one that
//! starts on Tileception's own interface (launcher, bar, panels, Overview,
//! gaps and tabs) acts as the left mouse button there.

use std::collections::HashSet;

use smithay::{
    backend::input::{ButtonState, TouchSlot},
    input::touch::{DownEvent, MotionEvent, UpEvent},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{Tileception, input::BTN_LEFT};

/// What the fingers on the screen are doing.
#[derive(Debug, Default)]
pub struct Touch {
    mode: Option<Mode>,
    down: HashSet<TouchSlot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Passed on to programs.
    Client,
    /// This finger is the mouse; others are ignored until all are lifted.
    Pointer(TouchSlot),
}

impl Tileception {
    /// Where a touch starting here goes: a program's surface, or None when
    /// Tileception's own interface takes it.
    fn touch_target(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        if matches!(self.lock, crate::lock::LockState::BuiltIn) {
            return None;
        }
        if self.is_locked() {
            return self.lock_surface_under(pos);
        }
        let at = tileception_core::Point::new(pos.x, pos.y);
        let now = self.now();
        let ours = (self.launcher.open && self.launcher_takes(at))
            || self.power_menu.is_some()
            || self.doctor_contains(at)
            || self.notifications_contain(at)
            || self.bar_takes(at)
            || self.strip_takes(at)
            || self.world.in_overview()
            || self.selection.anchor.is_some()
            || self.move_drag.is_some()
            || self.gap_drag.is_some()
            || self.resize_drag.is_some()
            || self.gap_at(pos).is_some()
            || self.tab_at(at, now).is_some();
        if ours { None } else { self.surface_under(pos) }
    }

    pub(crate) fn touch_down(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, time: u32) {
        self.tap_armed = None;
        self.touch.down.insert(slot);
        let mode = match self.touch.mode {
            Some(mode) => mode,
            None => {
                let mode = if self.touch_target(pos).is_some() { Mode::Client } else { Mode::Pointer(slot) };
                self.touch.mode = Some(mode);
                if mode == Mode::Client {
                    self.touch_focus(pos);
                }
                mode
            }
        };
        match mode {
            Mode::Client => {
                let focus = self.touch_target(pos);
                let touch = self.seat.get_touch().expect("seat has touch");
                touch.down(self, focus, &DownEvent { slot, location: pos, serial: SERIAL_COUNTER.next_serial(), time });
            }
            Mode::Pointer(first) if first == slot => {
                self.on_motion(pos, time);
                self.on_button(BTN_LEFT, ButtonState::Pressed, time);
            }
            Mode::Pointer(_) => {}
        }
    }

    /// Focuses the window a touch lands on, as a click would.
    fn touch_focus(&mut self, pos: Point<f64, Logical>) {
        if self.is_locked() {
            return;
        }
        let now = self.now();
        let p = tileception_core::Point::new(pos.x, pos.y);
        self.selection.windows.clear();
        if self.world.is_animating(now) {
            self.world.finish_animation();
        }
        let under = self.drawn_windows(now).into_iter().find(|d| d.content.contains(p)).map(|d| d.id);
        if let Some(id) = under.filter(|id| self.world.focus() != Some(*id)) {
            self.world.focus_window(id, now);
            self.apply_layout();
        }
    }

    pub(crate) fn touch_motion(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, time: u32) {
        match self.touch.mode {
            Some(Mode::Client) => {
                let touch = self.seat.get_touch().expect("seat has touch");
                // The grab keeps each finger on the surface it went down on.
                touch.motion(self, None, &MotionEvent { slot, location: pos, time });
            }
            Some(Mode::Pointer(first)) if first == slot => self.on_motion(pos, time),
            _ => {}
        }
    }

    pub(crate) fn touch_up(&mut self, slot: TouchSlot, time: u32) {
        self.touch.down.remove(&slot);
        match self.touch.mode {
            Some(Mode::Client) => {
                let touch = self.seat.get_touch().expect("seat has touch");
                touch.up(self, &UpEvent { slot, serial: SERIAL_COUNTER.next_serial(), time });
            }
            Some(Mode::Pointer(first)) if first == slot => self.on_button(BTN_LEFT, ButtonState::Released, time),
            _ => {}
        }
        if self.touch.down.is_empty() {
            self.touch.mode = None;
        }
    }

    pub(crate) fn touch_frame(&mut self) {
        if self.touch.mode == Some(Mode::Client) {
            let touch = self.seat.get_touch().expect("seat has touch");
            touch.frame(self);
        }
    }

    pub(crate) fn touch_cancel(&mut self, time: u32) {
        match self.touch.mode.take() {
            Some(Mode::Client) => {
                let touch = self.seat.get_touch().expect("seat has touch");
                touch.cancel(self);
            }
            Some(Mode::Pointer(_)) => self.on_button(BTN_LEFT, ButtonState::Released, time),
            None => {}
        }
        self.touch.down.clear();
    }

    /// One finger down at `from`, moved to `to` in steps, then lifted: a tap
    /// when they are the same. For scripted tests.
    pub(crate) fn inject_touch(&mut self, from: (f64, f64), to: (f64, f64), time: u32) {
        const STEPS: u32 = 8;
        let slot = TouchSlot::from(Some(0));
        let screen = self.output_rect();
        let clamp = |(x, y): (f64, f64)| -> Point<f64, Logical> {
            (x.clamp(screen.x, screen.right() - 1.0), y.clamp(screen.y, screen.bottom() - 1.0)).into()
        };
        self.touch_down(slot, clamp(from), time);
        self.touch_frame();
        if from != to {
            for i in 1..=STEPS {
                let t = f64::from(i) / f64::from(STEPS);
                let at = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
                self.touch_motion(slot, clamp(at), time + i);
                self.touch_frame();
            }
        }
        self.touch_up(slot, time + STEPS + 1);
        self.touch_frame();
    }
}
