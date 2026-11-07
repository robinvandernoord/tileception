//! Native collection screens. The source item travels with each row so moving
//! or changing an entry retains comments rather than materialising defaults.

mod displays;
mod draw;
pub(super) use draw::draw_scrollbar;
pub(super) mod model;

use super::*;
use crate::{launcher::Paste, text_input::TextInput};
use model::{Field, Form, Kind, Path, Row, Screen};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Row(usize),
    Field(usize),
    /// One option of a choice shown as buttons.
    Choose(usize, usize),
    Add,
    Remove,
    Up,
    Down,
    AddValue,
    RemoveValue(usize),
    Pick(usize),
    Record,
    Identify,
    /// Full-edge crossing between screens, kept with the arrangement.
    Edges,
    Automatic,
    Reveal,
    Generate,
    Advanced,
    Search,
    Cancel,
    Back,
    Stage,
    Touchscreens,
}

struct Layout {
    panel: Rect,
    list: Rect,
    body: Rect,
    map: Option<Rect>,
    hits: Vec<(Rect, Action)>,
    fields: Vec<(Rect, Field)>,
    max_scroll: f64,
}

#[derive(Clone, PartialEq)]
enum PointerAction {
    Action(Action),
    Choose { field: usize, value: String },
    Search,
    Dismiss,
    Monitor(usize),
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _: Rect, point: Point) {
        match self {
            Self::Action(action) => state.section_activate(*action),
            Self::Choose { field, value } => state.section_choose_value(*field, value),
            Self::Search => {}
            Self::Dismiss => {
                if let Some(screen) = &mut state.settings.section {
                    screen.menu = None;
                }
            }
            Self::Monitor(row) => {
                if let Some(area) = state.section_layout().map {
                    state.section_map_start(*row, point, area);
                }
            }
        }
        state.schedule_render();
    }
}

struct Menu {
    field: usize,
    options: Vec<(String, String)>,
    selected: usize,
    first: usize,
    filter: TextInput,
    picker: bool,
    icons: std::collections::BTreeMap<String, String>,
}

impl Tileception {
    pub(super) fn section_text_caret(&mut self, point: Point, extend: bool) {
        let layout = self.section_layout();
        let Some(screen) = self.settings.section.as_mut() else { return };
        let Some((field, input)) = &mut screen.edit else { return };
        let Some((rect, _)) =
            layout.fields.iter().find(|(_, candidate)| candidate.path == field.path && candidate.label == field.label)
        else {
            return;
        };
        let secret = matches!(field.form, Form::Secret) && !screen.reveal;
        let shown = if secret { "•".repeat(input.text.chars().count()) } else { input.text.clone() };
        let cursor = if secret { input.text[..input.cursor].chars().count() * "•".len() } else { input.cursor };
        let position = self.text.borrow_mut().field_hit(
            &shown,
            &self.theme.font,
            11.,
            (rect.w - 20.) as f32,
            cursor,
            (point.x - rect.x - 10.).max(0.) as f32,
        );
        let position = if secret {
            input
                .text
                .char_indices()
                .nth(shown[..position].chars().count())
                .map_or(input.text.len(), |(offset, _)| offset)
        } else {
            position
        };
        input.move_to(position, extend);
    }

