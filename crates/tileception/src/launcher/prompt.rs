//! A plugin's prompt in the launcher, in place of the results: steps that
//! ask for input, such as a passphrase, or wait for something, such as a
//! security key being touched. Each step calls one of the plugin's tools,
//! whose answer moves to another step, runs an action or ends the prompt.
//! Secret fields are masked, kept in [`SecretField`] and wiped once sent.

use serde_json::Value;
use smithay::input::keyboard::keysyms;
use tileception_core::{Point, Rect};

use super::PointerAction;
use crate::{
    Tileception,
    password::SecretField,
    plugins::{ActionKind, Arguments, Place, Prompt, Row, Step, StepKind},
    shapes::Shape,
    text_input::TextInput,
    ui::{Button, Look, Painter, Style, glyph},
};

pub(super) struct PromptView {
    plugin: String,
    prompt: Prompt,
    /// The result it was opened from, which says Copied when the prompt
    /// ends in a copy.
    origin: Option<Row>,
    step: usize,
    /// Which step entered this is, counting all prompts; pastes carry it.
    serial: u64,
    /// The current step's fields, in order.
    inputs: Vec<Input>,
    focus: usize,
    /// The tool call in flight, by its launcher generation.
    pending: Option<u64>,
    started: std::time::Instant,
    /// What the plugin said when it moved to this step.
    notice: String,
    error: String,
}

impl PromptView {
    fn current(&self) -> &Step {
        &self.prompt.steps[self.step]
    }

    /// Whether this tool call's answer is for the prompt.
    pub(super) fn awaits(&self, generation: u64) -> bool {
        self.pending == Some(generation)
    }

    /// The submit button's label, when the step has one now.
    fn submit_label(&self) -> Option<&str> {
        match &self.current().kind {
            StepKind::Fields { submit, .. } => Some(submit.as_deref().unwrap_or("Continue")),
            StepKind::Wait if self.pending.is_none() => Some("Try again"),
            StepKind::Wait => None,
        }
    }
}

enum Input {
    Text(TextInput),
    /// Boxed: its fixed buffer is large.
    Secret(Box<SecretField>),
}

/// What the pointer can do in a prompt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Target {
    Field(usize),
    Submit,
    Alternate,
}

struct PromptLayout {
    icon: Rect,
    title: Rect,
    detail: Option<Rect>,
    text: Option<Rect>,
    message: Option<Rect>,
    /// Each field's label and box.
    fields: Vec<(Rect, Rect)>,
    busy: Option<Rect>,
    submit: Option<Rect>,
    alternate: Option<Rect>,
    bottom: f64,
}

const HEADER_H: f64 = 56.0;
const TEXT_H: f64 = 40.0;
const MESSAGE_H: f64 = 38.0;
const LABEL_H: f64 = 22.0;
const INPUT_H: f64 = 38.0;
const BUSY_H: f64 = 30.0;
const BUTTON_H: f64 = 34.0;

fn button_width(label: &str) -> f64 {
    (label.chars().count() as f64 * 7.5 + 36.0).max(96.0)
}

