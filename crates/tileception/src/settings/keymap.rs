//! Searchable XKB layout and variant choices, using the greeter's catalogue.
use super::*;
use crate::{greeter::keymap::Known, text_input::TextInput};

pub(super) struct Picker {
    entries: Vec<Known>,
    search: TextInput,
    selected: usize,
    first: usize,
    index: usize,
}

impl Picker {
    fn filtered(&self) -> Vec<&Known> {
        let query = self.search.text.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| {
                format!("{} {} {}", entry.description, entry.layout, entry.variant).to_lowercase().contains(&query)
            })
            .collect()
    }
}

impl Tileception {
    pub(super) fn open_keymap_picker(&mut self, index: usize) {
        let mut entries = crate::greeter::keymap::catalogue();
        entries.sort_by(|left, right| left.description.cmp(&right.description));
        entries.insert(
            0,
            Known {
                layout: String::new(),
                variant: String::new(),
                description: "System default (US international)".into(),
            },
        );
        let layout = self.settings.value("keyboard.layout").and_then(toml::Value::as_str);
        let variant = self.settings.value("keyboard.variant").and_then(toml::Value::as_str);
        let current = (layout.unwrap_or("us"), variant.unwrap_or(if layout.is_none() { "altgr-intl" } else { "" }));
        if layout.is_some() && !entries.iter().any(|entry| entry.layout == current.0 && entry.variant == current.1) {
            entries.insert(
                1,
                Known {
                    layout: current.0.into(),
                    variant: current.1.into(),
                    description: format!("Current custom layouts ({})", current.0),
                },
            );
        }
        let selected = if layout.is_none() && variant.is_none() {
            0
        } else {
            entries.iter().position(|entry| entry.layout == current.0 && entry.variant == current.1).unwrap_or(0)
        };
        let (_, list, _, _) = self.keymap_layout();
        let visible = (list.h / 38.).floor().max(1.) as usize;
        let first = selected.saturating_sub(visible / 2).min(entries.len().saturating_sub(visible));
        self.settings.keymap = Some(Picker { entries, search: TextInput::default(), selected, first, index });
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    fn keymap_layout(&self) -> (Rect, Rect, Rect, Rect) {
        let frame = self.settings_frame();
        let width = (frame.panel.w - 48.).min(700.);
        let height = (frame.panel.h - 48.).min(620.);
        let rect = Rect::new(
            frame.panel.x + (frame.panel.w - width) / 2.,
            frame.panel.y + (frame.panel.h - height) / 2.,
            width,
            height,
        );
        let list = Rect::new(rect.x + 16., rect.y + 100., rect.w - 32., rect.h - 166.);
        (
            rect,
            list,
            Rect::new(rect.right() - 120., rect.bottom() - 48., 100., 30.),
            Rect::new(rect.x + 20., rect.bottom() - 48., 170., 30.),
        )
    }

    fn choose_keymap(&mut self, selected: usize) {
        let Some(picker) = self.settings.keymap.take() else { return };
        if let Some(entry) = picker.filtered().get(selected) {
            self.settings.draft[picker.index] =
                (!entry.layout.is_empty()).then(|| toml::Value::String(entry.layout.clone()));
            if let Some(index) = FIELDS.iter().position(|field| field.key == "keyboard.variant") {
                self.settings.draft[index] =
                    (!entry.variant.is_empty()).then(|| toml::Value::String(entry.variant.clone()));
            }
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    pub(super) fn keymap_click(&mut self, point: Point) {
        let (_, list, close, custom) = self.keymap_layout();
        if close.contains(point) {
            self.settings.keymap = None;
        } else if custom.contains(point) {
            if let Some(picker) = self.settings.keymap.take() {
                let text =
                    self.settings.draft[picker.index].as_ref().and_then(toml::Value::as_str).unwrap_or("").to_string();
                self.settings.input = TextInput { cursor: text.len(), text, anchor: None };
                self.settings.editing = Some(picker.index);
                self.settings.focus = Some(Hit::Field(picker.index));
            }
        } else if list.contains(point) {
            let first = self.settings.keymap.as_ref().map_or(0, |picker| picker.first);
            let offset = ((point.y - list.y) / 38.) as usize;
            if (offset + 1) as f64 * 38. <= list.h {
                self.choose_keymap(first + offset);
            }
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    pub(super) fn keymap_scroll(&mut self, amount: f64) {
        let (_, list, _, _) = self.keymap_layout();
        if let Some(picker) = &mut self.settings.keymap {
            let maximum = picker.filtered().len().saturating_sub((list.h / 38.).floor() as usize);
            picker.first = (picker.first as f64 + (amount / 38.).round()).clamp(0., maximum as f64) as usize;
        }
        self.schedule_render();
    }

    pub(super) fn keymap_paste(&mut self, text: &str) {
        if let Some(picker) = &mut self.settings.keymap {
            picker.search.insert(text.trim());
            picker.selected = 0;
            picker.first = 0;
        }
        self.schedule_render();
    }

    pub(super) fn keymap_key(&mut self, sym: u32, mods: Mods) {
        let (_, list, _, _) = self.keymap_layout();
        let Some(picker) = &mut self.settings.keymap else { return };
        let count = picker.filtered().len();
        if sym == keysyms::KEY_Escape {
            self.settings.keymap = None;
            self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        } else if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) {
            self.paste(crate::launcher::Paste::Settings(self.settings.editor_epoch));
        } else if sym == keysyms::KEY_Return && count > 0 {
            let selected = picker.selected;
            self.choose_keymap(selected);
        } else if matches!(sym, keysyms::KEY_Up | keysyms::KEY_Down) && count > 0 {
            picker.selected = if sym == keysyms::KEY_Down {
                (picker.selected + 1) % count
            } else {
                (picker.selected + count - 1) % count
            };
            let visible = (list.h / 38.).floor().max(1.) as usize;
            picker.first = picker.first.min(picker.selected).max(picker.selected.saturating_sub(visible - 1));
        } else {
            super::sections::edit_key(&mut picker.search, sym, mods);
            picker.first = 0;
            picker.selected = 0;
        }
        self.schedule_render();
    }

    pub(super) fn draw_keymap_picker(&self, painter: &mut Painter<'_>) {
        let Some(picker) = &self.settings.keymap else { return };
        let (rect, list, close, custom) = self.keymap_layout();
        let theme = &self.theme;
        painter.fill(self.settings_frame().panel, Colour([0., 0., 0., 0.6]), 10.);
        painter.shape(rect, Shape { border_width: 1., border: theme.border, ..Shape::filled(theme.background, 8.) });
        painter.text(
            "Keyboard layout",
            Rect::new(rect.x + 20., rect.y + 12., rect.w - 40., 32.),
            Style::new(22., theme.text).weight(500),
        );
        let search = Rect::new(rect.x + 16., rect.y + 54., rect.w - 32., 34.);
        painter.shape(search, Shape { border_width: 1., border: theme.accent, ..Shape::filled(theme.surface, 5.) });
        painter.glyph(glyph::SEARCH, Rect::new(search.x + 10., search.y + 8., 18., 18.), 16, theme.muted);
        if picker.search.text.is_empty() {
            painter.text(
                "Search layouts and variants...",
                Rect::new(search.x + 36., search.y, search.w - 48., search.h),
                Style::new(12., theme.muted),
            );
        }
        if let Some(image) = self.text.borrow_mut().field(
            &picker.search.text,
            &theme.font,
            12.,
            theme.text,
            (search.w - 48.) as f32,
            crate::text::Field {
                cursor: picker.search.cursor,
                anchor: picker.search.anchor,
                visible: true,
                accent: theme.accent.straight_rgba8(),
            },
        ) {
            painter.image(&image.buffer, (search.x + 36., search.y + (search.h - image.height) / 2.));
        }
        let filtered = picker.filtered();
        for (index, entry) in filtered.iter().enumerate().skip(picker.first).take((list.h / 38.).floor() as usize) {
            let row = Rect::new(list.x, list.y + (index - picker.first) as f64 * 38., list.w, 36.);
            if index == picker.selected {
                painter.fill(row, theme.accent.alpha(0.12), 4.);
            }
            painter.text(
                &entry.description,
                Rect::new(row.x + 10., row.y, row.w - 100., row.h),
                Style::new(12., theme.text),
            );
            painter.text(&entry.layout, Rect::new(row.right() - 80., row.y, 70., row.h), Style::new(11., theme.muted));
        }
        if filtered.is_empty() {
            painter.text("No matching layouts", list, Style::new(12., theme.muted));
        }
        for (button, label) in [(close, "Cancel"), (custom, "Custom layouts...")] {
            painter.fill(button, theme.surface_raised, 5.);
            painter.text(label, button, Style::new(12., theme.text).align(Align::Centre));
        }
    }
}
