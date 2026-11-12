//! The dashboard: columns of cards and pictures beside the welcome screen,
//! shown while the desk is clear or empty and behind a peek. Tileception adds
//! a summary of the notifications; plugins with a `[dashboard]` block add
//! the items of their `tileception://dashboard` resource.
//!
//! A plugin's item is a card (drawn like the panel's), a picture it writes
//! in any format and language it likes, or a surface it draws itself over a
//! restricted Wayland connection (see `dashboard_surface.rs`).

use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{Hash, Hasher},
    io::Read,
    path::Path,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    panel::{CardLayout, block::Card},
    plugins::ActionKind,
    ui::{Align, Painter, Style, glyph},
};

/// The most items one plugin puts on the dashboard.
pub const PLUGIN_ITEMS: usize = 6;
/// Space around and between the columns.
const MARGIN: f64 = 24.0;
/// The widest a column gets, and the narrowest worth showing.
const COLUMN: (f64, f64) = (340.0, 220.0);
/// A picture's or surface's title strip.
const TITLE: f64 = 28.0;
/// The largest picture file read, and the largest side decoded.
const PICTURE_BYTES: u64 = 8 * 1024 * 1024;
const PICTURE_SIDE: u32 = 4096;

/// A plugin's `tileception://dashboard` resource.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Items {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    /// The panel's card.
    Card(Box<Card>),
    /// A PNG, JPEG, WebP or SVG from a file or a `data:` URI.
    Image(Picture),
    /// A Wayland surface the plugin draws itself.
    Surface(Surface),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Picture {
    pub id: String,
    pub title: String,
    pub size: Size,
    /// An absolute path, or a `data:image/...;base64,` URI.
    pub src: String,
    /// What clicking it does.
    pub action: Option<ActionKind>,
    /// Which picture `src` names, worked out once when the resource is
    /// read, so the source itself need not be copied or hashed each frame.
    #[serde(skip)]
    pub digest: u64,
}

impl Picture {
    /// Notes which picture this is and keeps a copy of it without its
    /// source, which may be a large `data:` URI, for laying out.
    pub fn seal(&mut self) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.src.hash(&mut hasher);
        self.digest = hasher.finish();
    }

    fn light(&self) -> Picture {
        Picture {
            id: self.id.clone(),
            title: self.title.clone(),
            size: self.size,
            src: String::new(),
            action: self.action.clone(),
            digest: self.digest,
        }
    }
}

