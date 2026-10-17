//! The touchscreen. A touch that starts on a program's window goes to that
//! program as touch (wl_touch), with every finger that joins it; one that
//! starts on Tileception's own interface (launcher, bar, panels, Overview,
//! gaps and tabs) acts as the left mouse button there.
//!
//! Tileception keeps a few gestures for itself: a swipe in from a screen edge
//! (`TouchEdgeTop` and the others in `[binds]`), three or more fingers that
//! land together (`TouchSwipe3Left`, `TouchPinch3In`, taken back from the
//! program they started on), and in Overview a two-finger pinch, which zooms
//! into the tile under it or leaves Overview.

use std::collections::HashMap;

use smithay::{
    backend::input::{ButtonState, TouchSlot},
    input::{
        pointer::ButtonEvent,
        touch::{DownEvent, MotionEvent, UpEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    Tileception,
    gestures::{Edge, GestureKey, Motion},
    input::BTN_LEFT,
};

/// How close to a screen edge, in points, a touch must start to be an edge swipe.
const EDGE: f64 = 12.0;
/// How far a finger travels before an edge swipe or a swipe counts.
const EDGE_TRAVEL: f64 = 48.0;
const SWIPE_TRAVEL: f64 = 80.0;
/// How much fingers spread or close before a pinch counts.
const PINCH_IN: f64 = 0.7;
const PINCH_OUT: f64 = 1.4;
/// Fingers landing within this many milliseconds of the first form one gesture.
const TOGETHER_MS: u32 = 400;

/// What the fingers on the screen are doing.
#[derive(Debug, Default)]
pub struct Touch {
    mode: Option<Mode>,
    fingers: HashMap<TouchSlot, Finger>,
    /// When the first finger of this touch went down.
    first_down: u32,
}

#[derive(Debug, Clone, Copy)]
struct Finger {
    start: Point<f64, Logical>,
    now: Point<f64, Logical>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Passed on to programs.
    Client,
    /// This finger is the mouse; others are ignored until all are lifted.
    Pointer(TouchSlot),
    /// One finger that started on a screen edge, not yet sure to be a swipe.
    Edge(TouchSlot, Edge),
    /// Several fingers that Tileception took for a gesture.
    Gesture { count: usize, centre: Point<f64, Logical>, spread: f64 },
    /// A gesture ran; ignore the fingers until all are lifted.
    Done,
}

/// The middle of the fingers and their mean distance from it.
fn shape<'a>(points: impl Iterator<Item = &'a Point<f64, Logical>> + Clone) -> (Point<f64, Logical>, f64) {
    let n = points.clone().count().max(1) as f64;
    let sum = points.clone().fold(Point::<f64, Logical>::default(), |acc, p| acc + *p);
    let centre: Point<f64, Logical> = (sum.x / n, sum.y / n).into();
    let spread = points.map(|p| (p.x - centre.x).hypot(p.y - centre.y)).sum::<f64>() / n;
    (centre, spread)
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

    /// The screen edge a touch starts on, if it may be an edge swipe.
    fn touch_edge(&self, pos: Point<f64, Logical>) -> Option<Edge> {
        if self.is_locked() || self.power_menu.is_some() {
            return None;
        }
        let screen = self.output_rect();
        [
            (pos.y - screen.y, Edge::Top),
            (screen.bottom() - pos.y, Edge::Bottom),
            (pos.x - screen.x, Edge::Left),
            (screen.right() - pos.x, Edge::Right),
        ]
        .into_iter()
        .filter(|(d, _)| *d < EDGE)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, edge)| edge)
    }

    pub(crate) fn touch_down(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, time: u32) {
        self.tap_armed = None;
        self.touch.fingers.insert(slot, Finger { start: pos, now: pos });
        let Some(mode) = self.touch.mode else {
            self.touch.first_down = time;
            match self.touch_edge(pos) {
                Some(edge) => self.touch.mode = Some(Mode::Edge(slot, edge)),
                None => self.touch_start(slot, pos, time),
            }
            return;
        };
        let together = time.wrapping_sub(self.touch.first_down) <= TOGETHER_MS;
        let count = self.touch.fingers.len();
        match mode {
            Mode::Edge(first, _) => {
                // A second finger: not an edge swipe after all.
                self.touch_unedge(first, time);
                self.touch_down_more(slot, pos, time);
            }
            Mode::Client if together && count >= 3 && !self.is_locked() => {
                let touch = self.seat.get_touch().expect("seat has touch");
                touch.cancel(self);
                self.touch_take(count);
            }
            Mode::Pointer(_) if together && (count >= 3 || self.world.in_overview()) && !self.is_locked() => {
                self.cancel_touch_press(time);
                self.touch_take(count);
            }
            Mode::Gesture { .. } => self.touch_take(count),
            _ => self.touch_down_more(slot, pos, time),
        }
    }

    /// The first finger, as a program's touch or as the mouse.
    fn touch_start(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, time: u32) {
        if self.touch_target(pos).is_some() {
            self.touch.mode = Some(Mode::Client);
            self.touch_focus(pos);
            self.touch_down_more(slot, pos, time);
        } else {
            self.touch.mode = Some(Mode::Pointer(slot));
            self.on_motion(pos, time);
            self.on_button(BTN_LEFT, ButtonState::Pressed, time);
        }
    }

    /// Another finger joining a touch that is not a gesture.
    fn touch_down_more(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, time: u32) {
        if self.touch.mode == Some(Mode::Client) {
            let focus = self.touch_target(pos);
            let touch = self.seat.get_touch().expect("seat has touch");
            touch.down(self, focus, &DownEvent { slot, location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        }
    }

    /// An edge touch that turned out not to be a swipe: starts it where it
    /// went down and catches up with where it is now.
    fn touch_unedge(&mut self, slot: TouchSlot, time: u32) {
        let Some(finger) = self.touch.fingers.get(&slot).copied() else { return };
        self.touch_start(slot, finger.start, time);
        if finger.now != finger.start {
            self.touch_motion_to(slot, finger.now, time);
        }
    }

    /// Takes the fingers on the screen for a gesture, from where they are.
    fn touch_take(&mut self, count: usize) {
        let (centre, spread) = shape(self.touch.fingers.values().map(|f| &f.now));
        self.touch.mode = Some(Mode::Gesture { count, centre, spread });
    }

    /// Undoes what a pressed mouse button started, without the click.
    fn cancel_touch_press(&mut self, time: u32) {
        self.pending_press = None;
        if self.selection.anchor.take().is_some() {
            self.selection.windows = std::mem::take(&mut self.selection.additive);
        }
        self.gap_drag = None;
        self.resize_drag = None;
        self.move_drag = None;
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if pointer.is_grabbed() {
            let event = ButtonEvent {
                button: BTN_LEFT,
                state: ButtonState::Released,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            };
            pointer.button(self, &event);
            pointer.frame(self);
        }
        self.apply_layout();
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
        let Some(finger) = self.touch.fingers.get_mut(&slot) else { return };
        finger.now = pos;
        let finger = *finger;
        match self.touch.mode {
            Some(Mode::Edge(first, edge)) if first == slot => {
                let d = finger.now - finger.start;
                let (inward, along) = match edge {
                    Edge::Top => (d.y, d.x),
                    Edge::Bottom => (-d.y, d.x),
                    Edge::Left => (d.x, d.y),
                    Edge::Right => (-d.x, d.y),
                };
                if inward >= EDGE_TRAVEL && inward > along.abs() {
                    self.touch.mode = Some(Mode::Done);
                    let mods = self.gesture_mods();
                    self.run_gesture_key(GestureKey::Edge(edge), mods);
                } else if along.abs() >= EDGE_TRAVEL || inward < -EDGE {
                    self.touch_unedge(slot, time);
                }
            }
            Some(Mode::Gesture { count, centre, spread }) => {
                let (now, spread_now) = shape(self.touch.fingers.values().map(|f| &f.now));
                let ratio = if spread > 1.0 { spread_now / spread } else { 1.0 };
                let travel = now - centre;
                let motion = if ratio <= PINCH_IN {
                    Some(Motion::In)
                } else if ratio >= PINCH_OUT {
                    Some(Motion::Out)
                } else if travel.x.hypot(travel.y) >= SWIPE_TRAVEL {
                    Some(match (travel.x.abs() > travel.y.abs(), travel.x > 0.0, travel.y > 0.0) {
                        (true, true, _) => Motion::Right,
                        (true, false, _) => Motion::Left,
                        (false, _, true) => Motion::Down,
                        (false, _, false) => Motion::Up,
                    })
                } else {
                    None
                };
                if let Some(motion) = motion {
                    self.touch.mode = Some(Mode::Done);
                    self.run_touch_gesture(count, motion, now);
                }
            }
            _ => self.touch_motion_to(slot, pos, time),
        }
    }

    fn touch_motion_to(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, time: u32) {
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

    /// A gesture of several fingers, around `centre`.
    fn run_touch_gesture(&mut self, count: usize, motion: Motion, centre: Point<f64, Logical>) {
        // Commands such as `step-in pointer` aim where the fingers are.
        self.pointer = centre;
        if self.world.in_overview() && count == 2 {
            let now = self.now();
            match motion {
                Motion::Out => self.world.click_in_overview(tileception_core::Point::new(centre.x, centre.y), now),
                Motion::In => self.world.exit_overview(now),
                _ => return,
            }
            self.apply_layout();
            return;
        }
        let mods = self.gesture_mods();
        self.run_gesture_key(GestureKey::Touch(count.min(5) as u32, motion), mods);
    }

    pub(crate) fn touch_up(&mut self, slot: TouchSlot, time: u32) {
        match self.touch.mode {
            Some(Mode::Edge(first, _)) if first == slot => {
                // A tap on the edge, such as on the bar.
                self.touch_unedge(slot, time);
                self.touch_frame();
                self.touch_lift(slot, time);
            }
            // A finger lifted before the gesture counted: the rest would
            // move its middle, so it ends here.
            Some(Mode::Gesture { .. }) => self.touch.mode = Some(Mode::Done),
            _ => self.touch_lift(slot, time),
        }
        self.touch.fingers.remove(&slot);
        if self.touch.fingers.is_empty() {
            self.touch.mode = None;
        }
    }

    fn touch_lift(&mut self, slot: TouchSlot, time: u32) {
        match self.touch.mode {
            Some(Mode::Client) => {
                let touch = self.seat.get_touch().expect("seat has touch");
                touch.up(self, &UpEvent { slot, serial: SERIAL_COUNTER.next_serial(), time });
            }
            Some(Mode::Pointer(first)) if first == slot => self.on_button(BTN_LEFT, ButtonState::Released, time),
            _ => {}
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
            Some(Mode::Pointer(_)) => self.cancel_touch_press(time),
            _ => {}
        }
        self.touch.fingers.clear();
    }

    /// Fingers that go down at `from`, move in steps to `to` and lift, all in
    /// one frame per step. For scripted tests.
    fn inject_fingers(&mut self, from: &[(f64, f64)], to: &[(f64, f64)], time: u32) {
        const STEPS: u32 = 8;
        let screen = self.output_rect();
        let clamp = |(x, y): (f64, f64)| -> Point<f64, Logical> {
            (x.clamp(screen.x, screen.right() - 1.0), y.clamp(screen.y, screen.bottom() - 1.0)).into()
        };
        let slot = |i: usize| TouchSlot::from(Some(i as u32));
        for (i, p) in from.iter().enumerate() {
            self.touch_down(slot(i), clamp(*p), time + i as u32);
        }
        self.touch_frame();
        if from != to {
            for step in 1..=STEPS {
                let t = f64::from(step) / f64::from(STEPS);
                for (i, (a, b)) in from.iter().zip(to).enumerate() {
                    let at = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                    self.touch_motion(slot(i), clamp(at), time + 10 + step);
                }
                self.touch_frame();
            }
        }
        for i in 0..from.len() {
            self.touch_up(slot(i), time + 20 + STEPS);
        }
        self.touch_frame();
    }

    /// One finger: a tap, or a drag to a second point.
    pub(crate) fn inject_touch(&mut self, from: (f64, f64), to: (f64, f64), time: u32) {
        self.inject_fingers(&[from], &[to], time);
    }

    /// A touchscreen gesture around the pointer, or in from an edge.
    pub(crate) fn inject_touch_gesture(&mut self, gesture: GestureKey, time: u32) {
        let screen = self.output_rect();
        let c = (self.pointer.x, self.pointer.y);
        match gesture {
            GestureKey::Edge(edge) => {
                let (from, to) = match edge {
                    Edge::Top => ((c.0, screen.y + 2.0), (c.0, screen.y + 150.0)),
                    Edge::Bottom => ((c.0, screen.bottom() - 2.0), (c.0, screen.bottom() - 150.0)),
                    Edge::Left => ((screen.x + 2.0, c.1), (screen.x + 150.0, c.1)),
                    Edge::Right => ((screen.right() - 2.0, c.1), (screen.right() - 150.0, c.1)),
                };
                self.inject_fingers(&[from], &[to], time);
            }
            GestureKey::Touch(count, motion) | GestureKey::Pad(count, motion) => {
                let around = |r: f64, (dx, dy): (f64, f64)| -> Vec<(f64, f64)> {
                    (0..count)
                        .map(|i| {
                            let a = std::f64::consts::TAU * f64::from(i) / f64::from(count);
                            (c.0 + dx + r * a.cos(), c.1 + dy + r * a.sin())
                        })
                        .collect()
                };
                let (from, to) = match motion {
                    Motion::In => (around(150.0, (0.0, 0.0)), around(60.0, (0.0, 0.0))),
                    Motion::Out => (around(60.0, (0.0, 0.0)), around(150.0, (0.0, 0.0))),
                    Motion::Left => (around(60.0, (0.0, 0.0)), around(60.0, (-160.0, 0.0))),
                    Motion::Right => (around(60.0, (0.0, 0.0)), around(60.0, (160.0, 0.0))),
                    Motion::Up => (around(60.0, (0.0, 0.0)), around(60.0, (0.0, -160.0))),
                    Motion::Down => (around(60.0, (0.0, 0.0)), around(60.0, (0.0, 160.0))),
                };
                self.inject_fingers(&from, &to, time);
            }
        }
    }
}
