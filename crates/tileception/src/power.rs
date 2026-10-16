//! The built-in power menu (Super+X): lock, log out, suspend, restart and
//! power off, as a row of buttons over a dimmed screen.

use smithay::backend::renderer::gles::GlesRenderer;
use tileception_core::Rect;

use crate::{
    render::OutputElement,
    shapes::Shape,
    state::Tileception,
    theme::Colour,
    ui::{Align, Painter, Style, ease_out, glyph},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Lock,
    LogOut,
    Suspend,
    Restart,
    PowerOff,
}

impl PowerAction {
    pub const ALL: [PowerAction; 5] =
        [PowerAction::Lock, PowerAction::LogOut, PowerAction::Suspend, PowerAction::Restart, PowerAction::PowerOff];

    fn label(self) -> &'static str {
        match self {
            PowerAction::Lock => "Lock",
            PowerAction::LogOut => "Log out",
            PowerAction::Suspend => "Suspend",
            PowerAction::Restart => "Restart",
            PowerAction::PowerOff => "Power off",
        }
    }

    /// The key that picks this action directly. These follow the Windows
    /// Win+X "Shut down or sign out" menu (I, S, R, U); Windows has no lock
    /// entry there, so Lock keeps L as in Win+L.
    pub fn key(self) -> char {
        match self {
            PowerAction::Lock => 'l',
            PowerAction::LogOut => 'i',
            PowerAction::Suspend => 's',
            PowerAction::Restart => 'r',
            PowerAction::PowerOff => 'u',
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            PowerAction::Lock => glyph::LOCK,
            PowerAction::LogOut => glyph::LOG_OUT,
            PowerAction::Suspend => glyph::MOON,
            PowerAction::Restart => glyph::RESTART,
            PowerAction::PowerOff => glyph::POWER,
        }
    }
}

/// The open power menu and which button is highlighted.
#[derive(Debug, Clone, Copy)]
pub struct PowerMenu {
    pub selected: usize,
    opened: std::time::Instant,
}

impl Default for PowerMenu {
    fn default() -> Self {
        PowerMenu { selected: 0, opened: std::time::Instant::now() }
    }
}

const BUTTON: f64 = 112.0;
const GAP: f64 = 14.0;

impl Tileception {
    pub fn open_power_menu(&mut self) {
        if self.is_locked() {
            return;
        }
        self.launcher.open = false;
        self.power_menu = Some(PowerMenu::default());
        self.force_keyboard_focus_sync();
    }

    pub fn close_power_menu(&mut self) {
        if self.power_menu.take().is_some() {
            self.force_keyboard_focus_sync();
        }
    }

    /// Where each button sits, centred on the output.
    fn power_buttons(&self) -> Vec<(PowerAction, Rect)> {
        let out = self.output_rect();
        let n = PowerAction::ALL.len() as f64;
        let total = n * BUTTON + (n - 1.0) * GAP;
        let x0 = out.x + (out.w - total) / 2.0;
        let y = out.y + (out.h - BUTTON) / 2.0;
        PowerAction::ALL
            .iter()
            .enumerate()
            .map(|(i, a)| (*a, Rect::new(x0 + i as f64 * (BUTTON + GAP), y, BUTTON, BUTTON)))
            .collect()
    }

    pub fn power_action_at(&self, x: f64, y: f64) -> Option<usize> {
        self.power_buttons().iter().position(|(_, r)| r.contains(tileception_core::Point::new(x, y)))
    }

    /// Runs an action; the menu closes first.
    pub fn run_power_action(&mut self, action: PowerAction) {
        self.close_power_menu();
        match action {
            PowerAction::Lock => self.lock_session(false),
            PowerAction::LogOut => self.loop_signal.stop(),
            // Lock first, so the machine wakes up locked.
            PowerAction::Suspend => self.lock_session(true),
            PowerAction::Restart => self.spawn("systemctl", &["reboot".into()]),
            PowerAction::PowerOff => self.spawn("systemctl", &["poweroff".into()]),
        }
    }

