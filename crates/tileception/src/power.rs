//! The built-in power menu (Super+X): lock, log out, suspend, restart and
//! power off, as a row of buttons over a dimmed screen.

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer, renderer::gles::GlesRenderer},
    utils::Transform,
};
use tileception_core::Rect;

use crate::{
    render::{push_image, push_shape, OutputElement},
    shapes::{Shape, Shapes},
    state::Tileception,
    theme::Colour,
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

    /// The key that picks this action directly.
    pub fn key(self) -> char {
        match self {
            PowerAction::Lock => 'l',
            PowerAction::LogOut => 'o',
            PowerAction::Suspend => 's',
            PowerAction::Restart => 'r',
            PowerAction::PowerOff => 'p',
        }
    }

    /// SVG path data on a 24 by 24 grid, drawn as strokes.
    fn icon(self) -> &'static str {
        match self {
            PowerAction::Lock => {
                r#"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>"#
            }
            PowerAction::LogOut => r#"<path d="M16 17l5-5-5-5M21 12H9M12 21H5V3h7"/>"#,
            PowerAction::Suspend => r#"<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/>"#,
            PowerAction::Restart => r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5"/>"#,
            PowerAction::PowerOff => r#"<path d="M12 2v10M18.4 6.6a9 9 0 1 1-12.8 0"/>"#,
        }
    }
}

/// The open power menu and which button is highlighted.
#[derive(Debug, Clone, Copy, Default)]
pub struct PowerMenu {
    pub selected: usize,
}

const BUTTON: f64 = 112.0;
const GAP: f64 = 14.0;

/// Draws an icon at `size` pixels in one colour.
fn icon_image(action: PowerAction, size: u32, colour: Colour) -> Option<MemoryRenderBuffer> {
    let [r, g, b, a] = colour.straight_rgba8();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="rgb({r},{g},{b})" stroke-opacity="{}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
        a as f32 / 255.0,
        action.icon()
    );
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / 24.0;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia pixels are premultiplied RGBA, which is what the renderer wants.
    Some(MemoryRenderBuffer::from_slice(
        pixmap.data(),
        Fourcc::Abgr8888,
        (size as i32, size as i32),
        1,
        Transform::Normal,
        None,
    ))
}

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

    pub fn power_menu_elements(&self, renderer: &mut GlesRenderer, shapes: Option<&Shapes>) -> Vec<OutputElement> {
        let Some(menu) = self.power_menu else { return Vec::new() };
        let t = &self.theme;
        let mut out = Vec::new();
        for (i, (action, rect)) in self.power_buttons().into_iter().enumerate() {
            let selected = i == menu.selected;
            let enabled = action != PowerAction::Suspend || self.config.file.general.suspend_enabled;
            let fg = if !enabled {
                t.muted.alpha(0.5)
            } else if selected {
                t.on_accent
            } else {
                t.muted
            };
            let mut text = self.text.borrow_mut();
            let label = if enabled { action.label() } else { "Suspend disabled" };
            if let Some(label) = text.line(label, &t.font, t.font_size, fg, BUTTON as f32 - 12.0, selected) {
                let x = rect.x + (rect.w - label.width as f64) / 2.0;
                push_image(&mut out, renderer, &label.buffer, (x, rect.y + rect.h * 0.64));
            }
            drop(text);
            if let Some(icon) = icon_image(action, 28, fg) {
                push_image(&mut out, renderer, &icon, (rect.x + (rect.w - 28.0) / 2.0, rect.y + rect.h * 0.28));
            }
            let shape = if selected && enabled {
                Shape {
                    radius: t.popup_radius,
                    fill: (t.accent, t.accent_end),
                    glow_radius: t.glow,
                    glow: t.accent.alpha(0.3),
                    ..Default::default()
                }
            } else {
                Shape {
                    radius: t.popup_radius,
                    fill: (t.surface_raised, t.surface_raised),
                    border_width: 1.0,
                    border: t.border,
                    ..Default::default()
                }
            };
            push_shape(&mut out, shapes, &rect, shape);
        }
        let hint = "Arrows and Enter, or the first letter; Esc to close";
        let mut text = self.text.borrow_mut();
        if let Some(img) = text.line(hint, &t.font, t.font_size * 0.9, t.muted, 600.0, false) {
            let o = self.output_rect();
            let y = o.y + (o.h + BUTTON) / 2.0 + 24.0;
            push_image(&mut out, renderer, &img.buffer, (o.x + (o.w - img.width as f64) / 2.0, y));
        }
        drop(text);
        push_shape(&mut out, shapes, &self.output_rect(), Shape::filled(Colour([0.0, 0.0, 0.0, 0.55]), 0.0));
        out
    }
}
