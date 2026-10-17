//! Touchpad gestures and settings. Swipes and pinches can be bound in
//! `[binds]` (`Swipe3Left`, `Super+Pinch2In`); the ones that are not bound go
//! to the program under the pointer through pointer-gestures. `[touchpad]`
//! sets libinput's options: tap to click, natural scrolling and the rest.

use serde::Deserialize;
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
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
        if let Some(method) = method.filter(|m| device.config_click_methods().contains(m)) {
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
        let motion = |m| MOTIONS.iter().position(|x| *x == m).unwrap_or(0) as u32;
        match self {
            GestureKey::Pad(fingers, m) => GESTURE_KEYS + fingers * 8 + motion(m),
            GestureKey::Touch(fingers, m) => TOUCH_KEYS + fingers * 8 + motion(m),
            GestureKey::Edge(e) => EDGE_KEYS + EDGES.iter().position(|x| *x == e).unwrap_or(0) as u32,
        }
    }
}

/// The gesture a pseudo keysym stands for.
#[cfg(test)]
fn decode(key: u32) -> Option<GestureKey> {
    if let Some(offset) = key.checked_sub(EDGE_KEYS) {
        return EDGES.get(offset as usize).map(|e| GestureKey::Edge(*e));
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
    parse_gesture(name, false).map(|g| g.map(GestureKey::key))
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
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
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
        Ok(n) if (least..=5).contains(&n) => {
            Some(Ok(if touch { GestureKey::Touch(n, motion) } else { GestureKey::Pad(n, motion) }))
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

#[derive(Debug, Clone, Copy)]
pub struct Gesture {
    pinch: bool,
    fingers: u32,
    mods: Mods,
    owner: Owner,
    travelled: Point<f64, Logical>,
    fired: bool,
}

/// How far a pinch goes, as a scale, before it counts.
const PINCH_IN: f64 = 0.8;
const PINCH_OUT: f64 = 1.25;

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
        self.touchpads.retain(|d| d != device);
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
        MOTIONS.iter().filter(|m| m.is_pinch() == pinch).any(|m| self.config.lookup(mods, key(fingers, *m)).is_some())
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
        self.gesture = Some(Gesture { pinch, fingers, mods, owner, travelled: Point::default(), fired: false });
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
        let Some(mut gesture) = self.gesture else {
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
                let motion = if gesture.fired {
                    None
                } else if gesture.pinch {
                    (scale <= PINCH_IN).then_some(Motion::In).or((scale >= PINCH_OUT).then_some(Motion::Out))
                } else {
                    let Point { x, y, .. } = gesture.travelled;
                    (x.hypot(y) >= self.config.file.touchpad.swipe_distance).then(|| {
                        match (x.abs() > y.abs(), x > 0.0, y > 0.0) {
                            (true, true, _) => Motion::Right,
                            (true, false, _) => Motion::Left,
                            (false, _, true) => Motion::Down,
                            (false, _, false) => Motion::Up,
                        }
                    })
                };
                // One action per gesture: lift the fingers to do it again.
                gesture.fired |= motion.is_some();
                self.gesture = Some(gesture);
                if let Some(motion) = motion {
                    self.run_gesture(gesture.fingers, motion, gesture.mods);
                }
            }
        }
    }

    pub(crate) fn gesture_end(&mut self, cancelled: bool, time: u32) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
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

    /// A whole gesture for scripted tests, taking the same path as a real one.
    pub(crate) fn inject_gesture(&mut self, mods: Mods, fingers: u32, motion: Motion, time: u32) {
        const STEPS: u32 = 8;
        let pinch = motion.is_pinch();
        self.gesture_begin(pinch, fingers, Some(mods), time);
        // Twice the distance a binding needs, in even steps.
        let step = self.config.file.touchpad.swipe_distance * 2.0 / f64::from(STEPS);
        let delta: Point<f64, Logical> = match motion {
            Motion::Left => (-step, 0.0),
            Motion::Right => (step, 0.0),
            Motion::Up => (0.0, -step),
            Motion::Down => (0.0, step),
            Motion::In | Motion::Out => (0.0, 0.0),
        }
        .into();
        let target = if motion == Motion::In { 0.6 } else { 1.6 };
        for i in 1..=STEPS {
            let scale = 1.0 + (target - 1.0) * f64::from(i) / f64::from(STEPS);
            self.gesture_update(delta, scale, 0.0, time + i);
        }
        self.gesture_end(false, time + STEPS + 1);
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

    #[test]
    fn touchpad_settings_are_checked() {
        assert!(TouchpadConfig::default().validate().is_ok());
        let fast = TouchpadConfig { speed: 2.0, ..Default::default() };
        assert!(fast.validate().is_err());
        let odd = TouchpadConfig { click_method: Some("corners".into()), ..Default::default() };
        assert!(odd.validate().is_err());
    }
}
