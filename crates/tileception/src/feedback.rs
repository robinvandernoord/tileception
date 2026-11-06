//! "Send feedback": confirm an exact preview and optional system details
//! before opening a pre-filled GitHub issue in the browser. The issue is
//! filed under the user's own GitHub account; nothing is sent from here.

use std::{
    fmt::Write as _,
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use smithay::{backend::renderer::gles::GlesRenderer, input::keyboard::keysyms};
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    config::Mods,
    render::OutputElement,
    shapes::Shape,
    ui::{Align, Painter, PointerAction, PointerTarget, Style},
};

const NEW_ISSUE: &str = "https://github.com/robinvandernoord/tileception/issues/new";
static NEXT_DIALOG: AtomicU64 = AtomicU64::new(0);

/// The new-issue page with `title` (may be empty) and a body holding the
/// version. The `feedback` label comes from the issue template, which GitHub
/// applies for anyone; labels in the query only stick for people who may
/// label issues.
pub fn issue_url(title: &str) -> String {
    issue_url_with_body(title, &issue_body("", None))
}

fn issue_body(message: &str, details: Option<&str>) -> String {
    let mut body = format!("{}\n\n---\nTileception {}\n", message.trim_end(), env!("CARGO_PKG_VERSION"));
    if let Some(details) = details {
        body.push_str("\nSystem details:\n");
        body.push_str(details);
    }
    body
}

fn issue_url_with_body(title: &str, body: &str) -> String {
    format!("{NEW_ISSUE}?template=feedback.md&labels=feedback&title={}&body={}", encode(title), encode(body))
}

/// A snapshot of exactly what the browser will receive. Details are only
/// collected after the user checks the option, and discarded on unchecking.
pub struct Feedback {
    title: crate::text_input::TextInput,
    message: crate::text_input::TextInput,
    details: Option<String>,
    selected: Action,
    scroll: usize,
    edit_scroll: usize,
    review: bool,
    epoch: u64,
    compose: Option<smithay::input::keyboard::xkb::compose::State>,
    selecting: Option<Action>,
    edited: Instant,
    repeat_text: bool,
}

