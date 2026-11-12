//! Touchpad gestures and settings. Swipes and pinches can be bound in
//! `[binds]` (`Swipe3Left`, `Super+Pinch2In`); the ones that are not bound go
//! to the program under the pointer through pointer-gestures. `[touchpad]`
//! sets libinput's options: tap to click, natural scrolling and the rest.
//!
//! With `follow_fingers`, a swipe bound to moving around moves the view with
//! the fingers, and one bound to `resize` drags the divider with them.

use serde::{Deserialize, Serialize};
use smithay::{
    delegate_pointer_gestures,
    input::pointer::{
        GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
        GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
    },
    reexports::input::{ClickMethod, Device},
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{Tileception, commands::Command, config::Mods};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TouchpadConfig {
    /// Tap to click: one finger left, two right, three middle.
    pub tap: bool,
    pub natural_scroll: bool,
    pub disable_while_typing: bool,
    /// Pointer speed, -1 to 1.
    pub speed: f64,
    /// "areas" or "clickfinger"; unset keeps the touchpad's own default.
    pub click_method: Option<String>,
    /// Pixels a swipe travels before its binding runs.
    pub swipe_distance: f64,
    /// Swipes and pinches bound to navigation move the view with the
    /// fingers, and letting go early goes back. Swipes bound to `resize`
    /// drag the divider with the fingers.
    pub follow_fingers: bool,
}

impl Default for TouchpadConfig {
    fn default() -> Self {
        TouchpadConfig {
            tap: true,
            natural_scroll: false,
            disable_while_typing: true,
            speed: 0.0,
            click_method: None,
            swipe_distance: 60.0,
            follow_fingers: true,
        }
    }
}

impl TouchpadConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(-1.0..=1.0).contains(&self.speed) {
            return Err(format!("[touchpad] speed {} is not between -1 and 1", self.speed));
        }
        if self.swipe_distance <= 0.0 {
            return Err("[touchpad] swipe_distance must be more than 0".into());
        }
        self.click_method().map(|_| ())
    }

    fn click_method(&self) -> Result<Option<ClickMethod>, String> {
        match self.click_method.as_deref() {
            None => Ok(None),
            Some("areas") => Ok(Some(ClickMethod::ButtonAreas)),
            Some("clickfinger") => Ok(Some(ClickMethod::Clickfinger)),
            Some(other) => Err(format!("[touchpad] click_method {other:?} is not \"areas\" or \"clickfinger\"")),
        }
    }

    fn apply(&self, device: &mut Device) {
        let _ = device.config_tap_set_enabled(self.tap);
        let _ = device.config_scroll_set_natural_scroll_enabled(self.natural_scroll);
        let _ = device.config_dwt_set_enabled(self.disable_while_typing);
        let _ = device.config_accel_set_speed(self.speed);
        let method = self.click_method().ok().flatten().or_else(|| device.config_click_default_method());
        if let Some(method) = method.filter(|method| device.config_click_methods().contains(method)) {
            let _ = device.config_click_set_method(method);
        }
    }
}

/// Which way the fingers went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// Fingers pinched together.
    In,
    /// Fingers spread apart.
    Out,
}

impl Motion {
    pub(crate) fn is_pinch(self) -> bool {
        matches!(self, Motion::In | Motion::Out)
    }
}

/// The screen edge a touch swipe comes in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// A gesture as `[binds]` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureKey {
    /// On the touchpad: `Swipe3Left`, `Pinch2In`.
    Pad(u32, Motion),
    /// Three or more fingers on the touchscreen: `TouchSwipe3Left`.
    Touch(u32, Motion),
    /// One finger in from a screen edge: `TouchEdgeTop`.
    Edge(Edge),
}

