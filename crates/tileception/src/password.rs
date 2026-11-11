//! The password prompt: a small dialog over a dimmed screen that takes the
//! keyboard until answered. Polkit asks through it when a program wants an
//! administrator (Tileception is the session's authentication agent), and
//! so does the update manager where it falls back to `sudo`.

use smithay::{backend::renderer::gles::GlesRenderer, input::keyboard::keysyms};
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    config::Mods,
    render::OutputElement,
    shapes::Shape,
    text_input::SecretField,
    theme::Colour,
    ui::{Button, Painter, PointerTarget, Style, glyph},
    updates::Task,
};

/// Who asked for the password, and gets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// `sudo`, for one task of the update manager.
    Sudo(Task),
    /// Polkit, for the authentication with this cookie.
    Polkit(String),
}

pub(crate) struct Prompt {
    purpose: Purpose,
    title: String,
    /// What the password is for, in the asker's words.
    message: String,
    /// The account whose password is asked.
    user: String,
    secret: SecretField,
    error: Option<String>,
    /// The password was handed over and is being checked.
    checking: bool,
}

impl Prompt {
    pub(crate) fn new(purpose: Purpose, title: &str, message: &str, user: &str) -> Prompt {
        Prompt {
            purpose,
            title: title.to_string(),
            message: message.to_string(),
            user: user.to_string(),
            secret: SecretField::appending(),
            error: None,
            checking: false,
        }
    }

    pub(crate) fn with_error(mut self, error: Option<String>) -> Prompt {
        self.error = error;
        self
    }

    pub(crate) fn purpose(&self) -> &Purpose {
        &self.purpose
    }

    /// The password did not work: ask again.
    pub(crate) fn retry(&mut self, error: String) {
        self.error = Some(error);
        self.checking = false;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Cancel,
    Submit,
}

impl crate::ui::PointerAction for Hit {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: Point) {
        match self {
            Hit::Cancel => state.cancel_password_prompt(),
            Hit::Submit => state.submit_password_prompt(),
        }
    }
}

const WIDTH: f64 = 420.0;

impl Tileception {
    pub(crate) fn open_password_prompt(&mut self, prompt: Prompt) {
        if self.is_locked() {
            // Nobody may be asked behind the lock: the asker hears no.
            return self.password_prompt_answered(prompt.purpose, None);
        }
        self.close_updates_popover();
        self.close_launcher();
        self.power_menu = None;
        self.password_prompt = Some(prompt);
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    /// Takes the prompt away without telling who asked.
    pub(crate) fn close_password_prompt(&mut self) {
        if self.password_prompt.take().is_some() {
            self.force_keyboard_focus_sync();
            self.schedule_render();
        }
    }

    /// The person said no.
    pub(crate) fn cancel_password_prompt(&mut self) {
        let Some(prompt) = self.password_prompt.take() else { return };
        self.force_keyboard_focus_sync();
        self.schedule_render();
        self.password_prompt_answered(prompt.purpose, None);
    }

    fn submit_password_prompt(&mut self) {
        let Some(prompt) = self.password_prompt.as_mut() else { return };
        if prompt.checking {
            return;
        }
        if prompt.secret.is_empty() {
            prompt.error = Some("Type your password.".into());
            return self.schedule_render();
        }
        let password = prompt.secret.take();
        let purpose = prompt.purpose.clone();
        match purpose {
            // Polkit says whether it worked: the prompt waits for that.
            Purpose::Polkit(_) => {
                prompt.checking = true;
                prompt.error = None;
            }
            Purpose::Sudo(_) => {
                self.password_prompt = None;
                self.force_keyboard_focus_sync();
            }
        }
        self.schedule_render();
        self.password_prompt_answered(purpose, Some(password));
    }

    /// Hands the answer, a password or a refusal, to who asked.
    fn password_prompt_answered(&mut self, purpose: Purpose, password: Option<zeroize::Zeroizing<String>>) {
        match (purpose, password) {
            (Purpose::Sudo(task), Some(password)) => {
                if let Err(error) = self.updates_run(task, Some(password)) {
                    tracing::warn!("{error}");
                }
            }
            (Purpose::Sudo(_), None) => {}
            (Purpose::Polkit(cookie), password) => self.polkit_answer(&cookie, password),
        }
    }

    pub(crate) fn password_prompt_key(&mut self, sym: u32, mods: Mods) {
        let Some(prompt) = self.password_prompt.as_mut() else { return };
        match sym {
            keysyms::KEY_Escape => return self.cancel_password_prompt(),
            keysyms::KEY_Return | keysyms::KEY_KP_Enter => return self.submit_password_prompt(),
            _ if prompt.checking => return,
            // The field shows no caret: its keys work at the end. A paste
            // it asks for is not taken here.
            _ => {
                prompt.secret.key(sym, mods);
            }
        }
        prompt.error = None;
        self.schedule_render();
    }

    /// The lines of the asker's message, wrapped to the card.
    fn password_prompt_message(&self) -> Vec<String> {
        let Some(prompt) = &self.password_prompt else { return Vec::new() };
        let columns = ((WIDTH - 40.0) / (f64::from(self.theme.font_size) * 0.5)).max(20.0) as usize;
        wrap(&prompt.message, columns, 3)
    }

    fn password_prompt_card(&self) -> Rect {
        let out = self.output_rect();
        let width = WIDTH.min(out.w - 32.0).max(1.0);
        let height = 206.0 + self.password_prompt_message().len() as f64 * 20.0;
        Rect::new(out.x + (out.w - width) / 2.0, out.y + (out.h - height) / 2.0, width, height)
    }

    fn password_prompt_controls(&self) -> [(Rect, Hit); 2] {
        let card = self.password_prompt_card();
        let top = card.bottom() - 48.0;
        [
            (Rect::new(card.right() - 228.0, top, 100.0, 32.0), Hit::Cancel),
            (Rect::new(card.right() - 120.0, top, 104.0, 32.0), Hit::Submit),
        ]
    }

    pub(crate) fn password_prompt_pointer_target(&self, point: Point) -> Option<PointerTarget> {
        self.password_prompt_controls()
            .into_iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(rect, hit)| PointerTarget::button(rect, hit))
    }

