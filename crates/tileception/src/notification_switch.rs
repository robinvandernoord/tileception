//! The notification panel's mode switch: Sound, Silent and Do not disturb
//! side by side, each with an arrow that opens how long it lasts. Clicking
//! a mode keeps it until another is chosen; its menu offers a time, typed
//! or from a few presets. Under the switch: what the mode does and, for one
//! with an end, until when, with an x that drops the end.
//!
//! Also here: the bell on a card in the panel, which opens the sound that
//! app makes.

use smithay::input::{keyboard::keysyms, pointer::CursorIcon};
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    notification_mode::{self, End, Mode, Preset, Until},
    shapes::Shape,
    sound::{self, AppSound, Family},
    text_input::TextInput,
    ui::{Button, Look, Painter, PointerTarget, Split, Style, glyph},
    wake,
};

/// The switch and the line under it.
pub const SWITCH_H: f64 = TRACK_H + 6.0 + SUB_H;
const TRACK_H: f64 = 40.0;
const SUB_H: f64 = 24.0;
const ARROW_W: f64 = 28.0;
const MENU_W: f64 = 250.0;
const PAD: f64 = 4.0;
const HEADER_H: f64 = 26.0;
const ROW_H: f64 = 32.0;
const SEP_H: f64 = 9.0;
const FIELD_H: f64 = 36.0;
const READING_H: f64 = 24.0;
const TOGGLE_H: f64 = 36.0;
/// What the typed time field shows when empty, and examples it reads.
const EXAMPLES: &str = "45m, 1h30, 18:00, 6pm, tomorrow 9";

/// The menu under one mode's arrow.
pub(crate) struct ModeMenu {
    pub mode: Mode,
    /// The row the keyboard is on, in [`rows`] order.
    pub row: usize,
    /// "Other time...", while it is being typed.
    pub field: Option<TextInput>,
    compose: Option<smithay::input::keyboard::xkb::compose::State>,
}

/// The sounds menu of the card with this notification.
pub(crate) struct SoundMenu {
    pub notification: u32,
    /// Desktop entry and app name, as the exception is written by.
    pub apps: [String; 2],
}

/// A row of the mode menu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Row {
    Preset(usize),
    Other,
    /// Silent's menu: whether alarms ring anyway.
    Alarms,
}

/// The rows the keyboard moves between.
fn rows(mode: Mode, presets: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = (0..presets).map(Row::Preset).collect();
    rows.push(Row::Other);
    if mode == Mode::Silent {
        rows.push(Row::Alarms);
    }
    rows
}

pub(crate) struct Segment {
    pub mode: Mode,
    pub rect: Rect,
    pub arrow: Rect,
}

pub(crate) struct SwitchLayout {
    pub track: Rect,
    pub segments: Vec<Segment>,
    pub summary: Rect,
    /// The x that drops the end, while the mode has one.
    pub clear: Option<Rect>,
}

pub(crate) struct MenuLayout {
    pub outline: Rect,
    pub header: Rect,
    pub rows: Vec<(Row, Rect)>,
    pub separators: Vec<Rect>,
    /// The typed time, and the line that says what it reads as.
    pub field: Option<(Rect, Rect)>,
}

/// The sound choices on a card's menu: like the others, a family, or off.
const SOUND_ROWS: usize = 5;