    pub(super) fn section_pointer_target(&self, point: Point) -> Option<interaction::Target> {
        use smithay::input::pointer::CursorIcon;
        let screen = self.settings.section.as_ref()?;
        let layout = self.section_layout();
        if let Some(menu) = &screen.menu {
            let rect = self.section_menu_rect(&layout);
            if rect.contains(point) {
                if point.y < rect.y + 44. && menu.picker {
                    return Some(interaction::Target::new(
                        Rect::new(rect.x, rect.y, rect.w, 44.),
                        PointerAction::Search,
                        CursorIcon::Text,
                    ));
                }
                let offset = ((point.y - rect.y - 44.) / 32.).floor();
                if offset >= 0. && menu.first + (offset as usize) < menu.filtered().len() {
                    return Some(interaction::Target::button(
                        Rect::new(rect.x, rect.y + 44. + offset * 32., rect.w, 32.),
                        PointerAction::Choose {
                            field: menu.field,
                            value: menu.filtered()[menu.first + offset as usize].1.to_string(),
                        },
                    ));
                }
            }
            return Some(interaction::Target::new(
                self.settings_frame().panel,
                PointerAction::Dismiss,
                CursorIcon::Default,
            ));
        }
        if !layout.panel.contains(point) {
            return self.settings_main_target(point);
        }
        if let Some(map) = layout.map
            && map.contains(point)
        {
            let equal = self.settings.draft[edges()].as_ref().and_then(toml::Value::as_bool).unwrap_or(false);
            let (origin, factor) = screen.display_map(map, equal);
            return screen.display_map_rects(equal).iter().rev().find_map(|(index, rect)| {
                let shown =
                    Rect::new(origin.x + rect.x * factor, origin.y + rect.y * factor, rect.w * factor, rect.h * factor);
                shown
                    .contains(point)
                    .then(|| interaction::Target::new(shown, PointerAction::Monitor(*index), CursorIcon::Grab))
            });
        }
        layout.hits.iter().rev().find_map(|(rect, action)| {
            if !rect.contains(point) {
                return None;
            }
            let cursor = match action {
                Action::Search => CursorIcon::Text,
                Action::Field(index)
                    if layout
                        .fields
                        .get(*index)
                        .is_some_and(|(_, field)| !matches!(field.form, Form::Toggle | Form::Choice(_))) =>
                {
                    CursorIcon::Text
                }
                _ => CursorIcon::Pointer,
            };
            let action = PointerAction::Action(*action);
            Some(if cursor == CursorIcon::Pointer {
                interaction::Target::button(*rect, action)
            } else {
                interaction::Target::new(*rect, action, cursor)
            })
        })
    }

    pub(super) fn section_is_page(&self) -> bool {
        self.settings.section.as_ref().is_some_and(|screen| {
            matches!((self.settings.page, screen.kind), (Some(8), Kind::Displays) | (Some(9), Kind::Sharing))
        })
    }

    pub(super) fn open_settings_page_section(&mut self) {
        if self.settings.page == Some(5) {
            self.settings_home_arrive();
        }
        if self.settings.section.is_some() {
            return;
        }
        let key = match self.settings.page {
            Some(8) => "outputs",
            Some(9) => "handoff",
            _ => return,
        };
        if let Some(index) = FIELDS.iter().position(|field| field.key == key) {
            self.open_settings_section(index);
        }
    }

    pub(super) fn open_settings_section(&mut self, index: usize) {
        let result = (|| {
            if let Some(position) = self.settings.parked_sections.iter().position(|screen| screen.field == index) {
                return Ok(self.settings.parked_sections.remove(position));
            }
            let source = self.settings_document()?;
            let mut screen = Screen::new(index, &source)?;
            self.section_detected_displays(&mut screen);
            Ok::<_, String>(screen)
        })();
        match result {
            Ok(mut screen) => {
                screen.error = screen.validate_displays().err().unwrap_or_default();
                self.settings.edges_before =
                    (screen.kind == Kind::Displays).then(|| self.settings.draft[edges()].clone());
                self.settings.section = Some(screen);
                self.settings.dropdown = None;
                self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
            }
            Err(error) => {
                self.settings.message = error;
                self.settings.error = true;
            }
        }
        self.schedule_render();
    }

