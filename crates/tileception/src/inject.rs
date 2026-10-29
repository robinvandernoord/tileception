//! Pretend input for scripted tests: `tileception msg pointer/click/key/type/gesture/touch`.
//! Keys and clicks take the same path as real ones. Only headless or nested,
//! so a script can never drive a real session.

use smithay::{
    backend::input::{ButtonState, KeyState},
    input::keyboard::{Keycode, xkb},
    utils::{Logical, Point},
};

use crate::Tileception;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

#[derive(Debug, Clone, PartialEq)]
pub enum Inject {
    /// Moves the pointer.
    Pointer(f64, f64),
    /// Presses and lets go of a button, after moving there when given.
    Click { position: Option<(f64, f64)>, button: u32 },
    /// A key combination such as `Super+slash` or `Return`.
    Key(String),
    /// Types text, with Shift where a character needs it.
    Type(String),
    /// A whole touchpad gesture, such as `Swipe3Left` or `Super+Pinch2In`.
    /// A touchpad gesture (`Super+Pinch2In`), or a touchscreen one
    /// (`TouchSwipe3Left`, `TouchEdgeTop`) around the pointer.
    Gesture { mods: crate::config::Mods, gesture: crate::gestures::GestureKey },
    /// One finger on the touchscreen: a tap, or a drag to a second point.
    Touch { from: (f64, f64), target: (f64, f64) },
}

pub fn parse(verb: &str, rest: &str) -> Result<Inject, String> {
    let args: Vec<&str> = rest.split_whitespace().collect();
    let number = |arg: &str| arg.parse::<f64>().map_err(|_| format!("{verb}: {arg:?} is not a number"));
    match verb {
        "pointer" => match args[..] {
            [x_arg, y_arg] => Ok(Inject::Pointer(number(x_arg)?, number(y_arg)?)),
            _ => Err("pointer needs x and y".into()),
        },
        "click" => {
            let (position, button) = match args[..] {
                [] => (None, "left"),
                [button_name] => (None, button_name),
                [x_arg, y_arg] => (Some((number(x_arg)?, number(y_arg)?)), "left"),
                [x_arg, y_arg, button_name] => (Some((number(x_arg)?, number(y_arg)?)), button_name),
                _ => return Err("click takes [x y] [left|right|middle]".into()),
            };
            let button = match button {
                "left" => BTN_LEFT,
                "right" => BTN_RIGHT,
                "middle" => BTN_MIDDLE,
                other => return Err(format!("click: no button {other:?}")),
            };
            Ok(Inject::Click { position, button })
        }
        "key" if !rest.trim().is_empty() => Ok(Inject::Key(rest.trim().to_string())),
        "key" => Err("key needs a key, such as Return or Super+slash".into()),
        "type" => Ok(Inject::Type(rest.to_string())),
        "touch" => match args[..] {
            [x_arg, y_arg] => {
                Ok(Inject::Touch { from: (number(x_arg)?, number(y_arg)?), target: (number(x_arg)?, number(y_arg)?) })
            }
            [x_arg, y_arg, target_x_arg, target_y_arg] => Ok(Inject::Touch {
                from: (number(x_arg)?, number(y_arg)?),
                target: (number(target_x_arg)?, number(target_y_arg)?),
            }),
            _ => Err("touch takes x y, or x y x2 y2 to drag".into()),
        },
        "gesture" => {
            let rest = rest.trim();
            let (held, name) = rest.rsplit_once('+').unwrap_or(("", rest));
            let mods = match held {
                "" => crate::config::Mods::default(),
                held => crate::config::parse_combo(&format!("{held}+space"))?.0,
            };
            let gesture = crate::gestures::parse_gesture(name, true)
                .ok_or("gesture needs a gesture, such as Swipe3Left, Super+Pinch2In or TouchEdgeTop")??;
            Ok(Inject::Gesture { mods, gesture })
        }
        _ => Err(format!("unknown input command {verb:?}")),
    }
}

/// The key (and whether Shift is needed) that gives a keysym, in a keymap
/// compiled like the seat's.
struct Keys {
    keymap: xkb::Keymap,
}