impl Items {
    /// Seals every picture; see [`Picture::seal`].
    pub fn seal(&mut self) {
        for item in &mut self.items {
            if let Item::Image(picture) = item {
                picture.seal();
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Surface {
    pub id: String,
    pub title: String,
    pub size: Size,
}

/// How tall an item is; every item is as wide as its column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    #[default]
    Small,
    Medium,
    Large,
}

impl Size {
    pub fn height(self) -> f64 {
        match self {
            Size::Small => 140.0,
            Size::Medium => 260.0,
            Size::Large => 380.0,
        }
    }
}

/// A decoded picture and its size in pixels.
type Decoded = (MemoryRenderBuffer, (f64, f64));
/// Premultiplied RGBA pixels, their width and height.
type Pixels = (Vec<u8>, u32, u32);
/// A picture by plugin and digest of its source.
type PictureKey = (String, u64);
/// What the decoding threads finished: `None` for a picture that failed.
type Finished = Arc<Mutex<Vec<(PictureKey, Option<Pixels>)>>>;

/// A picture being read and decoded on its own thread, or done.
enum Loading {
    Busy,
    Ready(Decoded),
    Failed,
}

/// Pictures by plugin and source. They are read and decoded off the
/// compositor's thread, which a large or complex picture would hold up.
#[derive(Default)]
pub struct Dashboard {
    pictures: RefCell<HashMap<PictureKey, Loading>>,
    /// Decoded on their threads, waiting to become buffers here.
    decoded: Finished,
    /// Rung by a thread that finished, for a frame that shows it.
    ping: Option<smithay::reexports::calloop::ping::Ping>,
}

/// One item on the dashboard and where it goes.
pub(crate) struct Slot {
    pub rect: Rect,
    pub what: Shown,
}

pub(crate) enum Shown {
    /// How many notifications there are, and the newest one's summary.
    Notifications {
        count: usize,
        latest: String,
    },
    Card {
        plugin: String,
        card: Box<Card>,
        laid: CardLayout,
    },
    Picture {
        plugin: String,
        picture: Picture,
    },
    Surface {
        plugin: String,
        surface: Surface,
    },
}

/// What a click on the dashboard does.
#[derive(Clone, PartialEq)]
pub(crate) enum Click {
    Notifications,
    Plugin(String, ActionKind),
}

/// Places items of these heights in columns, top to bottom and then the
/// next column, leaving out what does not fit. Returns each placed item's
/// index and rectangle.
pub(crate) fn place(columns: &[Rect], heights: &[f64]) -> Vec<(usize, Rect)> {
    let mut placed = Vec::new();
    let mut column = 0;
    let mut top = columns.first().map_or(0.0, |rect| rect.y);
    for (index, height) in heights.iter().enumerate() {
        while let Some(rect) = columns.get(column) {
            if top + height <= rect.bottom() {
                break;
            }
            column += 1;
            top = columns.get(column).map_or(0.0, |rect| rect.y);
        }
        let Some(rect) = columns.get(column) else { break };
        placed.push((index, Rect::new(rect.x, top, rect.w, *height)));
        top += height + MARGIN / 2.0;
    }
    placed
}

/// The columns beside a centre panel within an area: left, then right.
pub(crate) fn columns(area: Rect, centre: Rect) -> Vec<Rect> {
    let room = centre.x - area.x - 2.0 * MARGIN;
    let width = room.min(COLUMN.0);
    if width < COLUMN.1 {
        return Vec::new();
    }
    let (top, height) = (area.y + MARGIN, area.h - 2.0 * MARGIN);
    vec![
        Rect::new(centre.x - MARGIN - width, top, width, height),
        Rect::new(centre.right() + MARGIN, top, width, height),
    ]
}

impl Tileception {
    /// Whether the dashboard shows: with the welcome screen, or behind a peek.
    pub(crate) fn dashboard_shown(&self) -> bool {
        (self.launcher.open && self.launcher.home) || self.desk.peeking
    }

    /// A plugin's items were read again, or all plugins' (`None`). Its
    /// pictures from files are read anew; a `data:` URI names its picture
    /// by its content, so one that stays the same is kept.
    pub(crate) fn dashboard_changed(&mut self, plugin: Option<&str>) {
        if self.dashboard.ping.is_none() {
            match smithay::reexports::calloop::ping::make_ping() {
                Ok((ping, source)) => {
                    let inserted = self.loop_handle.insert_source(source, |_, _, state| state.schedule_render());
                    match inserted {
                        Ok(_) => self.dashboard.ping = Some(ping),
                        Err(error) => tracing::warn!("dashboard pictures: {error}"),
                    }
                }
                Err(error) => tracing::warn!("dashboard pictures: {error}"),
            }
        }
        let offered = self.plugins.as_ref().map(crate::plugins::Host::dashboard_items).unwrap_or_default();
        let keep: std::collections::HashSet<(String, u64)> = offered
            .iter()
            .filter_map(|(owner, item)| match item {
                Item::Image(picture) if plugin.is_some_and(|plugin| plugin != *owner) => {
                    Some((owner.to_string(), picture.digest))
                }
                Item::Image(picture) if plugin.is_some() && picture.src.starts_with("data:") => {
                    Some((owner.to_string(), picture.digest))
                }
                _ => None,
            })
            .collect();
        self.dashboard.pictures.borrow_mut().retain(|key, _| keep.contains(key));
        if self.dashboard_shown() {
            self.schedule_render();
        }
    }

    /// The items and where they go, beside the welcome screen's panel.
    pub(crate) fn dashboard_slots(&self, centre: Rect) -> Vec<Slot> {
        let columns = columns(self.world.screen(), centre);
        let Some(width) = columns.first().map(|column| column.w) else { return Vec::new() };
        let mut shown = Vec::new();
        let count = self.notification_count();
        if count > 0 {
            shown.push(Shown::Notifications { count, latest: self.latest_notification().unwrap_or_default() });
        }
        let offered = self.plugins.as_ref().map(crate::plugins::Host::dashboard_items).unwrap_or_default();
        for (plugin, item) in offered {
            let plugin = plugin.to_string();
            shown.push(match item {
                Item::Card(card) => {
                    let laid = crate::panel::card_layout(card, 0.0, 0.0, width);
                    Shown::Card { plugin, card: card.clone(), laid }
                }
                Item::Image(picture) => Shown::Picture { plugin, picture: picture.light() },
                Item::Surface(surface) => Shown::Surface { plugin, surface: surface.clone() },
            });
        }
        let heights: Vec<f64> = shown
            .iter()
            .map(|what| match what {
                Shown::Notifications { .. } => 76.0,
                Shown::Card { laid, .. } => laid.rect.h,
                Shown::Picture { picture, .. } => picture.size.height(),
                Shown::Surface { surface, .. } => surface.size.height(),
            })
            .collect();
        let mut shown: Vec<Option<Shown>> = shown.into_iter().map(Some).collect();
        place(&columns, &heights)
            .into_iter()
            .filter_map(|(index, rect)| {
                let mut what = shown[index].take()?;
                if let Shown::Card { card, laid, .. } = &mut what {
                    *laid = crate::panel::card_layout(card, rect.x, rect.y, rect.w);
                }
                Some(Slot { rect, what })
            })
            .collect()
    }

    /// What clicking here on the dashboard does, and the control's rectangle.
    pub(crate) fn dashboard_click_at(slots: &[Slot], point: Point) -> Option<(Rect, Click)> {
        let slot = slots.iter().find(|slot| slot.rect.contains(point))?;
        match &slot.what {
            Shown::Notifications { .. } => Some((slot.rect, Click::Notifications)),
            Shown::Card { plugin, card, laid } => {
                let (rect, action) = crate::panel::card_action_at(card, laid, point)?;
                Some((rect, Click::Plugin(plugin.clone(), action)))
            }
            Shown::Picture { plugin, picture } => {
                picture.action.clone().map(|action| (slot.rect, Click::Plugin(plugin.clone(), action)))
            }
            Shown::Surface { .. } => None,
        }
    }

    pub(crate) fn dashboard_click(&mut self, click: Click) {
        match click {
            Click::Notifications => self.toggle_notifications(),
            Click::Plugin(plugin, action) => self.launcher_card_action(&plugin, action),
        }
    }

    pub(crate) fn draw_dashboard(&self, painter: &mut Painter, slots: &[Slot]) {
        let theme = painter.theme();
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        for slot in slots {
            match &slot.what {
                Shown::Notifications { count, latest } => {
                    painter.popup(slot.rect);
                    let icon = Rect::new(slot.rect.x + 12.0, slot.rect.y, 28.0, slot.rect.h);
                    painter.glyph(glyph::BELL, icon, 20, theme.accent);
                    let title =
                        if *count == 1 { "1 notification".to_string() } else { format!("{count} notifications") };
                    let text = Rect::new(slot.rect.x + 48.0, slot.rect.y + 14.0, slot.rect.w - 60.0, 22.0);
                    painter.text(&title, text, Style::new(theme.font_size * 1.05, theme.text).bold());
                    let detail = Rect::new(text.x, text.bottom() + 2.0, text.w, 22.0);
                    painter.text(latest, detail, Style::new(theme.font_size * 0.9, theme.muted).lines(1));
                }
                Shown::Card { card, laid, .. } => {
                    painter.popup(laid.rect);
                    self.draw_plugin_card(painter, card, laid, pointer, theme.surface);
                }
                Shown::Picture { plugin, picture } => {
                    let inner = self.draw_frame(painter, slot.rect, &picture.title, plugin);
                    if let Some((buffer, size)) = self.dashboard_picture(plugin, picture) {
                        painter.image_scaled(&buffer, size, fit(size, inner));
                    } else if self.dashboard_picture_failed(plugin, picture.digest) {
                        let style = Style::new(theme.font_size * 0.9, theme.muted).align(Align::Centre);
                        painter.text("No picture", inner, style);
                    }
                }
                Shown::Surface { plugin, surface } => {
                    let inner = self.draw_frame(painter, slot.rect, &surface.title, plugin);
                    if !self.dashboard_surface_drawn(plugin, &surface.id) {
                        let style = Style::new(theme.font_size * 0.9, theme.muted).align(Align::Centre);
                        painter.text("Starting…", inner, style);
                    }
                }
            }
        }
    }

    /// The frame Tileception draws around what a plugin drew: a title
    /// strip with the plugin's name, so it cannot pass for Tileception's
    /// own. Returns the room inside.
    fn draw_frame(&self, painter: &mut Painter, rect: Rect, title: &str, plugin: &str) -> Rect {
        let theme = painter.theme();
        painter.popup(rect);
        let strip = Rect::new(rect.x + 12.0, rect.y + 4.0, rect.w - 24.0, TITLE - 4.0);
        let name = Style::new(theme.font_size * 0.8, theme.muted).lines(1);
        let name_width = painter.measure(plugin, strip.w / 2.0, name).0;
        painter.text(plugin, Rect::new(strip.right() - name_width, strip.y, name_width, strip.h), name);
        let heading = Rect::new(strip.x, strip.y, strip.w - name_width - 8.0, strip.h);
        painter.text(title, heading, Style::new(theme.font_size * 0.9, theme.text).bold().lines(1));
        inner(rect)
    }

    fn dashboard_picture_failed(&self, plugin: &str, digest: u64) -> bool {
        matches!(self.dashboard.pictures.borrow().get(&(plugin.to_string(), digest)), Some(Loading::Failed))
    }

    /// A plugin's picture once decoded; until then `None`, and decoding
    /// starts on a thread of its own.
    fn dashboard_picture(&self, plugin: &str, picture: &Picture) -> Option<Decoded> {
        let mut pictures = self.dashboard.pictures.borrow_mut();
        // Take in what the threads finished.
        let finished =
            std::mem::take(&mut *self.dashboard.decoded.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        for (key, pixels) in finished {
            if !pictures.contains_key(&key) {
                continue;
            }
            let loaded = pixels.map(|(pixels, width, height)| {
                let size = (width as i32, height as i32);
                let buffer =
                    MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, size, 1, Transform::Normal, None);
                (buffer, (f64::from(width), f64::from(height)))
            });
            if loaded.is_none() {
                tracing::warn!("Plugin {}: could not load a dashboard picture", key.0);
            }
            pictures.insert(key, loaded.map_or(Loading::Failed, Loading::Ready));
        }
        let key = (plugin.to_string(), picture.digest);
        match pictures.get(&key) {
            Some(Loading::Ready(decoded)) => return Some(decoded.clone()),
            Some(_) => return None,
            None => {}
        }
        // The source itself stays with the plugin's items, not the layout.
        let offered = self.plugins.as_ref().map(crate::plugins::Host::dashboard_items).unwrap_or_default();
        let src = offered.iter().find_map(|(owner, item)| match item {
            Item::Image(found) if *owner == plugin && found.digest == picture.digest => Some(found.src.clone()),
            _ => None,
        })?;
        if pictures.len() >= 64 {
            pictures.clear();
        }
        pictures.insert(key.clone(), Loading::Busy);
        let decoded = self.dashboard.decoded.clone();
        let ping = self.dashboard.ping.clone();
        let spawned = std::thread::Builder::new().name("dashboard picture".into()).spawn(move || {
            let pixels = load_picture(&src);
            decoded.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((key, pixels));
            if let Some(ping) = ping {
                ping.ping();
            }
        });
        if let Err(error) = spawned {
            tracing::warn!("could not start decoding a dashboard picture: {error}");
        }
        None
    }
}

/// The room inside an item's frame, under its title strip.
pub(crate) fn inner(rect: Rect) -> Rect {
    Rect::new(rect.x + 8.0, rect.y + TITLE, rect.w - 16.0, rect.h - TITLE - 8.0)
}

/// The largest rectangle of this aspect that fits in `room`, centred.
pub(crate) fn fit((width, height): (f64, f64), room: Rect) -> Rect {
    if width <= 0.0 || height <= 0.0 {
        return room;
    }
    let scale = (room.w / width).min(room.h / height);
    let (shown_w, shown_h) = (width * scale, height * scale);
    Rect::new(room.x + (room.w - shown_w) / 2.0, room.y + (room.h - shown_h) / 2.0, shown_w, shown_h)
}

/// Reads and decodes a picture: an absolute path to a regular file, or a
/// base64 `data:` URI. Runs on a thread of its own.
fn load_picture(src: &str) -> Option<Pixels> {
    let bytes = picture_bytes(src)?;
    let svg = src.ends_with(".svg") || src.starts_with("data:image/svg") || bytes.trim_ascii_start().starts_with(b"<");
    if svg { render_svg(&bytes) } else { decode_raster(&bytes) }
}

fn picture_bytes(src: &str) -> Option<Vec<u8>> {
    if let Some(rest) = src.strip_prefix("data:") {
        let (_, encoded) = rest.split_once(";base64,")?;
        if encoded.len() as u64 > PICTURE_BYTES * 4 / 3 + 4 {
            return None;
        }
        return base64(encoded);
    }
    let path = Path::new(src);
    // A regular file only: a pipe or a device could block or never end.
    let metadata = std::fs::metadata(path).ok()?;
    if !path.is_absolute() || !metadata.is_file() || metadata.len() > PICTURE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(PICTURE_BYTES).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Premultiplied RGBA pixels, as the renderer wants them.
fn decode_raster(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let format = image::guess_format(bytes).ok()?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(PICTURE_SIDE);
    limits.max_image_height = Some(PICTURE_SIDE);
    limits.max_alloc = Some(PICTURE_SIDE as u64 * PICTURE_SIDE as u64 * 4);
    reader.limits(limits);
    let mut image = reader.decode().ok()?.into_rgba8();
    for pixel in image.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = (u32::from(*channel) * alpha / 255) as u8;
        }
    }
    let (width, height) = image.dimensions();
    Some((image.into_raw(), width, height))
}

/// An SVG drawn with its longest side at 512 pixels.
fn render_svg(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    static FONTS: std::sync::OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> = std::sync::OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let mut fonts = resvg::usvg::fontdb::Database::new();
        fonts.load_system_fonts();
        std::sync::Arc::new(fonts)
    });
    let options = resvg::usvg::Options { resources_dir: None, fontdb: fonts.clone(), ..Default::default() };
    let tree = resvg::usvg::Tree::from_data(bytes, &options).ok()?;
    let size = tree.size();
    let scale = 512.0 / size.width().max(size.height()).max(1.0);
    let (width, height) =
        ((size.width() * scale).ceil().max(1.0) as u32, (size.height() * scale).ceil().max(1.0) as u32);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Some((pixmap.take(), width, height))
}

