//! Closing Settings: unapplied changes are applied or discarded first, in a
//! small confirmation inside the window. Closing keeps no draft, so Settings
//! always reopens on config.toml as it is then.
use super::*;

/// The confirmation's choices, left to right.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    Discard,
    Cancel,
    Apply,
    /// Clicks elsewhere in the window do nothing while it asks.
    Block,
}

const CHOICES: [(Choice, &str); 3] =
    [(Choice::Discard, "Discard"), (Choice::Cancel, "Cancel"), (Choice::Apply, "Apply")];

impl crate::ui::PointerAction for Choice {
    fn activate(&self, state: &mut Tileception, _: Rect, _: Point) {
        state.settings_close_choose(*self);
    }
}

impl Tileception {
    /// Whether Settings holds changes that are not applied yet, including
    /// text still being typed.
    pub(super) fn settings_unsaved(&self) -> bool {
        let settings = &self.settings;
        settings.changed() > 0
            || settings.editing.is_some_and(|index| {
                settings.input.text.trim()
                    != settings.draft[index].as_ref().and_then(toml::Value::as_str).unwrap_or("").trim()
            })
            || settings.section.as_ref().is_some_and(sections::model::Screen::changed)
            || self.settings_editor_unsaved()
    }

    /// Closes Settings as the person asked, by its close button, Escape, its
    /// keybinding or the window's close request. Unapplied changes ask first.
    pub(crate) fn request_close_settings(&mut self) {
        if !self.settings.open {
            return;
        }
        if self.settings.close_confirm.is_none() {
            self.section_cancel_recording();
            // Closing has always put back displays still waiting to be kept.
            self.settings_revert_displays();
            if !self.settings_unsaved() {
                self.close_settings();
                return;
            }
            self.settings.close_confirm = Some(Choice::Apply);
            self.settings.dropdown = None;
            self.settings.colour = None;
            self.settings.apps_menu = None;
            self.settings.pressed = None;
            self.stop_key_repeat();
        }
        // The question needs an answer, so bring Settings forward to ask it.
        if !self.settings_focused()
            && let Some(window) = self.settings.window
        {
            self.fly_to_window(window);
        }
        self.invalidate_settings_client();
        self.schedule_render();
    }

    fn settings_close_choose(&mut self, choice: Choice) {
        match choice {
            Choice::Block => return,
            Choice::Cancel => self.settings.close_confirm = None,
            Choice::Discard => self.close_settings(),
            Choice::Apply => {
                self.settings.close_confirm = None;
                if self.settings.section.is_some() {
                    self.section_stage(true);
                } else {
                    self.settings_editor_apply();
                }
                // Stay open when something still needs the person: an error,
                // a screen to finish, or displays waiting to be kept.
                if !self.settings_unsaved() && self.settings.display_preview.is_none() {
                    self.close_settings();
                }
            }
        }
        self.invalidate_settings_client();
        self.schedule_render();
    }

    /// Keys while the confirmation shows. Enter applies, the default;
    /// Escape cancels.
    pub(super) fn settings_close_key(&mut self, sym: u32, mods: Mods) -> bool {
        let Some(focused) = self.settings.close_confirm else { return false };
        let position = CHOICES.iter().position(|(choice, _)| *choice == focused).unwrap_or(2);
        let backward =
            sym == keysyms::KEY_ISO_Left_Tab || sym == keysyms::KEY_Left || sym == keysyms::KEY_Tab && mods.shift;
        match sym {
            keysyms::KEY_Escape => self.settings_close_choose(Choice::Cancel),
            keysyms::KEY_Up if mods.alt => self.settings_close_choose(Choice::Cancel),
            keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab | keysyms::KEY_Left | keysyms::KEY_Right => {
                let next = (if backward { position + CHOICES.len() - 1 } else { position + 1 }) % CHOICES.len();
                self.settings.close_confirm = Some(CHOICES[next].0);
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => self.settings_close_choose(focused),
            _ => {}
        }
        self.invalidate_settings_client();
        self.schedule_render();
        true
    }

    fn close_confirm_rect(&self) -> Rect {
        let panel = self.settings_frame().panel;
        let width = (panel.w - 32.).clamp(240., 480.);
        Rect::new(panel.x + (panel.w - width) / 2., panel.y + (panel.h - 180.) / 2., width, 180.)
    }

    fn close_confirm_buttons(&self) -> [(Rect, Choice, &'static str); 3] {
        let rect = self.close_confirm_rect();
        let width = (rect.w - 64.) / 3.;
        CHOICES.map(|(choice, label)| {
            let position = CHOICES.iter().position(|(candidate, _)| *candidate == choice).unwrap_or(0) as f64;
            (Rect::new(rect.x + 16. + position * (width + 16.), rect.bottom() - 50., width, 34.), choice, label)
        })
    }

    pub(super) fn close_confirm_pointer_target(&self, point: Point) -> Option<interaction::Target> {
        self.settings.close_confirm?;
        for (rect, choice, _) in self.close_confirm_buttons() {
            if rect.contains(point) {
                return Some(interaction::Target::button(rect, choice));
            }
        }
        Some(interaction::Target::new(
            self.settings_frame().panel,
            Choice::Block,
            smithay::input::pointer::CursorIcon::Default,
        ))
    }

    pub(super) fn settings_draw_close_confirm(&self, painter: &mut Painter<'_>) {
        let Some(focused) = self.settings.close_confirm else { return };
        let rect = self.close_confirm_rect();
        let theme = &self.theme;
        painter.fill(self.settings_frame().panel, Colour([0., 0., 0., 0.65]), 10.);
        painter.shape(rect, Shape { border_width: 1., border: theme.accent, ..Shape::filled(theme.background, 8.) });
        painter.text(
            "Apply your changes?",
            Rect::new(rect.x + 16., rect.y + 12., rect.w - 32., 38.),
            Style::new(18., theme.text),
        );
        painter.text(
            "Settings has changes that are not applied yet. Discard closes without them; Cancel keeps Settings open.",
            Rect::new(rect.x + 16., rect.y + 54., rect.w - 32., 60.),
            Style::new(12., theme.muted).lines(3),
        );
        let pointer = Point::new(self.settings.pointer.0, self.settings.pointer.1);
        for (button, choice, label) in self.close_confirm_buttons() {
            let look = match choice {
                Choice::Apply => crate::ui::Look::Primary,
                Choice::Discard => crate::ui::Look::Warning,
                Choice::Cancel | Choice::Block => crate::ui::Look::Plain,
            };
            painter.button(
                button,
                crate::ui::Button::new(label, look).focused(choice == focused),
                button.contains(pointer),
            );
        }
    }
}