/// Pseudo keysyms for gestures, outside the range xkb uses: one per finger
/// count and motion, touchpad and touchscreen apart, and one per edge.
const GESTURE_KEYS: u32 = 0x7f00_0100;
const TOUCH_KEYS: u32 = GESTURE_KEYS + 64;
const EDGE_KEYS: u32 = GESTURE_KEYS + 128;
const MOTIONS: [Motion; 6] = [Motion::Left, Motion::Right, Motion::Up, Motion::Down, Motion::In, Motion::Out];
const EDGES: [Edge; 4] = [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right];

pub fn key(fingers: u32, motion: Motion) -> u32 {
    GestureKey::Pad(fingers, motion).key()
}

impl GestureKey {
    pub fn key(self) -> u32 {
        let motion_index = |motion| MOTIONS.iter().position(|candidate| *candidate == motion).unwrap_or(0) as u32;
        match self {
            GestureKey::Pad(fingers, motion) => GESTURE_KEYS + fingers * 8 + motion_index(motion),
            GestureKey::Touch(fingers, motion) => TOUCH_KEYS + fingers * 8 + motion_index(motion),
            GestureKey::Edge(edge) => {
                EDGE_KEYS + EDGES.iter().position(|candidate| *candidate == edge).unwrap_or(0) as u32
            }
        }
    }
}

/// The gesture a pseudo keysym stands for.
#[cfg(test)]
fn decode(key: u32) -> Option<GestureKey> {
    if let Some(offset) = key.checked_sub(EDGE_KEYS) {
        return EDGES.get(offset as usize).map(|edge| GestureKey::Edge(*edge));
    }
    let (touch, offset) = match key.checked_sub(TOUCH_KEYS) {
        Some(offset) => (true, offset),
        None => (false, key.checked_sub(GESTURE_KEYS)?),
    };
    let motion = *MOTIONS.get((offset % 8) as usize)?;
    let fingers = offset / 8;
    (2..=5).contains(&fingers).then_some(if touch {
        GestureKey::Touch(fingers, motion)
    } else {
        GestureKey::Pad(fingers, motion)
    })
}

/// Parses `Swipe3Left`, `Pinch2In`, `TouchSwipe3Up` or `TouchEdgeTop`. None
/// when the name is not a gesture.
pub fn parse_key(name: &str) -> Option<Result<u32, String>> {
    parse_gesture(name, false).map(|gesture| gesture.map(GestureKey::key))
}

/// As `parse_key`, also allowing two-finger touch pinches, which Tileception
/// only takes in Overview: for scripted tests.
pub fn parse_gesture(name: &str, test: bool) -> Option<Result<GestureKey, String>> {
    let lower = name.to_ascii_lowercase();
    let (touch, lower) = match lower.strip_prefix("touch") {
        Some(rest) => (true, rest.to_string()),
        None => (false, lower),
    };
    if touch && let Some(edge) = lower.strip_prefix("edge") {
        return Some(match edge {
            "top" => Ok(GestureKey::Edge(Edge::Top)),
            "bottom" => Ok(GestureKey::Edge(Edge::Bottom)),
            "left" => Ok(GestureKey::Edge(Edge::Left)),
            "right" => Ok(GestureKey::Edge(Edge::Right)),
            _ => Err(format!("{name:?}: an edge is TouchEdgeTop, Bottom, Left or Right")),
        });
    }
    let (pinch, rest) = match lower.strip_prefix("swipe") {
        Some(rest) => (false, rest),
        None => (true, lower.strip_prefix("pinch")?),
    };
    let digits = rest.find(|character: char| !character.is_ascii_digit()).unwrap_or(rest.len());
    let (count, way) = rest.split_at(digits);
    let motion = match (pinch, way) {
        (false, "left") => Motion::Left,
        (false, "right") => Motion::Right,
        (false, "up") => Motion::Up,
        (false, "down") => Motion::Down,
        (true, "in") => Motion::In,
        (true, "out") => Motion::Out,
        _ => {
            let ways = if pinch { "In or Out" } else { "Left, Right, Up or Down" };
            return Some(Err(format!("{name:?}: a {} ends in {ways}", if pinch { "pinch" } else { "swipe" })));
        }
    };
    // Two fingers on the touchscreen belong to programs.
    let least = if pinch && (!touch || test) { 2 } else { 3 };
    let example = match (touch, pinch) {
        (false, false) => "Swipe3Left",
        (false, true) => "Pinch2In",
        (true, false) => "TouchSwipe3Left",
        (true, true) => "TouchPinch3In",
    };
    match count.parse::<u32>() {
        Ok(fingers) if (least..=5).contains(&fingers) => {
            Some(Ok(if touch { GestureKey::Touch(fingers, motion) } else { GestureKey::Pad(fingers, motion) }))
        }
        _ => Some(Err(format!("{name:?}: give {least} to 5 fingers, as in {example}"))),
    }
}