    /// Handles a key while the menu is open. Every key press is taken.
    pub fn power_menu_key(&mut self, sym: u32) {
        use smithay::input::keyboard::keysyms;
        let Some(menu) = self.power_menu.as_mut() else { return };
        let n = PowerAction::ALL.len();
        match sym {
            keysyms::KEY_Escape => self.close_power_menu(),
            keysyms::KEY_Left | keysyms::KEY_ISO_Left_Tab => menu.selected = (menu.selected + n - 1) % n,
            keysyms::KEY_Right | keysyms::KEY_Tab => menu.selected = (menu.selected + 1) % n,
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => {
                let action = PowerAction::ALL[menu.selected];
                self.run_power_action(action);
            }
            other => {
                let ch = char::from_u32(other).map(|c| c.to_ascii_lowercase());
                if let Some(action) = PowerAction::ALL.iter().find(|a| Some(a.key()) == ch) {
                    self.run_power_action(*action);
                }
            }
        }
    }

    pub fn power_menu_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(menu) = self.power_menu else { return Vec::new() };
        let t = &self.theme;
        let o = self.output_rect();
        let mut p = Painter::new(self, renderer);
        let shown = ease_out(menu.opened.elapsed().as_secs_f64() / 0.16);
        p.alpha = shown as f32;
        p.fill(o, Colour([0.0, 0.0, 0.0, 0.62]), 0.0);
        p.offset = (0.0, (1.0 - shown) * 10.0);

        let buttons = self.power_buttons();
        let top = buttons.first().map_or(o.y, |(_, r)| r.y);
        let user = self.builtin_locker_user().unwrap_or("you");
        p.text(
            &format!("Goodbye for now, {user}"),
            Rect::new(o.x, top - 64.0, o.w, 32.0),
            Style::new(t.font_size * 1.5, t.text).weight(300).align(Align::Centre),
        );
        for (i, (action, rect)) in buttons.into_iter().enumerate() {
            let selected = i == menu.selected;
            let enabled = action != PowerAction::Suspend || self.config.file.general.suspend_enabled;
            if selected && enabled {
                p.shape(
                    Rect::new(rect.x, rect.y + 6.0, rect.w, rect.h),
                    Shape {
                        radius: t.popup_radius,
                        glow_radius: 28.0,
                        glow: Colour([0.0, 0.0, 0.0, 0.42]),
                        ..Default::default()
                    },
                );
                p.shape(
                    rect,
                    Shape {
                        radius: t.popup_radius,
                        fill: (t.accent, t.accent_end),
                        glow_radius: t.glow,
                        glow: t.accent.alpha(0.3),
                        ..Default::default()
                    },
                );
            } else {
                p.popup(rect);
            }
            let fg = if !enabled {
                t.muted.alpha(0.5)
            } else if selected {
                t.on_accent
            } else {
                t.text.alpha(0.85)
            };
            p.glyph(action.glyph(), Rect::new(rect.x, rect.y + 18.0, rect.w, 40.0), 28, fg);
            let label = if enabled { action.label() } else { "Suspend off" };
            let style = Style::new(t.font_size, fg).align(Align::Centre);
            p.text(
                label,
                Rect::new(rect.x + 6.0, rect.y + 66.0, rect.w - 12.0, 22.0),
                if selected { style.bold() } else { style },
            );
            // The letter that picks it, in the corner.
            let key = action.key().to_ascii_uppercase().to_string();
            let chip = Rect::new(rect.right() - 26.0, rect.y + 8.0, 18.0, 18.0);
            let (chip_fill, chip_text) =
                if selected && enabled { (t.on_accent.alpha(0.18), t.on_accent) } else { (t.surface_raised, t.muted) };
            p.fill(chip, chip_fill, 4.0);
            p.text(&key, chip, Style::new(t.font_size * 0.75, chip_text).bold().align(Align::Centre));
        }
        let hints = [("\u{2190}\u{2192}", "Choose"), ("Enter", "Confirm"), ("Esc", "Cancel")];
        let width = p.key_hints(&hints, 0.0, 0.0, true);
        let y = o.y + (o.h + BUTTON) / 2.0 + 40.0;
        p.key_hints(&hints, o.x + (o.w - width) / 2.0, y, false);
        p.finish()
    }
}