impl Keys {
    fn new(keyboard: &crate::config::Keyboard) -> Option<Keys> {
        let (model, layout, variant, options) = keyboard.names();
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap =
            xkb::Keymap::new_from_names(&context, "", &model, &layout, &variant, options, xkb::KEYMAP_COMPILE_NO_FLAGS)
                .or_else(|| {
                    xkb::Keymap::new_from_names(&context, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
                })?;
        Some(Keys { keymap })
    }

    fn find(&self, sym: xkb::Keysym) -> Option<(Keycode, bool)> {
        let (min, max) = (self.keymap.min_keycode().raw(), self.keymap.max_keycode().raw());
        // Unshifted first, so "a" is not typed as Shift+A.
        for level in 0..2 {
            for raw in min..=max {
                let code = Keycode::new(raw);
                if level < self.keymap.num_levels_for_key(code, 0)
                    && self.keymap.key_get_syms_by_level(code, 0, level).contains(&sym)
                {
                    return Some((code, level == 1));
                }
            }
        }
        None
    }
}

impl Tileception {
    pub fn inject(&mut self, input: Inject) -> Result<(), String> {
        if self.headless.is_none() && !self.nested {
            return Err("pretend input only works headless or nested".into());
        }
        let time = (self.now() * 1000.0) as u32;
        match input {
            Inject::Pointer(x_pos, y_pos) => self.inject_motion(x_pos, y_pos, time),
            Inject::Click { position, button } => {
                if let Some((x_pos, y_pos)) = position {
                    self.inject_motion(x_pos, y_pos, time);
                }
                self.on_button(button, ButtonState::Pressed, time);
                self.on_button(button, ButtonState::Released, time + 1);
            }
            Inject::Key(combo) => {
                let keys = Keys::new(&self.config.file.keyboard).ok_or("no keymap")?;
                let mut parts: Vec<&str> = combo.split('+').map(str::trim).collect();
                let key = parts.pop().ok_or("empty key")?;
                let mut held = Vec::new();
                for modifier in parts {
                    let name = match modifier.to_ascii_lowercase().as_str() {
                        "super" | "logo" | "win" => "Super_L",
                        "shift" => "Shift_L",
                        "ctrl" | "control" => "Control_L",
                        "alt" => "Alt_L",
                        other => return Err(format!("unknown modifier {other:?}")),
                    };
                    held.push(keys.find(xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS)).ok_or(name)?.0);
                }
                let mut sym = xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS);
                if sym.raw() == 0 {
                    sym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
                }
                let (code, shift) = keys.find(sym).ok_or_else(|| format!("no key gives {key:?}"))?;
                if shift && held.is_empty() {
                    held.extend(keys.find(xkb::Keysym::Shift_L).map(|shift_key| shift_key.0));
                }
                self.tap(&held, code, time);
            }
            Inject::Type(text) => {
                let keys = Keys::new(&self.config.file.keyboard).ok_or("no keymap")?;
                let shift = keys.find(xkb::Keysym::Shift_L).map(|shift_key| shift_key.0);
                for (index, character) in text.chars().enumerate() {
                    let sym = xkb::utf32_to_keysym(character as u32);
                    let (code, shifted) = keys.find(sym).ok_or_else(|| format!("no key types {character:?}"))?;
                    let held: Vec<Keycode> = if shifted { shift.into_iter().collect() } else { Vec::new() };
                    self.tap(&held, code, time + 4 * index as u32);
                }
            }
            Inject::Gesture { mods, gesture: crate::gestures::GestureKey::Pad(fingers, motion) } => {
                self.inject_gesture(mods, fingers, motion, time)
            }
            Inject::Gesture { gesture, .. } => self.inject_touch_gesture(gesture, time),
            Inject::Touch { from, target } => self.inject_touch(from, target, time),
        }
        Ok(())
    }

    fn inject_motion(&mut self, x_pos: f64, y_pos: f64, time: u32) {
        let pos: Point<f64, Logical> = self.desktop_motion((x_pos, y_pos).into());
        self.on_motion(pos, time);
    }

    /// Holds `held`, presses and lets go of `key`, then lets go of `held`.
    fn tap(&mut self, held: &[Keycode], key: Keycode, time: u32) {
        for modifier in held {
            self.handle_key(*modifier, KeyState::Pressed, time);
        }
        self.handle_key(key, KeyState::Pressed, time + 1);
        self.handle_key(key, KeyState::Released, time + 2);
        for modifier in held.iter().rev() {
            self.handle_key(*modifier, KeyState::Released, time + 3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_input_commands() {
        assert_eq!(parse("pointer", "10 20"), Ok(Inject::Pointer(10.0, 20.0)));
        assert_eq!(parse("click", ""), Ok(Inject::Click { position: None, button: BTN_LEFT }));
        assert_eq!(parse("click", "5 6 right"), Ok(Inject::Click { position: Some((5.0, 6.0)), button: BTN_RIGHT }));
        assert_eq!(parse("key", "Super+slash"), Ok(Inject::Key("Super+slash".into())));
        assert_eq!(parse("type", "make gaps smaller"), Ok(Inject::Type("make gaps smaller".into())));
        assert!(parse("pointer", "1").is_err());
        assert!(parse("key", " ").is_err());
        let logo = crate::config::Mods { logo: true, ..Default::default() };
        assert_eq!(
            parse("gesture", "Super+Pinch2In"),
            Ok(Inject::Gesture {
                mods: logo,
                gesture: crate::gestures::GestureKey::Pad(2, crate::gestures::Motion::In)
            })
        );
        assert!(parse("gesture", "Super+t").is_err());
        assert_eq!(
            parse("gesture", "TouchPinch2Out"),
            Ok(Inject::Gesture {
                mods: Default::default(),
                gesture: crate::gestures::GestureKey::Touch(2, crate::gestures::Motion::Out)
            })
        );
        assert_eq!(parse("touch", "1 2"), Ok(Inject::Touch { from: (1.0, 2.0), target: (1.0, 2.0) }));
        assert!(parse("touch", "1 2 3").is_err());
    }

    #[test]
    fn finds_keys_for_symbols() {
        let keys =
            Keys::new(&crate::config::Keyboard { layout: Some("us".into()), ..Default::default() }).expect("us keymap");
        let (small, a_shift) = keys.find(xkb::utf32_to_keysym('a' as u32)).expect("a");
        let (big, big_shift) = keys.find(xkb::utf32_to_keysym('A' as u32)).expect("A");
        assert_eq!((small, a_shift, big_shift), (big, false, true));
        assert!(keys.find(xkb::utf32_to_keysym('?' as u32)).is_some_and(|found| found.1));
        assert!(keys.find(xkb::Keysym::Return).is_some());
    }
}