/// Who a gesture in progress belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// A binding runs once it has gone far enough.
    Bindings,
    /// Passed on to the program under the pointer.
    Client,
    /// Swallowed, while a menu has the pointer.
    Nobody,
}

#[derive(Debug, Clone)]
pub struct Gesture {
    pinch: bool,
    fingers: u32,
    mods: Mods,
    owner: Owner,
    travelled: Point<f64, Logical>,
    /// The pinch's scale so far, 1 at the start.
    scale: f64,
    fired: bool,
    /// Set while the view follows the fingers (`follow_fingers`).
    live: Option<Live>,
    /// Set while a divider follows the fingers (`follow_fingers`).
    resizing: Option<Resizing>,
}

impl Gesture {
    fn new(pinch: bool, fingers: u32, mods: Mods, owner: Owner) -> Self {
        Gesture {
            pinch,
            fingers,
            mods,
            owner,
            travelled: Point::default(),
            scale: 1.0,
            fired: false,
            live: None,
            resizing: None,
        }
    }

    /// Which way the gesture has gone once it has travelled `part` of the way
    /// a binding needs; a swipe needs `distance`.
    fn motion(&self, distance: f64, part: f64) -> Option<Motion> {
        if self.pinch {
            let scale = self.scale;
            return (scale <= 1.0 - (1.0 - PINCH_IN) * part)
                .then_some(Motion::In)
                .or((scale >= 1.0 + (PINCH_OUT - 1.0) * part).then_some(Motion::Out));
        }
        let Point { x: delta_x, y: delta_y, .. } = self.travelled;
        (delta_x.hypot(delta_y) >= distance * part).then(|| {
            match (delta_x.abs() > delta_y.abs(), delta_x > 0.0, delta_y > 0.0) {
                (true, true, _) => Motion::Right,
                (true, false, _) => Motion::Left,
                (false, _, true) => Motion::Down,
                (false, _, false) => Motion::Up,
            }
        })
    }

    /// How far a swipe has gone in the direction of `motion`; negative when
    /// the fingers came back past where they started.
    fn along(&self, motion: Motion) -> f64 {
        let Point { x: across, y: down, .. } = self.travelled;
        match motion {
            Motion::Left => -across,
            Motion::Right => across,
            Motion::Up => -down,
            Motion::Down => down,
            Motion::In | Motion::Out => 0.0,
        }
    }

    /// How far along `motion` the fingers are, from 0 to 1 (the whole move).
    fn progress(&self, motion: Motion, distance: f64) -> f64 {
        let progress = match motion {
            Motion::In => (1.0 - self.scale) / (1.0 - PINCH_IN_FULL),
            Motion::Out => (self.scale - 1.0) / (PINCH_OUT_FULL - 1.0),
            swipe => self.along(swipe) / (distance * SWIPE_FULL),
        };
        progress.clamp(0.0, 1.0)
    }

    /// Whether a following gesture has gone as far as a binding needs, so
    /// letting go now keeps the move.
    fn counts(&self, motion: Motion, distance: f64) -> bool {
        match motion {
            Motion::In => self.scale <= PINCH_IN,
            Motion::Out => self.scale >= PINCH_OUT,
            swipe => self.along(swipe) >= distance,
        }
    }
}

