//! Pretend input for scripted tests: `tileception msg pointer/click/key/type/gesture`.
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
    Click { at: Option<(f64, f64)>, button: u32 },
    /// A key combination such as `Super+slash` or `Return`.
    Key(String),
    /// Types text, with Shift where a character needs it.
    Type(String),
    /// A whole touchpad gesture, such as `Swipe3Left` or `Super+Pinch2In`.
    Gesture { mods: crate::config::Mods, fingers: u32, motion: crate::gestures::Motion },
}

pub fn parse(verb: &str, rest: &str) -> Result<Inject, String> {
    let args: Vec<&str> = rest.split_whitespace().collect();
    let number = |s: &str| s.parse::<f64>().map_err(|_| format!("{verb}: {s:?} is not a number"));
    match verb {
        "pointer" => match args[..] {
            [x, y] => Ok(Inject::Pointer(number(x)?, number(y)?)),
            _ => Err("pointer needs x and y".into()),
        },
        "click" => {
            let (at, button) = match args[..] {
                [] => (None, "left"),
                [b] => (None, b),
                [x, y] => (Some((number(x)?, number(y)?)), "left"),
                [x, y, b] => (Some((number(x)?, number(y)?)), b),
                _ => return Err("click takes [x y] [left|right|middle]".into()),
            };
            let button = match button {
                "left" => BTN_LEFT,
                "right" => BTN_RIGHT,
                "middle" => BTN_MIDDLE,
                other => return Err(format!("click: no button {other:?}")),
            };
            Ok(Inject::Click { at, button })
        }
        "key" if !rest.trim().is_empty() => Ok(Inject::Key(rest.trim().to_string())),
        "key" => Err("key needs a key, such as Return or Super+slash".into()),
        "type" => Ok(Inject::Type(rest.to_string())),
        "gesture" => {
            let (mods, key) = crate::config::parse_combo(rest.trim())?;
            let (fingers, motion) =
                crate::gestures::decode(key).ok_or("gesture needs a gesture, such as Swipe3Left or Super+Pinch2In")?;
            Ok(Inject::Gesture { mods, fingers, motion })
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
    fn new(k: &crate::config::Keyboard) -> Option<Keys> {
        let pick =
            |file: &Option<String>, env: &str| file.clone().unwrap_or_else(|| std::env::var(env).unwrap_or_default());
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            &pick(&k.model, "XKB_DEFAULT_MODEL"),
            &pick(&k.layout, "XKB_DEFAULT_LAYOUT"),
            &pick(&k.variant, "XKB_DEFAULT_VARIANT"),
            k.options.clone().or_else(|| std::env::var("XKB_DEFAULT_OPTIONS").ok()),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .or_else(|| xkb::Keymap::new_from_names(&context, "", "", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS))?;
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
            Inject::Pointer(x, y) => self.inject_motion(x, y, time),
            Inject::Click { at, button } => {
                if let Some((x, y)) = at {
                    self.inject_motion(x, y, time);
                }
                self.on_button(button, ButtonState::Pressed, time);
                self.on_button(button, ButtonState::Released, time + 1);
            }
            Inject::Key(combo) => {
                let keys = Keys::new(&self.config.file.keyboard).ok_or("no keymap")?;
                let mut parts: Vec<&str> = combo.split('+').map(str::trim).collect();
                let key = parts.pop().ok_or("empty key")?;
                let mut held = Vec::new();
                for m in parts {
                    let name = match m.to_ascii_lowercase().as_str() {
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
                    held.extend(keys.find(xkb::Keysym::Shift_L).map(|k| k.0));
                }
                self.tap(&held, code, time);
            }
            Inject::Type(text) => {
                let keys = Keys::new(&self.config.file.keyboard).ok_or("no keymap")?;
                let shift = keys.find(xkb::Keysym::Shift_L).map(|k| k.0);
                for (i, ch) in text.chars().enumerate() {
                    let sym = xkb::utf32_to_keysym(ch as u32);
                    let (code, shifted) = keys.find(sym).ok_or_else(|| format!("no key types {ch:?}"))?;
                    let held: Vec<Keycode> = if shifted { shift.into_iter().collect() } else { Vec::new() };
                    self.tap(&held, code, time + 4 * i as u32);
                }
            }
            Inject::Gesture { mods, fingers, motion } => self.inject_gesture(mods, fingers, motion, time),
        }
        Ok(())
    }

    fn inject_motion(&mut self, x: f64, y: f64, time: u32) {
        let screen = self.output_rect();
        let pos: Point<f64, Logical> =
            (x.clamp(screen.x, screen.right() - 1.0), y.clamp(screen.y, screen.bottom() - 1.0)).into();
        self.on_motion(pos, time);
    }

    /// Holds `held`, presses and lets go of `key`, then lets go of `held`.
    fn tap(&mut self, held: &[Keycode], key: Keycode, time: u32) {
        for m in held {
            self.handle_key(*m, KeyState::Pressed, time);
        }
        self.handle_key(key, KeyState::Pressed, time + 1);
        self.handle_key(key, KeyState::Released, time + 2);
        for m in held.iter().rev() {
            self.handle_key(*m, KeyState::Released, time + 3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_input_commands() {
        assert_eq!(parse("pointer", "10 20"), Ok(Inject::Pointer(10.0, 20.0)));
        assert_eq!(parse("click", ""), Ok(Inject::Click { at: None, button: BTN_LEFT }));
        assert_eq!(parse("click", "5 6 right"), Ok(Inject::Click { at: Some((5.0, 6.0)), button: BTN_RIGHT }));
        assert_eq!(parse("key", "Super+slash"), Ok(Inject::Key("Super+slash".into())));
        assert_eq!(parse("type", "make gaps smaller"), Ok(Inject::Type("make gaps smaller".into())));
        assert!(parse("pointer", "1").is_err());
        assert!(parse("key", " ").is_err());
        let logo = crate::config::Mods { logo: true, ..Default::default() };
        assert_eq!(
            parse("gesture", "Super+Pinch2In"),
            Ok(Inject::Gesture { mods: logo, fingers: 2, motion: crate::gestures::Motion::In })
        );
        assert!(parse("gesture", "Super+t").is_err());
    }

    #[test]
    fn finds_keys_for_symbols() {
        let keys =
            Keys::new(&crate::config::Keyboard { layout: Some("us".into()), ..Default::default() }).expect("us keymap");
        let (a, a_shift) = keys.find(xkb::utf32_to_keysym('a' as u32)).expect("a");
        let (big, big_shift) = keys.find(xkb::utf32_to_keysym('A' as u32)).expect("A");
        assert_eq!((a, a_shift, big_shift), (big, false, true));
        assert!(keys.find(xkb::utf32_to_keysym('?' as u32)).is_some_and(|k| k.1));
        assert!(keys.find(xkb::Keysym::Return).is_some());
    }
}
