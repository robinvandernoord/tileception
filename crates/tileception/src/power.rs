//! The built-in power menu (Super+X): lock, log out, suspend, restart and
//! power off, as a responsive grid of buttons over a dimmed screen. More...
//! holds suspending until a set time and restarting into the UEFI settings.

use smithay::backend::renderer::gles::GlesRenderer;
use tileception_core::Rect;

use crate::{
    render::OutputElement,
    shapes::Shape,
    state::Tileception,
    theme::Colour,
    ui::{Align, Painter, Style, ease_out, glyph},
    wake::{self, Typed},
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
            PowerAction::More => "More...",
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
    wake: wake::Support,
    /// The wake-up time as typed.
    wake_text: Typed,
    /// The whole text is selected: typing replaces it.
    wake_fresh: bool,
    wake_status: WakeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    More,
    ConfirmUefi,
    Wake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeStatus {
    Idle,
    /// The alarm is being set; suspend follows.
    Arming,
    Failed,
}

/// The rows of the More... page.
const MORE_WAKE: usize = 0;
const MORE_UEFI: usize = 1;
const MORE_BACK: usize = 2;

impl Default for PowerMenu {
    fn default() -> Self {
        PowerMenu {
            selected: 0,
            opened: std::time::Instant::now(),
            page: Page::Main,
            uefi_available: std::path::Path::new("/sys/firmware/efi").exists(),
            wake: wake::support(),
            wake_text: Typed::new("07:00"),
            wake_fresh: true,
            wake_status: WakeStatus::Idle,
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
        self.power_page(if menu.uefi_available { Page::ConfirmUefi } else { Page::More }, MORE_UEFI);
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
            let rows = if menu.page == Page::More { 3 } else { 2 };
            let width = if menu.page == Page::Wake { 440.0 } else { 360.0_f64 }.min((screen.w - 32.0).max(1.0));
            let left = screen.x + (screen.w - width) / 2.0;
            let top = screen.y + screen.h / 2.0 - (rows as f64 * 52.0 - 8.0) / 2.0;
            (0..rows).map(|row| Rect::new(left, top + row as f64 * 52.0, width, 44.0)).collect()
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
            (Page::More, MORE_WAKE) if self.wake_enabled() => self.power_page(Page::Wake, 1),
            (Page::More, MORE_UEFI) if menu.uefi_available => self.power_page(Page::ConfirmUefi, 0),
            (Page::More, MORE_BACK) => self.power_page(Page::Main, PowerAction::ALL.len()),
            (Page::ConfirmUefi, 0) => self.power_page(Page::More, MORE_UEFI),
            (Page::ConfirmUefi, 1) => self.run_power_action(PowerAction::Uefi),
            (Page::Wake, 1) => self.suspend_until(),
            _ => {}
        }
    }

    /// Suspending until a time is on, and set up on this computer.
    fn wake_enabled(&self) -> bool {
        self.config.file.general.suspend_enabled
            && self.power_menu.is_some_and(|menu| menu.wake == wake::Support::Ready)
    }

    /// The typed wake-up time, in seconds since 1970, with now.
    fn wake_time(menu: &PowerMenu) -> (Option<i64>, i64) {
        let now = wake::now();
        (wake::parse(menu.wake_text.as_str()).and_then(|typed| wake::resolve(typed, now)), now)
    }

    /// Sets the hardware clock's alarm, then locks and suspends.
    fn suspend_until(&mut self) {
        let Some(menu) = self.power_menu else { return };
        let (Some(when), _) = Self::wake_time(&menu) else { return };
        if menu.wake_status == WakeStatus::Arming {
            return;
        }
        self.set_wake_status(WakeStatus::Arming);
        self.arm_wake(when, move |state, result| {
            let waiting =
                state.power_menu.is_some_and(|menu| menu.page == Page::Wake && menu.wake_status == WakeStatus::Arming);
            match result {
                Ok(()) if waiting => {
                    tracing::info!("suspending until {when} (seconds since 1970)");
                    state.run_power_action(PowerAction::Suspend);
                }
                // Left before the alarm was set: it goes off on a computer
                // that is already awake.
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!("could not set the wake-up alarm: {error}");
                    if waiting {
                        state.set_wake_status(WakeStatus::Failed);
                    }
                }
            }
        });
    }

    fn set_wake_status(&mut self, status: WakeStatus) {
        if let Some(menu) = self.power_menu.as_mut() {
            menu.wake_status = status;
        }
        self.schedule_render();
    }

    /// A key on the wake-up page: arrows step the time, the rest is typed.
    fn wake_key(&mut self, sym: u32) {
        use smithay::input::keyboard::keysyms;
        let Some(menu) = self.power_menu.as_mut() else { return };
        if menu.wake_status == WakeStatus::Arming {
            return;
        }
        menu.wake_status = WakeStatus::Idle;
        let step = match sym {
            keysyms::KEY_Up | keysyms::KEY_KP_Up => Some(15),
            keysyms::KEY_Down | keysyms::KEY_KP_Down => Some(-15),
            keysyms::KEY_Page_Up | keysyms::KEY_KP_Page_Up => Some(60),
            keysyms::KEY_Page_Down | keysyms::KEY_KP_Page_Down => Some(-60),
            _ => None,
        };
        if let Some(step) = step {
            let (when, now) = Self::wake_time(menu);
            let (hour, minute) = when.and_then(wake::clock_at).or_else(|| wake::clock_at(now)).unwrap_or((7, 0));
            let minutes = (hour * 60 + minute) as i32;
            // Onto the step's grid first, so 07:10 goes to 07:15 or 07:00.
            let snapped =
                if step > 0 { minutes - minutes.rem_euclid(step) } else { minutes + (-minutes).rem_euclid(-step) };
            let next = (snapped + step).rem_euclid(24 * 60);
            menu.wake_text = Typed::new(&format!("{:02}:{:02}", next / 60, next % 60));
            menu.wake_fresh = true;
            return;
        }
        let character = match sym {
            keysyms::KEY_BackSpace | keysyms::KEY_Delete => {
                if std::mem::take(&mut menu.wake_fresh) {
                    menu.wake_text = Typed::new("");
                } else {
                    menu.wake_text.pop();
                }
                return;
            }
            keysyms::KEY_KP_0..=keysyms::KEY_KP_9 => char::from_digit(sym - keysyms::KEY_KP_0, 10),
            keysyms::KEY_KP_Decimal => Some('.'),
            keysyms::KEY_KP_Separator => Some(','),
            keysyms::KEY_KP_Add => Some('+'),
            keysyms::KEY_KP_Space => Some(' '),
            0x20..=0x7e => char::from_u32(sym),
            _ => None,
        };
        if let Some(character) = character {
            if std::mem::take(&mut menu.wake_fresh) {
                menu.wake_text = Typed::new("");
            }
            menu.wake_text.push(character);
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
        if page == Page::Wake {
            return match sym {
                keysyms::KEY_Escape => {
                    self.set_wake_status(WakeStatus::Idle);
                    self.power_page(Page::More, MORE_WAKE);
                }
                keysyms::KEY_Return | keysyms::KEY_KP_Enter => self.suspend_until(),
                _ => self.wake_key(sym),
            };
        }
        let columns = if page == Page::Main { self.power_columns() } else { 1 };
        let Some(menu) = self.power_menu.as_mut() else { return };
        let count = match page {
            Page::Main => PowerAction::ALL.len() + 1,
            Page::More => 3,
            Page::ConfirmUefi | Page::Wake => 2,
        };
        match sym {
            keysyms::KEY_Escape => match page {
                Page::Main => self.close_power_menu(),
                Page::More => self.power_page(Page::Main, PowerAction::ALL.len()),
                Page::ConfirmUefi | Page::Wake => self.power_page(Page::More, MORE_UEFI),
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
                } else if page == Page::More && letter == Some('s') {
                    self.activate_power_item(MORE_WAKE);
                } else if page == Page::More && letter == Some('u') {
                    self.activate_power_item(MORE_UEFI);
                } else if page == Page::Main
                    && let Some(action) = PowerAction::ALL.iter().find(|candidate| Some(candidate.key()) == letter)
                {
                    self.run_power_action(*action);
                }
            }
        }
    }

    /// The wake-up time being typed: selected while typing would replace it.
    fn draw_wake_field(&self, painter: &mut Painter, menu: &PowerMenu, rect: Rect) {
        let theme = &self.theme;
        painter.shape(
            rect,
            Shape {
                radius: theme.radius,
                border_width: 1.0,
                border: theme.accent,
                fill: (theme.popup(), theme.popup()),
                ..Default::default()
            },
        );
        let text = menu.wake_text.as_str();
        let inner = Rect::new(rect.x + 14.0, rect.y, rect.w - 28.0, rect.h);
        let style = Style::new(theme.font_size * 1.15, theme.text);
        if text.is_empty() {
            painter.fill(Rect::new(inner.x, rect.y + 12.0, 2.0, rect.h - 24.0), theme.accent, 1.0);
            painter.text(
                "7:30, 2300, 7am, +8h",
                Rect::new(inner.x + 8.0, rect.y, inner.w - 8.0, rect.h),
                style.colour(theme.muted),
            );
            return;
        }
        let (width, _) = painter.measure(text, inner.w, style);
        if menu.wake_fresh {
            painter.fill(
                Rect::new(inner.x - 3.0, rect.y + 9.0, width + 6.0, rect.h - 18.0),
                theme.accent.alpha(0.35),
                4.0,
            );
        }
        painter.text(text, inner, style);
        if !menu.wake_fresh {
            painter.fill(Rect::new(inner.x + width + 2.0, rect.y + 12.0, 2.0, rect.h - 24.0), theme.accent, 1.0);
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
            let heading = match menu.page {
                Page::More => "More...",
                Page::Wake => "Suspend until",
                _ => "Restart into UEFI settings?",
            };
            painter.text(
                heading,
                Rect::new(screen.x, rows[0].y - 64.0, screen.w, 32.0),
                Style::new(theme.font_size * 1.5, theme.text).weight(300).align(Align::Centre),
            );
            let labels: Vec<(String, bool)> = match menu.page {
                Page::More => {
                    let wake = match (self.config.file.general.suspend_enabled, menu.wake) {
                        (false, _) => "Suspend is off",
                        (true, wake::Support::Ready) => "Suspend until...",
                        (true, wake::Support::NoClock) => "Suspend until: no wake-up clock",
                        (true, wake::Support::NotInstalled) => "Suspend until: not installed",
                    };
                    let uefi = if menu.uefi_available { "UEFI" } else { "UEFI unavailable" };
                    vec![(wake.into(), self.wake_enabled()), (uefi.into(), menu.uefi_available), ("Back".into(), true)]
                }
                Page::Wake => {
                    let (when, now) = Self::wake_time(&menu);
                    let button = match (menu.wake_status, when) {
                        (WakeStatus::Arming, _) => ("Setting the alarm...".into(), false),
                        (WakeStatus::Failed, _) => ("Could not set the alarm, see the log".into(), false),
                        (WakeStatus::Idle, Some(when)) => (format!("Suspend, {}", wake::describe(when, now)), true),
                        (WakeStatus::Idle, None) if wake::parse(menu.wake_text.as_str()).is_some() => {
                            ("More than a day away".into(), false)
                        }
                        (WakeStatus::Idle, None) => ("Try 7:30, 2300, 7am or +8h".into(), false),
                    };
                    vec![(String::new(), true), button]
                }
                _ => vec![("Cancel".into(), true), ("Restart into UEFI".into(), true)],
            };
            for (index, (rect, (label, enabled))) in rows.iter().zip(&labels).enumerate() {
                if menu.page == Page::Wake && index == 0 {
                    self.draw_wake_field(&mut painter, &menu, *rect);
                    continue;
                }
                let active = index == menu.selected && *enabled;
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
                painter.text(label, *rect, Style::new(theme.font_size, colour).align(Align::Centre));
            }
            let hints: &[(&str, &str)] = if menu.page == Page::Wake {
                &[("\u{2191}\u{2193}", "15 min"), ("Enter", "Suspend"), ("Esc", "Back")]
            } else {
                &[("\u{2191}\u{2193}", "Choose"), ("Enter", "Confirm"), ("Esc", "Back")]
            };
            let width = painter.key_hints(hints, 0.0, 0.0, true);
            let last = rows.last().map_or(screen.bottom(), Rect::bottom);
            painter.key_hints(hints, screen.x + (screen.w - width) / 2.0, last + 40.0, false);
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