/// A navigation gesture moving the view with the fingers.
#[derive(Debug, Clone)]
struct Live {
    motion: Motion,
    /// Where navigation stood before, to go back to if let go early.
    before: tileception_core::Navigation,
    area: Option<crate::outputs::AreaId>,
}

/// A swipe bound to `resize`, dragging the divider beside the focused window
/// as far as the fingers go.
#[derive(Debug, Clone)]
struct Resizing {
    motion: Motion,
    /// The way the bound `resize` moves the divider, as a unit vector.
    way: (f64, f64),
    /// How far along `motion` the divider has been moved so far.
    moved: f64,
}

impl Resizing {
    /// The divider's move, in pixels across and down, that takes it to where
    /// fingers `along` the swipe's direction have got to.
    fn step(&mut self, along: f64) -> (f64, f64) {
        let distance = along - self.moved;
        self.moved = along;
        (self.way.0 * distance, self.way.1 * distance)
    }
}

/// The way commands bound to a swipe move a divider, as a unit vector, when
/// every one of them is `resize`.
fn resize_way(commands: &[Command]) -> Option<(f64, f64)> {
    use crate::input::Action;
    let mut way = (0.0, 0.0);
    for command in commands {
        let Command::Action(Action::Resize(across, down)) = command else { return None };
        way = (way.0 + across, way.1 + down);
    }
    let length = way.0.hypot(way.1);
    (length > 0.0).then(|| (way.0 / length, way.1 / length))
}

/// How far a pinch goes, as a scale, before it counts.
const PINCH_IN: f64 = 0.8;
const PINCH_OUT: f64 = 1.25;
/// Following the fingers, the scale at which a pinch has moved the view all
/// the way.
const PINCH_IN_FULL: f64 = 0.5;
const PINCH_OUT_FULL: f64 = 2.0;
/// Following the fingers, the part of `swipe_distance` (or of the way to
/// `PINCH_IN` or `PINCH_OUT`) after which the direction is settled.
const SETTLE: f64 = 0.25;
/// Following the fingers, how many times `swipe_distance` a swipe travels to
/// move the view all the way. Letting go after `swipe_distance` still counts.
const SWIPE_FULL: f64 = 3.0;

impl Tileception {
    /// A touchpad turned up: remembered, so a reload can set it again.
    pub fn touchpad_added(&mut self, mut device: Device) {
        if device.config_tap_finger_count() == 0 {
            return;
        }
        self.config.file.touchpad.apply(&mut device);
        self.touchpads.push(device);
    }

    pub fn touchpad_removed(&mut self, device: &Device) {
        self.touchpads.retain(|candidate| candidate != device);
    }

    pub(crate) fn configure_touchpads(&mut self) {
        let config = self.config.file.touchpad.clone();
        for device in &mut self.touchpads {
            config.apply(device);
        }
    }

    /// The modifiers bindings are looked up with: nested, Alt stands in for
    /// Super, as for keys.
    pub(crate) fn gesture_mods(&self) -> Mods {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let mut held = Mods::from_state(&keyboard.modifier_state());
        if self.nested && held.alt && !held.logo {
            held.alt = false;
            held.logo = true;
        }
        held
    }

    fn gesture_bound(&self, pinch: bool, fingers: u32, mods: Mods) -> bool {
        MOTIONS
            .iter()
            .filter(|motion| motion.is_pinch() == pinch)
            .any(|motion| self.config.lookup(mods, key(fingers, *motion)).is_some())
    }

