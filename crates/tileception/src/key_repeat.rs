//! Holding a key down where no client repeats it: volume and brightness keys,
//! and typing in the compositor's own screens (the launcher, the duck, the
//! doctor, the power menu, the lock screen and the greeter).

use std::time::Duration;

use smithay::input::keyboard::{Keysym, keysyms};
use smithay::reexports::calloop::{
    RegistrationToken,
    timer::{TimeoutAction, Timer},
};

use crate::{Tileception, commands::Command, config::Mods};

/// What a held key does again.
#[derive(Clone)]
pub enum Repeat {
    /// Volume and brightness, or a key for one of the compositor's panels.
    Commands(Vec<Command>),
    /// A key on the lock screen or the greeter.
    Greeter { sym: u32, mods: Mods, caps_lock: bool, layout: u32 },
}

impl Repeat {
    /// The repeat for a key that ran `commands`, if holding it should.
    pub fn for_commands(commands: &[Command]) -> Option<Repeat> {
        let repeats = !commands.is_empty()
            && commands.iter().all(|command| match command {
                Command::Media(media) => media.repeats(),
                Command::LauncherKey(sym, mods) | Command::DuckKey(sym, mods) | Command::DoctorKey(sym, mods) => {
                    edits(*sym, *mods)
                }
                Command::PowerMenuKey(sym) => edits(*sym, Mods::default()),
                _ => false,
            });
        repeats.then(|| Repeat::Commands(commands.to_vec()))
    }
}

/// Whether holding the key keeps editing or moving: deleting, the arrows, and
/// typing without Ctrl, Alt or Super. Enter, Esc and Tab act only once.
pub fn edits(sym: u32, mods: Mods) -> bool {
    const EDITING: [u32; 9] = [
        keysyms::KEY_BackSpace,
        keysyms::KEY_Delete,
        keysyms::KEY_KP_Delete,
        keysyms::KEY_Left,
        keysyms::KEY_Right,
        keysyms::KEY_Up,
        keysyms::KEY_Down,
        keysyms::KEY_Page_Up,
        keysyms::KEY_Page_Down,
    ];
    EDITING.contains(&sym)
        || (!mods.ctrl
            && !mods.alt
            && !mods.logo
            && Keysym::new(sym).key_char().is_some_and(|character| !character.is_control()))
}

/// The key being held and its timer.
#[derive(Default)]
pub struct KeyRepeat {
    held: Option<(u32, RegistrationToken)>,
}

impl Tileception {
    /// Repeats `repeat` while `keycode` is held, at the keyboard's delay and rate.
    pub fn start_key_repeat(&mut self, keycode: u32, repeat: Repeat) {
        self.stop_key_repeat();
        let keyboard_config = &self.config.file.keyboard;
        let delay = Duration::from_millis(keyboard_config.repeat_delay.unwrap_or(400).max(1) as u64);
        let interval = Duration::from_secs_f64(1.0 / f64::from(keyboard_config.repeat_rate.unwrap_or(30).max(1)));
        let token = self.loop_handle.insert_source(Timer::from_duration(delay), move |_, _, state| {
            if !state.repeat_key(&repeat) {
                state.key_repeat.held = None;
                return TimeoutAction::Drop;
            }
            state.schedule_render();
            TimeoutAction::ToDuration(interval)
        });
        match token {
            Ok(token) => self.key_repeat.held = Some((keycode, token)),
            Err(error) => tracing::warn!("could not repeat the key: {error}"),
        }
    }

    pub fn stop_key_repeat(&mut self) {
        if let Some((_, token)) = self.key_repeat.held.take() {
            self.loop_handle.remove(token);
        }
    }

    /// Letting go of the held key, or of a modifier, which would change it.
    pub fn key_released(&mut self, keycode: u32, modifier: bool) {
        if modifier || self.key_repeat.held.is_some_and(|(held, _)| held == keycode) {
            self.stop_key_repeat();
        }
    }

    /// Runs the key again. False once what it was for has gone, such as the
    /// launcher closing after Backspace emptied a search that ran something.
    fn repeat_key(&mut self, repeat: &Repeat) -> bool {
        match repeat {
            Repeat::Commands(commands) => {
                let still = commands.iter().all(|command| match command {
                    Command::LauncherKey(..) => self.launcher.open,
                    Command::DuckKey(..) => self.duck.typing(),
                    Command::DoctorKey(..) => self.doctor.open && self.doctor.focused,
                    Command::PowerMenuKey(_) => self.power_menu.is_some(),
                    _ => true,
                });
                if !still {
                    return false;
                }
                for command in commands {
                    if let Err(error) = self.execute(command.clone()) {
                        tracing::warn!("{error}");
                    }
                }
            }
            &Repeat::Greeter { sym, mods, caps_lock, layout } => {
                if self.greeter.is_none() {
                    return false;
                }
                self.greeter_key(sym, mods, caps_lock, layout, true);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use smithay::input::keyboard::keysyms;

    use super::{Repeat, edits};
    use crate::{commands::Command, config::Mods};

    #[test]
    fn editing_and_typing_repeat_but_shortcuts_and_enter_do_not() {
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        assert!(edits(keysyms::KEY_BackSpace, Mods::default()));
        assert!(edits(keysyms::KEY_BackSpace, ctrl));
        assert!(edits(keysyms::KEY_a, Mods::default()));
        assert!(!edits(keysyms::KEY_a, ctrl));
        assert!(!edits(keysyms::KEY_Return, Mods::default()));
        assert!(!edits(keysyms::KEY_Escape, Mods::default()));
        assert!(!edits(keysyms::KEY_Tab, Mods::default()));
        assert!(Repeat::for_commands(&[Command::LauncherKey(keysyms::KEY_BackSpace, Mods::default())]).is_some());
        assert!(Repeat::for_commands(&[Command::LauncherKey(keysyms::KEY_Return, Mods::default())]).is_none());
        assert!(Repeat::for_commands(&[]).is_none());
    }
}
