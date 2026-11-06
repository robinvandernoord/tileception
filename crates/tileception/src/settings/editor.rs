//! Multiline TOML editor for structured settings and the complete config.

use super::*;
use crate::{launcher::Paste, text_input::TextInput};

const LINE: f64 = 21.;
const LIMIT: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq)]
enum PointerAction {
    Text,
    Cancel,
    Apply,
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _rect: Rect, point: Point) {
        match self {
            Self::Text => state.settings_editor_caret(point, false),
            Self::Cancel => state.settings.editor = None,
            Self::Apply => state.finish_settings_editor(true),
        }
        state.schedule_render();
    }
}

fn buttons(frame: &Frame, reference: bool) -> [(Rect, &'static str, PointerAction); 2] {
    [
        (Rect::new(frame.panel.right() - 280., frame.panel.bottom() - 49., 110., 33.), "Cancel", PointerAction::Cancel),
        (
            Rect::new(frame.panel.right() - 160., frame.panel.bottom() - 49., 110., 33.),
            if reference { "Close" } else { "Apply changes" },
            PointerAction::Apply,
        ),
    ]
}

pub(super) struct Editor {
    input: TextInput,
    pub(super) field: Option<usize>,
    first: usize,
    horizontal: usize,
    error: String,
    reference: bool,
    scroll_remainder: f64,
    original: String,
}

impl Editor {
    fn insert(&mut self, text: &str) {
        self.input.insert_with_limit(text, LIMIT);
        self.error.clear();
    }

    fn keep_caret(&mut self, visible: usize, columns: usize) {
        let before = &self.input.text[..self.input.cursor];
        let row = before.bytes().filter(|byte| *byte == b'\n').count();
        if row < self.first {
            self.first = row;
        }
        if row >= self.first + visible {
            self.first = row + 1 - visible;
        }
        let column = before.rsplit('\n').next().unwrap_or("").chars().count();
        if column < self.horizontal {
            self.horizontal = column;
        }
        if column > self.horizontal + columns {
            self.horizontal = column - columns;
        }
    }
}

fn body(frame: &Frame) -> Rect {
    Rect::new(frame.panel.x + 34., frame.panel.y + 70., frame.panel.w - 68., frame.panel.h - 140.)
}

fn columns(text: &str, start: usize) -> &str {
    let offset = text.char_indices().nth(start).map_or(text.len(), |(offset, _)| offset);
    &text[offset..]
}

impl Tileception {
    pub(super) fn editor_pointer_target(&self, point: Point) -> Option<interaction::Target> {
        let frame = self.settings_frame();
        let area = body(&frame);
        if area.contains(point) {
            return Some(interaction::Target::new(
                area,
                PointerAction::Text,
                smithay::input::pointer::CursorIcon::Text,
            ));
        }
        buttons(&frame, self.settings.editor.as_ref()?.reference)
            .into_iter()
            .find_map(|(rect, _, action)| rect.contains(point).then(|| interaction::Target::button(rect, action)))
    }

    pub(super) fn open_settings_editor(&mut self, field: Option<usize>, reference: bool) {
        if let Some(position) = self
            .settings
            .parked_editors
            .iter()
            .position(|editor| editor.field == field && editor.reference == reference)
        {
            self.settings.editor = Some(self.settings.parked_editors.remove(position));
            self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
            self.schedule_render();
            return;
        }
        let result = (|| {
            let document = self.settings_document()?;
            let text = if reference {
                crate::config::DEFAULT.into()
            } else if let Some(index) = field {
                crate::config_edit::section_text(&document, FIELDS[index].key)?
            } else {
                document
            };
            Ok::<_, String>(text)
        })();
        match result {
            Ok(text) => {
                self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
                self.settings.editor = Some(Editor {
                    original: text.clone(),
                    input: TextInput { text, ..Default::default() },
                    field,
                    first: 0,
                    horizontal: 0,
                    error: String::new(),
                    reference,
                    scroll_remainder: 0.,
                });
                self.settings.dropdown = None;
            }
            Err(error) => {
                self.settings.message = error;
                self.settings.error = true;
            }
        }
        self.schedule_render();
    }

    fn finish_settings_editor(&mut self, apply: bool) {
        let Some(mut editor) = self.settings.editor.take() else { return };
        if editor.reference {
            self.settings.editor = None;
            return;
        }
        let result = (|| {
            if editor.input.text == editor.original {
                return Ok(());
            }
            let current = self.settings_document()?;
            let current_excerpt = if let Some(index) = editor.field {
                crate::config_edit::section_text(&current, FIELDS[index].key)?
            } else {
                current.clone()
            };
            if current_excerpt != editor.original {
                return Err(
                    "This setting changed while its editor was hidden. Copy your edit, cancel and reopen to merge it."
                        .into(),
                );
            }
            if let Some(index) = editor.field {
                let document = crate::config_edit::splice_section(
                    &self.settings_document()?,
                    FIELDS[index].key,
                    &editor.input.text,
                )?;
                let config = crate::config::Config::parse(&document)?;
                self.settings.draft = values(&config.file)?;
                self.settings.document_draft = document;
            } else {
                let config = crate::config::Config::parse(&editor.input.text)?;
                self.settings.draft = values(&config.file)?;
                self.settings.document_draft = editor.input.text.clone();
            }
            Ok::<_, String>(())
        })();
        match result {
            Ok(()) => {
                self.settings.editor = None;
                self.settings.message.clear();
                self.settings.error = false;
                if apply {
                    self.settings_activate(Hit::Apply, None);
                }
            }
            Err(error) => {
                editor.error = error;
                self.settings.editor = Some(editor);
            }
        }
        self.schedule_render();
    }

    pub(super) fn settings_editor_scroll(&mut self, amount: f64) {
        let visible = (body(&self.settings_frame()).h / LINE).max(1.) as usize;
        if let Some(editor) = self.settings.editor.as_mut() {
            let max = editor.input.text.split('\n').count().saturating_sub(visible);
            editor.scroll_remainder += amount;
            let lines = (editor.scroll_remainder / LINE).trunc();
            editor.scroll_remainder -= lines * LINE;
            editor.first = (editor.first as f64 + lines).clamp(0., max as f64) as usize;
        }
        self.schedule_render();
    }

    pub(super) fn settings_editor_caret(&mut self, point: Point, extend: bool) {
        let area = body(&self.settings_frame());
        let Some(editor) = self.settings.editor.as_mut() else { return };
        let row = editor.first + ((point.y - area.y) / LINE).max(0.) as usize;
        let lines: Vec<&str> = editor.input.text.split('\n').collect();
        let row = row.min(lines.len() - 1);
        let offset: usize = lines.iter().take(row).map(|line| line.len() + 1).sum();
        let shown = columns(lines[row], editor.horizontal);
        let hidden = lines[row].len() - shown.len();
        let byte = self.text.borrow_mut().field_hit(
            shown,
            "monospace",
            12.,
            (area.w - 58.) as f32,
            0,
            (point.x - area.x - 48.).max(0.) as f32,
        );
        editor.input.move_to(offset + hidden + byte, extend);
        self.schedule_render();
    }

    pub fn settings_insert_paste(&mut self, epoch: u64, text: &str) {
        self.invalidate_settings_client();
        if self.settings.editor_epoch != epoch {
            return;
        }
        if self.settings.keymap.is_some() {
            self.keymap_paste(text);
            return;
        }
        if self.settings.section.is_some() {
            self.section_paste(text);
            return;
        }
        if self.settings.connect.is_some() {
            self.settings_connect_paste(text);
            return;
        }
        if let Some(editor) = self.settings.editor.as_mut().filter(|editor| !editor.reference) {
            let text: String =
                text.chars().filter(|character| !character.is_control() || matches!(character, '\n' | '\t')).collect();
            editor.insert(&text);
            let visible = (body(&self.settings_frame()).h / LINE).max(1.) as usize;
            let columns = ((body(&self.settings_frame()).w - 70.) / 7.2).max(1.) as usize;
            if let Some(editor) = self.settings.editor.as_mut() {
                editor.keep_caret(visible, columns);
            }
        } else if self.settings.editor.is_none() {
            let text: String = text
                .chars()
                .map(|character| if matches!(character, '\n' | '\r' | '\t') { ' ' } else { character })
                .filter(|character| !character.is_control())
                .collect();
            if self.settings.editing.is_some() {
                self.settings.input.insert(&text);
            } else if self.settings.focus == Some(Hit::Search) && self.settings.query.len() + text.len() <= 4096 {
                self.settings.search_input.insert(&text);
                self.settings.query.clone_from(&self.settings.search_input.text);
            }
        }
        self.schedule_render();
    }

    pub(super) fn settings_editor_key(&mut self, sym: u32, mods: Mods) {
        if sym == keysyms::KEY_Escape {
            if let Some(editor) = self.settings.editor.take()
                && !editor.reference
                && editor.input.text != editor.original
            {
                self.settings.parked_editors.push(editor);
                self.settings.message.clear();
            }
            self.schedule_render();
            return;
        }
        if mods.ctrl && matches!(sym, keysyms::KEY_s | keysyms::KEY_S) {
            self.finish_settings_editor(true);
            return;
        }
        if mods.ctrl && sym == keysyms::KEY_Return {
            self.finish_settings_editor(true);
            return;
        }
        if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) {
            self.paste(Paste::Settings(self.settings.editor_epoch));
            return;
        }
        let visible = (body(&self.settings_frame()).h / LINE).max(1.) as usize;
        let columns = ((body(&self.settings_frame()).w - 70.) / 7.2).max(1.) as usize;
        let Some(editor) = self.settings.editor.as_mut() else { return };
        if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C) {
            let selection = editor.input.text[editor.input.selection()].to_string();
            self.copy_text(selection);
            return;
        }
        if editor.reference {
            self.settings_editor_scroll(if matches!(sym, keysyms::KEY_Down | keysyms::KEY_Page_Down) {
                LINE * 5.
            } else if matches!(sym, keysyms::KEY_Up | keysyms::KEY_Page_Up) {
                -LINE * 5.
            } else {
                0.
            });
            return;
        }
        match sym {
            keysyms::KEY_a | keysyms::KEY_A if mods.ctrl => editor.input.select_all(),
            keysyms::KEY_Left | keysyms::KEY_Right => {
                editor.input.move_horizontal(sym == keysyms::KEY_Right, mods.ctrl, mods.shift)
            }
            keysyms::KEY_BackSpace | keysyms::KEY_Delete => {
                editor.input.delete_with_limit(sym == keysyms::KEY_Delete, mods.ctrl, LIMIT)
            }
            keysyms::KEY_Home | keysyms::KEY_End => {
                let position = if mods.ctrl {
                    if sym == keysyms::KEY_Home { 0 } else { editor.input.text.len() }
                } else if sym == keysyms::KEY_Home {
                    editor.input.text[..editor.input.cursor].rfind('\n').map_or(0, |offset| offset + 1)
                } else {
                    editor.input.cursor
                        + editor.input.text[editor.input.cursor..]
                            .find('\n')
                            .unwrap_or(editor.input.text.len() - editor.input.cursor)
                };
                editor.input.move_to(position, mods.shift);
            }
            keysyms::KEY_Up | keysyms::KEY_Down | keysyms::KEY_Page_Up | keysyms::KEY_Page_Down => {
                let cursor = editor.input.cursor;
                let row = editor.input.text[..cursor].bytes().filter(|byte| *byte == b'\n').count();
                let column = editor.input.text[..cursor].rsplit('\n').next().unwrap_or("").chars().count();
                let lines: Vec<&str> = editor.input.text.split('\n').collect();
                let delta = if matches!(sym, keysyms::KEY_Page_Up | keysyms::KEY_Page_Down) { visible } else { 1 };
                let next = if matches!(sym, keysyms::KEY_Up | keysyms::KEY_Page_Up) {
                    row.saturating_sub(delta)
                } else {
                    (row + delta).min(lines.len() - 1)
                };
                let offset: usize = lines.iter().take(next).map(|line| line.len() + 1).sum();
                let byte = lines[next].char_indices().nth(column).map_or(lines[next].len(), |(byte, _)| byte);
                editor.input.move_to(offset + byte, mods.shift);
            }
            keysyms::KEY_Return => editor.insert("\n"),
            keysyms::KEY_Tab => editor.insert("    "),
            _ if !mods.ctrl && !mods.alt && !mods.logo => {
                if let Some(character) =
                    char::from_u32(xkb::keysym_to_utf32(sym.into())).filter(|character| !character.is_control())
                {
                    editor.insert(&character.to_string());
                }
            }
            _ => {}
        }
        editor.keep_caret(visible, columns);
        self.schedule_render();
    }

    pub(super) fn settings_draw_editor(&self, painter: &mut Painter<'_>, frame: &Frame) {
        let Some(editor) = &self.settings.editor else { return };
        let theme = &self.theme;
        let area = body(frame);
        painter.shape(
            frame.panel,
            Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.background, 10.) },
        );
        let title = if editor.reference {
            "Configuration reference"
        } else {
            editor.field.map_or("config.toml", |index| FIELDS[index].label)
        };
        painter.text(
            title,
            Rect::new(area.x, frame.panel.y + 18., area.w, 35.),
            Style::new(18., theme.text).weight(500),
        );
        painter.shape(area, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.surface, 6.) });
        let selection = editor.input.selection();
        painter.clipped(area, |painter| {
            let mut offset = 0;
            for (row, line) in editor.input.text.split('\n').enumerate() {
                let end = offset + line.len();
                if row >= editor.first && row < editor.first + (area.h / LINE).ceil() as usize {
                    let top = area.y + (row - editor.first) as f64 * LINE;
                    painter.text(
                        &(row + 1).to_string(),
                        Rect::new(area.x + 8., top, 32., LINE),
                        Style::new(10., theme.muted),
                    );
                    let shown = columns(line, editor.horizontal);
                    let hidden = line.len() - shown.len();
                    let style = Style::new(
                        12.,
                        if line.trim_start().starts_with('#') {
                            theme.muted
                        } else if line.trim_start().starts_with('[') {
                            theme.accent
                        } else {
                            theme.text
                        },
                    );
                    let left = area.x + 48.;
                    if selection.start <= end && selection.end > offset + hidden {
                        let start = selection.start.saturating_sub(offset + hidden).min(shown.len());
                        let finish = selection.end.saturating_sub(offset + hidden).min(shown.len());
                        let width_before = painter.measure_font(&shown[..start], 10000., style, "monospace").0;
                        let width = painter.measure_font(&shown[start..finish], 10000., style, "monospace").0;
                        painter.fill(
                            Rect::new(left + width_before, top + 2., width.max(3.), LINE - 3.),
                            theme.accent.alpha(0.22),
                            0.,
                        );
                    }
                    painter.text_font(shown, Rect::new(left, top, area.w - 58., LINE), style, "monospace");
                    if !editor.reference && editor.input.cursor >= offset + hidden && editor.input.cursor <= end {
                        let before = &shown[..editor.input.cursor - offset - hidden];
                        let width = painter.measure_font(before, 10000., style, "monospace").0;
                        painter.fill(Rect::new(left + width, top + 4., 1., LINE - 7.), theme.text, 0.);
                    }
                }
                offset = end + 1;
            }
        });
        let hint = if editor.error.is_empty() {
            "Ctrl+Enter: use draft   Ctrl+S: apply   Esc: keep for later"
        } else {
            &editor.error
        };
        painter.text(
            hint,
            Rect::new(area.x, frame.panel.bottom() - 61., area.w - 285., 52.),
            Style::new(11., if editor.error.is_empty() { theme.muted } else { theme.danger }).lines(3),
        );
        for (rect, label, _) in buttons(frame, editor.reference) {
            painter.fill(rect, theme.surface_raised, 6.);
            painter.text(label, rect, Style::new(12., theme.text).align(Align::Centre));
        }
    }
}