    pub(crate) fn gesture_begin(&mut self, pinch: bool, fingers: u32, mods: Option<Mods>, time: u32) {
        // Super+pinch must not open the launcher when Super is let go.
        self.tap_armed = None;
        let mods = mods.unwrap_or_else(|| self.gesture_mods());
        let owner = if self.is_locked() {
            Owner::Client
        } else if self.power_menu.is_some() || (self.launcher.open && !self.launcher.home) {
            Owner::Nobody
        } else if self.gesture_bound(pinch, fingers, mods) {
            Owner::Bindings
        } else {
            Owner::Client
        };
        self.gesture = Some(Gesture::new(pinch, fingers, mods, owner));
        if owner == Owner::Client {
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            let serial = SERIAL_COUNTER.next_serial();
            if pinch {
                pointer.gesture_pinch_begin(self, &GesturePinchBeginEvent { serial, time, fingers });
            } else {
                pointer.gesture_swipe_begin(self, &GestureSwipeBeginEvent { serial, time, fingers });
            }
            pointer.frame(self);
        }
    }

    /// Movement of a gesture; `scale` and `rotation` only mean something for
    /// pinches.
    pub(crate) fn gesture_update(&mut self, delta: Point<f64, Logical>, scale: f64, rotation: f64, time: u32) {
        let Some(mut gesture) = self.gesture.take() else {
            return;
        };
        match gesture.owner {
            Owner::Nobody => {}
            Owner::Client => {
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                if gesture.pinch {
                    pointer.gesture_pinch_update(self, &GesturePinchUpdateEvent { time, delta, scale, rotation });
                } else {
                    pointer.gesture_swipe_update(self, &GestureSwipeUpdateEvent { time, delta });
                }
                pointer.frame(self);
            }
            Owner::Bindings => {
                gesture.travelled += delta;
                gesture.scale = scale;
                let distance = self.config.file.touchpad.swipe_distance;
                if let Some(mut resizing) = gesture.resizing.take() {
                    let (across, down) = resizing.step(gesture.along(resizing.motion));
                    self.resize_focused_by(across, down);
                    gesture.resizing = Some(resizing);
                } else if let Some(live) = &gesture.live {
                    let progress = gesture.progress(live.motion, distance);
                    if self.active_area() == live.area {
                        self.world.hold_camera(progress);
                    }
                } else if !gesture.fired {
                    let (fingers, mods) = (gesture.fingers, gesture.mods);
                    // Following the fingers, a navigation or resize gesture
                    // starts as soon as its direction is clear.
                    let settled = gesture.motion(distance, SETTLE).filter(|_| self.config.file.touchpad.follow_fingers);
                    let resize = settled
                        .filter(|motion| !motion.is_pinch())
                        .and_then(|motion| Some((motion, self.gesture_resize_way(fingers, motion, mods)?)));
                    let live = settled.is_some_and(|motion| self.gesture_navigates(fingers, motion, mods));
                    if let Some((motion, way)) = resize {
                        gesture.fired = true;
                        let mut resizing = Resizing { motion, way, moved: 0.0 };
                        let (across, down) = resizing.step(gesture.along(motion));
                        self.resize_focused_by(across, down);
                        gesture.resizing = Some(resizing);
                    } else {
                        let motion = gesture.motion(distance, if live { SETTLE } else { 1.0 });
                        // One action per gesture: lift the fingers to do it again.
                        gesture.fired = motion.is_some();
                        if let Some(motion) = motion {
                            let before = live.then(|| (self.world.navigation(), self.active_area()));
                            self.gesture = Some(gesture.clone());
                            self.run_gesture(fingers, motion, mods);
                            if let Some((before, area)) = before {
                                self.world.hold_camera(gesture.progress(motion, distance));
                                gesture.live = Some(Live { motion, before, area });
                            }
                        }
                    }
                }
            }
        }
        self.gesture = Some(gesture);
    }