fn sound_row(index: usize) -> Option<AppSound> {
    match index {
        1 => Some(AppSound::Vibes),
        2 => Some(AppSound::Knock),
        3 => Some(AppSound::Breath),
        4 => Some(AppSound::Off),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Hit {
    Mode(Mode),
    Arrow(Mode),
    ClearEnd,
    Row(Row),
    Field,
    /// A card's bell: the menu of its app's sound.
    Bell {
        notification: u32,
        apps: [String; 2],
    },
    /// A row of that menu: like other apps (None), a family or off.
    Sound(Option<AppSound>),
}

impl crate::ui::PointerAction for Hit {
    fn activate(&self, state: &mut Tileception, rect: Rect, point: Point) {
        match self {
            Hit::Mode(mode) => {
                state.notifications.mode_menu = None;
                state.set_notification_mode(*mode, Until::Off);
            }
            Hit::Arrow(mode) => state.toggle_mode_menu(*mode),
            Hit::ClearEnd => state.clear_notification_mode_end(),
            Hit::Row(row) => state.choose_mode_row(*row),
            Hit::Field => state.mode_field_press(rect, point, false),
            Hit::Bell { notification, apps } => state.toggle_sound_menu(*notification, apps),
            Hit::Sound(sound) => state.choose_app_sound(*sound),
        }
        state.schedule_render();
    }

    fn middle_click(&self, state: &mut Tileception, rect: Rect, point: Point) -> bool {
        if *self != Hit::Field {
            return false;
        }
        state.mode_field_press(rect, point, false);
        // A middle button lets go unseen: nothing is selecting.
        state.notifications.field_drag = false;
        state.paste_primary(crate::launcher::Paste::NotificationMode);
        true
    }
}

impl Tileception {
    /// The switch, filling `area`'s width from its top.
    pub(crate) fn switch_layout(&self, area: Rect) -> SwitchLayout {
        let track = Rect::new(area.x, area.y, area.w, TRACK_H);
        // Do not disturb has the longest name.
        let weights = [1.0, 1.0, 1.45];
        let total: f64 = weights.iter().sum();
        let inner = track.inset(3.0);
        let mut left = inner.x;
        let segments = Mode::ALL
            .iter()
            .zip(weights)
            .map(|(mode, weight)| {
                let width = inner.w * weight / total;
                let rect = Rect::new(left, inner.y, width, inner.h);
                left += width;
                Segment { mode: *mode, rect, arrow: Rect::new(rect.right() - ARROW_W, rect.y, ARROW_W, rect.h) }
            })
            .collect();
        let sub = Rect::new(area.x + 2.0, track.bottom() + 6.0, area.w - 4.0, SUB_H);
        let clear = (self.notifications.mode.end != End::Off)
            .then(|| Rect::new(sub.right() - 22.0, sub.y + (sub.h - 22.0) / 2.0, 22.0, 22.0));
        SwitchLayout { track, segments, summary: sub, clear }
    }

    /// The open mode menu, under its segment.
    pub(crate) fn mode_menu_layout(&self, switch: &SwitchLayout) -> Option<MenuLayout> {
        let menu = self.notifications.mode_menu.as_ref()?;
        let segment = switch.segments.iter().find(|segment| segment.mode == menu.mode)?;
        let presets = notification_mode::presets(wake::now(), &self.config.file.notifications).len();
        // Against the segment's right edge; the first, against its left.
        let left = if menu.mode == Mode::ALL[0] { segment.rect.x } else { segment.rect.right() - MENU_W };
        let left = left.max(switch.track.x);
        let mut cursor = switch.track.bottom() + 6.0 + PAD;
        let header = Rect::new(left + PAD, cursor, MENU_W - 2.0 * PAD, HEADER_H);
        cursor = header.bottom();
        let mut placed = Vec::new();
        let mut separators = Vec::new();
        let mut field = None;
        let separator = |cursor: &mut f64, separators: &mut Vec<Rect>| {
            separators.push(Rect::new(left + PAD + 6.0, *cursor + (SEP_H - 1.0) / 2.0, MENU_W - 2.0 * PAD - 12.0, 1.0));
            *cursor += SEP_H;
        };
        for row in rows(menu.mode, presets) {
            match row {
                Row::Preset(1) | Row::Other | Row::Alarms => separator(&mut cursor, &mut separators),
                Row::Preset(_) => {}
            }
            if row == Row::Other && menu.field.is_some() {
                let input = Rect::new(left + PAD + 3.0, cursor + 3.0, MENU_W - 2.0 * PAD - 6.0, FIELD_H - 6.0);
                let reading = Rect::new(left + PAD + 12.0, cursor + FIELD_H, MENU_W - 2.0 * PAD - 24.0, READING_H);
                field = Some((input, reading));
                cursor += FIELD_H + READING_H;
                continue;
            }
            let height = if row == Row::Alarms { TOGGLE_H } else { ROW_H };
            placed.push((row, Rect::new(left + PAD, cursor, MENU_W - 2.0 * PAD, height)));
            cursor += height;
        }
        let top = switch.track.bottom() + 6.0;
        let outline = Rect::new(left, top, MENU_W, cursor + PAD - top);
        Some(MenuLayout { outline, header, rows: placed, separators, field })
    }

    /// Where the open sounds menu shows, under the card's bell.
    fn sound_menu_layout(&self, bell: Rect) -> (Rect, Vec<Rect>) {
        let panel = self.notification_panel();
        let width = 210.0;
        let height = 2.0 * PAD + HEADER_H + SOUND_ROWS as f64 * ROW_H + SEP_H;
        let left = (bell.right() - width).max(panel.x + 8.0);
        let below = bell.bottom() + 4.0;
        let top = if below + height > panel.bottom() - 8.0 { bell.y - 4.0 - height } else { below };
        let outline = Rect::new(left, top, width, height);
        let mut cursor = top + PAD + HEADER_H;
        let rows = (0..SOUND_ROWS)
            .map(|index| {
                if index == 1 {
                    cursor += SEP_H;
                }
                let row = Rect::new(left + PAD, cursor, width - 2.0 * PAD, ROW_H);
                cursor += ROW_H;
                row
            })
            .collect();
        (outline, rows)
    }

    /// What the switch and its menu take under the pointer: `Some(None)`
    /// inside the menu between its rows, which keeps it from what is behind.
    pub(crate) fn switch_pointer_target(&self, area: Rect, point: Point) -> Option<Option<PointerTarget>> {
        let switch = self.switch_layout(area);
        if let Some(menu) = self.mode_menu_layout(&switch)
            && menu.outline.contains(point)
        {
            if let Some((input, _)) = menu.field
                && input.contains(point)
            {
                return Some(Some(PointerTarget::new(input, Hit::Field, CursorIcon::Text)));
            }
            let row = menu.rows.iter().find(|(_, rect)| rect.contains(point));
            return Some(row.map(|(row, rect)| PointerTarget::button(*rect, Hit::Row(*row))));
        }
        if let Some(clear) = switch.clear.filter(|clear| clear.contains(point)) {
            return Some(Some(PointerTarget::button(clear, Hit::ClearEnd)));
        }
        let segment = switch.segments.iter().find(|segment| segment.rect.contains(point))?;
        if segment.arrow.contains(point) {
            return Some(Some(PointerTarget::button(segment.arrow, Hit::Arrow(segment.mode))));
        }
        Some(Some(PointerTarget::button(segment.rect, Hit::Mode(segment.mode))))
    }

    /// The open sounds menu's rows under the pointer, and anything else
    /// inside it, which it keeps from what is behind.
    pub(crate) fn sound_menu_pointer_target(&self, point: Point) -> Option<Option<PointerTarget>> {
        let bell = self.sound_menu_bell()?;
        let (outline, rows) = self.sound_menu_layout(bell);
        if !outline.contains(point) {
            return None;
        }
        let target = rows
            .iter()
            .enumerate()
            .find(|(_, rect)| rect.contains(point))
            .map(|(index, rect)| PointerTarget::button(*rect, Hit::Sound(sound_row(index))));
        Some(target)
    }

    /// A card's bell, which opens the menu of its app's sound.
    pub(crate) fn bell_target(&self, bell: Rect, notification: u32, apps: [String; 2]) -> PointerTarget {
        PointerTarget::button(bell, Hit::Bell { notification, apps })
    }

    /// A left press while the panel is open: anywhere but inside the open
    /// menus, or on what opened them (which closes them itself), closes them.
    pub(crate) fn switch_press(&mut self, area: Option<Rect>, point: Point) {
        if let Some(area) = area
            && let Some(menu_mode) = self.notifications.mode_menu.as_ref().map(|menu| menu.mode)
        {
            let switch = self.switch_layout(area);
            let inside = self.mode_menu_layout(&switch).is_some_and(|menu| menu.outline.contains(point));
            let arrow =
                switch.segments.iter().any(|segment| segment.mode == menu_mode && segment.arrow.contains(point));
            if !inside && !arrow {
                self.notifications.mode_menu = None;
                self.schedule_render();
            }
        }
        if self.notifications.sound_menu.is_some() {
            let keep = self
                .sound_menu_bell()
                .is_some_and(|bell| bell.contains(point) || self.sound_menu_layout(bell).0.contains(point));
            if !keep {
                self.notifications.sound_menu = None;
                self.schedule_render();
            }
        }
    }

    pub(crate) fn close_switch_menus(&mut self) {
        self.notifications.mode_menu = None;
        self.notifications.sound_menu = None;
        self.notifications.field_drag = false;
    }

    /// Opens the mode's menu, on the row of what is in effect, or closes it.
    pub(crate) fn toggle_mode_menu(&mut self, mode: Mode) {
        if self.notifications.mode_menu.as_ref().is_some_and(|menu| menu.mode == mode) {
            self.notifications.mode_menu = None;
            return;
        }
        self.notifications.sound_menu = None;
        let presets = notification_mode::presets(wake::now(), &self.config.file.notifications);
        let row = self.chosen_preset(mode, &presets).unwrap_or(0);
        self.notifications.mode_menu = Some(ModeMenu { mode, row, field: None, compose: None });
    }

    /// The preset in effect for `mode`, if it is the mode and one is.
    fn chosen_preset(&self, mode: Mode, presets: &[Preset]) -> Option<usize> {
        let state = self.notifications.mode;
        if state.mode != mode {
            return None;
        }
        match state.end {
            End::Off => Some(0),
            End::Restart => presets.iter().position(|preset| preset.until == Until::Restart),
            // This evening or tomorrow morning still end then; an hour has moved on.
            End::Time(when) => presets.iter().position(|preset| {
                !matches!(preset.until, Until::After(_))
                    && preset.until.resolve(wake::now(), &self.config.file.notifications) == Some(End::Time(when))
            }),
        }
    }

    fn choose_mode_row(&mut self, row: Row) {
        let Some(menu) = &mut self.notifications.mode_menu else { return };
        let mode = menu.mode;
        match row {
            Row::Preset(index) => {
                let presets = notification_mode::presets(wake::now(), &self.config.file.notifications);
                let Some(preset) = presets.get(index) else { return };
                self.notifications.mode_menu = None;
                self.set_notification_mode(mode, preset.until);
            }
            Row::Other => {
                menu.field = Some(TextInput::new(""));
                menu.compose = crate::ui::compose_state();
                menu.row = rows(mode, notification_mode::presets(wake::now(), &self.config.file.notifications).len())
                    .iter()
                    .position(|row| *row == Row::Other)
                    .unwrap_or(0);
            }
            Row::Alarms => {
                let ring = !self.config.file.notifications.alarms_in_silent;
                let written = crate::config_edit::set_value(
                    &crate::config::path(),
                    "notifications.alarms_in_silent",
                    if ring { "true" } else { "false" },
                );
                match written {
                    // At once, before the config file is read again.
                    Ok(()) => self.config.file.notifications.alarms_in_silent = ring,
                    Err(error) => {
                        self.notify_locally("Could not change the setting", &error, tileception_notify::Urgency::Normal)
                    }
                }
            }
        }
    }

    /// Keys while the mode menu is open: the arrows move, Enter or Space
    /// chooses, Esc or Tab closes. While a time is typed, its field has the
    /// keys, Enter takes the time and Esc or Up goes back to the rows.
    pub(crate) fn mode_menu_key(&mut self, sym: u32, mods: crate::config::Mods) {
        let presets = notification_mode::presets(wake::now(), &self.config.file.notifications).len();
        let Some(menu) = &mut self.notifications.mode_menu else { return };
        let rows = rows(menu.mode, presets);
        if let Some(input) = &mut menu.field {
            match crate::text_input::plain(sym) {
                keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                    if let Some(until) = Until::parse(&input.text) {
                        let mode = menu.mode;
                        self.notifications.mode_menu = None;
                        self.set_notification_mode(mode, until);
                    }
                }
                keysyms::KEY_Escape | keysyms::KEY_Up => {
                    menu.field = None;
                    let other = rows.iter().position(|row| *row == Row::Other).unwrap_or(0);
                    menu.row = if sym == keysyms::KEY_Up { other.saturating_sub(1) } else { other };
                }
                _ => {
                    let key = input.key(&mut menu.compose, sym, mods);
                    self.field_clipboard(key.clipboard, crate::launcher::Paste::NotificationMode);
                }
            }
            return self.schedule_render();
        }
        let count = rows.len();
        match crate::text_input::plain(sym) {
            keysyms::KEY_Down => menu.row = (menu.row + 1) % count,
            keysyms::KEY_Up => menu.row = (menu.row + count - 1) % count,
            keysyms::KEY_Home => menu.row = 0,
            keysyms::KEY_End => menu.row = count - 1,
            keysyms::KEY_Return | keysyms::KEY_KP_Enter | keysyms::KEY_space => {
                let row = rows[menu.row.min(count - 1)];
                self.choose_mode_row(row);
            }
            keysyms::KEY_Escape | keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => self.notifications.mode_menu = None,
            _ => return,
        }
        self.schedule_render();
    }

    /// Whether the typed time field takes pasted text.
    pub(crate) fn mode_field_typing(&self) -> bool {
        self.notifications.open && self.notifications.mode_menu.as_ref().is_some_and(|menu| menu.field.is_some())
    }

    pub(crate) fn mode_field_insert_paste(&mut self, text: &str) {
        if let Some(input) = self.notifications.mode_menu.as_mut().and_then(|menu| menu.field.as_mut()) {
            input.paste(text);
        }
    }

    /// The text of the typed time field, as `draw_mode_menu` draws it.
    fn mode_field_text(field: Rect) -> Rect {
        Rect::new(field.x + 30.0, field.y, field.w - 38.0, field.h)
    }

    /// A press in the typed time field places the caret; a drag on selects.
    fn mode_field_press(&mut self, field: Rect, point: Point, extend: bool) {
        let shift = self.seat.get_keyboard().is_some_and(|keyboard| keyboard.modifier_state().shift);
        let text = Self::mode_field_text(field);
        let (font, size) = (self.theme.font.clone(), self.theme.font_size * 0.92);
        let Some(input) = self.notifications.mode_menu.as_mut().and_then(|menu| menu.field.as_mut()) else { return };
        input.click(
            &mut self.text.borrow_mut(),
            &font,
            size,
            text.w as f32,
            (point.x - text.x).max(0.0) as f32,
            extend || shift,
        );
        self.notifications.field_drag = !extend;
        self.notifications.focused = true;
    }

    /// Pointer motion while selecting in the typed time field. True when it took it.
    pub fn mode_field_drag(&mut self, point: Point) -> bool {
        if !self.notifications.field_drag {
            return false;
        }
        let Some(area) = self.mode_switch_area() else { return false };
        let switch = self.switch_layout(area);
        let Some((field, _)) = self.mode_menu_layout(&switch).and_then(|menu| menu.field) else {
            self.notifications.field_drag = false;
            return false;
        };
        self.mode_field_press(field, point, true);
        self.notifications.field_drag = true;
        self.schedule_render();
        true
    }

    /// The button coming up after selecting in the typed time field.
    pub fn mode_field_release(&mut self) -> bool {
        if !std::mem::take(&mut self.notifications.field_drag) {
            return false;
        }
        let selected = self
            .notifications
            .mode_menu
            .as_ref()
            .and_then(|menu| menu.field.as_ref())
            .and_then(TextInput::selected_text);
        self.offer_primary(selected);
        true
    }

    /// The card whose bell opened the sounds menu, and the bell, while it shows.
    fn sound_menu_bell(&self) -> Option<Rect> {
        let menu = self.notifications.sound_menu.as_ref()?;
        self.card_bell(menu.notification)
    }

    fn toggle_sound_menu(&mut self, notification: u32, apps: &[String; 2]) {
        if self.notifications.sound_menu.as_ref().is_some_and(|menu| menu.notification == notification) {
            self.notifications.sound_menu = None;
            return;
        }
        self.notifications.mode_menu = None;
        self.notifications.sound_menu = Some(SoundMenu { notification, apps: apps.clone() });
    }

    /// The app's sound from its menu: like other apps (`None`), a family or
    /// off. A family plays once, to be heard.
    fn choose_app_sound(&mut self, sound: Option<AppSound>) {
        let Some(menu) = self.notifications.sound_menu.take() else { return };
        let keys: Vec<&str> = menu.apps.iter().map(String::as_str).filter(|app| !app.is_empty()).collect();
        let written = crate::config_edit::set_app_sound(&crate::config::path(), &keys, sound);
        match written {
            Ok(()) => {
                // At once, before the config file is read again.
                let sounds = &mut self.config.file.notifications.app_sounds;
                sounds.retain(|key, _| !keys.contains(&key.as_str()));
                if let (Some(sound), Some(first)) = (sound, keys.first()) {
                    sounds.insert((*first).to_string(), sound);
                }
            }
            Err(error) => {
                return self.notify_locally("Could not change the sound", &error, tileception_notify::Urgency::Normal);
            }
        }
        let config = &self.config.file.notifications;
        if let Some(family) = sound.map_or(Some(config.sounds), AppSound::family) {
            let volume = f32::from(config.volume.min(100)) / 100.0;
            self.notifications.sounds.play(family, sound::Clip::Normal, volume);
        }
    }

    pub(crate) fn draw_mode_switch(&self, painter: &mut Painter, area: Rect, pointer: Point) {
        let theme = &self.theme;
        let switch = self.switch_layout(area);
        let state = self.notifications.mode;
        let open = self.notifications.mode_menu.as_ref().map(|menu| menu.mode);
        painter.shape(
            switch.track,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background, theme.radius) },
        );
        for segment in &switch.segments {
            let chosen = segment.mode == state.mode;
            let split = Split {
                width: ARROW_W,
                hover: segment.arrow.contains(pointer),
                open: open == Some(segment.mode),
                marked: chosen && state.end != End::Off,
            };
            let look = if chosen { Look::Chosen } else { Look::Quiet };
            let hover = segment.rect.contains(pointer) && !segment.arrow.contains(pointer);
            let button = Button::new(segment.mode.name(), look).icon(segment.mode.glyph()).split(split);
            painter.button(segment.rect, button, hover);
        }
        let style = Style::new(theme.font_size * 0.9, theme.muted);
        painter.text(state.mode.summary(), switch.summary, style);
        if let Some(clear) = switch.clear {
            let until = notification_mode::describe(state.end, wake::now());
            let text_style = Style::new(theme.font_size * 0.9, theme.text);
            let (width, _) = painter.measure(&until, switch.summary.w, text_style);
            let text = Rect::new(clear.x - 4.0 - width, switch.summary.y, width + 2.0, switch.summary.h);
            painter.text(&until, text, text_style);
            painter.glyph(glyph::CLOCK, Rect::new(text.x - 20.0, text.y, 16.0, text.h), 13, theme.muted);
            painter.button(clear, Button::glyph(glyph::CLOSE, Look::Quiet).round(), clear.contains(pointer));
        }
    }

    /// The open mode menu, over the cards.
    pub(crate) fn draw_mode_menu(&self, painter: &mut Painter, area: Rect, pointer: Point) {
        let theme = &self.theme;
        let switch = self.switch_layout(area);
        let (Some(menu), Some(layout)) = (self.notifications.mode_menu.as_ref(), self.mode_menu_layout(&switch)) else {
            return;
        };
        let now = wake::now();
        let config = &self.config.file.notifications;
        let presets = notification_mode::presets(now, config);
        let chosen = self.chosen_preset(menu.mode, &presets);
        let keyboard_rows = rows(menu.mode, presets.len());
        painter.popup(layout.outline);
        let muted = Style::new(theme.font_size * 0.8, theme.muted);
        painter.glyph(
            menu.mode.glyph(),
            Rect::new(layout.header.x + 8.0, layout.header.y, 14.0, layout.header.h),
            12,
            theme.muted,
        );
        painter.text(
            &format!("{}: how long", menu.mode.name()),
            Rect::new(layout.header.x + 28.0, layout.header.y, layout.header.w - 32.0, layout.header.h),
            muted,
        );
        for separator in &layout.separators {
            painter.fill(*separator, theme.border, 0.0);
        }
        let label = Style::new(theme.font_size * 0.92, theme.text);
        for (row, rect) in &layout.rows {
            let keyboard = keyboard_rows.get(menu.row) == Some(row) && menu.field.is_none();
            let lit = rect.contains(pointer) || (keyboard && self.notifications_keyboard());
            if lit {
                painter.fill(*rect, theme.surface_raised, theme.radius);
            }
            let inside = Rect::new(rect.x + 12.0, rect.y, rect.w - 20.0, rect.h);
            match row {
                Row::Preset(index) => {
                    let Some(preset) = presets.get(*index) else { continue };
                    let picked = chosen == Some(*index);
                    let colour = if picked { theme.accent } else { theme.text };
                    painter.text(&preset.label, inside, label.colour(colour));
                    let check = Rect::new(inside.right() - 16.0, inside.y, 16.0, inside.h);
                    if picked {
                        painter.glyph(glyph::CHECK, check, 14, theme.accent);
                    }
                    if let Some(clock) = &preset.clock {
                        let style = label.colour(theme.muted);
                        let (width, _) = painter.measure(clock, inside.w, style);
                        painter.text(clock, Rect::new(check.x - 8.0 - width, inside.y, width + 2.0, inside.h), style);
                    }
                }
                Row::Other => {
                    painter.text("Other time...", inside, label);
                }
                Row::Alarms => {
                    painter.text("Alarms still ring", inside, label);
                    painter.switch(*rect, config.alarms_in_silent);
                }
            }
        }
        if let (Some((input, reading)), Some(field)) = (layout.field, menu.field.as_ref()) {
            painter.shape(
                input,
                Shape { border_width: 1.0, border: theme.accent, ..Shape::filled(theme.background, theme.radius) },
            );
            painter.glyph(glyph::CLOCK, Rect::new(input.x + 6.0, input.y, 18.0, input.h), 13, theme.muted);
            let text = Self::mode_field_text(input);
            let size = theme.font_size * 0.92;
            if field.text.is_empty() {
                painter.text("Other time", text, Style::new(size, theme.muted));
            }
            let image = self.text.borrow_mut().field(
                &field.text,
                &theme.font,
                size,
                theme.text,
                text.w as f32,
                crate::text::Field {
                    cursor: field.cursor,
                    anchor: field.anchor,
                    visible: true,
                    accent: theme.accent.straight_rgba8(),
                },
            );
            if let Some(image) = image {
                painter.image(&image.buffer, (text.x, text.y + (text.h - image.height) / 2.0));
            }
            let small = Style::new(theme.font_size * 0.8, theme.muted);
            let line = if field.text.trim().is_empty() {
                EXAMPLES.to_string()
            } else {
                match Until::parse(&field.text).and_then(|until| until.resolve(now, config)) {
                    None => "Can't read that. Try 45m, 2h, 18:00, tomorrow 9.".to_string(),
                    Some(End::Time(when)) => {
                        format!(
                            "Enter: {}, {}",
                            notification_mode::describe(End::Time(when), now),
                            notification_mode::left(when - now)
                        )
                    }
                    Some(end) => format!("Enter: {}", notification_mode::describe(end, now)),
                }
            };
            painter.text(&line, reading, small);
        }
    }

    /// The open sounds menu, over the cards.
    pub(crate) fn draw_sound_menu(&self, painter: &mut Painter, pointer: Point) {
        let (Some(menu), Some(bell)) = (self.notifications.sound_menu.as_ref(), self.sound_menu_bell()) else {
            return;
        };
        let theme = &self.theme;
        let (outline, rows) = self.sound_menu_layout(bell);
        painter.popup(outline);
        let config = &self.config.file.notifications;
        let apps: Vec<&str> = menu.apps.iter().map(String::as_str).collect();
        let exception = apps.iter().filter(|app| !app.is_empty()).find_map(|app| config.app_sounds.get(*app).copied());
        let name = menu.apps.iter().rev().find(|app| !app.is_empty()).map_or("This app", String::as_str);
        let header = Rect::new(outline.x + PAD + 12.0, outline.y + PAD, outline.w - 2.0 * PAD - 24.0, HEADER_H);
        painter.text(&format!("{name}: sound"), header, Style::new(theme.font_size * 0.8, theme.muted));
        painter.fill(
            Rect::new(outline.x + PAD + 6.0, rows[1].y - (SEP_H + 1.0) / 2.0, outline.w - 2.0 * PAD - 12.0, 1.0),
            theme.border,
            0.0,
        );
        let label = Style::new(theme.font_size * 0.92, theme.text);
        for (index, rect) in rows.iter().enumerate() {
            if rect.contains(pointer) {
                painter.fill(*rect, theme.surface_raised, theme.radius);
            }
            let sound = sound_row(index);
            let text = match sound {
                None => format!("Like other apps ({})", config.sounds.name()),
                Some(AppSound::Off) => "Off".to_string(),
                Some(sound) => sound.family().map_or("", Family::name).to_string(),
            };
            let picked = sound == exception;
            let inside = Rect::new(rect.x + 12.0, rect.y, rect.w - 20.0, rect.h);
            painter.text(&text, inside, label.colour(if picked { theme.accent } else { theme.text }));
            if picked {
                painter.glyph(
                    glyph::CHECK,
                    Rect::new(inside.right() - 16.0, inside.y, 16.0, inside.h),
                    14,
                    theme.accent,
                );
            }
        }
    }

    /// The bar's tooltip over the bell: the mode, until when, and how many
    /// notifications there are.
    pub(crate) fn mode_tooltip(&self, count: usize) -> (String, String) {
        let state = self.notifications.mode;
        let until = notification_mode::describe(state.end, wake::now());
        let title =
            if until.is_empty() { state.mode.name().to_string() } else { format!("{} {until}", state.mode.name()) };
        let unread = match count {
            0 => "No notifications".to_string(),
            1 => "1 notification".to_string(),
            count => format!("{count} notifications"),
        };
        (title, format!("{}. {unread}.", state.mode.summary()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_has_the_alarm_row() {
        assert_eq!(rows(Mode::Silent, 5).last(), Some(&Row::Alarms));
        assert_eq!(rows(Mode::Sound, 5).last(), Some(&Row::Other));
        assert_eq!(rows(Mode::DoNotDisturb, 5).len(), 6);
    }
}
