//! The password prompt for a task `sudo` wants a password for: a small
//! dialog over a dimmed screen that takes the keyboard until answered.

use smithay::{backend::renderer::gles::GlesRenderer, input::keyboard::keysyms};
use tileception_core::{Point, Rect};

use super::Task;
use crate::{
    Tileception,
    config::Mods,
    render::OutputElement,
    shapes::Shape,
    text_input::SecretField,
    theme::Colour,
    ui::{Align, Painter, PointerTarget, Style, glyph},
};

pub(crate) struct Prompt {
    task: Task,
    secret: SecretField,
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Cancel,
    Submit,
}

impl crate::ui::PointerAction for Hit {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: Point) {
        match self {
            Hit::Cancel => state.close_updates_prompt(),
            Hit::Submit => state.submit_updates_prompt(),
        }
    }
}

const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 206.0;

impl Tileception {
    pub(super) fn open_updates_prompt(&mut self, task: Task, error: Option<String>) {
        if self.is_locked() {
            return;
        }
        self.close_updates_popover();
        self.close_launcher();
        self.power_menu = None;
        self.updates.prompt = Some(Prompt { task, secret: SecretField::appending(), error });
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    pub(crate) fn close_updates_prompt(&mut self) {
        if self.updates.prompt.take().is_some() {
            self.force_keyboard_focus_sync();
            self.schedule_render();
        }
    }

    fn submit_updates_prompt(&mut self) {
        let Some(mut prompt) = self.updates.prompt.take() else { return };
        if prompt.secret.is_empty() {
            prompt.error = Some("Type your password.".into());
            self.updates.prompt = Some(prompt);
            self.schedule_render();
            return;
        }
        let password = prompt.secret.take();
        self.force_keyboard_focus_sync();
        if let Err(error) = self.updates_run(prompt.task, Some(password)) {
            tracing::warn!("{error}");
        }
        self.schedule_render();
    }

    pub(crate) fn updates_prompt_key(&mut self, sym: u32, mods: Mods) {
        let Some(prompt) = self.updates.prompt.as_mut() else { return };
        match sym {
            keysyms::KEY_Escape => return self.close_updates_prompt(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => return self.submit_updates_prompt(),
            // The field shows no caret: its keys work at the end. A paste
            // it asks for is not taken here.
            _ => {
                prompt.secret.key(sym, mods);
            }
        }
        prompt.error = None;
        self.schedule_render();
    }

    fn updates_prompt_card(&self) -> Rect {
        let out = self.output_rect();
        let width = WIDTH.min(out.w - 32.0).max(1.0);
        Rect::new(out.x + (out.w - width) / 2.0, out.y + (out.h - HEIGHT) / 2.0, width, HEIGHT)
    }

    fn updates_prompt_controls(&self) -> [(Rect, Hit); 2] {
        let card = self.updates_prompt_card();
        let top = card.bottom() - 48.0;
        [
            (Rect::new(card.right() - 228.0, top, 100.0, 32.0), Hit::Cancel),
            (Rect::new(card.right() - 120.0, top, 104.0, 32.0), Hit::Submit),
        ]
    }

    pub(crate) fn updates_prompt_pointer_target(&self, point: Point) -> Option<PointerTarget> {
        self.updates_prompt_controls()
            .into_iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(rect, hit)| PointerTarget::button(rect, hit))
    }

    pub(crate) fn updates_prompt_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(prompt) = &self.updates.prompt else { return Vec::new() };
        let theme = &self.theme;
        let card = self.updates_prompt_card();
        let mut painter = Painter::new(self, renderer);
        painter.fill(self.output_rect(), Colour([0.0, 0.0, 0.0, 0.5]), 0.0);
        painter.popup(card);
        let inner = Rect::new(card.x + 20.0, card.y, card.w - 40.0, card.h);
        painter.glyph(glyph::LOCK, Rect::new(inner.x, card.y + 18.0, 20.0, 24.0), 18, theme.accent);
        painter.text(
            prompt.task.title(),
            Rect::new(inner.x + 30.0, card.y + 18.0, inner.w - 30.0, 24.0),
            Style::new(theme.font_size * 1.1, theme.text).bold(),
        );
        let user = std::env::var("USER").unwrap_or_default();
        let ask = if user.is_empty() { "Your password".to_string() } else { format!("Password for {user}") };
        painter.text(
            &ask,
            Rect::new(inner.x, card.y + 52.0, inner.w, 20.0),
            Style::new(theme.font_size * 0.9, theme.muted),
        );
        let field = Rect::new(inner.x, card.y + 78.0, inner.w, 36.0);
        painter.shape(
            field,
            Shape {
                border_width: 1.0,
                border: if prompt.error.is_some() { theme.danger } else { theme.accent.alpha(0.6) },
                glow_radius: theme.glow * 0.5,
                glow: theme.accent.alpha(0.18),
                ..Shape::filled(theme.surface, theme.radius)
            },
        );
        let mask = "\u{2022}".repeat(prompt.secret.chars().min(48));
        let shown = crate::text::Field {
            cursor: mask.len(),
            anchor: prompt.secret.is_selected().then_some(0),
            visible: true,
            accent: theme.accent.straight_rgba8(),
        };
        let image = self.text.borrow_mut().field(&mask, &theme.font, 14.0, theme.text, (field.w - 24.0) as f32, shown);
        if let Some(image) = image {
            painter.image(&image.buffer, (field.x + 12.0, field.y + (field.h - image.height) / 2.0));
        }
        let note = prompt.error.as_deref().unwrap_or("Passed to sudo for this task only; it is not kept.");
        painter.text(
            note,
            Rect::new(inner.x, card.y + 120.0, inner.w, 20.0),
            Style::new(theme.font_size * 0.82, if prompt.error.is_some() { theme.danger } else { theme.muted }),
        );
        for (rect, hit) in self.updates_prompt_controls() {
            let primary = hit == Hit::Submit;
            let shape = if primary {
                Shape { fill: (theme.accent, theme.accent_end), ..Shape::filled(theme.accent, theme.radius) }
            } else {
                Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface_raised, theme.radius) }
            };
            painter.shape(rect, shape);
            let label = if primary { "Continue" } else { "Cancel" };
            let colour = if primary { theme.on_accent } else { theme.text };
            painter.text(label, rect, Style::new(theme.font_size * 0.92, colour).bold().align(Align::Centre));
        }
        painter.finish()
    }
}