fn prompt_layout(view: &PromptView, area: Rect) -> PromptLayout {
    let left = area.x + 14.0;
    let width = area.w - 28.0;
    let mut top = area.y + 4.0;
    let icon = Rect::new(left, top + (HEADER_H - 32.0) / 2.0, 32.0, 32.0);
    let text_x = icon.right() + 14.0;
    let text_w = area.right() - 14.0 - text_x;
    let (title, detail) = if view.prompt.detail.is_empty() {
        (Rect::new(text_x, top, text_w, HEADER_H), None)
    } else {
        (Rect::new(text_x, top + 7.0, text_w, 22.0), Some(Rect::new(text_x, top + 29.0, text_w, 18.0)))
    };
    top += HEADER_H + 4.0;
    let mut take = |height: f64| {
        let rect = Rect::new(left, top, width, height);
        top += height;
        rect
    };
    let step = view.current();
    let text = (!step.text.is_empty()).then(|| take(TEXT_H));
    let message = (!view.error.is_empty() || !view.notice.is_empty()).then(|| take(MESSAGE_H));
    let mut fields: Vec<(Rect, Rect)> = view
        .inputs
        .iter()
        .map(|_| {
            let label = take(LABEL_H);
            let input = take(INPUT_H);
            take(8.0);
            (label, input)
        })
        .collect();
    // A lone field with nothing but a submit button shares its row with it.
    if let ([(_, input)], Some(label), None) = (fields.as_mut_slice(), view.submit_label(), &step.alternate) {
        let button = button_width(label);
        input.w -= button + 10.0;
        let submit = Rect::new(input.right() + 10.0, input.y, button, input.h);
        return PromptLayout {
            icon,
            title,
            detail,
            text,
            message,
            fields,
            busy: None,
            submit: Some(submit),
            alternate: None,
            bottom: top + 2.0,
        };
    }
    let busy = matches!(step.kind, StepKind::Wait).then(|| take(BUSY_H));
    take(6.0);
    let row = take(BUTTON_H);
    let mut right = row.right();
    let submit = view.submit_label().map(|label| {
        let width = button_width(label);
        right -= width;
        Rect::new(right, row.y, width, row.h)
    });
    if submit.is_some() {
        right -= 10.0;
    }
    let alternate = step.alternate.as_ref().map(|alternate| {
        let width = button_width(&alternate.title);
        Rect::new(right - width, row.y, width, row.h)
    });
    PromptLayout { icon, title, detail, text, message, fields, busy, submit, alternate, bottom: top + 10.0 }
}

impl Tileception {
    /// Shows a plugin's prompt instead of its results, on its first step.
    pub(super) fn open_prompt(&mut self, plugin: String, prompt: Prompt, origin: Option<Row>) {
        // From a result's actions, Esc comes back to the results themselves.
        if self.launcher.plugin_actions.take().is_some() {
            self.launcher.entries = self.launcher_entries();
            self.launcher.selected = origin
                .as_ref()
                .and_then(|origin| {
                    self.launcher
                        .entries
                        .iter()
                        .position(|entry| matches!(entry, super::Entry::Plugin { row, .. } if row == origin))
                })
                .unwrap_or(0);
            self.launcher.first = 0;
        }
        self.launcher.prompt = Some(PromptView {
            plugin,
            prompt,
            origin,
            step: 0,
            serial: 0,
            inputs: Vec::new(),
            focus: 0,
            pending: None,
            started: std::time::Instant::now(),
            notice: String::new(),
            error: String::new(),
        });
        self.prompt_enter_step(0, String::new());
    }

    /// Goes back to the results, cancelling what is in flight. The fields
    /// are wiped as they go.
    pub(super) fn leave_prompt(&mut self) {
        if let Some(view) = self.launcher.prompt.take()
            && view.pending.is_some()
            && let Some(plugins) = &self.plugins
        {
            plugins.cancel(Place::Launcher);
        }
    }

    fn prompt_enter_step(&mut self, index: usize, notice: String) {
        let cancel = self.launcher.prompt.as_ref().is_some_and(|view| view.pending.is_some());
        if cancel && let Some(plugins) = &self.plugins {
            plugins.cancel(Place::Launcher);
        }
        self.launcher.prompt_serial += 1;
        let serial = self.launcher.prompt_serial;
        let Some(view) = self.launcher.prompt.as_mut() else { return };
        view.step = index;
        view.serial = serial;
        view.pending = None;
        view.focus = 0;
        view.notice = notice;
        view.error.clear();
        view.inputs = match &view.current().kind {
            StepKind::Fields { fields, .. } => fields
                .iter()
                .map(
                    |field| {
                        if field.secret { Input::Secret(Box::default()) } else { Input::Text(TextInput::default()) }
                    },
                )
                .collect(),
            StepKind::Wait => Vec::new(),
        };
        if matches!(view.current().kind, StepKind::Wait) {
            self.prompt_call();
        }
    }

