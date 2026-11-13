//! Multiline TOML editor for structured settings and the complete config.

use super::*;
use crate::{launcher::Paste, text_input::TextInput};

const LINE: f64 = 21.;
const LIMIT: usize = 256 * 1024;
/// Why the whole-file editor refuses its text after config.toml changed.
const STALE: &str = "config.toml changed on disk, and your edits here cannot be carried onto it, so applying them \
                     is refused. Copy what you want to keep; Discard reloads the file.";

#[derive(Clone, Copy, PartialEq)]
enum PointerAction {
    Text,
    Cancel,
    Apply,
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: Point) {
        match self {
            // The press that got here places the caret, as `settings_text_caret`.
            Self::Text => {}
            Self::Cancel => state.settings_editor_key(keysyms::KEY_Escape, Mods::default()),
            Self::Apply => state.finish_settings_editor(true),
        }
        state.schedule_render();
    }
}

fn buttons(frame: &Frame, reference: bool) -> [(Rect, &'static str, PointerAction); 2] {
    [
        (Rect::new(frame.panel.right() - 280., frame.panel.bottom() - 49., 110., 33.), "Back", PointerAction::Cancel),
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
    /// The file changed on disk while this editor held edits.
    notice: String,
    /// The whole file changed on disk under edits that cannot move onto
    /// it: applying them would write over the change.
    stale: bool,
    reference: bool,
    scroll_remainder: f64,
    original: String,
}

impl Editor {
    /// The text changed: what was wrong with it may be no longer.
    fn changed(&mut self) {
        self.error.clear();
    }

    /// Brings the whole-file editor onto config.toml's new text. It goes over
    /// the file as it is, so edits are carried onto the new text, or, when
    /// they cannot be, kept for copying while applying them is refused.
    fn follow_file(&mut self, source: &str) {
        if source == self.original {
            if self.stale {
                // Back to the text the edits were made on.
                self.stale = false;
                self.error.clear();
                self.notice.clear();
            }
            return;
        }
        let mut text = std::mem::take(&mut self.input.text);
        let edited = text != self.original;
        if super::refresh::merge_document(&mut self.original, &mut text, source) {
            self.stale = false;
            self.error.clear();
            if edited {
                self.notice = "config.toml changed on disk. Your edits here were carried onto it.".into();
            }
        } else {
            self.stale = true;
            STALE.clone_into(&mut self.error);
            STALE.clone_into(&mut self.notice);
        }
        super::refresh::replace_input(&mut self.input, text);
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
                let original = text.clone();
                let input = TextInput::new(text).multiline(LIMIT).code().read_only(reference).at_start();
                self.settings.editor = Some(Editor {
                    original,
                    input,
                    field,
                    first: 0,
                    horizontal: 0,
                    error: String::new(),
                    notice: String::new(),
                    stale: false,
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

    /// Brings the open and parked editors up to date after config.toml
    /// changed on disk. Returns the editors whose edits were kept over it.
    pub(super) fn settings_editors_refreshed(&mut self) -> Vec<&'static str> {
        let Ok(document) = self.settings_document() else { return Vec::new() };
        let mut kept = Vec::new();
        for editor in self.settings.editor.iter_mut().chain(&mut self.settings.parked_editors) {
            if editor.reference {
                continue;
            }
            let source = match editor.field {
                Some(index) => match crate::config_edit::section_text(&document, FIELDS[index].key) {
                    Ok(source) => source,
                    Err(_) => continue,
                },
                None => document.clone(),
            };
            if editor.field.is_none() {
                editor.follow_file(&source);
                continue;
            }
            if source == editor.original {
                continue;
            }
            let mut text = std::mem::take(&mut editor.input.text);
            if text != editor.original && super::refresh::same_toml(&editor.original, &source) {
                // Only the layout around the edits changed; applying compares the text.
                editor.original = source;
            } else if super::refresh::merge_text(&mut editor.original, &mut text, &source) {
                kept.push(editor.field.map_or("config.toml", |index| FIELDS[index].label));
                editor.notice =
                    "config.toml changed on disk. Your edits here are kept; applying them replaces the file's.".into();
            }
            super::refresh::replace_input(&mut editor.input, text);
            editor.error.clear();
        }
        kept
    }

    /// Whether the whole-file editor, open or kept for later, holds edits
    /// that would write over a change on disk.
    pub(super) fn settings_editor_stale(&self) -> bool {
        self.settings.editor.iter().chain(&self.settings.parked_editors).any(|editor| editor.stale)
    }

    /// Whether an editor holds text not yet in the draft.
    pub(super) fn settings_editor_unsaved(&self) -> bool {
        self.settings.editor.as_ref().is_some_and(|editor| !editor.reference && editor.input.text != editor.original)
    }

    /// Takes an open editor's text into the draft and applies it all.
    pub(super) fn settings_editor_apply(&mut self) {
        if self.settings.editor.as_ref().is_some_and(|editor| editor.reference) {
            self.settings.editor = None;
        }
        if self.settings.editor.is_some() {
            self.finish_settings_editor(true);
        } else {
            self.settings_activate(Hit::Apply, None);
        }
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
            if editor.stale {
                return Err(STALE.into());
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
        editor.input.press(offset + hidden + byte, extend);
        self.schedule_render();
    }

    /// What is selected in the editor, for a middle click.
    pub(super) fn settings_editor_selected_text(&self) -> Option<String> {
        self.settings.editor.as_ref()?.input.selected_text()
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
        if self.settings_network_paste(text) {
            return;
        }
        if self.settings.connect.is_some() {
            self.settings_connect_paste(text);
            return;
        }
        if let Some(editor) = self.settings.editor.as_mut().filter(|editor| !editor.reference) {
            if editor.input.paste(text) {
                editor.changed();
            }
            let visible = (body(&self.settings_frame()).h / LINE).max(1.) as usize;
            let columns = ((body(&self.settings_frame()).w - 70.) / 7.2).max(1.) as usize;
            if let Some(editor) = self.settings.editor.as_mut() {
                editor.keep_caret(visible, columns);
            }
        } else if self.settings.editor.is_none() {
            if self.settings.editing.is_some() {
                self.settings.input.paste(text);
            } else if self.settings.focus == Some(Hit::Search) {
                self.settings.search_input.paste(text);
                self.settings.query.clone_from(&self.settings.search_input.text);
            }
        }
        self.schedule_render();
    }

    pub(super) fn settings_editor_key(&mut self, sym: u32, mods: Mods) {
        // The keypad's arrows move between lines too.
        let sym = crate::text_input::plain(sym);
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
            self.finish_settings_editor(false);
            return;
        }
        let visible = (body(&self.settings_frame()).h / LINE).max(1.) as usize;
        let columns = ((body(&self.settings_frame()).w - 70.) / 7.2).max(1.) as usize;
        let Some(editor) = self.settings.editor.as_mut() else { return };
        match sym {
            // The reference is only read: these scroll it.
            keysyms::KEY_Up | keysyms::KEY_Down | keysyms::KEY_Page_Up | keysyms::KEY_Page_Down if editor.reference => {
                let down = matches!(sym, keysyms::KEY_Down | keysyms::KEY_Page_Down);
                self.settings_editor_scroll(if down { LINE * 5. } else { -LINE * 5. });
                return;
            }
            // Between lines, keeping the column.
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
            _ => {
                let key = editor.input.key(&mut self.settings.compose, sym, mods);
                if key.changed {
                    editor.changed();
                }
                let reference = editor.reference;
                self.field_clipboard(key.clipboard, Paste::Settings(self.settings.editor_epoch));
                if reference {
                    return self.schedule_render();
                }
            }
        }
        let Some(editor) = self.settings.editor.as_mut() else { return };
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
        let (hint, colour) = if !editor.error.is_empty() {
            (editor.error.as_str(), theme.danger)
        } else if !editor.notice.is_empty() {
            (editor.notice.as_str(), theme.text)
        } else {
            ("Ctrl+Enter: use draft   Ctrl+S: apply   Esc: keep for later", theme.muted)
        };
        painter.text(
            hint,
            Rect::new(area.x, frame.panel.bottom() - 61., area.w - 285., 52.),
            Style::new(11., colour).lines(3),
        );
        let pointer = Point::new(self.settings.pointer.0, self.settings.pointer.1);
        for (rect, label, _) in buttons(frame, editor.reference) {
            painter.button(rect, Button::plain(label), rect.contains(pointer));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole_file(original: &str, text: &str) -> Editor {
        Editor {
            input: TextInput { text: text.into(), ..Default::default() },
            field: None,
            first: 0,
            horizontal: 0,
            error: String::new(),
            notice: String::new(),
            stale: false,
            reference: false,
            scroll_remainder: 0.,
            original: original.into(),
        }
    }

    #[test]
    fn unstaged_file_edits_do_not_write_over_a_change_on_disk() {
        let before = "[theme]\nradius = 4\n";
        let disk = "[theme]\nradius = 4\n\n[plugins.demo]\ncommand = \"/bin/true\"\n";
        // A comment is no setting, so the edits cannot move: they stay for
        // copying, and staging or applying them is refused.
        let mut editor = whole_file(before, "[theme]\nradius = 4\n# note\n");
        editor.follow_file(disk);
        assert!(editor.stale);
        assert_eq!(editor.error, STALE);
        assert_eq!(editor.input.text, "[theme]\nradius = 4\n# note\n");
        // Another change keeps it so; going back to the old text ends it.
        editor.follow_file("[theme]\nradius = 6\n");
        assert!(editor.stale);
        editor.follow_file(before);
        assert!(!editor.stale && editor.error.is_empty());
        assert_eq!(editor.input.text, "[theme]\nradius = 4\n# note\n");

        // A changed setting is carried onto the new file.
        let mut editor = whole_file(before, "[theme]\nradius = 9\n");
        editor.follow_file(disk);
        assert!(!editor.stale);
        assert_eq!(editor.original, disk);
        assert!(editor.input.text.contains("plugins.demo") && editor.input.text.contains("radius = 9"));

        // Untouched text follows the file.
        let mut editor = whole_file(before, before);
        editor.follow_file(disk);
        assert!(!editor.stale && editor.notice.is_empty());
        assert_eq!(editor.input.text, disk);
    }
}
