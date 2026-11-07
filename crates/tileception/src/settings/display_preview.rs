//! Display changes stay temporary until the person at the screen keeps them.
use super::*;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use std::time::Duration;

pub(super) struct Preview {
    token: u64,
    document: String,
    before: String,
    keep_focused: bool,
    /// When the preview reverts by itself, to tell a timeout from a preview
    /// that a display change or reload ended early.
    deadline: Option<std::time::Instant>,
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Keep,
    Revert,
    Block,
}

impl crate::ui::PointerAction for Action {
    fn activate(&self, state: &mut Tileception, _: Rect, _: Point) {
        match self {
            Self::Keep => state.settings_keep_displays(),
            Self::Revert => state.settings_revert_displays(),
            Self::Block => {}
        }
    }
}

impl Tileception {
    pub(super) fn settings_start_display_preview(&mut self) -> Result<bool, String> {
        if self.settings.display_preview.is_some() {
            return Ok(true);
        }
        let document = self.settings_document()?;
        let proposed = crate::config::Config::parse(&document)?.file.outputs;
        if proposed == self.config.file.outputs {
            return Ok(false);
        }
        let token = self.preview_outputs(proposed)?;
        let deadline = self.output_preview_status().map(|status| status.deadline);
        self.settings.display_preview = Some(Preview {
            token,
            document,
            before: self.settings.document_before.clone(),
            keep_focused: false,
            deadline,
        });
        self.settings.dropdown = None;
        self.settings.pressed = None;
        self.invalidate_settings_client();
        self.schedule_render();
        if let Err(error) =
            self.loop_handle.insert_source(Timer::from_duration(Duration::from_millis(250)), move |_, _, state| {
                state.settings_sync_display_preview();
                if state.settings.display_preview.as_ref().is_some_and(|preview| preview.token == token) {
                    state.invalidate_settings_client();
                    state.schedule_render();
                    TimeoutAction::ToDuration(Duration::from_millis(250))
                } else {
                    TimeoutAction::Drop
                }
            })
        {
            self.settings_revert_displays();
            return Err(format!("Could not start display confirmation: {error}"));
        }
        Ok(true)
    }

    pub(super) fn settings_sync_display_preview(&mut self) {
        let Some(preview) = &self.settings.display_preview else { return };
        if self.output_preview_status().is_some_and(|status| status.token == preview.token) {
            return;
        }
        let token = preview.token;
        let timed_out = preview.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        self.settings.display_preview = None;
        (self.settings.message, self.settings.error) = match self.revert_outputs(token) {
            Err(error) if error != "output preview expired or was superseded" => {
                (format!("Could not restore displays: {error}"), true)
            }
            _ if timed_out => (
                "The changes were not kept within 15 seconds, so the displays went back. Your changes are \
                 still here: Apply to try again."
                    .into(),
                false,
            ),
            _ => (
                "The screens changed or the config reloaded, so the displays went back. Your changes are still \
                 here: Apply to try again."
                    .into(),
                false,
            ),
        };
        self.invalidate_settings_client();
        self.schedule_render();
    }

    pub(super) fn settings_revert_displays(&mut self) {
        let Some(preview) = self.settings.display_preview.take() else { return };
        let result = match self.revert_outputs(preview.token) {
            Err(error) if error == "output preview expired or was superseded" => Ok(()),
            result => result,
        };
        match result {
            Ok(()) => {
                self.settings.message =
                    "The displays went back. Your changes are still here: Apply to try again.".into();
                self.settings.error = false;
            }
            Err(error) => {
                self.settings.message = format!("Could not restore displays: {error}");
                self.settings.error = true;
            }
        }
        self.invalidate_settings_client();
        self.schedule_render();
    }

    fn settings_keep_displays(&mut self) {
        let Some(preview) = self.settings.display_preview.take() else { return };
        let result = self.confirm_outputs_with_persistence(preview.token, |_| {
            crate::config_edit::apply_document(&preview.before, &preview.document)
        });
        match result.and_then(|_| self.settings_after_save()) {
            Ok(()) => {
                // Reopen from the saved document, without an obsolete section snapshot.
                self.settings.section = None;
                self.open_settings_page_section();
                self.settings.message = "Display changes kept. Settings saved.".into();
                self.settings.error = false;
            }
            Err(error) => {
                self.settings.message = error;
                self.settings.error = true;
            }
        }
        self.invalidate_settings_client();
        self.schedule_render();
    }