    /// Whether every command bound to a gesture only moves the view, so the
    /// view can follow the fingers and go back if let go early.
    fn gesture_navigates(&self, fingers: u32, motion: Motion, mods: Mods) -> bool {
        use crate::input::Action;
        self.config.lookup(mods, key(fingers, motion)).is_some_and(|commands| {
            !commands.is_empty()
                && commands.iter().all(|command| {
                    matches!(
                        command,
                        Command::Action(
                            Action::Overview
                                | Action::ExitOverview
                                | Action::StepIn
                                | Action::StepInPointer
                                | Action::StepOut
                                | Action::Neighbour(_)
                                | Action::Back
                                | Action::Forward
                                | Action::Maximize
                        )
                    )
                })
        })
    }

    /// The way a swipe's `resize` binding moves a divider, if `resize` is all
    /// it is bound to.
    fn gesture_resize_way(&self, fingers: u32, motion: Motion, mods: Mods) -> Option<(f64, f64)> {
        self.config.lookup(mods, key(fingers, motion)).and_then(resize_way)
    }

    /// Moves the dividers around the focused window, as `resize` does, while
    /// the fingers drag them.
    fn resize_focused_by(&mut self, across: f64, down: f64) {
        if across == 0.0 && down == 0.0 {
            return;
        }
        let now = self.now();
        self.world.resize_focused(across, down, now);
        self.grow_area_of_focus(across);
        self.apply_layout();
    }

    pub(crate) fn gesture_end(&mut self, cancelled: bool, time: u32) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if let Some(live) = &gesture.live
            && self.active_area() == live.area
        {
            let now = self.now();
            if !cancelled && gesture.counts(live.motion, self.config.file.touchpad.swipe_distance) {
                self.world.release_camera(now);
            } else {
                self.world.restore_navigation(live.before.clone(), now);
                self.apply_layout();
            }
        }
        // A dragged divider stays where the fingers left it, as with the pointer.
        if gesture.resizing.is_some() {
            self.persist_layout();
        }
        if gesture.owner == Owner::Client {
            let pointer = self.seat.get_pointer().expect("seat has a pointer");
            let serial = SERIAL_COUNTER.next_serial();
            if gesture.pinch {
                pointer.gesture_pinch_end(self, &GesturePinchEndEvent { serial, time, cancelled });
            } else {
                pointer.gesture_swipe_end(self, &GestureSwipeEndEvent { serial, time, cancelled });
            }
            pointer.frame(self);
        }
    }

    /// Holds (fingers resting on the pad) only stop kinetic scrolling in
    /// programs, so they always go there.
    pub(crate) fn gesture_hold(&mut self, begin: Option<u32>, cancelled: bool, time: u32) {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let serial = SERIAL_COUNTER.next_serial();
        match begin {
            Some(fingers) => pointer.gesture_hold_begin(self, &GestureHoldBeginEvent { serial, time, fingers }),
            None => pointer.gesture_hold_end(self, &GestureHoldEndEvent { serial, time, cancelled }),
        }
        pointer.frame(self);
    }

    fn run_gesture(&mut self, fingers: u32, motion: Motion, mods: Mods) {
        self.run_gesture_key(GestureKey::Pad(fingers, motion), mods);
    }

    /// Runs what `[binds]` gives a gesture, if anything.
    pub(crate) fn run_gesture_key(&mut self, gesture: GestureKey, mods: Mods) {
        tracing::debug!(?gesture, "gesture");
        let commands = self.config.lookup(mods, gesture.key()).map(<[Command]>::to_vec).unwrap_or_default();
        for command in commands {
            if let Err(err) = self.execute(command) {
                tracing::warn!("{err}");
            }
        }
    }

    /// A gesture for scripted tests, taking the same path as a real one: the
    /// fingers go `amount` times the distance a binding needs, in even steps.
    /// Held, they stay down; the next call with the same fingers moves them on
    /// from there instead of starting again.
    pub(crate) fn inject_gesture(
        &mut self,
        mods: Mods,
        fingers: u32,
        motion: Motion,
        amount: f64,
        hold: bool,
        time: u32,
    ) {
        const STEPS: u32 = 8;
        let pinch = motion.is_pinch();
        let carries_on =
            self.gesture.as_ref().is_some_and(|gesture| gesture.pinch == pinch && gesture.fingers == fingers);
        if !carries_on {
            self.gesture_end(true, time);
            self.gesture_begin(pinch, fingers, Some(mods), time);
        }
        let (travelled, scale) =
            self.gesture.as_ref().map_or((Point::default(), 1.0), |gesture| (gesture.travelled, gesture.scale));
        let distance = self.config.file.touchpad.swipe_distance * amount;
        let target: Point<f64, Logical> = match motion {
            Motion::Left => (-distance, 0.0),
            Motion::Right => (distance, 0.0),
            Motion::Up => (0.0, -distance),
            Motion::Down => (0.0, distance),
            Motion::In | Motion::Out => (0.0, 0.0),
        }
        .into();
        let step = (target - travelled).downscale(f64::from(STEPS));
        let target_scale = match motion {
            Motion::In => (1.0 - (1.0 - PINCH_IN) * amount).max(0.05),
            Motion::Out => 1.0 + (PINCH_OUT - 1.0) * amount,
            _ => 1.0,
        };
        for index in 1..=STEPS {
            let along = scale + (target_scale - scale) * f64::from(index) / f64::from(STEPS);
            self.gesture_update(step, along, 0.0, time + index);
        }
        if !hold {
            self.gesture_end(false, time + STEPS + 1);
        }
    }
}

