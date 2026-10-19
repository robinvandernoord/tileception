//! The built-in power menu (Super+X): lock, log out, suspend, restart and
//! power off, as a responsive grid of buttons over a dimmed screen.

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
const GAP: f64 = 12.0;

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

    fn power_columns(&self) -> usize {
        (((self.output_rect().w - 32.0 + GAP) / (BUTTON + GAP)).floor() as usize).clamp(1, PowerAction::ALL.len())
    }

    /// Where each button sits, with each row centred on the output.
    fn power_buttons(&self) -> Vec<(PowerAction, Rect)> {
        let out = self.output_rect();
        let n = PowerAction::ALL.len();
        let columns = self.power_columns();
        let rows = n.div_ceil(columns);
        let width = BUTTON.min((out.w - 32.0).max(1.0));
        let height = BUTTON.min(((out.h - 160.0).max(1.0) - (rows - 1) as f64 * GAP) / rows as f64).max(72.0);
        let total_h = rows as f64 * height + (rows - 1) as f64 * GAP;
        let y = out.y + (out.h - total_h) / 2.0;
        PowerAction::ALL
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let row = i / columns;
                let in_row = (n - row * columns).min(columns);
                let total_w = in_row as f64 * width + (in_row - 1) as f64 * GAP;
                let x = out.x + (out.w - total_w) / 2.0 + (i % columns) as f64 * (width + GAP);
                (*a, Rect::new(x, y + row as f64 * (height + GAP), width, height))
            })
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
        let columns = self.power_columns();
        let Some(menu) = self.power_menu.as_mut() else { return };
        let n = PowerAction::ALL.len();
        match sym {
            keysyms::KEY_Escape => self.close_power_menu(),
            keysyms::KEY_Left | keysyms::KEY_ISO_Left_Tab => menu.selected = (menu.selected + n - 1) % n,
            keysyms::KEY_Right | keysyms::KEY_Tab => menu.selected = (menu.selected + 1) % n,
            keysyms::KEY_Up => menu.selected = menu.selected.saturating_sub(columns),
            keysyms::KEY_Down => menu.selected = (menu.selected + columns).min(n - 1),
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
        let bottom = buttons.last().map_or(o.bottom(), |(_, r)| r.bottom());
        let user = self.builtin_locker_user().unwrap_or("you");
        p.text(
            &format!("Goodbye for now, {user}"),
            Rect::new(o.x, top - 64.0, o.w, 32.0),
            Style::new(t.font_size * 1.5, t.text).weight(300).align(Align::Centre),
        );
        for (i, (action, rect)) in buttons.into_iter().enumerate() {
            let selected = i == menu.selected;
            let enabled = action != PowerAction::Suspend || self.config.file.general.suspend_enabled;
            let active = selected && enabled;
            p.shape(
                rect,
                Shape {
                    radius: t.radius,
                    border_width: 1.0,
                    border: if active { t.accent.alpha(0.7) } else { t.border },
                    fill: if active { (t.accent, t.accent_end) } else { (t.popup(), t.popup()) },
                    glow_radius: if active { t.glow * 0.5 } else { 0.0 },
                    glow: t.accent.alpha(0.16),
                    ..Default::default()
                },
            );
            let fg = if !enabled {
                t.muted.alpha(0.5)
            } else if selected {
                t.on_accent
            } else {
                t.text.alpha(0.85)
            };
            p.glyph(action.glyph(), Rect::new(rect.x, rect.y + rect.h * 0.18, rect.w, rect.h * 0.36), 28, fg);
            let label = if enabled { action.label() } else { "Suspend off" };
            let style = Style::new(t.font_size, fg).align(Align::Centre);
            p.text(
                label,
                Rect::new(rect.x + 6.0, rect.y + rect.h * 0.62, rect.w - 12.0, 22.0),
                if active { style.bold() } else { style },
            );
            // The letter that picks it, in the corner.
            let key = action.key().to_ascii_uppercase().to_string();
            let chip = Rect::new(rect.right() - 26.0, rect.y + 8.0, 18.0, 18.0);
            let (chip_fill, chip_text) =
                if selected && enabled { (t.on_accent.alpha(0.18), t.on_accent) } else { (t.surface_raised, t.muted) };
            p.fill(chip, chip_fill, 4.0);
            p.text(&key, chip, Style::new(t.font_size * 0.75, chip_text).bold().align(Align::Centre));
        }
        let arrows = if self.power_columns() < PowerAction::ALL.len() {
            "\u{2190}\u{2191}\u{2193}\u{2192}"
        } else {
            "\u{2190}\u{2192}"
        };
        let hints = [(arrows, "Choose"), ("Enter", "Confirm"), ("Esc", "Cancel")];
        let width = p.key_hints(&hints, 0.0, 0.0, true);
        let y = bottom + 40.0;
        p.key_hints(&hints, o.x + (o.w - width) / 2.0, y, false);
        p.finish()
    }
}