    pub(super) fn settings_display_preview_key(&mut self, sym: u32, mods: Mods) -> bool {
        let Some(preview) = &mut self.settings.display_preview else { return false };
        match sym {
            keysyms::KEY_Escape => self.settings_revert_displays(),
            keysyms::KEY_Up if mods.alt => self.settings_revert_displays(),
            keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab | keysyms::KEY_Left | keysyms::KEY_Right => {
                preview.keep_focused = !preview.keep_focused;
            }
            keysyms::KEY_Return | keysyms::KEY_space => {
                if preview.keep_focused {
                    self.settings_keep_displays();
                } else {
                    self.settings_revert_displays();
                }
            }
            _ => {}
        }
        self.invalidate_settings_client();
        self.schedule_render();
        true
    }

    fn display_confirmation_rect(&self) -> Rect {
        let panel = self.settings_frame().panel;
        let width = (panel.w - 32.).clamp(240., 480.);
        Rect::new(panel.x + (panel.w - width) / 2., panel.y + (panel.h - 200.) / 2., width, 200.)
    }

    fn display_confirmation_buttons(&self) -> [(Rect, Action, &'static str); 2] {
        let rect = self.display_confirmation_rect();
        let width = (rect.w - 48.) / 2.;
        [
            (Rect::new(rect.x + 16., rect.bottom() - 50., width, 34.), Action::Revert, "Revert"),
            (Rect::new(rect.x + 32. + width, rect.bottom() - 50., width, 34.), Action::Keep, "Keep changes"),
        ]
    }

    pub(super) fn display_preview_pointer_target(&self, point: Point) -> Option<interaction::Target> {
        self.settings.display_preview.as_ref()?;
        if self.settings_frame().hits.iter().any(|(rect, hit)| *hit == Hit::Close && rect.contains(point)) {
            return self.settings_main_target(point);
        }
        for (rect, action, _) in self.display_confirmation_buttons() {
            if rect.contains(point) {
                return Some(interaction::Target::button(rect, action));
            }
        }
        Some(interaction::Target::new(
            self.settings_frame().panel,
            Action::Block,
            smithay::input::pointer::CursorIcon::Default,
        ))
    }

    pub(super) fn settings_draw_display_preview(&self, painter: &mut Painter<'_>) {
        let Some(preview) = &self.settings.display_preview else { return };
        let seconds = self
            .output_preview_status()
            .filter(|status| status.token == preview.token)
            .map_or(0, |status| status.deadline.saturating_duration_since(std::time::Instant::now()).as_secs() + 1);
        let rect = self.display_confirmation_rect();
        let theme = &self.theme;
        painter.fill(self.settings_frame().panel, Colour([0., 0., 0., 0.65]), 10.);
        painter.shape(rect, Shape { border_width: 1., border: theme.accent, ..Shape::filled(theme.background, 8.) });
        painter.text(
            "Keep these display settings?",
            Rect::new(rect.x + 16., rect.y + 12., rect.w - 32., 38.),
            Style::new(18., theme.text),
        );
        painter.text(&format!("Reverting in {seconds} seconds. Nothing is saved until Keep changes. Escape reverts and keeps your draft."), Rect::new(rect.x + 16., rect.y + 58., rect.w - 32., 72.), Style::new(12., theme.muted).lines(3));
        for (button, action, label) in self.display_confirmation_buttons() {
            let focused = preview.keep_focused == (action == Action::Keep);
            painter.shape(
                button,
                Shape {
                    border_width: 1.,
                    border: if focused { theme.accent } else { theme.border },
                    ..Shape::filled(theme.surface, 5.)
                },
            );
            painter.text(label, button, Style::new(12., theme.text).align(Align::Centre));
        }
    }
}
