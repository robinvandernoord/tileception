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
    More,
    Uefi,
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
            PowerAction::More => "More…",
            PowerAction::Uefi => "UEFI",
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
            PowerAction::More => 'm',
            PowerAction::Uefi => 'u',
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            PowerAction::Lock => glyph::LOCK,
            PowerAction::LogOut => glyph::LOG_OUT,
            PowerAction::Suspend => glyph::MOON,
            PowerAction::Restart => glyph::RESTART,
            PowerAction::PowerOff => glyph::POWER,
            PowerAction::More => glyph::CHEVRON_RIGHT,
            PowerAction::Uefi => glyph::RESTART,
        }
    }
}

/// The open power menu and which button is highlighted.
#[derive(Debug, Clone, Copy)]
pub struct PowerMenu {
    pub selected: usize,
    opened: std::time::Instant,
    page: Page,
    uefi_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    More,
    ConfirmUefi,
}

impl Default for PowerMenu {
    fn default() -> Self {
        PowerMenu {
            selected: 0,
            opened: std::time::Instant::now(),
            page: Page::Main,
            uefi_available: std::path::Path::new("/sys/firmware/efi").exists(),
        }
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

    /// A search result opens confirmation directly, never executing the reboot.
    pub fn confirm_uefi_restart(&mut self) {
        self.open_power_menu();
        let Some(menu) = self.power_menu else { return };
        self.power_page(if menu.uefi_available { Page::ConfirmUefi } else { Page::More }, 0);
    }

    fn power_columns(&self) -> usize {
        (((self.output_rect().w - 32.0 + GAP) / (BUTTON + GAP)).floor() as usize).clamp(1, PowerAction::ALL.len() + 1)
    }

    /// Where each button sits, with each row centred on the output.
    fn power_buttons(&self) -> Vec<(PowerAction, Rect)> {
        let out = self.output_rect();
        let count = PowerAction::ALL.len() + 1;
        let columns = self.power_columns();
        let rows = count.div_ceil(columns);
        let width = BUTTON.min((out.w - 32.0).max(1.0));
        let height = BUTTON.min(((out.h - 160.0).max(1.0) - (rows - 1) as f64 * GAP) / rows as f64).max(72.0);
        let total_h = rows as f64 * height + (rows - 1) as f64 * GAP;
        let top = out.y + (out.h - total_h) / 2.0;
        PowerAction::ALL
            .iter()
            .copied()
            .chain(std::iter::once(PowerAction::More))
            .enumerate()
            .map(|(index, action)| {
                let row = index / columns;
                let in_row = (count - row * columns).min(columns);
                let total_w = in_row as f64 * width + (in_row - 1) as f64 * GAP;
                let left = out.x + (out.w - total_w) / 2.0 + (index % columns) as f64 * (width + GAP);
                (action, Rect::new(left, top + row as f64 * (height + GAP), width, height))
            })
            .collect()
    }

    pub fn power_action_at(&self, x_pos: f64, y_pos: f64) -> Option<usize> {
        self.power_items().iter().position(|rect| rect.contains(tileception_core::Point::new(x_pos, y_pos)))
    }

    /// The six main tiles, or compact submenu rows.
    fn power_items(&self) -> Vec<Rect> {
        let Some(menu) = self.power_menu else { return Vec::new() };
        let screen = self.output_rect();
        if menu.page == Page::Main {
            self.power_buttons().into_iter().map(|(_, rect)| rect).collect()
        } else {
            let width = 360.0_f64.min((screen.w - 32.0).max(1.0));
            let left = screen.x + (screen.w - width) / 2.0;
            let top = screen.y + screen.h / 2.0 - 46.0;
            vec![Rect::new(left, top, width, 44.0), Rect::new(left, top + 52.0, width, 44.0)]
        }
    }

    fn power_page(&mut self, page: Page, selected: usize) {
        if let Some(menu) = self.power_menu.as_mut() {
            menu.page = page;
            menu.selected = selected;
        }
    }

    pub fn activate_power_item(&mut self, index: usize) {
        let Some(menu) = self.power_menu else { return };
        match (menu.page, index) {
            (Page::Main, index) if index < PowerAction::ALL.len() => self.run_power_action(PowerAction::ALL[index]),
            (Page::Main, index) if index == PowerAction::ALL.len() => self.power_page(Page::More, 0),
            (Page::More, 0) if menu.uefi_available => self.power_page(Page::ConfirmUefi, 0),
            (Page::More, 1) => self.power_page(Page::Main, PowerAction::ALL.len()),
            (Page::ConfirmUefi, 0) => self.power_page(Page::More, 0),
            (Page::ConfirmUefi, 1) => self.run_power_action(PowerAction::Uefi),
            _ => {}
        }
    }

    /// Runs an action; the menu closes first.
    pub fn run_power_action(&mut self, action: PowerAction) {
        if action != PowerAction::More {
            self.close_power_menu();
        }
        match action {
            PowerAction::Lock => self.lock_session(false),
            PowerAction::LogOut => self.loop_signal.stop(),
            // Lock first, so the machine wakes up locked.
            PowerAction::Suspend => self.lock_session(true),
            PowerAction::Restart => self.spawn("systemctl", &["reboot".into()]),
            PowerAction::PowerOff => self.spawn("systemctl", &["poweroff".into()]),
            PowerAction::More => self.power_page(Page::More, 0),
            PowerAction::Uefi => self.spawn("systemctl", &["reboot".into(), "--firmware-setup".into()]),
        }
    }

    /// Handles a key while the menu is open. Every key press is taken.
    pub fn power_menu_key(&mut self, sym: u32) {
        use smithay::input::keyboard::keysyms;
        let Some(page) = self.power_menu.map(|menu| menu.page) else { return };
        let columns = if page == Page::Main { self.power_columns() } else { 1 };
        let Some(menu) = self.power_menu.as_mut() else { return };
        let count = if page == Page::Main { PowerAction::ALL.len() + 1 } else { 2 };
        match sym {
            keysyms::KEY_Escape => match page {
                Page::Main => self.close_power_menu(),
                Page::More => self.power_page(Page::Main, PowerAction::ALL.len()),
                Page::ConfirmUefi => self.power_page(Page::More, 0),
            },
            keysyms::KEY_Left | keysyms::KEY_ISO_Left_Tab => menu.selected = (menu.selected + count - 1) % count,
            keysyms::KEY_Right | keysyms::KEY_Tab => menu.selected = (menu.selected + 1) % count,
            keysyms::KEY_Up => menu.selected = menu.selected.saturating_sub(columns),
            keysyms::KEY_Down => menu.selected = (menu.selected + columns).min(count - 1),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => {
                let selected = menu.selected;
                self.activate_power_item(selected);
            }
            other => {
                let letter = char::from_u32(other).map(|character| character.to_ascii_lowercase());
                if page == Page::Main && letter == Some('m') {
                    self.power_page(Page::More, 0);
                } else if page == Page::More && letter == Some('u') {
                    self.activate_power_item(0);
                } else if page == Page::Main
                    && let Some(action) = PowerAction::ALL.iter().find(|candidate| Some(candidate.key()) == letter)
                {
                    self.run_power_action(*action);
                }
            }
        }
    }

    pub fn power_menu_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(menu) = self.power_menu else { return Vec::new() };
        let theme = &self.theme;
        let screen = self.output_rect();
        let mut painter = Painter::new(self, renderer);
        let shown = ease_out(menu.opened.elapsed().as_secs_f64() / 0.16);
        painter.alpha = shown as f32;
        painter.fill(screen, Colour([0.0, 0.0, 0.0, 0.62]), 0.0);
        painter.offset = (0.0, (1.0 - shown) * 10.0);

        if menu.page != Page::Main {
            let rows = self.power_items();
            let heading = if menu.page == Page::More { "More…" } else { "Restart into UEFI settings?" };
            painter.text(
                heading,
                Rect::new(screen.x, rows[0].y - 64.0, screen.w, 32.0),
                Style::new(theme.font_size * 1.5, theme.text).weight(300).align(Align::Centre),
            );
            let labels = if menu.page == Page::More {
                [if menu.uefi_available { "UEFI" } else { "UEFI unavailable" }, "Back"]
            } else {
                ["Cancel", "Restart into UEFI"]
            };
            for (index, rect) in rows.iter().enumerate() {
                let enabled = menu.page != Page::More || index != 0 || menu.uefi_available;
                let active = index == menu.selected && enabled;
                painter.shape(
                    *rect,
                    Shape {
                        radius: theme.radius,
                        border_width: 1.0,
                        border: if active { theme.accent } else { theme.border },
                        fill: if active { (theme.accent, theme.accent_end) } else { (theme.popup(), theme.popup()) },
                        ..Default::default()
                    },
                );
                let colour = if !enabled {
                    theme.muted
                } else if active {
                    theme.on_accent
                } else {
                    theme.text
                };
                painter.text(labels[index], *rect, Style::new(theme.font_size, colour).align(Align::Centre));
            }
            let hints = [("↑↓", "Choose"), ("Enter", "Confirm"), ("Esc", "Back")];
            let width = painter.key_hints(&hints, 0.0, 0.0, true);
            painter.key_hints(&hints, screen.x + (screen.w - width) / 2.0, rows[1].bottom() + 40.0, false);
            return painter.finish();
        }

        let buttons = self.power_buttons();
        let top = buttons.first().map_or(screen.y, |(_, rect)| rect.y);
        let bottom = buttons.last().map_or(screen.bottom(), |(_, rect)| rect.bottom());
        let user = self.builtin_locker_user().unwrap_or("you");
        painter.text(
            &format!("Goodbye for now, {user}"),
            Rect::new(screen.x, top - 64.0, screen.w, 32.0),
            Style::new(theme.font_size * 1.5, theme.text).weight(300).align(Align::Centre),
        );
        for (index, (action, rect)) in buttons.into_iter().enumerate() {
            let selected = index == menu.selected;
            let enabled = action != PowerAction::Suspend || self.config.file.general.suspend_enabled;
            let active = selected && enabled;
            painter.shape(
                rect,
                Shape {
                    radius: theme.radius,
                    border_width: 1.0,
                    border: if active { theme.accent.alpha(0.7) } else { theme.border },
                    fill: if active { (theme.accent, theme.accent_end) } else { (theme.popup(), theme.popup()) },
                    glow_radius: if active { theme.glow * 0.5 } else { 0.0 },
                    glow: theme.accent.alpha(0.16),
                    ..Default::default()
                },
            );
            let foreground = if !enabled {
                theme.muted.alpha(0.5)
            } else if selected {
                theme.on_accent
            } else {
                theme.text.alpha(0.85)
            };
            painter.glyph(
                action.glyph(),
                Rect::new(rect.x, rect.y + rect.h * 0.18, rect.w, rect.h * 0.36),
                28,
                foreground,
            );
            let label = if enabled { action.label() } else { "Suspend off" };
            let style = Style::new(theme.font_size, foreground).align(Align::Centre);
            painter.text(
                label,
                Rect::new(rect.x + 6.0, rect.y + rect.h * 0.62, rect.w - 12.0, 22.0),
                if active { style.bold() } else { style },
            );
            // The letter that picks it, in the corner.
            let key = action.key().to_ascii_uppercase().to_string();
            let chip = Rect::new(rect.right() - 26.0, rect.y + 8.0, 18.0, 18.0);
            let (chip_fill, chip_text) = if selected && enabled {
                (theme.on_accent.alpha(0.18), theme.on_accent)
            } else {
                (theme.surface_raised, theme.muted)
            };
            painter.fill(chip, chip_fill, 4.0);
            painter.text(&key, chip, Style::new(theme.font_size * 0.75, chip_text).bold().align(Align::Centre));
        }
        let arrows = if self.power_columns() < PowerAction::ALL.len() + 1 {
            "\u{2190}\u{2191}\u{2193}\u{2192}"
        } else {
            "\u{2190}\u{2192}"
        };
        let hints = [(arrows, "Choose"), ("Enter", "Confirm"), ("Esc", "Cancel")];
        let width = painter.key_hints(&hints, 0.0, 0.0, true);
        let hints_y = bottom + 40.0;
        painter.key_hints(&hints, screen.x + (screen.w - width) / 2.0, hints_y, false);
        painter.finish()
    }
}