    pub(crate) fn password_prompt_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(prompt) = &self.password_prompt else { return Vec::new() };
        let theme = &self.theme;
        let card = self.password_prompt_card();
        let message = self.password_prompt_message();
        let mut painter = Painter::new(self, renderer);
        painter.fill(self.output_rect(), Colour([0.0, 0.0, 0.0, 0.5]), 0.0);
        painter.popup(card);
        let inner = Rect::new(card.x + 20.0, card.y, card.w - 40.0, card.h);
        painter.glyph(glyph::LOCK, Rect::new(inner.x, card.y + 18.0, 20.0, 24.0), 18, theme.accent);
        painter.text(
            &prompt.title,
            Rect::new(inner.x + 30.0, card.y + 18.0, inner.w - 30.0, 24.0),
            Style::new(theme.font_size * 1.1, theme.text).bold(),
        );
        let mut top = card.y + 50.0;
        for line in &message {
            painter.text(line, Rect::new(inner.x, top, inner.w, 20.0), Style::new(theme.font_size * 0.9, theme.text));
            top += 20.0;
        }
        let ask =
            if prompt.user.is_empty() { "Your password".to_string() } else { format!("Password for {}", prompt.user) };
        painter.text(
            &ask,
            Rect::new(inner.x, top + 2.0, inner.w, 20.0),
            Style::new(theme.font_size * 0.9, theme.muted),
        );
        let field = Rect::new(inner.x, top + 28.0, inner.w, 36.0);
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
            visible: !prompt.checking,
            accent: theme.accent.straight_rgba8(),
        };
        let image = self.text.borrow_mut().field(&mask, &theme.font, 14.0, theme.text, (field.w - 24.0) as f32, shown);
        if let Some(image) = image {
            painter.image(&image.buffer, (field.x + 12.0, field.y + (field.h - image.height) / 2.0));
        }
        let note = match (&prompt.error, prompt.checking, &prompt.purpose) {
            (Some(error), ..) => error.as_str(),
            (None, true, _) => "Checking...",
            (None, false, Purpose::Sudo(_)) => "Passed to sudo for this task only; it is not kept.",
            (None, false, Purpose::Polkit(_)) => "Checked by polkit; Tileception does not keep it.",
        };
        painter.text(
            note,
            Rect::new(inner.x, top + 70.0, inner.w, 20.0),
            Style::new(theme.font_size * 0.82, if prompt.error.is_some() { theme.danger } else { theme.muted }),
        );
        for (rect, hit) in self.password_prompt_controls() {
            let primary = hit == Hit::Submit;
            let button = if primary { Button::primary("Continue") } else { Button::plain("Cancel") };
            let button = button.enabled(!(primary && prompt.checking));
            painter.button(rect, button, rect.contains(Point::new(self.pointer.x, self.pointer.y)));
        }
        painter.finish()
    }
}

/// Words as lines of about `columns` characters, at most `limit` of them.
fn wrap(text: &str, columns: usize, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + word.chars().count() < columns => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    if lines.len() > limit {
        lines.truncate(limit);
        if let Some(last) = lines.last_mut() {
            last.push_str("...");
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_wrap_and_end() {
        assert!(wrap("", 40, 3).is_empty());
        assert_eq!(
            wrap("Authentication is required to install updates", 24, 3),
            ["Authentication is", "required to install", "updates"]
        );
        assert_eq!(wrap("one two three four five six", 8, 2), ["one two", "three..."]);
    }
}
