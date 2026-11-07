//! Pointer semantics shared by every Settings page and dialog.
use super::*;
use smithay::input::pointer::CursorIcon;

pub(super) use crate::ui::PointerTarget as Target;

#[derive(Clone, PartialEq)]
enum PageAction {
    Hit(Hit),
    Move,
    Dismiss,
}

impl crate::ui::PointerAction for PageAction {
    fn activate(&self, state: &mut Tileception, rect: Rect, point: Point) {
        match self {
            Self::Move => {
                if let Some(client) = &state.settings.client {
                    client.start_move();
                }
            }
            Self::Dismiss => {
                state.settings.dropdown = None;
                state.settings.colour = None;
            }
            Self::Hit(hit) => {
                if *hit == Hit::Close {
                    state.close_settings();
                    return;
                }
                if state.settings.section.is_some() && !state.section_stage(false) {
                    return;
                }
                state.settings_page_click(rect, *hit, point);
            }
        }
        state.schedule_render();
    }
}

impl Tileception {
    pub(super) fn settings_main_target(&self, point: Point) -> Option<Target> {
        let frame = self.settings_frame();
        if let Some((rect, hit)) = frame.hits.iter().rev().find(|(rect, _)| rect.contains(point)) {
            if matches!(hit, Hit::Apply | Hit::Discard) && self.settings.changed() == 0 {
                return None;
            }
            let cursor = match hit {
                Hit::Search => CursorIcon::Text,
                Hit::LevelAt(_) => CursorIcon::EwResize,
                Hit::Field(index) => match FIELDS[*index].control {
                    Control::Text | Control::Colour => CursorIcon::Text,
                    Control::Number(..) if self.settings.editing != Some(*index) => CursorIcon::EwResize,
                    Control::Number(..) => CursorIcon::Text,
                    _ => CursorIcon::Pointer,
                },
                _ => CursorIcon::Pointer,
            };
            return Some(if cursor == CursorIcon::Pointer {
                Target::button(*rect, PageAction::Hit(*hit))
            } else {
                Target::new(*rect, PageAction::Hit(*hit), cursor)
            });
        }
        let title = Rect::new(frame.panel.x, frame.panel.y, frame.panel.w, 64.);
        if title.contains(point) {
            return Some(Target::new(title, PageAction::Move, CursorIcon::Grab));
        }
        (self.settings.dropdown.is_some() || self.settings.colour.is_some())
            .then(|| Target::new(frame.panel, PageAction::Dismiss, CursorIcon::Default))
    }

    pub(super) fn settings_pointer_target(&self, point: Point) -> Option<Target> {
        if self.settings.keymap.is_some() {
            return self.keymap_pointer_target(point);
        }
        if self.settings.section.is_some() {
            return self.section_pointer_target(point);
        }
        if self.settings.connect.is_some() {
            return self.connect_pointer_target(point);
        }
        if self.settings.editor.is_some() {
            return self.editor_pointer_target(point);
        }
        if let Some(target) = self.colour_pointer_target(point) {
            return Some(target);
        }
        self.settings_main_target(point)
    }

    pub(super) fn settings_pointer_button(&mut self, pressed: bool) {
        let point = Point::new(self.settings.pointer.0, self.settings.pointer.1);
        let target = self.settings_pointer_target(point);
        match Target::button_event(&mut self.settings.pressed, target, pressed) {
            crate::ui::PointerEvent::Press(target) => {
                self.settings.text_drag = target.cursor == CursorIcon::Text;
                target.activate(self, point);
                if self.settings.text_drag {
                    self.settings_text_caret(point, false);
                }
            }
            crate::ui::PointerEvent::Release(target) => {
                self.settings_release();
                target.activate(self, point);
            }
            _ if !pressed => self.settings_release(),
            _ => {}
        }
    }

    pub(super) fn settings_update_cursor(&self) {
        let point = Point::new(self.settings.pointer.0, self.settings.pointer.1);
        let cursor = if self.settings.text_drag {
            CursorIcon::Text
        } else if self.settings.colour.as_ref().is_some_and(|picker| picker.drag.is_some()) {
            CursorIcon::Crosshair
        } else if self.settings.drag.is_some() || self.settings.level_drag.is_some() {
            CursorIcon::EwResize
        } else if self.section_map_dragging() {
            CursorIcon::Grabbing
        } else {
            self.settings_pointer_target(point).map_or(CursorIcon::Default, |target| target.cursor)
        };
        if let Some(client) = &self.settings.client {
            client.cursor(cursor);
        }
    }

    pub(super) fn settings_text_caret(&mut self, point: Point, extend: bool) {
        if self.settings.editor.is_some() {
            self.settings_editor_caret(point, extend);
            return;
        }
        if self.settings.section.is_some() {
            self.section_text_caret(point, extend);
            return;
        }
        if self.settings.keymap.is_some() {
            self.keymap_text_caret(point, extend);
            return;
        }
        let frame = self.settings_frame();
        if let Some(index) = self.settings.editing {
            let Some((rect, _)) = frame.hits.iter().find(|(_, hit)| *hit == Hit::Field(index)) else { return };
            let inset = if matches!(FIELDS[index].control, Control::Colour) { 34. } else { 10. };
            let position = self.text.borrow_mut().field_hit(
                &self.settings.input.text,
                &self.theme.font,
                11.,
                (rect.w - inset - 8.) as f32,
                self.settings.input.cursor,
                (point.x - rect.x - inset).max(0.) as f32,
            );
            self.settings.input.move_to(position, extend);
        } else if self.settings.focus == Some(Hit::Search) {
            let Some((rect, _)) = frame.hits.iter().find(|(_, hit)| *hit == Hit::Search) else { return };
            let position = self.text.borrow_mut().field_hit(
                &self.settings.search_input.text,
                &self.theme.font,
                12.,
                (rect.w - 45.) as f32,
                self.settings.search_input.cursor,
                (point.x - rect.x - 35.).max(0.) as f32,
            );
            self.settings.search_input.move_to(position, extend);
        }
    }

    pub(super) fn settings_pointer_feedback(&self, painter: &mut Painter<'_>) {
        let point = Point::new(self.settings.pointer.0, self.settings.pointer.1);
        if let Some(target) = self.settings_pointer_target(point)
            && target.cursor == CursorIcon::Pointer
        {
            let pressed = self.settings.pressed.as_ref() == Some(&target);
            painter.fill(target.rect, self.theme.text.alpha(if pressed { 0.12 } else { 0.04 }), 5.);
        }
    }
}
