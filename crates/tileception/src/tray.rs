//! The bar's status icons, overflow list and application menus.

use std::collections::HashMap;
use std::sync::mpsc;

use smithay::backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer};
use smithay::reexports::calloop::{RegistrationToken, channel};
use smithay::utils::Transform;
use tileception_core::{Point, Rect};

use crate::tray_bus::{Event, Item, MenuRow, Request};
use crate::ui::{Align, Painter, Style, glyph};
use crate::{Tileception, shell::Part};

const SLOT: f64 = 26.0;
const ROW: f64 = 32.0;
const WIDTH: f64 = 288.0;

#[derive(Default)]
pub struct Tray {
    items: Vec<Item>,
    debug: Vec<Item>,
    next_debug: usize,
    images: HashMap<String, MemoryRenderBuffer>,
    requests: Option<mpsc::Sender<Request>>,
    source: Option<RegistrationToken>,
    pub open: bool,
    menu: Option<Menu>,
    offset: usize,
    scrolled: f64,
}

struct Menu {
    key: String,
    path: String,
    parents: Vec<i32>,
    rows: Vec<MenuRow>,
    loading: bool,
}

#[derive(Clone, Copy)]
pub enum Slot {
    Item(usize),
    More,
}

impl Tileception {
    /// Also used after a config reload, so an external bar can own the watcher.
    pub fn apply_tray(&mut self) {
        self.close_tray();
        let builtin = Part::parse(&self.shell().bar).is_builtin();
        let bus = std::env::var("TILECEPTION_TRAY_BUS").ok();
        let enabled = builtin && (self.session_services || bus.is_some());
        if !enabled {
            self.tray.requests = None;
            if let Some(source) = self.tray.source.take() {
                self.loop_handle.remove(source);
            }
            self.tray.items.clear();
            self.tray.images.clear();
            self.close_tray();
            return;
        }
        if self.tray.requests.is_some() {
            return;
        }
        let (sender, receiver) = channel::channel();
        let source = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                match event {
                    Event::Items(items) => {
                        state.tray.images = items
                            .iter()
                            .filter_map(|item| bitmap(item).map(|image| (item.key.clone(), image)))
                            .collect();
                        state.tray.items = items;
                        state.tray.offset = 0;
                        if state.tray.menu.as_ref().is_some_and(|menu| {
                            !state.tray.items.iter().chain(&state.tray.debug).any(|item| item.key == menu.key)
                        }) {
                            state.close_tray();
                        }
                    }
                    Event::Menu { key, parent, rows } => {
                        if let Some(menu) = &mut state.tray.menu
                            && menu.key == key
                            && menu.parents.last() == Some(&parent)
                        {
                            menu.rows = rows;
                            menu.loading = false;
                            state.tray.offset = 0;
                        }
                    }
                    Event::Stopped => {
                        state.tray.requests = None;
                        state.tray.items.clear();
                        state.close_tray();
                        if let Some(source) = state.tray.source.take() {
                            state.loop_handle.remove(source);
                        }
                    }
                }
            }
        });
        let source = match source {
            Ok(source) => source,
            Err(error) => return tracing::warn!("tray source: {error}"),
        };
        let (requests, incoming) = mpsc::channel();
        match std::thread::Builder::new()
            .name("tileception-tray".into())
            .spawn(move || crate::tray_bus::run(bus.filter(|bus| bus != "session"), sender, incoming))
        {
            Ok(_) => {
                self.tray.requests = Some(requests);
                self.tray.source = Some(source);
            }
            Err(error) => {
                self.loop_handle.remove(source);
                tracing::warn!("tray worker: {error}");
            }
        }
    }

    fn tray_items(&self) -> Vec<&Item> {
        self.tray.items.iter().chain(&self.tray.debug).filter(|item| !item.passive).collect()
    }

    fn tray_limit(&self) -> usize {
        self.shell().tray_max_icons.max(1)
    }

    fn tray_inline_count(&self) -> usize {
        let count = self.tray_items().len();
        if count > self.tray_limit() { self.tray_limit() - 1 } else { count }
    }

    /// Places slots from left to right, reserving the final one for overflow.
    pub fn tray_slots(&self, right: &mut f64, y: f64, h: f64) -> Vec<(Slot, Rect)> {
        let count = self.tray_items().len();
        let slots = count.min(self.tray_limit());
        let left = *right - slots as f64 * SLOT;
        let result = (0..slots)
            .map(|i| {
                let slot = if count > self.tray_limit() && i == slots - 1 { Slot::More } else { Slot::Item(i) };
                (slot, Rect::new(left + i as f64 * SLOT, y, SLOT, h))
            })
            .collect();
        if slots > 0 {
            *right = left - 6.0;
        }
        result
    }

    pub fn close_tray(&mut self) -> bool {
        let open = std::mem::take(&mut self.tray.open);
        self.tray.menu = None;
        self.tray.offset = 0;
        self.tray.scrolled = 0.0;
        open
    }

    fn tray_request(&self, request: Request) {
        if let Some(sender) = &self.tray.requests {
            let _ = sender.send(request);
        }
    }

    pub fn tray_slot_button(&mut self, slot: Slot, button: u32) {
        match slot {
            Slot::More if button == crate::input::BTN_LEFT => {
                let open = self.tray.open && self.tray.menu.is_none();
                self.close_calendar();
                self.close_tray();
                self.tray.open = !open;
            }
            Slot::Item(index) => {
                let item = self.tray_items().get(index).copied().cloned();
                if let Some(item) = item {
                    self.tray_activate(item, button);
                }
            }
            Slot::More => {}
        }
    }

    fn tray_activate(&mut self, item: Item, button: u32) {
        let context = button == crate::input::BTN_RIGHT || (button == crate::input::BTN_LEFT && item.is_menu);
        if item.key.starts_with("debug:") {
            self.close_calendar();
            self.tray.open = true;
            self.tray.offset = 0;
            self.tray.menu = Some(Menu {
                key: item.key,
                path: String::new(),
                parents: vec![0],
                loading: false,
                rows: vec![
                    MenuRow {
                        id: 1,
                        label: "Needs attention".into(),
                        enabled: true,
                        separator: false,
                        submenu: false,
                        checked: Some(item.attention),
                    },
                    MenuRow {
                        id: 2,
                        label: "Remove debug entry".into(),
                        enabled: true,
                        separator: false,
                        submenu: false,
                        checked: None,
                    },
                ],
            });
        } else if context && let Some(path) = item.menu {
            self.close_calendar();
            self.tray.open = true;
            self.tray.offset = 0;
            self.tray.menu = Some(Menu {
                key: item.key.clone(),
                path: path.clone(),
                parents: vec![0],
                rows: Vec::new(),
                loading: true,
            });
            self.tray_request(Request::Menu { key: item.key, path, parent: 0 });
        } else {
            self.close_tray();
            let method = match button {
                crate::input::BTN_LEFT if context => "ContextMenu",
                crate::input::BTN_LEFT => "Activate",
                crate::input::BTN_RIGHT => "ContextMenu",
                274 => "SecondaryActivate",
                _ => return,
            };
            self.tray_request(Request::Activate {
                key: item.key,
                method,
                x: self.pointer.x as i32,
                y: self.pointer.y as i32,
            });
        }
    }

    fn tray_popup(&self, anchor: Rect) -> Rect {
        let count = self
            .tray
            .menu
            .as_ref()
            .map(|menu| menu.rows.len())
            .unwrap_or_else(|| self.tray_items().len().saturating_sub(self.tray_inline_count()));
        let screen = self.output_rect();
        let width = WIDTH.min((screen.w - 16.0).max(1.0));
        let available =
            if self.bar_at_bottom() { anchor.y - screen.y - 20.0 } else { screen.bottom() - anchor.bottom() - 20.0 };
        let available = available.clamp(ROW + 40.0, 424.0);
        let height = (40.0 + count.max(1) as f64 * ROW).min(available);
        Rect::new(
            (anchor.right() - width).clamp(screen.x + 8.0, (screen.right() - width - 8.0).max(screen.x + 8.0)),
            self.bar_popup_y(anchor, height),
            width,
            height,
        )
    }

    fn tray_visible_rows(&self, rect: Rect) -> usize {
        ((rect.h - 40.0) / ROW).floor().max(1.0) as usize
    }

    /// An open popup captures outside clicks too, so they dismiss it safely.
    pub fn tray_popup_button(&mut self, anchor: Rect, button: u32) {
        let rect = self.tray_popup(anchor);
        let point = Point::new(self.pointer.x, self.pointer.y);
        if !rect.contains(point) {
            self.close_tray();
            return;
        }
        if button != crate::input::BTN_LEFT && button != crate::input::BTN_RIGHT {
            return;
        }
        if point.y < rect.y + 36.0 {
            if self.tray.menu.is_some() {
                self.tray_menu_back();
            }
            return;
        }
        let row = ((point.y - rect.y - 36.0) / ROW) as usize + self.tray.offset;
        if let Some(menu) = &self.tray.menu {
            let Some(entry) = menu.rows.get(row).cloned() else {
                return;
            };
            if !entry.enabled || entry.separator {
                return;
            }
            let key = menu.key.clone();
            let path = menu.path.clone();
            if key.starts_with("debug:") {
                if entry.id == 1
                    && let Some(item) = self.tray.debug.iter_mut().find(|item| item.key == key)
                {
                    item.attention = !item.attention;
                }
                if entry.id == 2 {
                    self.tray.debug.retain(|item| item.key != key);
                }
                self.close_tray();
            } else if entry.submenu {
                let menu = self.tray.menu.as_mut().expect("a menu was selected above");
                menu.parents.push(entry.id);
                menu.rows.clear();
                menu.loading = true;
                self.tray.offset = 0;
                self.tray_request(Request::Menu { key, path, parent: entry.id });
            } else {
                self.tray_request(Request::Click { key, path, id: entry.id });
                self.close_tray();
            }
        } else {
            let item = self.tray_items().get(self.tray_inline_count() + row).copied().cloned();
            if let Some(item) = item {
                self.tray_activate(item, button);
            }
        }
    }

    fn tray_menu_back(&mut self) {
        let Some(menu) = &mut self.tray.menu else {
            return;
        };
        if menu.parents.len() <= 1 {
            self.tray.menu = None;
            self.tray.offset = 0;
            if self.tray_items().len() <= self.tray_limit() {
                self.close_tray();
            }
        } else {
            menu.parents.pop();
            let parent = *menu.parents.last().expect("the root menu remains");
            menu.rows.clear();
            menu.loading = true;
            let request = Request::Menu { key: menu.key.clone(), path: menu.path.clone(), parent };
            self.tray.offset = 0;
            self.tray_request(request);
        }
    }

    pub fn tray_scroll(&mut self, slots: &[(Slot, Rect)], anchor: Rect, amount: f64) -> bool {
        if self.tray.open {
            let rect = self.tray_popup(anchor);
            let count = self
                .tray
                .menu
                .as_ref()
                .map(|menu| menu.rows.len())
                .unwrap_or_else(|| self.tray_items().len().saturating_sub(self.tray_inline_count()));
            let limit = count.saturating_sub(self.tray_visible_rows(rect));
            self.tray.scrolled += amount;
            let steps = self.tray.scrolled.trunc();
            self.tray.scrolled -= steps;
            self.tray.offset = self.tray.offset.saturating_add_signed(steps as isize).min(limit);
            return true;
        }
        let point = Point::new(self.pointer.x, self.pointer.y);
        if let Some((slot, _)) = slots.iter().find(|(_, rect)| rect.contains(point)) {
            if let Slot::Item(index) = slot
                && let Some(item) = self.tray_items().get(*index)
            {
                self.tray_request(Request::Scroll { key: item.key.clone(), delta: (-amount * 120.0) as i32 });
            }
            return true;
        }
        false
    }

    fn draw_tray_item(&self, painter: &mut Painter, item: &Item, rect: Rect) {
        if let Some(image) = self.launcher.app_icon(&item.icon, 16) {
            painter.image(&image, (rect.x + (rect.w - 16.0) / 2.0, rect.y + (rect.h - 16.0) / 2.0));
        } else if let Some(image) = self.tray.images.get(&item.key) {
            painter.image_sized(
                image,
                (rect.x + (rect.w - 16.0) / 2.0, rect.y + (rect.h - 16.0) / 2.0),
                Some((16, 16)),
            );
        } else if item.key.starts_with("debug:") {
            let text = item.key.trim_start_matches("debug:");
            painter.text(text, rect, Style::new(11.0, self.theme.accent).bold().align(Align::Centre));
        } else {
            painter.glyph(glyph::WINDOW, rect, 16, self.theme.text);
        }
        if item.attention {
            painter.fill(Rect::new(rect.right() - 6.0, rect.y + 2.0, 4.0, 4.0), self.theme.warning, 2.0);
        }
    }

    pub fn draw_tray_slots(&self, painter: &mut Painter, slots: &[(Slot, Rect)]) {
        let items = self.tray_items();
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        for (slot, rect) in slots {
            if rect.contains(pointer) {
                painter.fill(*rect, self.theme.surface_raised, 4.0);
            }
            match slot {
                Slot::More => {
                    painter.text("…", *rect, Style::new(18.0, self.theme.text).align(Align::Centre));
                }
                Slot::Item(index) => {
                    if let Some(item) = items.get(*index) {
                        self.draw_tray_item(painter, item, *rect);
                    }
                }
            }
        }
    }

    pub fn draw_tray_popup(&self, painter: &mut Painter, slots: &[(Slot, Rect)], anchor: Rect) {
        if !self.tray.open {
            let pointer = Point::new(self.pointer.x, self.pointer.y);
            if let Some((Slot::Item(index), rect)) = slots.iter().find(|(_, rect)| rect.contains(pointer))
                && let Some(item) = self.tray_items().get(*index)
            {
                self.bar_tooltip(painter, rect.x - 100.0, *rect, &item.title, &item.description);
            }
            return;
        }
        let rect = self.tray_popup(anchor);
        painter.popup(rect);
        let style = Style::new(self.theme.font_size * 0.9, self.theme.text);
        let title = self
            .tray
            .menu
            .as_ref()
            .and_then(|menu| {
                self.tray_items().iter().find(|item| item.key == menu.key).map(|item| format!("‹  {}", item.title))
            })
            .unwrap_or_else(|| "Background applications".into());
        painter.text(&title, Rect::new(rect.x + 12.0, rect.y + 4.0, rect.w - 24.0, 28.0), style.bold());
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let count = self.tray_visible_rows(rect);
        let total = self
            .tray
            .menu
            .as_ref()
            .map(|menu| menu.rows.len())
            .unwrap_or_else(|| self.tray_items().len().saturating_sub(self.tray_inline_count()));
        if total > count {
            let track = rect.h - 44.0;
            let thumb = (track * count as f64 / total as f64).max(12.0);
            let y = rect.y + 38.0 + (track - thumb) * self.tray.offset as f64 / (total - count) as f64;
            painter.fill(Rect::new(rect.right() - 4.0, y, 2.0, thumb), self.theme.muted.alpha(0.6), 1.0);
        }
        if let Some(menu) = &self.tray.menu {
            if menu.loading || menu.rows.is_empty() {
                painter.text(
                    if menu.loading { "Loading…" } else { "No actions available" },
                    Rect::new(rect.x + 12.0, rect.y + 36.0, rect.w - 24.0, ROW),
                    style.colour(self.theme.muted),
                );
            }
            for (i, entry) in menu.rows.iter().skip(self.tray.offset).take(count).enumerate() {
                let row = Rect::new(rect.x + 6.0, rect.y + 36.0 + i as f64 * ROW, rect.w - 12.0, ROW);
                if entry.separator {
                    painter.fill(Rect::new(row.x + 6.0, row.y + ROW / 2.0, row.w - 12.0, 1.0), self.theme.border, 0.0);
                    continue;
                }
                if entry.enabled && row.contains(pointer) {
                    painter.fill(row, self.theme.surface_raised, 4.0);
                }
                let mark = match entry.checked {
                    Some(true) => "✓ ",
                    Some(false) => "○ ",
                    None => "",
                };
                let label = format!("{mark}{}{}", entry.label, if entry.submenu { "  ›" } else { "" });
                painter.text(
                    &label,
                    Rect::new(row.x + 8.0, row.y, row.w - 16.0, row.h),
                    style.colour(if entry.enabled { self.theme.text } else { self.theme.muted }),
                );
            }
        } else {
            for (i, item) in
                self.tray_items().iter().skip(self.tray_inline_count() + self.tray.offset).take(count).enumerate()
            {
                let row = Rect::new(rect.x + 6.0, rect.y + 36.0 + i as f64 * ROW, rect.w - 12.0, ROW);
                if row.contains(pointer) {
                    painter.fill(row, self.theme.surface_raised, 4.0);
                }
                self.draw_tray_item(painter, item, Rect::new(row.x + 4.0, row.y, 24.0, row.h));
                painter.text(&item.title, Rect::new(row.x + 34.0, row.y, row.w - 42.0, row.h), style);
            }
        }
    }

    /// Without a count add one entry; with a count replace the debug entries.
    pub fn tray_debug(&mut self, count: Option<usize>) -> String {
        if let Some(count) = count {
            self.tray.debug.clear();
            self.tray.next_debug = 0;
            for _ in 0..count {
                self.add_debug_tray_item();
            }
        } else {
            self.add_debug_tray_item();
        }
        self.close_tray();
        format!("{} debug tray entries, {} total visible entries\n", self.tray.debug.len(), self.tray_items().len())
    }

    fn add_debug_tray_item(&mut self) {
        self.tray.next_debug += 1;
        let n = self.tray.next_debug;
        self.tray.debug.push(Item {
            key: format!("debug:{n}"),
            title: format!("Debug entry {n}"),
            description: "Click to test the tray menu".into(),
            icon: String::new(),
            theme_path: String::new(),
            pixmaps: Vec::new(),
            attention: false,
            passive: false,
            menu: None,
            is_menu: true,
        });
    }

    pub fn tray_status(&self) -> String {
        let items = self.tray_items();
        let mut text =
            format!("{} visible tray entries; limit {} (including overflow)\n", items.len(), self.tray_limit());
        for item in items {
            text.push_str(&format!("{}: {}\n", item.key, item.title));
        }
        text
    }
}

fn bitmap(item: &Item) -> Option<MemoryRenderBuffer> {
    let (w, h, bytes) = item
        .pixmaps
        .iter()
        .filter(|(w, h, bytes)| {
            *w > 0 && *h > 0 && *w <= 1024 && *h <= 1024 && bytes.len() == (*w as usize * *h as usize * 4)
        })
        .min_by_key(|(w, _, _)| (*w - 32).unsigned_abs())?;
    let mut rgba = bytes.clone();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let [a, r, g, b] = [pixel[0], pixel[1], pixel[2], pixel[3]];
        pixel.copy_from_slice(&[r, g, b, a]);
    }
    let image = image::RgbaImage::from_raw(*w as u32, *h as u32, rgba)?;
    let mut image = image::imageops::resize(&image, 16, 16, image::imageops::FilterType::Triangle);
    for pixel in image.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = (u32::from(*channel) * alpha / 255) as u8;
        }
    }
    Some(MemoryRenderBuffer::from_slice(image.as_raw(), Fourcc::Abgr8888, (16, 16), 1, Transform::Normal, None))
}