/// Standard base64, ignoring whitespace; `None` for anything else.
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;

    #[test]
    fn items_fill_the_left_column_then_the_right_and_leave_out_the_rest() {
        let columns = [Rect::new(0.0, 0.0, 100.0, 300.0), Rect::new(500.0, 0.0, 100.0, 300.0)];
        let placed = place(&columns, &[140.0, 140.0, 260.0, 140.0]);
        let rects: Vec<_> = placed.iter().map(|(index, rect)| (*index, rect.x, rect.y)).collect();
        assert_eq!(rects, vec![(0, 0.0, 0.0), (1, 0.0, 152.0), (2, 500.0, 0.0)], "the last does not fit");
    }

    #[test]
    fn no_columns_on_a_narrow_area() {
        let area = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert!(columns(area, Rect::new(100.0, 100.0, 600.0, 400.0)).is_empty());
        let wide = columns(Rect::new(0.0, 0.0, 1920.0, 1080.0), Rect::new(650.0, 300.0, 620.0, 400.0));
        assert_eq!(wide.len(), 2);
        assert!(wide[0].right() < 650.0 && wide[1].x > 1270.0);
    }

    #[test]
    fn items_parse_by_type() {
        let items: Items = serde_json::from_str(
            r#"{"items":[{"type":"card","id":"t","title":"Tea"},
                {"type":"image","id":"lamp","size":"medium","src":"/tmp/lamp.png"},
                {"type":"surface","id":"lava","title":"Lava lamp","size":"large"}]}"#,
        )
        .unwrap();
        assert!(matches!(&items.items[0], Item::Card(card) if card.title == "Tea"));
        assert!(matches!(&items.items[1], Item::Image(picture) if picture.size == Size::Medium));
        assert!(matches!(&items.items[2], Item::Surface(surface) if surface.size == Size::Large));
    }

    #[test]
    fn base64_decodes_and_rejects_other_text() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVs\nbG8").unwrap(), b"hello");
        assert!(base64("not*base64").is_none());
    }

    #[test]
    fn pictures_come_only_from_regular_files_and_data() {
        assert!(picture_bytes("relative.png").is_none());
        assert!(picture_bytes("/dev/zero").is_none());
        assert!(picture_bytes("data:image/png;base64,aGVsbG8=").is_some());
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20"><rect width="10" height="20" fill="red"/></svg>"#;
        let (_, width, height) = render_svg(svg).unwrap();
        assert_eq!((width, height), (256, 512));
    }

    #[test]
    fn fitting_keeps_the_aspect() {
        let fitted = fit((200.0, 100.0), Rect::new(0.0, 0.0, 100.0, 100.0));
        assert!(fitted.approx_eq(&Rect::new(0.0, 25.0, 100.0, 50.0), 1e-9));
    }
}