delegate_pointer_gestures!(Tileception);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gesture_names() {
        assert_eq!(parse_key("Swipe3Left"), Some(Ok(key(3, Motion::Left))));
        assert_eq!(parse_key("pinch2out"), Some(Ok(key(2, Motion::Out))));
        assert_eq!(decode(key(4, Motion::Down)), Some(GestureKey::Pad(4, Motion::Down)));
        let touch = GestureKey::Touch(3, Motion::In);
        assert_eq!(parse_key("TouchPinch3In"), Some(Ok(touch.key())));
        assert_eq!(decode(touch.key()), Some(touch));
        let edge = GestureKey::Edge(Edge::Right);
        assert_eq!(parse_key("TouchEdgeRight"), Some(Ok(edge.key())));
        assert_eq!(decode(edge.key()), Some(edge));
        assert!(matches!(parse_key("TouchPinch2In"), Some(Err(_))));
        assert_eq!(parse_gesture("TouchPinch2In", true), Some(Ok(GestureKey::Touch(2, Motion::In))));
        assert!(matches!(parse_key("TouchEdgeMiddle"), Some(Err(_))));
        assert!(matches!(parse_key("Swipe2Left"), Some(Err(_))));
        assert!(matches!(parse_key("Pinch3Left"), Some(Err(_))));
        assert!(matches!(parse_key("Swipe6Up"), Some(Err(_))));
        assert_eq!(parse_key("space"), None);
    }

    fn swipe(across: f64, down: f64) -> Gesture {
        let mut gesture = Gesture::new(false, 3, Mods::default(), Owner::Bindings);
        gesture.travelled = Point::from((across, down));
        gesture
    }

    fn pinch(scale: f64) -> Gesture {
        let mut gesture = Gesture::new(true, 2, Mods::default(), Owner::Bindings);
        gesture.scale = scale;
        gesture
    }

    #[test]
    fn a_gesture_settles_on_the_way_it_went_furthest() {
        assert_eq!(swipe(10.0, 5.0).motion(60.0, 1.0), None);
        assert_eq!(swipe(-50.0, 40.0).motion(60.0, 1.0), Some(Motion::Left));
        assert_eq!(swipe(5.0, 70.0).motion(60.0, 1.0), Some(Motion::Down));
        assert_eq!(swipe(0.0, -15.0).motion(60.0, SETTLE), Some(Motion::Up));
        assert_eq!(swipe(0.0, -15.0).motion(60.0, 1.0), None);
        assert_eq!(pinch(0.95).motion(60.0, 1.0), None);
        assert_eq!(pinch(0.95).motion(60.0, SETTLE), Some(Motion::In));
        assert_eq!(pinch(1.3).motion(60.0, 1.0), Some(Motion::Out));
    }

    #[test]
    fn following_fingers_counts_how_far_they_went() {
        let gesture = swipe(-90.0, 0.0);
        assert_eq!(gesture.along(Motion::Left), 90.0);
        assert_eq!(gesture.along(Motion::Right), -90.0);
        assert!((gesture.progress(Motion::Left, 60.0) - 0.5).abs() < 1e-9);
        assert_eq!(gesture.progress(Motion::Right, 60.0), 0.0);
        assert!(gesture.counts(Motion::Left, 60.0));
        assert!(!swipe(-30.0, 0.0).counts(Motion::Left, 60.0));
        assert_eq!(pinch(PINCH_IN_FULL).progress(Motion::In, 60.0), 1.0);
        assert!(pinch(PINCH_OUT).counts(Motion::Out, 60.0));
        assert!(!pinch(1.1).counts(Motion::Out, 60.0));
    }

    #[test]
    fn only_resize_bindings_drag_a_divider() {
        use crate::input::Action;
        let resize = |across, down| Command::Action(Action::Resize(across, down));
        assert_eq!(resize_way(&[resize(-40.0, 0.0)]), Some((-1.0, 0.0)));
        assert_eq!(resize_way(&[resize(0.0, 10.0), resize(0.0, 30.0)]), Some((0.0, 1.0)));
        assert_eq!(resize_way(&[resize(40.0, 0.0), Command::Action(Action::StepOut)]), None);
        assert_eq!(resize_way(&[resize(0.0, 0.0)]), None);
        assert_eq!(resize_way(&[]), None);
    }

    #[test]
    fn a_dragged_divider_goes_as_far_as_the_fingers() {
        let mut resizing = Resizing { motion: Motion::Left, way: (-1.0, 0.0), moved: 0.0 };
        assert_eq!(resizing.step(swipe(-20.0, 3.0).along(Motion::Left)), (-20.0, 0.0));
        assert_eq!(resizing.step(swipe(-50.0, 0.0).along(Motion::Left)), (-30.0, 0.0));
        // Back past where the fingers started: the divider follows.
        assert_eq!(resizing.step(swipe(10.0, 0.0).along(Motion::Left)), (60.0, 0.0));
        assert_eq!(resizing.step(-10.0), (0.0, 0.0));
    }

    #[test]
    fn super_swipes_manage_windows_by_default() {
        use crate::input::Action;
        use tileception_core::Dir;
        let config = crate::config::Config::default_config();
        let super_key = Mods { logo: true, ..Mods::default() };
        let bound = |fingers, motion| config.lookup(super_key, key(fingers, motion)).map(<[Command]>::to_vec);
        assert_eq!(bound(3, Motion::Left), Some(vec![Command::Action(Action::Move(Dir::Left))]));
        assert_eq!(bound(3, Motion::Down), Some(vec![Command::Action(Action::Move(Dir::Down))]));
        for (motion, way) in [
            (Motion::Left, (-1.0, 0.0)),
            (Motion::Right, (1.0, 0.0)),
            (Motion::Up, (0.0, -1.0)),
            (Motion::Down, (0.0, 1.0)),
        ] {
            assert_eq!(bound(4, motion).as_deref().and_then(resize_way), Some(way), "{motion:?}");
        }
    }

    #[test]
    fn touchpad_settings_are_checked() {
        assert!(TouchpadConfig::default().validate().is_ok());
        let fast = TouchpadConfig { speed: 2.0, ..Default::default() };
        assert!(fast.validate().is_err());
        let odd = TouchpadConfig { click_method: Some("corners".into()), ..Default::default() };
        assert!(odd.validate().is_err());
    }
}