    /// Calls the step's tool, with the fields' values added to its
    /// arguments. Secret values leave their fields here and are wiped once
    /// sent.
    fn prompt_call(&mut self) {
        let Some(plugins) = &self.plugins else { return };
        let Some(view) = self.launcher.prompt.as_mut() else { return };
        let step = &view.prompt.steps[view.step];
        let secret = view.inputs.iter().any(|input| matches!(input, Input::Secret(_)));
        let mut arguments =
            if secret { Arguments::secret(step.arguments.clone()) } else { Arguments::from(step.arguments.clone()) };
        if let StepKind::Fields { fields, .. } = &step.kind
            && let Value::Object(map) = arguments.value_mut()
        {
            for (field, input) in fields.iter().zip(&mut view.inputs) {
                let value = match input {
                    Input::Text(input) => input.text.clone(),
                    Input::Secret(input) => String::from(input.take().as_str()),
                };
                map.insert(field.name.clone(), Value::String(value));
            }
        }
        view.error.clear();
        view.started = std::time::Instant::now();
        view.pending = Some(plugins.prompt_tool(view.plugin.clone(), step.tool.clone(), arguments, step.timeout()));
    }

    /// Enter, or the submit button: submits the fields, or tries a wait again.
    fn prompt_submit(&mut self) {
        let Some(view) = self.launcher.prompt.as_mut() else { return };
        if view.pending.is_some() {
            return;
        }
        view.notice.clear();
        self.prompt_call();
    }

    /// Ctrl+Enter, or the alternate button.
    fn prompt_alternate(&mut self) {
        let Some(view) = self.launcher.prompt.as_ref() else { return };
        let Some(index) =
            view.current().alternate.as_ref().and_then(|alternate| view.prompt.step_index(&alternate.step))
        else {
            return;
        };
        self.prompt_enter_step(index, String::new());
    }

