//! `send-key` and `send-scroll`: a key or wheel notches for the program, as
//! a binding such as `"Alt+WheelDown" = "send-key Page_Down"` asks.
//!
//! The binding's own modifiers are still held, and the program would read
//! them with what is sent: Shift with Page Down extends a selection, Alt with
//! the wheel goes back in a browser. So the program is told no modifiers are
//! held while it gets the key or the scroll, then told what is held again.

use smithay::{
    backend::input::{AxisSource, KeyState},
    input::keyboard::{FilterResult, KeyboardTarget, ModifiersState, xkb},
    utils::SERIAL_COUNTER,
};

use crate::{Tileception, inject::Keys, input::AxisInput};

/// A wheel notch, in the pixels a wheel reports for one.
const NOTCH: f64 = 15.0;

/// Parses `send-key <key>`: one key, by its keysym name (`Page_Down`,
/// `Home`, `Up`, `space`).
pub fn parse_key(rest: &str) -> Result<String, String> {
    let key = rest.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return Err("send-key needs one key name, such as Page_Down".into());
    }
    if key.len() > 1 && key.contains('+') {
        return Err("send-key sends one key without modifiers, such as Page_Down".into());
    }
    if keysym(key).raw() == 0 {
        return Err(format!("send-key does not know the key {key:?}"));
    }
    Ok(key.to_string())
}

/// Parses `send-scroll <notches>`: wheel notches, down when positive.
pub fn parse_scroll(rest: &str) -> Result<f64, String> {
    rest.trim()
        .parse::<f64>()
        .ok()
        .filter(|notches| notches.is_finite() && *notches != 0.0 && notches.abs() <= 100.0)
        .ok_or_else(|| "send-scroll needs a number of wheel notches from -100 to 100, down when positive".into())
}

fn keysym(name: &str) -> xkb::Keysym {
    let sym = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
    if sym.raw() == 0 { xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE) } else { sym }
}

/// `held` as a program would see it with no modifier keys down: locks such as
/// Caps Lock and the layout stay.
fn released(held: ModifiersState) -> ModifiersState {
    let mut state = held;
    state.ctrl = false;
    state.alt = false;
    state.shift = false;
    state.logo = false;
    state.iso_level3_shift = false;
    state.iso_level5_shift = false;
    state.serialized.depressed = 0;
    state.serialized.latched = 0;
    state
}

impl Tileception {
    /// Presses and lets go of `key` for the program with the keyboard.
    pub(crate) fn send_key(&mut self, key: &str) -> Result<(), String> {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        if keyboard.current_focus().is_none() {
            return Err("no window has the keyboard".into());
        }
        let layout = keyboard.modifier_state().serialized.layout_effective;
        let keys = Keys::new(&self.config.file.keyboard).ok_or("no keymap")?;
        // Only keys typed without Shift or AltGr: those would need modifiers.
        let (code, _) = keys.locate(keysym(key), layout, 1).ok_or_else(|| format!("no key gives {key:?}"))?;
        if keyboard.pressed_keys().contains(&code) {
            return Err(format!("{key} is held down already"));
        }
        let time = self.input_time();
        self.with_modifiers_released(|state| {
            for (pressed, offset) in [(KeyState::Pressed, 0), (KeyState::Released, 1)] {
                let serial = SERIAL_COUNTER.next_serial();
                keyboard.input::<(), _>(state, code, pressed, serial, time + offset, |_, _, _| FilterResult::Forward);
            }
        });
        Ok(())
    }

    /// Scrolls the program under the pointer by `notches`, as a wheel would.
    pub(crate) fn send_scroll(&mut self, notches: f64) {
        let event = AxisInput {
            source: AxisSource::Wheel,
            time: self.input_time(),
            horizontal: None,
            vertical: Some(notches * NOTCH),
            horizontal_v120: None,
            vertical_v120: Some(notches * 120.0),
        };
        self.with_modifiers_released(|state| state.deliver_axis(&event));
    }

    /// Runs `send` while the program with the keyboard is told that no
    /// modifiers are held, then tells it what is held again.
    fn with_modifiers_released(&mut self, send: impl FnOnce(&mut Self)) {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let held = keyboard.modifier_state();
        let focus = keyboard.current_focus();
        let tell = |state: &mut Self, modifiers: ModifiersState| {
            if let Some(focus) = &focus {
                let seat = state.seat.clone();
                focus.modifiers(&seat, state, modifiers, SERIAL_COUNTER.next_serial());
            }
        };
        let masked = released(held);
        if masked.serialized != held.serialized {
            tell(self, masked);
        }
        send(self);
        if masked.serialized != held.serialized {
            tell(self, keyboard.modifier_state());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys_and_notches() {
        assert_eq!(parse_key("Page_Down"), Ok("Page_Down".into()));
        assert_eq!(parse_key(" page_up "), Ok("page_up".into()));
        assert!(parse_key("").is_err());
        assert!(parse_key("Shift+Page_Down").is_err());
        assert!(parse_key("NoSuchKey").is_err());
        assert!(parse_key("Page_Down Home").is_err());
        assert_eq!(parse_scroll("5"), Ok(5.0));
        assert_eq!(parse_scroll("-2.5"), Ok(-2.5));
        assert!(parse_scroll("0").is_err());
        assert!(parse_scroll("many").is_err());
        assert!(parse_scroll("1000").is_err());
    }

    #[test]
    fn released_keeps_locks_and_layout() {
        let mut held = ModifiersState { shift: true, caps_lock: true, ..Default::default() };
        held.serialized.depressed = 1;
        held.serialized.locked = 2;
        held.serialized.layout_effective = 1;
        let masked = released(held);
        assert!(!masked.shift && masked.caps_lock);
        assert_eq!(masked.serialized.depressed, 0);
        assert_eq!(masked.serialized.locked, 2);
        assert_eq!(masked.serialized.layout_effective, 1);
    }
}