    fn section_back(&mut self, discard: bool) {
        self.section_cancel_recording();
        if discard && let Some(before) = self.settings.edges_before.take() {
            self.settings.draft[edges()] = before;
        }
        if !discard {
            self.settings_leave_section();
        }
        self.settings.section = None;
        self.open_settings_page_section();
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    pub(super) fn section_keep_back(&mut self) {
        self.section_back(false);
    }

    /// Navigation keeps unfinished input, including invalid input, in memory.
    pub(super) fn settings_leave_section(&mut self) {
        if self.settings.section.is_none() {
            return;
        }
        self.section_cancel_recording();
        if !self.section_stage(false)
            && let Some(screen) = self.settings.section.take()
        {
            self.settings.parked_sections.push(screen);
            self.settings.message = "Unfinished draft kept. Reopen its screen to finish it before Apply.".into();
            self.settings.error = false;
        } else {
            self.settings.message = "Draft kept. Apply saves it; closing Settings keeps it for later.".into();
        }
    }

    pub(super) fn section_stage(&mut self, apply: bool) -> bool {
        let Some(mut screen) = self.settings.section.take() else { return true };
        let result = (|| {
            screen.finish_input()?;
            screen.validate_displays()?;
            if !screen.changed() {
                return Ok(());
            }
            let current = self.settings_document()?;
            let key = FIELDS[screen.field].key;
            if crate::config_edit::section_text(&current, key)? != screen.original {
                return Err("This section changed while hidden. Cancel and reopen it to load the current draft.".into());
            }
            let excerpt = screen.source()?;
            let document = crate::config_edit::splice_section(&current, key, &excerpt)?;
            let config = crate::config::Config::parse(&document)?;
            self.settings.draft = values(&config.file)?;
            self.settings.document_draft = document;
            Ok::<_, String>(())
        })();
        let success = result.is_ok();
        match result {
            Ok(()) => {
                self.settings.message.clear();
                self.settings.error = false;
                if apply {
                    self.settings_activate(Hit::Apply, None);
                    self.open_settings_page_section();
                }
            }
            Err(error) => {
                screen.error = error.lines().last().unwrap_or(&error).to_string();
                self.settings.section = Some(screen);
            }
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
        success
    }

    fn section_activate(&mut self, action: Action) {
        if action != Action::Record {
            self.section_cancel_recording();
        }
        let list_height = self.section_layout().list.h;
        if action == Action::Touchscreens {
            if self.section_stage(false)
                && let Some(index) = FIELDS.iter().position(|field| field.key == "touchscreens")
            {
                self.open_settings_section(index);
            }
            return;
        }
        if action == Action::Cancel || action == Action::Back {
            self.section_back(action == Action::Cancel);
            return;
        }
        if action == Action::Stage {
            self.section_stage(true);
            return;
        }
        if action == Action::Edges {
            let value = self.settings.draft[edges()].as_ref().and_then(toml::Value::as_bool).unwrap_or(false);
            self.settings.draft[edges()] = Some(toml::Value::Boolean(!value));
            if let Some(screen) = &mut self.settings.section {
                screen.focus = Some(Action::Edges);
            }
            self.schedule_render();
            return;
        }
        if action == Action::Identify {
            let wait = std::time::Duration::from_secs(4);
            let deadline = std::time::Instant::now() + wait;
            self.settings.identify_until = Some(deadline);
            let _ = self.loop_handle.insert_source(
                smithay::reexports::calloop::timer::Timer::from_duration(wait),
                move |_, _, state| {
                    if state.settings.identify_until == Some(deadline) {
                        state.settings.identify_until = None;
                        state.schedule_render();
                    }
                    smithay::reexports::calloop::timer::TimeoutAction::Drop
                },
            );
            self.schedule_render();
            return;
        }
        let Some(mut screen) = self.settings.section.take() else { return };
        if let Err(error) = screen.finish_input() {
            screen.error = error;
            self.settings.section = Some(screen);
            self.schedule_render();
            return;
        }
        screen.focus = Some(action);
        screen.searching = action == Action::Search;
        screen.error.clear();
        match action {
            Action::Row(index) => {
                screen.selected = Some(index);
                screen.scroll = 0.;
            }
            Action::Field(index) => {
                let fields = screen.fields(&self.section_outputs(), &self.touch.devices);
                if let Some(field) = fields.get(index) {
                    match &field.form {
                        Form::Choice(options) => {
                            let current = screen.read(&field.path);
                            let selected = options.iter().position(|(_, value)| *value == current).unwrap_or(0);
                            screen.menu = Some(Menu {
                                field: index,
                                options: options.clone(),
                                selected,
                                first: 0,
                                filter: TextInput::default(),
                                picker: false,
                                icons: Default::default(),
                            });
                        }
                        Form::Toggle => {
                            let value = screen.read(&field.path) != "true";
                            screen.write(&field.path, toml::Value::Boolean(value));
                        }
                        _ => {
                            screen.edit = Some((
                                field.clone(),
                                TextInput {
                                    text: screen.read(&field.path),
                                    cursor: screen.read(&field.path).len(),
                                    anchor: None,
                                },
                            ))
                        }
                    }
                }
            }
            Action::Choose(index, position) => {
                let fields = screen.fields(&self.section_outputs(), &self.touch.devices);
                if let Some(field) = fields.get(index)
                    && let Form::Choice(options) = &field.form
                    && let Some((_, value)) = options.get(position)
                    && let Err(error) = screen.set_text(field, value)
                {
                    screen.error = error;
                }
                screen.focus = Some(Action::Field(index));
            }
            Action::Add => {
                screen.search.clear();
                screen.add();
                screen.list_scroll = (screen.rows.len() as f64 * 52. - list_height).max(0.);
            }
            Action::Remove => screen.remove(),
            Action::Up | Action::Down => screen.move_row(action == Action::Down),
            Action::AddValue => screen.add_value(),
            Action::RemoveValue(index) => screen.remove_value(index),
            Action::Record => screen.recording = true,
            Action::Pick(index) => {
                let mut options = Vec::new();
                let mut icons = std::collections::BTreeMap::new();
                if screen.kind == Kind::Rules {
                    for window in self.windows.values() {
                        let app = crate::state::app_id(window);
                        if !app.is_empty() && !options.iter().any(|(_, value)| value == &app) {
                            options.push((format!("{} ({app})", crate::state::window_title(window)), app));
                        }
                    }
                }
                for app in &self.settings_applications() {
                    let value = match screen.kind {
                        Kind::Startup => tileception_apps::command_line(app, &self.terminal),
                        Kind::Shortcuts => format!("spawn {}", tileception_apps::command_line(app, &self.terminal)),
                        Kind::Rules => {
                            app.wm_class.clone().unwrap_or_else(|| app.id.trim_end_matches(".desktop").into())
                        }
                        _ => app.id.clone(),
                    };
                    icons.insert(value.clone(), app.id.clone());
                    options.push((app.name.clone(), value));
                }
                screen.menu = Some(Menu {
                    field: index,
                    options,
                    selected: 0,
                    first: 0,
                    filter: TextInput::default(),
                    picker: true,
                    icons,
                });
            }
            Action::Automatic => {
                screen.automatic_displays();
                screen.error = screen.validate_displays().err().unwrap_or_default();
            }
            Action::Reveal => screen.reveal = !screen.reveal,
            Action::Advanced => screen.advanced = !screen.advanced,
            Action::Search => {}
            Action::Generate => match crate::handoff::suggest_passphrase() {
                Ok(passphrase) => {
                    screen.write(&Path::Key("passphrase".into()), toml::Value::String(passphrase));
                    screen.reveal = true;
                }
                Err(error) => screen.error = error,
            },
            Action::Identify | Action::Edges | Action::Cancel | Action::Back | Action::Stage | Action::Touchscreens => {
            }
        }
        self.settings.section = Some(screen);
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
        self.schedule_render();
    }

    pub(super) fn section_scroll(&mut self, amount: f64) {
        let layout = self.section_layout();
        if let Some(screen) = self.settings.section.as_mut() {
            if let Some(menu) = screen.menu.as_mut() {
                let count = menu.filtered().len();
                menu.first =
                    (menu.first as f64 + (amount / 32.).round()).clamp(0., count.saturating_sub(1) as f64) as usize;
            } else if layout.list.contains(Point::new(self.settings.pointer.0, self.settings.pointer.1)) {
                let total = if screen.kind == Kind::Sharing {
                    430. + self.handoff.peers.len() as f64 * 48.
                } else {
                    screen.visible_rows().len() as f64 * 52.
                };
                screen.list_scroll = (screen.list_scroll + amount).clamp(0., (total - layout.list.h).max(0.));
            } else {
                screen.scroll = (screen.scroll + amount).clamp(0., layout.max_scroll);
            }
        }
        self.schedule_render();
    }

    fn section_choose(&mut self, offset: usize) {
        let Some(screen) = self.settings.section.as_mut() else { return };
        let Some(menu) = screen.menu.take() else { return };
        let Some((_, value)) = menu.filtered().get(menu.first + offset).copied() else {
            screen.menu = Some(menu);
            return;
        };
        self.section_choose_value(menu.field, value);
    }

    fn section_choose_value(&mut self, index: usize, value: &str) {
        let outputs = self.section_outputs();
        let Some(screen) = self.settings.section.as_mut() else { return };
        screen.menu = None;
        let fields = screen.fields(&outputs, &self.touch.devices);
        if let Some(field) = fields.get(index)
            && let Err(error) = screen.set_text(field, value)
        {
            screen.error = error;
        }
        self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
    }

    pub(super) fn section_paste(&mut self, text: &str) {
        let text: String = text
            .chars()
            .map(|character| if matches!(character, '\n' | '\r' | '\t') { ' ' } else { character })
            .filter(|character| !character.is_control())
            .collect();
        if let Some(screen) = self.settings.section.as_mut() {
            if let Some(menu) = screen.menu.as_mut().filter(|menu| menu.picker) {
                menu.filter.insert(text.trim());
                menu.first = 0;
                menu.selected = 0;
            } else if screen.searching {
                screen.search.insert(text.trim());
                screen.filter_changed();
            } else if let Some((_, input)) = screen.edit.as_mut() {
                input.insert(&text);
            }
        }
        self.schedule_render();
    }

    pub(crate) fn settings_recording(&self) -> bool {
        self.settings_focused() && self.settings.section.as_ref().is_some_and(|screen| screen.recording)
    }

    pub(super) fn section_cancel_recording(&mut self) {
        if let Some(screen) = self.settings.section.as_mut() {
            screen.recording = false;
            screen.record_modifier = None;
        }
    }

    pub(crate) fn settings_record_key(&mut self, pressed: bool, sym: u32, mods: Mods) -> bool {
        if !self.settings_recording() {
            return false;
        }
        self.invalidate_settings_client();
        let list_height = self.section_layout().list.h;
        let Some(screen) = self.settings.section.as_mut() else { return false };
        self.tap_armed = None;
        self.tap_fired.clear();
        let modifier = crate::config::modifier_of(sym);
        let captured = if let Some(modifier) = modifier {
            if pressed {
                screen.record_modifier = (mods == modifier || mods == Mods::default()).then_some(modifier);
                None
            } else if screen.record_modifier == Some(modifier) {
                Some((modifier, None))
            } else {
                None
            }
        } else if pressed && sym == keysyms::KEY_Escape && mods == Mods::default() {
            screen.recording = false;
            screen.record_modifier = None;
            None
        } else if pressed {
            screen.record_modifier = None;
            Some((mods, Some(sym)))
        } else {
            None
        };
        if let Some((mods, key)) = captured {
            let mut parts = Vec::new();
            for (enabled, name) in [(mods.logo, "Super"), (mods.ctrl, "Ctrl"), (mods.alt, "Alt"), (mods.shift, "Shift")]
            {
                if enabled {
                    parts.push(name.to_string());
                }
            }
            if let Some(key) = key {
                let name = xkb::keysym_get_name(key.into());
                parts.push(if name.len() == 1 { name.to_uppercase() } else { name });
            }
            screen.write(&Path::Name, toml::Value::String(parts.join("+")));
            screen.search.clear();
            screen.list_scroll = (screen.selected.unwrap_or(0) as f64 * 52. - list_height + 52.).max(0.);
            screen.recording = false;
            screen.record_modifier = None;
        }
        self.schedule_render();
        true
    }

    pub(super) fn section_key(&mut self, sym: u32, mods: Mods) {
        let inline = self.section_is_page();
        let layout = self.section_layout();
        let Some(screen) = self.settings.section.as_mut() else { return };
        if mods.ctrl && matches!(sym, keysyms::KEY_v | keysyms::KEY_V) {
            self.paste(Paste::Settings(self.settings.editor_epoch));
            return;
        }
        if mods.ctrl && matches!(sym, keysyms::KEY_c | keysyms::KEY_C | keysyms::KEY_x | keysyms::KEY_X) {
            let input = if let Some(menu) = screen.menu.as_mut().filter(|menu| menu.picker) {
                Some(&mut menu.filter)
            } else if screen.searching {
                Some(&mut screen.search)
            } else {
                screen
                    .edit
                    .as_mut()
                    .filter(|(field, _)| !matches!(field.form, Form::Secret) || screen.reveal)
                    .map(|(_, input)| input)
            };
            if let Some(input) = input {
                let text = input.text[input.selection()].to_string();
                if matches!(sym, keysyms::KEY_x | keysyms::KEY_X) {
                    input.delete(false, false);
                }
                self.copy_text(text);
            }
            self.schedule_render();
            return;
        }
        if let Some(menu) = screen.menu.as_mut() {
            let count = menu.filtered().len();
            match sym {
                keysyms::KEY_Escape => screen.menu = None,
                keysyms::KEY_Up | keysyms::KEY_Down | keysyms::KEY_Page_Up | keysyms::KEY_Page_Down if count > 0 => {
                    let page = ((layout.panel.h - 124.).min(count as f64 * 32.) / 32.).floor().max(1.) as usize;
                    menu.selected = match sym {
                        keysyms::KEY_Down => (menu.selected + 1) % count,
                        keysyms::KEY_Up => (menu.selected + count - 1) % count,
                        keysyms::KEY_Page_Down => (menu.selected + page).min(count - 1),
                        _ => menu.selected.saturating_sub(page),
                    };
                    let visible = ((self.section_menu_rect(&layout).h - 44.) / 32.).floor().max(1.) as usize;
                    if let Some(menu) = self.settings.section.as_mut().and_then(|screen| screen.menu.as_mut()) {
                        menu.first = menu.first.min(menu.selected).max(menu.selected.saturating_sub(visible - 1));
                    }
                }
                keysyms::KEY_Return if count > 0 => {
                    let selected = menu.selected.saturating_sub(menu.first);
                    self.section_choose(selected);
                }
                _ if menu.picker => {
                    edit_key(&mut menu.filter, &mut self.settings.compose, sym, mods);
                    menu.first = 0;
                    menu.selected = 0;
                }
                _ => {}
            }
        } else if mods.ctrl && matches!(sym, keysyms::KEY_f | keysyms::KEY_F) && screen.kind == Kind::Shortcuts {
            if let Err(error) = screen.finish_input() {
                screen.error = error;
                self.schedule_render();
                return;
            }
            screen.searching = true;
            screen.focus = Some(Action::Search);
        } else if mods.ctrl && matches!(sym, keysyms::KEY_Return | keysyms::KEY_s | keysyms::KEY_S) {
            self.section_stage(true);
            return;
        } else if sym == keysyms::KEY_Escape {
            if screen.searching {
                screen.search.clear();
                screen.searching = false;
                screen.list_scroll = 0.;
            } else if screen.edit.take().is_none() {
                if inline {
                    // Keep the draft when closing a page, just like the other pages.
                    self.close_settings();
                } else {
                    self.section_back(false);
                }
                return;
            }
        } else if matches!(sym, keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab) {
            if let Err(error) = screen.finish_input() {
                screen.error = error;
            } else {
                let mut actions: Vec<_> = layout
                    .hits
                    .iter()
                    .map(|(_, action)| *action)
                    .filter(|action| {
                        !matches!(
                            action,
                            Action::Field(_)
                                | Action::Pick(_)
                                | Action::RemoveValue(_)
                                | Action::Reveal
                                | Action::Record
                                | Action::Cancel
                                | Action::Stage
                        )
                    })
                    .collect();
                for (index, (_, field)) in layout.fields.iter().enumerate() {
                    actions.push(Action::Field(index));
                    if field.pick {
                        actions.push(Action::Pick(index));
                    } else if screen.kind == Kind::Shortcuts
                        && field.path == Path::Name
                        && matches!(field.form, Form::Text)
                    {
                        actions.push(Action::Record);
                    }
                    if let Some(position) = field.removable {
                        actions.push(Action::RemoveValue(position));
                    } else if matches!(field.form, Form::Secret) {
                        actions.push(Action::Reveal);
                    }
                }
                actions.extend([Action::Cancel, Action::Stage]);
                if !actions.is_empty() {
                    let current = actions.iter().position(|action| Some(*action) == screen.focus);
                    let next = if mods.shift || sym == keysyms::KEY_ISO_Left_Tab {
                        current.map_or(actions.len() - 1, |index| (index + actions.len() - 1) % actions.len())
                    } else {
                        current.map_or(0, |index| (index + 1) % actions.len())
                    };
                    let action = actions[next];
                    screen.focus = Some(action);
                    screen.searching = action == Action::Search;
                    if let Action::Field(index) = action {
                        let (rect, field) = &layout.fields[index];
                        let top = rect.y + screen.scroll - layout.body.y;
                        screen.scroll =
                            screen.scroll.min(top).max((top + 90. - layout.body.h).max(0.)).min(layout.max_scroll);
                        if !matches!(field.form, Form::Toggle | Form::Choice(_)) {
                            let text = screen.read(&field.path);
                            screen.edit = Some((field.clone(), TextInput { cursor: text.len(), text, anchor: None }));
                        }
                    }
                    self.settings.editor_epoch = self.settings.editor_epoch.wrapping_add(1);
                }
            }
        } else if sym == keysyms::KEY_Page_Down || sym == keysyms::KEY_Page_Up {
            self.section_scroll(if sym == keysyms::KEY_Page_Down { 180. } else { -180. });
        } else if screen.searching {
            edit_key(&mut screen.search, &mut self.settings.compose, sym, mods);
            screen.filter_changed();
        } else if let Some((_, input)) = screen.edit.as_mut() {
            if sym == keysyms::KEY_Return {
                if let Err(error) = screen.finish_input() {
                    screen.error = error;
                }
            } else {
                edit_key(input, &mut self.settings.compose, sym, mods);
            }
        } else if matches!(sym, keysyms::KEY_Return | keysyms::KEY_space)
            && let Some(action) = screen.focus
        {
            self.section_activate(action);
        }
        self.schedule_render();
    }
}

impl Menu {
    fn filtered(&self) -> Vec<(&str, &str)> {
        let query = self.filter.text.to_lowercase();
        self.options
            .iter()
            .filter(|(label, _)| label.to_lowercase().contains(&query))
            .map(|(label, value)| (label.as_str(), value.as_str()))
            .collect()
    }
}

/// The index of the full-edge crossing setting.
pub(super) fn edges() -> usize {
    FIELDS.iter().position(|field| field.key == "general.stretch_screen_edges").expect("full-edge crossing is a field")
}

pub(super) fn edit_key(input: &mut TextInput, compose: &mut Option<xkb::compose::State>, sym: u32, mods: Mods) {
    match sym {
        keysyms::KEY_a | keysyms::KEY_A if mods.ctrl => input.select_all(),
        keysyms::KEY_Left | keysyms::KEY_Right => {
            input.move_horizontal(sym == keysyms::KEY_Right, mods.ctrl || mods.alt, mods.shift)
        }
        keysyms::KEY_Home | keysyms::KEY_End => {
            input.move_to(if sym == keysyms::KEY_Home { 0 } else { input.text.len() }, mods.shift)
        }
        keysyms::KEY_BackSpace | keysyms::KEY_Delete => input.delete(sym == keysyms::KEY_Delete, mods.ctrl || mods.alt),
        keysyms::KEY_u | keysyms::KEY_U if mods.ctrl => input.clear(),
        _ if !mods.ctrl && !mods.alt && !mods.logo => {
            if let Some(text) = crate::ui::compose_text(compose, sym) {
                input.insert(&text);
            }
        }
        _ => {}
    }
}