    /// The plugin's answer to the step's tool.
    pub(super) fn prompt_result(&mut self, result: Result<Value, String>) {
        let Some(view) = self.launcher.prompt.as_mut() else { return };
        view.pending = None;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                view.error = error;
                // Secrets were taken when sent; type them again.
                if let Some(index) = view.inputs.iter().position(|input| matches!(input, Input::Secret(_))) {
                    view.focus = index;
                }
                return;
            }
        };
        if let Some(step_id) = value.get("step").and_then(Value::as_str) {
            let notice = value.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
            match view.prompt.step_index(step_id) {
                Some(index) => self.prompt_enter_step(index, notice),
                None => view.error = format!("The plugin asked for a step it does not have: {step_id}"),
            }
            return;
        }
        if let Some(action) = value.get("action") {
            match serde_json::from_value::<ActionKind>(action.clone()) {
                Ok(kind) if kind.after_prompt() => {
                    let plugin = view.plugin.clone();
                    let origin = view.origin.clone();
                    let copies = matches!(kind, ActionKind::Copy { .. });
                    self.leave_prompt();
                    if copies && let Some(origin) = origin {
                        self.launcher.copied_row = Some((origin, std::time::Instant::now()));
                    }
                    self.launcher_action(Some(plugin), kind);
                }
                _ => view.error = "The plugin answered with an action the launcher cannot run".into(),
            }
            return;
        }
        // Done: fresh results, which may well have changed.
        self.leave_prompt();
        self.refresh_launcher();
    }

    pub(super) fn prompt_key(&mut self, sym: u32, mods: crate::config::Mods) {
        let enter = matches!(sym, keysyms::KEY_Return | keysyms::KEY_KP_Enter);
        if sym == keysyms::KEY_Escape {
            return self.leave_prompt();
        }
        if enter && mods.ctrl {
            return self.prompt_alternate();
        }
        let Some(view) = self.launcher.prompt.as_mut() else { return };
        if matches!(view.current().kind, StepKind::Wait) || view.pending.is_some() {
            if enter {
                self.prompt_submit();
            }
            return;
        }
        let count = view.inputs.len();
        let backwards =
            sym == keysyms::KEY_ISO_Left_Tab || sym == keysyms::KEY_Up || (sym == keysyms::KEY_Tab && mods.shift);
        match sym {
            _ if enter && view.focus + 1 < count => view.focus += 1,
            _ if enter => self.prompt_submit(),
            keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab | keysyms::KEY_Up | keysyms::KEY_Down => {
                view.focus = if backwards { (view.focus + count - 1) % count } else { (view.focus + 1) % count };
            }
            _ if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) => {
                // For this field of this step only, however late it arrives.
                let target = super::Paste::Prompt(view.serial, view.focus);
                self.paste(target);
            }
            _ => match view.inputs.get_mut(view.focus) {
                // Copying and cutting are not among a secret's keys.
                Some(Input::Secret(field)) => {
                    field.edit_key(sym, mods);
                }
                Some(Input::Text(input)) => {
                    if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C | keysyms::KEY_x | keysyms::KEY_X) {
                        let range = input.selection();
                        if !range.is_empty() {
                            let text = input.text[range].to_owned();
                            if matches!(sym, keysyms::KEY_x | keysyms::KEY_X) {
                                input.insert("");
                            }
                            self.copy_text(text);
                        }
                    } else {
                        crate::settings::sections::edit_key(input, &mut self.launcher.compose, sym, mods);
                    }
                }
                None => {}
            },
        }
    }

    /// A paste for a prompt's field, one line only, if that field of that
    /// step is still there and not being sent. Otherwise it goes nowhere.
    pub(super) fn prompt_paste(&mut self, serial: u64, field: usize, text: &str) {
        let Some(view) = self.launcher.prompt.as_mut().filter(|view| view.serial == serial && view.pending.is_none())
        else {
            return;
        };
        match view.inputs.get_mut(field) {
            Some(Input::Secret(field)) => {
                if !field.insert(text) {
                    view.error = "Paste a single line of at most 4096 bytes".into();
                }
            }
            Some(Input::Text(input)) => {
                let text: String = text
                    .trim_end_matches(['\r', '\n'])
                    .chars()
                    .map(|character| if character.is_control() { ' ' } else { character })
                    .collect();
                input.insert(&text);
            }
            None => {}
        }
    }

    pub(super) fn prompt_activate(&mut self, target: Target) {
        match target {
            Target::Field(index) => {
                if let Some(view) = self.launcher.prompt.as_mut().filter(|view| view.pending.is_none()) {
                    view.focus = index.min(view.inputs.len().saturating_sub(1));
                    self.launcher.edited = std::time::Instant::now();
                }
            }
            Target::Submit => self.prompt_submit(),
            Target::Alternate => self.prompt_alternate(),
        }
    }

    /// How tall the prompt is at this width.
    pub(super) fn prompt_height(&self, width: f64) -> Option<f64> {
        let view = self.launcher.prompt.as_ref()?;
        Some(prompt_layout(view, Rect::new(0.0, 0.0, width, 0.0)).bottom)
    }

    pub(super) fn prompt_pointer_target(&self, area: Rect, point: Point) -> Option<crate::ui::PointerTarget> {
        let view = self.launcher.prompt.as_ref()?;
        let layout = prompt_layout(view, area);
        if view.pending.is_none()
            && let Some(index) = layout.fields.iter().position(|(_, input)| input.contains(point))
        {
            return Some(crate::ui::PointerTarget::new(
                layout.fields[index].1,
                PointerAction::Prompt(Target::Field(index)),
                smithay::input::pointer::CursorIcon::Text,
            ));
        }
        [(layout.submit, Target::Submit), (layout.alternate, Target::Alternate)].into_iter().find_map(
            |(rect, target)| {
                let rect = rect.filter(|rect| rect.contains(point))?;
                Some(crate::ui::PointerTarget::button(rect, PointerAction::Prompt(target)))
            },
        )
    }

    /// The keys along the bottom while a prompt shows.
    pub(super) fn prompt_hints(&self) -> Vec<(&str, &str)> {
        let Some(view) = self.launcher.prompt.as_ref() else { return Vec::new() };
        let mut hints = Vec::new();
        if view.inputs.len() > 1 && view.pending.is_none() {
            hints.push(("Tab", "Next field"));
        }
        if view.pending.is_none()
            && let Some(label) = view.submit_label()
        {
            hints.push(("Enter", label));
        }
        if let Some(alternate) = &view.current().alternate {
            hints.push(("Ctrl+Enter", alternate.title.as_str()));
        }
        hints.push(("Esc", "Back"));
        hints
    }

    pub(super) fn draw_prompt(&self, painter: &mut Painter, area: Rect) {
        let Some(view) = self.launcher.prompt.as_ref() else { return };
        let theme = &self.theme;
        let layout = prompt_layout(view, area);
        let step = view.current();

        let named = view.prompt.icon.as_deref();
        if let Some(buffer) =
            named.filter(|name| !name.starts_with("glyph:")).and_then(|name| self.launcher.icon(name, 32))
        {
            painter.image(&buffer, (layout.icon.x, layout.icon.y));
        } else {
            painter.fill(layout.icon, theme.surface_raised, theme.radius);
            let symbol = named.and_then(glyph::named).unwrap_or(glyph::PLUG);
            painter.glyph(symbol, layout.icon, 18, theme.accent);
        }
        painter.text(&view.prompt.title, layout.title, Style::new(theme.font_size * 1.05, theme.text).bold());
        if let Some(rect) = layout.detail {
            painter.text(&view.prompt.detail, rect, Style::new(theme.font_size * 0.92, theme.muted));
        }
        if let Some(rect) = layout.text {
            painter.text(&step.text, rect, Style::new(theme.font_size, theme.text).lines(2));
        }
        if let Some(rect) = layout.message {
            let (message, colour) =
                if view.error.is_empty() { (&view.notice, theme.accent) } else { (&view.error, theme.danger) };
            painter.text(message, rect, Style::new(theme.font_size * 0.92, colour).lines(2));
        }

        let typing = view.pending.is_none();
        let blink = self.launcher.edited.elapsed().as_millis() % 1000 < 600;
        let size = theme.font_size;
        if let StepKind::Fields { fields, .. } = &step.kind {
            for (index, ((label, rect), (field, input))) in
                layout.fields.iter().zip(fields.iter().zip(&view.inputs)).enumerate()
            {
                let focused = typing && view.focus == index;
                let name = if field.label.is_empty() { &field.name } else { &field.label };
                painter.text(name, *label, Style::new(size * 0.9, theme.muted));
                painter.shape(
                    *rect,
                    Shape {
                        border_width: 1.0,
                        border: if focused { theme.accent } else { theme.border },
                        ..Shape::filled(theme.background, theme.radius)
                    },
                );
                // A secret is drawn as one mask character each, so it never
                // reaches the text cache.
                let mask;
                let (text, cursor, anchor) = match input {
                    Input::Text(input) => (input.text.as_str(), input.cursor, input.anchor),
                    Input::Secret(secret) => {
                        // One mask character per secret character, so the
                        // caret and selection line up; the field scrolls.
                        mask = "*".repeat(secret.chars());
                        let (cursor, anchor) = secret.caret();
                        (mask.as_str(), cursor, anchor)
                    }
                };
                let caret = crate::text::Field {
                    cursor,
                    anchor: anchor.filter(|_| focused),
                    visible: focused && blink,
                    accent: theme.accent.straight_rgba8(),
                };
                let colour = if typing { theme.text } else { theme.muted };
                if let Some(image) =
                    self.text.borrow_mut().field(text, &theme.font, size, colour, (rect.w - 24.0) as f32, caret)
                {
                    painter.image(&image.buffer, (rect.x + 12.0, rect.y + (rect.h - image.height) / 2.0));
                }
            }
        }
        if let Some(rect) = layout.busy
            && view.pending.is_some()
        {
            let dots = ".".repeat(1 + (view.started.elapsed().as_millis() / 400 % 3) as usize);
            painter.text(&format!("Waiting{dots}"), rect, Style::new(size * 0.92, theme.muted));
        }

        let buttons = [
            (layout.alternate, step.alternate.as_ref().map(|alternate| alternate.title.as_str()), false),
            (layout.submit, view.submit_label(), true),
        ];
        for (rect, label, primary) in buttons {
            let (Some(rect), Some(label)) = (rect, label) else { continue };
            let label = if primary && !typing { "Working..." } else { label };
            let look = if primary { Look::Primary } else { Look::Plain };
            let hover = rect.contains(Point::new(self.pointer.x, self.pointer.y));
            painter.button(rect, Button::new(label, look).enabled(typing || !primary), hover);
        }
    }
}