impl Feedback {
    fn body(&self) -> String {
        issue_body(&self.message.text, self.details.as_deref())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Title,
    Message,
    Details,
    Cancel,
    Review,
    Edit,
    Open,
}

impl PointerAction for Action {
    fn activate(&self, state: &mut Tileception, _rect: Rect, point: Point) {
        if matches!(self, Self::Title | Self::Message) {
            let extend = state.seat.get_keyboard().expect("seat has a keyboard").modifier_state().shift;
            state.feedback_cursor_at(*self, point, extend);
            if let Some(feedback) = &mut state.feedback {
                feedback.selecting = Some(*self);
                let input = if *self == Self::Title { &mut feedback.title } else { &mut feedback.message };
                input.anchor.get_or_insert(input.cursor);
            }
        } else {
            state.feedback_activate(*self);
        }
    }
}

struct Layout {
    panel: Rect,
    preview: Rect,
    title: Rect,
    message: Rect,
    controls: Vec<(Action, Rect)>,
}

/// Percent-encodes everything but URL-safe characters, for a query value.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

impl Tileception {
    /// Shows the confirmation without collecting optional details or opening a browser.
    pub fn send_feedback(&mut self, title: &str) {
        if self.is_locked() {
            return;
        }
        self.launcher.open = false;
        self.tray.open = false;
        self.close_power_menu();
        self.ui_pressed = None;
        self.stop_key_repeat();
        self.feedback = Some(Feedback {
            title: crate::text_input::TextInput { text: title.into(), cursor: title.len(), anchor: None },
            message: Default::default(),
            details: None,
            selected: if title.is_empty() { Action::Title } else { Action::Message },
            scroll: 0,
            edit_scroll: 0,
            review: false,
            epoch: NEXT_DIALOG.fetch_add(1, Ordering::Relaxed),
            compose: crate::ui::compose_state(),
            selecting: None,
            edited: Instant::now(),
            repeat_text: false,
        });
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    fn close_feedback(&mut self) {
        self.feedback = None;
        self.ui_pressed = None;
        self.stop_key_repeat();
        self.force_keyboard_focus_sync();
        self.schedule_render();
    }

    fn feedback_activate(&mut self, action: Action) {
        match action {
            Action::Cancel => self.close_feedback(),
            Action::Open => {
                let Some(feedback) = &self.feedback else { return };
                let url = if feedback.details.is_none() && feedback.message.text.is_empty() {
                    issue_url(&feedback.title.text)
                } else {
                    issue_url_with_body(&feedback.title.text, &feedback.body())
                };
                self.close_feedback();
                self.spawn("xdg-open", &[url]);
            }
            Action::Details => {
                let Some(feedback) = &self.feedback else { return };
                let details = feedback.details.is_none().then(|| self.feedback_system_details());
                if let Some(feedback) = &mut self.feedback {
                    feedback.details = details;
                    feedback.selected = Action::Details;
                    feedback.scroll = 0;
                }
                self.schedule_render();
            }
            Action::Review | Action::Edit => {
                if let Some(feedback) = &mut self.feedback {
                    feedback.review = action == Action::Review;
                    feedback.selected = if feedback.review { Action::Edit } else { Action::Message };
                    feedback.scroll = 0;
                    feedback.selecting = None;
                }
                self.schedule_render();
            }
            Action::Title | Action::Message => {}
        }
    }

    fn feedback_system_details(&self) -> String {
        // Read only standardized OS identity fields. Never include arbitrary
        // os-release values, output names, configuration or process environment.
        let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        let value = |key: &str| {
            release.lines().find_map(|line| {
                line.strip_prefix(key).and_then(|line| line.strip_prefix('=')).map(|value| {
                    value
                        .trim_matches(['\'', '"'])
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(120)
                        .collect::<String>()
                })
            })
        };
        let backend = if self.headless.is_some() {
            "headless"
        } else if self.nested {
            "nested"
        } else {
            "DRM/KMS"
        };
        let mut details = format!(
            "OS: {} {}\nArchitecture: {}\nBackend: {backend}\n",
            value("ID").unwrap_or_else(|| std::env::consts::OS.into()),
            value("VERSION_ID").unwrap_or_else(|| "unknown version".into()),
            std::env::consts::ARCH,
        );
        for (index, output) in self.all_outputs().iter().enumerate() {
            if let Some(mode) = output.current_mode() {
                let _ = writeln!(
                    details,
                    "Output {}: {}x{} pixels, scale {}",
                    index + 1,
                    mode.size.w,
                    mode.size.h,
                    output.current_scale().fractional_scale(),
                );
            }
        }
        details
    }

    fn feedback_layout(&self) -> Layout {
        let screen = self.output_rect();
        let width = (screen.w - 24.0).clamp(1.0, 720.0);
        let height = (screen.h - 24.0).clamp(1.0, 620.0);
        let panel = Rect::new(screen.x + (screen.w - width) / 2.0, screen.y + (screen.h - height) / 2.0, width, height);
        let left = panel.x + 16.0;
        let inner = (width - 32.0).max(1.0);
        let tight = height < 300.0;
        let top = panel.y + if tight { 44.0 } else { 80.0 };
        let bottom = panel.bottom() - if tight { 40.0 } else { 48.0 };
        let button_h = if tight { 28.0 } else { 36.0 };
        let details = Rect::new(left, bottom - 36.0, inner, 28.0);
        let title = Rect::new(left, top + 16.0, inner, 28.0);
        let message = Rect::new(left, title.bottom() + 20.0, inner, (details.y - title.bottom() - 28.0).max(1.0));
        let review = self.feedback.as_ref().is_some_and(|feedback| feedback.review);
        let mut controls = if review {
            vec![(Action::Edit, Rect::new(panel.right() - 88.0, panel.y + 12.0, 72.0, 28.0))]
        } else {
            vec![(Action::Title, title), (Action::Message, message)]
        };
        controls.extend([
            (Action::Details, details),
            (Action::Cancel, Rect::new(left, bottom, inner * 0.36, button_h)),
            (
                if review { Action::Open } else { Action::Review },
                Rect::new(left + inner * 0.36 + 8.0, bottom, inner * 0.64 - 8.0, button_h),
            ),
        ]);
        Layout {
            panel,
            title,
            message,
            preview: Rect::new(left, top, inner, (details.y - top - 20.0).max(1.0)),
            controls,
        }
    }

    pub(crate) fn feedback_pointer_target(&self, point: Point) -> Option<PointerTarget> {
        self.feedback.as_ref()?;
        self.feedback_layout().controls.into_iter().find_map(|(action, rect)| {
            rect.contains(point).then(|| {
                if matches!(action, Action::Title | Action::Message) {
                    PointerTarget::new(rect, action, smithay::input::pointer::CursorIcon::Text)
                } else {
                    PointerTarget::button(rect, action)
                }
            })
        })
    }

    /// The preview is literal text, including blank lines. Check each chunk
    /// against the renderer too, since Unicode glyphs can span multiple cells.
    fn feedback_lines(&self, width: f64) -> Vec<String> {
        let Some(feedback) = &self.feedback else { return Vec::new() };
        let preview = format!("Title:\n{}\n\nBody:\n{}", feedback.title.text, feedback.body());
        self.feedback_text_rows(&preview, width).into_iter().map(|range| preview[range].to_string()).collect()
    }

    fn feedback_text_rows(&self, text: &str, width: f64) -> Vec<Range<usize>> {
        let size = self.theme.font_size;
        let cell = self
            .text
            .borrow_mut()
            .styled("M", "monospace", size, self.theme.text, 100., 400, 0)
            .map_or(f64::from(size), |image| image.width.max(1.0));
        let columns = ((width - 16.0) / cell).floor().max(1.0) as usize;
        let mut start = 0;
        text.split('\n')
            .flat_map(|line| {
                let positions: Vec<_> = line.char_indices().map(|(offset, _)| offset).chain([line.len()]).collect();
                let rows = if line.is_empty() {
                    std::iter::once(start..start).collect()
                } else {
                    let mut rows = Vec::new();
                    let mut offset = 0;
                    while offset + 1 < positions.len() {
                        let mut count = columns.min(positions.len() - 1 - offset);
                        loop {
                            let row = &line[positions[offset]..positions[offset + count]];
                            let fits = self
                                .text
                                .borrow_mut()
                                .styled(row, "monospace", size, self.theme.text, (width - 16.0).max(1.0) as f32, 400, 0)
                                .is_some_and(|image| image.text.as_ref() == row);
                            if fits || count == 1 {
                                rows.push(start + positions[offset]..start + positions[offset + count]);
                                offset += count;
                                break;
                            }
                            count -= 1;
                        }
                    }
                    rows
                };
                start += line.len() + 1;
                rows
            })
            .collect()
    }

    pub(crate) fn feedback_scroll(&mut self, amount: i32) {
        let layout = self.feedback_layout();
        if self.feedback.as_ref().is_some_and(|feedback| !feedback.review) {
            let rows = self
                .feedback_text_rows(&self.feedback.as_ref().expect("feedback is open").message.text, layout.message.w)
                .len();
            let visible = (layout.message.h / self.feedback_line_height()).floor().max(1.0) as usize;
            if let Some(feedback) = &mut self.feedback {
                feedback.edit_scroll =
                    feedback.edit_scroll.saturating_add_signed(amount as isize).min(rows.saturating_sub(visible));
            }
            self.schedule_render();
            return;
        }
        let rows = self.feedback_lines(layout.preview.w).len();
        let visible = (layout.preview.h / self.feedback_line_height()).floor().max(1.0) as usize;
        if let Some(feedback) = &mut self.feedback {
            feedback.scroll = feedback.scroll.saturating_add_signed(amount as isize).min(rows.saturating_sub(visible));
        }
        self.schedule_render();
    }

    fn feedback_line_height(&self) -> f64 {
        f64::from(self.theme.font_size) * 1.5
    }

    pub(crate) fn feedback_key(&mut self, sym: u32, mods: Mods) {
        let layout = self.feedback_layout();
        let Some(feedback) = &mut self.feedback else { return };
        feedback.edited = Instant::now();
        feedback.repeat_text = !feedback.review && matches!(feedback.selected, Action::Title | Action::Message);
        feedback.selecting = None;
        if sym == keysyms::KEY_Escape {
            if feedback.review {
                self.feedback_activate(Action::Edit)
            } else {
                self.close_feedback()
            }
            return;
        }
        if matches!(sym, keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab) {
            let count = layout.controls.len();
            let current = layout.controls.iter().position(|(action, _)| *action == feedback.selected).unwrap_or(0);
            let next = (current + if mods.shift || sym == keysyms::KEY_ISO_Left_Tab { count - 1 } else { 1 }) % count;
            feedback.selected = layout.controls[next].0;
        } else if !feedback.review && matches!(feedback.selected, Action::Title | Action::Message) {
            let title = feedback.selected == Action::Title;
            if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) {
                let target = crate::launcher::Paste::Feedback(feedback.epoch, title);
                self.paste(target);
                return;
            }
            let input = if title { &mut feedback.title } else { &mut feedback.message };
            let word = mods.ctrl || mods.alt;
            if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C | keysyms::KEY_x | keysyms::KEY_X) {
                let selection = input.selection();
                if !selection.is_empty() {
                    let text = input.text[selection].to_string();
                    if matches!(sym, keysyms::KEY_x | keysyms::KEY_X) {
                        input.insert("");
                    }
                    self.copy_text(text);
                }
            } else {
                match sym {
                    keysyms::KEY_Left | keysyms::KEY_Right => {
                        input.move_horizontal(sym == keysyms::KEY_Right, word, mods.shift)
                    }
                    keysyms::KEY_BackSpace | keysyms::KEY_Delete => input.delete(sym == keysyms::KEY_Delete, word),
                    keysyms::KEY_a | keysyms::KEY_A if mods.ctrl => input.select_all(),
                    keysyms::KEY_u | keysyms::KEY_U if mods.ctrl => input.clear(),
                    keysyms::KEY_Home | keysyms::KEY_End => {
                        let start = if title || mods.ctrl {
                            0
                        } else {
                            input.text[..input.cursor].rfind('\n').map_or(0, |offset| offset + 1)
                        };
                        let end = if title || mods.ctrl {
                            input.text.len()
                        } else {
                            input.cursor
                                + input.text[input.cursor..].find('\n').unwrap_or(input.text.len() - input.cursor)
                        };
                        input.move_to(if sym == keysyms::KEY_Home { start } else { end }, mods.shift);
                    }
                    keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                        if title {
                            feedback.selected = Action::Message;
                        } else {
                            input.insert("\n");
                        }
                    }
                    keysyms::KEY_Up | keysyms::KEY_Down | keysyms::KEY_Page_Up | keysyms::KEY_Page_Down if !title => {
                        self.feedback_move_vertical(sym, mods.shift);
                    }
                    _ if !mods.ctrl && !mods.alt && !mods.logo => {
                        if let Some(text) = crate::ui::compose_text(&mut feedback.compose, sym) {
                            input.insert(&text);
                        }
                    }
                    _ => {}
                }
            }
        } else {
            match sym {
                keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => {
                    let action = feedback.selected;
                    self.feedback_activate(action);
                }
                keysyms::KEY_Up => self.feedback_scroll(-1),
                keysyms::KEY_Down => self.feedback_scroll(1),
                keysyms::KEY_Page_Up => self.feedback_scroll(-8),
                keysyms::KEY_Page_Down => self.feedback_scroll(8),
                keysyms::KEY_Home => self.feedback_scroll(i32::MIN),
                keysyms::KEY_End => self.feedback_scroll(i32::MAX),
                _ => {}
            }
        }
        self.feedback_keep_caret();
        self.schedule_render();
    }

    pub(crate) fn feedback_key_repeats(&self, sym: u32, mods: Mods) -> bool {
        self.feedback.as_ref().is_some_and(|feedback| {
            if feedback.repeat_text && !feedback.review && matches!(feedback.selected, Action::Title | Action::Message)
            {
                crate::key_repeat::edits(sym, mods)
            } else {
                matches!(sym, keysyms::KEY_Up | keysyms::KEY_Down | keysyms::KEY_Page_Up | keysyms::KEY_Page_Down)
            }
        })
    }

    fn feedback_move_vertical(&mut self, sym: u32, extend: bool) {
        let layout = self.feedback_layout();
        let Some(feedback) = &self.feedback else { return };
        let rows = self.feedback_text_rows(&feedback.message.text, layout.message.w);
        let current = rows.iter().rposition(|range| range.start <= feedback.message.cursor).unwrap_or(0);
        let column = feedback.message.text[rows[current].start..feedback.message.cursor].chars().count();
        let page = (layout.message.h / self.feedback_line_height()).floor().max(1.0) as usize;
        let amount = if matches!(sym, keysyms::KEY_Page_Up | keysyms::KEY_Page_Down) { page } else { 1 };
        let next = if matches!(sym, keysyms::KEY_Up | keysyms::KEY_Page_Up) {
            current.saturating_sub(amount)
        } else {
            (current + amount).min(rows.len() - 1)
        };
        let line = &feedback.message.text[rows[next].clone()];
        let offset = line.char_indices().nth(column).map_or(line.len(), |(offset, _)| offset);
        let position = rows[next].start + offset;
        if let Some(feedback) = &mut self.feedback {
            feedback.message.move_to(position, extend);
        }
    }

    fn feedback_keep_caret(&mut self) {
        let layout = self.feedback_layout();
        let Some(feedback) = &self.feedback else { return };
        if feedback.review || feedback.selected != Action::Message {
            return;
        }
        let rows = self.feedback_text_rows(&feedback.message.text, layout.message.w);
        let caret = rows.iter().rposition(|range| range.start <= feedback.message.cursor).unwrap_or(0);
        let visible = (layout.message.h / self.feedback_line_height()).floor().max(1.0) as usize;
        if let Some(feedback) = &mut self.feedback {
            if caret < feedback.edit_scroll {
                feedback.edit_scroll = caret;
            } else if caret >= feedback.edit_scroll + visible {
                feedback.edit_scroll = caret + 1 - visible;
            }
        }
    }

    pub(crate) fn feedback_insert_paste(&mut self, epoch: u64, title: bool, text: &str) {
        let Some(feedback) = &mut self.feedback else { return };
        if feedback.epoch != epoch || feedback.review {
            return;
        }
        let line: String = text.chars().map(|character| if character.is_control() { ' ' } else { character }).collect();
        let input = if title { &mut feedback.title } else { &mut feedback.message };
        input.insert(&line);
        feedback.edited = Instant::now();
        self.feedback_keep_caret();
        self.schedule_render();
    }

    fn feedback_cursor_at(&mut self, action: Action, point: Point, extend: bool) {
        let layout = self.feedback_layout();
        let Some(feedback) = &self.feedback else { return };
        let title = action == Action::Title;
        let rect = if title { layout.title } else { layout.message };
        let input = if title { &feedback.title } else { &feedback.message };
        let rows = if title {
            std::iter::once(0..input.text.len()).collect()
        } else {
            self.feedback_text_rows(&input.text, rect.w)
        };
        let index = if title {
            0
        } else {
            (feedback.edit_scroll + ((point.y - rect.y).max(0.0) / self.feedback_line_height()) as usize)
                .min(rows.len() - 1)
        };
        let range = &rows[index];
        let byte = self.text.borrow_mut().field_hit(
            &input.text[range.clone()],
            if title { &self.theme.font } else { "monospace" },
            self.theme.font_size,
            (rect.w - 16.0).max(1.0) as f32,
            input.cursor.saturating_sub(range.start).min(range.len()),
            (point.x - rect.x - 8.0) as f32,
        );
        if let Some(feedback) = &mut self.feedback {
            feedback.selected = action;
            feedback.edited = Instant::now();
            let input = if title { &mut feedback.title } else { &mut feedback.message };
            input.move_to(range.start + byte, extend);
        }
        self.feedback_keep_caret();
        self.schedule_render();
    }

    pub(crate) fn feedback_motion(&mut self, point: Point) {
        if let Some(action) = self.feedback.as_ref().and_then(|feedback| feedback.selecting) {
            self.feedback_cursor_at(action, point, true);
        }
    }

    pub(crate) fn feedback_release(&mut self) {
        if let Some(feedback) = &mut self.feedback {
            feedback.selecting = None;
        }
    }

    pub(crate) fn feedback_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let Some(feedback) = &self.feedback else { return Vec::new() };
        let layout = self.feedback_layout();
        let theme = &self.theme;
        let mut painter = Painter::new(self, renderer);
        painter.fill(self.output_rect(), crate::theme::Colour([0.0, 0.0, 0.0, 0.62]), 0.0);
        painter.frosted(layout.panel, Shape::filled(theme.popup(), theme.radius));
        let label = |top, height| Rect::new(layout.panel.x + 16.0, layout.panel.y + top, layout.panel.w - 32.0, height);
        painter.text("Send feedback", label(12.0, 32.0), Style::new(theme.font_size * 1.4, theme.text).weight(300));
        if layout.panel.h >= 300.0 {
            painter.text(
                if feedback.review {
                    "Review exactly what will open on GitHub."
                } else {
                    "Write your feedback, then review before opening GitHub."
                },
                label(44.0, 32.0),
                Style::new(theme.font_size, theme.muted).lines(2),
            );
        }
        for (action, rect) in &layout.controls {
            if matches!(action, Action::Title | Action::Message) {
                continue;
            }
            let selected = feedback.selected == *action;
            painter.shape(
                *rect,
                Shape {
                    radius: theme.radius,
                    border_width: if selected { 2.0 } else { 1.0 },
                    border: if selected { theme.accent } else { theme.border },
                    fill: (theme.panel(theme.popup()), theme.panel(theme.popup())),
                    ..Default::default()
                },
            );
            let text = match action {
                Action::Details if feedback.details.is_some() => "[x] Include system details",
                Action::Details => "[ ] Include system details",
                Action::Cancel => "Cancel",
                Action::Review => "Review",
                Action::Edit => "Edit",
                Action::Open => "Open in browser",
                Action::Title | Action::Message => unreachable!("text fields drawn separately"),
            };
            painter.text(text, *rect, Style::new(theme.font_size, theme.text).align(Align::Centre));
        }
        if !feedback.review {
            for (action, rect, label, placeholder) in [
                (Action::Title, layout.title, "Title", "Short summary"),
                (
                    Action::Message,
                    layout.message,
                    "Feedback",
                    "What happened, or what would you like Tileception to do?",
                ),
            ] {
                painter.text(
                    label,
                    Rect::new(rect.x, rect.y - 18.0, rect.w, 16.0),
                    Style::new(theme.font_size * 0.85, theme.muted),
                );
                self.draw_feedback_field(&mut painter, action, rect, placeholder);
            }
            return painter.finish();
        }
        painter.fill(layout.preview, theme.background, 4.0);
        let rows = self.feedback_lines(layout.preview.w);
        let line_height = self.feedback_line_height();
        let visible = (layout.preview.h / line_height).floor().max(1.0) as usize;
        let scroll = feedback.scroll.min(rows.len().saturating_sub(visible));
        painter.clipped(layout.preview, |painter| {
            for (index, row) in rows.iter().skip(scroll).take(visible).enumerate() {
                painter.text_font(
                    row,
                    Rect::new(
                        layout.preview.x + 8.0,
                        layout.preview.y + index as f64 * line_height,
                        layout.preview.w - 16.0,
                        line_height,
                    ),
                    Style::new(theme.font_size, theme.text),
                    "monospace",
                );
            }
        });
        painter.text(
            &format!("Read-only. Up/Down, PgUp/PgDn ({}/{})", scroll + 1, rows.len()),
            Rect::new(layout.preview.x, layout.preview.y + layout.preview.h, layout.preview.w, 24.0),
            Style::new(theme.font_size * 0.8, theme.muted),
        );
        painter.finish()
    }

    fn draw_feedback_field(&self, painter: &mut Painter<'_>, action: Action, rect: Rect, placeholder: &str) {
        let Some(feedback) = &self.feedback else { return };
        let title = action == Action::Title;
        let input = if title { &feedback.title } else { &feedback.message };
        let focused = feedback.selected == action;
        let theme = &self.theme;
        painter.shape(
            rect,
            Shape {
                radius: 4.0,
                border_width: 1.0,
                border: if focused { theme.accent } else { theme.border },
                ..Shape::filled(theme.background, 4.0)
            },
        );
        let rows = if title {
            std::iter::once(0..input.text.len()).collect()
        } else {
            self.feedback_text_rows(&input.text, rect.w)
        };
        let first = if title { 0 } else { feedback.edit_scroll.min(rows.len() - 1) };
        let height = if title { rect.h } else { self.feedback_line_height() };
        let visible = (rect.h / height).floor().max(1.0) as usize;
        let caret = rows.iter().rposition(|range| range.start <= input.cursor).unwrap_or(0);
        painter.clipped(rect, |painter| {
            if input.text.is_empty() {
                painter.text(
                    placeholder,
                    Rect::new(rect.x + 8.0, rect.y, rect.w - 16.0, height),
                    Style::new(theme.font_size, theme.muted),
                );
            }
            for (index, range) in rows.iter().enumerate().skip(first).take(visible) {
                let field = self.text.borrow_mut().field(
                    &input.text[range.clone()],
                    if title { &theme.font } else { "monospace" },
                    theme.font_size,
                    theme.text,
                    (rect.w - 16.0).max(1.0) as f32,
                    crate::text::Field {
                        cursor: input.cursor.saturating_sub(range.start).min(range.len()),
                        anchor: input
                            .anchor
                            .filter(|_| focused)
                            .map(|anchor| anchor.saturating_sub(range.start).min(range.len())),
                        visible: focused && index == caret && feedback.edited.elapsed().as_millis() % 1000 < 600,
                        accent: theme.accent.straight_rgba8(),
                    },
                );
                if let Some(image) = field {
                    painter.image(
                        &image.buffer,
                        (rect.x + 8.0, rect.y + (index - first) as f64 * height + (height - image.height) / 2.0),
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(encode("plain-text_1.0~"), "plain-text_1.0~");
    }

    #[test]
    fn fills_in_the_issue() {
        let url = issue_url("Zoom is slow");
        assert!(
            url.starts_with(&format!("{NEW_ISSUE}?template=feedback.md&labels=feedback&title=Zoom%20is%20slow&body="))
        );
        assert!(url.contains(&encode(env!("CARGO_PKG_VERSION"))));
        assert!(!url.contains(' ') && !url.contains('\n'));
    }
}
