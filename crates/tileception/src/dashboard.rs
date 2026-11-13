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
    collections::{BTreeMap, HashMap},
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
    shapes::Shape,
    theme::Colour,
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
    /// `id`.
    #[serde(rename = "id")]
    pub key: String,
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
            key: self.key.clone(),
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
    /// `id`.
    #[serde(rename = "id")]
    pub key: String,
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

/// The file in the state directory with where the user put the items, by
/// key: `notifications` or `plugin/id`.
const LAYOUT: &str = "dashboard-layout.toml";
/// Space between items.
const GAP: f64 = MARGIN / 2.0;
/// The grid a dragged item snaps to.
const GRID: f64 = 8.0;

/// Pictures by plugin and source. They are read and decoded off the
/// compositor's thread, which a large or complex picture would hold up.
/// Also where the user put the items, and a drag under way.
#[derive(Default)]
pub struct Dashboard {
    /// Items by key, as last put; others go in the first free place.
    spots: BTreeMap<String, Spot>,
    drag: Option<Drag>,
    pictures: RefCell<HashMap<PictureKey, Loading>>,
    /// Decoded on their threads, waiting to become buffers here.
    decoded: Finished,
    /// Rung by a thread that finished, for a frame that shows it.
    ping: Option<smithay::reexports::calloop::ping::Ping>,
}

impl Dashboard {
    /// With the places saved last time.
    pub fn load() -> Self {
        let spots = crate::panel::order::read_state(LAYOUT)
            .and_then(|text| toml::from_str::<Stored>(&text).ok())
            .map(|stored| stored.spots)
            .unwrap_or_default();
        Dashboard { spots, ..Default::default() }
    }

    fn save(&self) {
        match toml::to_string(&Stored { spots: self.spots.clone() }) {
            Ok(text) => crate::panel::order::write_state(LAYOUT, &text),
            Err(error) => tracing::warn!("could not save {LAYOUT}: {error}"),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    spots: BTreeMap<String, Spot>,
}

/// Where the user put an item: its distance from the nearest corner of the
/// dashboard's room, so it keeps its place on a screen of another size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Spot {
    /// From the left edge, or the right with `right`.
    #[serde(rename = "x")]
    across: f64,
    /// From the top edge, or the bottom with `bottom`.
    #[serde(rename = "y")]
    down: f64,
    #[serde(default)]
    right: bool,
    #[serde(default)]
    bottom: bool,
}

impl Spot {
    fn from_rect(rect: Rect, bounds: Rect) -> Spot {
        let (centre, middle) = (rect.center(), bounds.center());
        let (right, bottom) = (centre.x > middle.x, centre.y > middle.y);
        Spot {
            across: if right { bounds.right() - rect.right() } else { rect.x - bounds.x },
            down: if bottom { bounds.bottom() - rect.bottom() } else { rect.y - bounds.y },
            right,
            bottom,
        }
    }

    fn rect(self, bounds: Rect, width: f64, height: f64) -> Rect {
        Rect::new(
            if self.right { bounds.right() - self.across - width } else { bounds.x + self.across },
            if self.bottom { bounds.bottom() - self.down - height } else { bounds.y + self.down },
            width,
            height,
        )
    }
}

/// An item held with the pointer: dragged elsewhere once the pointer goes
/// further than the threshold, else a click when let go.
struct Drag {
    key: String,
    origin: Point,
    /// Where it was held, from the item's top left.
    grab: Point,
    dragging: bool,
    /// What letting go without dragging does.
    click: Option<Click>,
}

/// One item on the dashboard and where it goes.
pub(crate) struct Slot {
    /// `notifications`, or `plugin/id`.
    pub key: String,
    pub rect: Rect,
    pub what: Shown,
}

impl Shown {
    fn height(&self) -> f64 {
        match self {
            Shown::Notifications { .. } => 76.0,
            Shown::Card { laid, .. } => laid.rect.h,
            Shown::Picture { picture, .. } => picture.size.height(),
            Shown::Surface { surface, .. } => surface.size.height(),
        }
    }
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

/// The dashboard's room beside a centre panel: the screen within a margin,
/// the two zones left and right of the panel where items go, and the
/// width every item gets.
pub(crate) struct Room {
    bounds: Rect,
    zones: [Rect; 2],
    width: f64,
}

impl Room {
    /// None when the zones are too narrow for an item.
    pub(crate) fn beside(area: Rect, centre: Rect) -> Option<Room> {
        let bounds = area.inset(MARGIN);
        let left = Rect::new(bounds.x, bounds.y, (centre.x - MARGIN - bounds.x).max(0.0), bounds.h);
        let right_x = centre.right() + MARGIN;
        let right = Rect::new(right_x, bounds.y, (bounds.right() - right_x).max(0.0), bounds.h);
        let width = left.w.min(right.w).min(COLUMN.0);
        (width >= COLUMN.1).then_some(Room { bounds, zones: [left, right], width })
    }

    /// Whether a rectangle lies in a zone, a gap away from the others.
    fn free(&self, rect: &Rect, others: &[Rect]) -> bool {
        let spaced =
            Rect::new(rect.x - GAP + 0.5, rect.y - GAP + 0.5, rect.w + 2.0 * GAP - 1.0, rect.h + 2.0 * GAP - 1.0);
        self.zones.iter().any(|zone| zone.contains_rect(rect)) && others.iter().all(|other| !spaced.intersects(other))
    }

    /// The rectangle moved into the zone on its side, if it fits there.
    fn clamp(&self, rect: Rect) -> Option<Rect> {
        let middle = (self.zones[0].right() + self.zones[1].x) / 2.0;
        let zone = if rect.center().x < middle { self.zones[0] } else { self.zones[1] };
        if rect.w > zone.w + 1e-9 || rect.h > zone.h + 1e-9 {
            return None;
        }
        let left = rect.x.clamp(zone.x, zone.right() - rect.w);
        let top = rect.y.clamp(zone.y, zone.bottom() - rect.h);
        Some(Rect::new(left, top, rect.w, rect.h))
    }

    /// The first free place for an item this tall, as a fresh dashboard
    /// fills: down beside the panel on the left, then on the right, then
    /// further out.
    fn first_free(&self, height: f64, others: &[Rect]) -> Option<Rect> {
        for (index, zone) in self.zones.iter().enumerate() {
            let near = if index == 0 { zone.right() - self.width } else { zone.x };
            let mut across = vec![near];
            for other in others {
                across.extend([other.x - GAP - self.width, other.right() + GAP]);
            }
            across.retain(|left| *left >= zone.x - 1e-9 && left + self.width <= zone.right() + 1e-9);
            across.sort_by(|one, other| (one - near).abs().total_cmp(&(other - near).abs()));
            let mut down = vec![zone.y];
            down.extend(others.iter().map(|other| other.bottom() + GAP));
            down.sort_by(f64::total_cmp);
            for left in &across {
                for top in &down {
                    let rect = Rect::new(*left, *top, self.width, height);
                    if self.free(&rect, others) {
                        return Some(rect);
                    }
                }
            }
        }
        None
    }

    /// The free place on the grid nearest to where an item is wanted.
    fn nearest_free(&self, wanted: Rect, others: &[Rect]) -> Option<Rect> {
        let snap = |value: f64, origin: f64| origin + ((value - origin) / GRID).round() * GRID;
        let wanted = Rect::new(snap(wanted.x, self.bounds.x), snap(wanted.y, self.bounds.y), wanted.w, wanted.h);
        let wanted = self.clamp(wanted)?;
        if self.free(&wanted, others) {
            return Some(wanted);
        }
        let mut best: Option<(f64, Rect)> = None;
        for zone in &self.zones {
            let mut top = zone.y;
            while top + wanted.h <= zone.bottom() + 1e-9 {
                let mut left = zone.x;
                while left + wanted.w <= zone.right() + 1e-9 {
                    let rect = Rect::new(left, top, wanted.w, wanted.h);
                    let distance = (left - wanted.x).hypot(top - wanted.y);
                    if best.is_none_or(|(closest, _)| distance < closest) && self.free(&rect, others) {
                        best = Some((distance, rect));
                    }
                    left += GRID;
                }
                top += GRID;
            }
        }
        best.map(|(_, rect)| rect)
    }

    /// Where each item goes: where the user put it if that is still free,
    /// else in the first free place; `None` for one that does not fit.
    fn arrange(&self, items: &[(Option<Spot>, f64)]) -> Vec<Option<Rect>> {
        let mut placed: Vec<Rect> = Vec::new();
        let mut rects = vec![None; items.len()];
        for (index, (spot, height)) in items.iter().enumerate() {
            let Some(spot) = spot else { continue };
            if let Some(rect) = self.clamp(spot.rect(self.bounds, self.width, *height))
                && self.free(&rect, &placed)
            {
                placed.push(rect);
                rects[index] = Some(rect);
            }
        }
        for (index, (_, height)) in items.iter().enumerate() {
            if rects[index].is_none()
                && let Some(rect) = self.first_free(*height, &placed)
            {
                placed.push(rect);
                rects[index] = Some(rect);
            }
        }
        rects
    }
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

    /// The items and where they go, beside the welcome screen's panel,
    /// with one being dragged at the free place nearest the pointer.
    pub(crate) fn dashboard_slots(&self, centre: Rect) -> Vec<Slot> {
        // Across the whole screen: the welcome screen and the peek both
        // centre on it, also on a split screen.
        let Some(room) = Room::beside(self.screen_room(), centre) else { return Vec::new() };
        let (keys, shown) = self.dashboard_items(room.width);
        let rects = self.dashboard_rects(&room, &keys, &shown);
        keys.into_iter()
            .zip(shown)
            .zip(rects)
            .filter_map(|((key, mut what), rect)| {
                let rect = rect?;
                if let Shown::Card { card, laid, .. } = &mut what {
                    *laid = crate::panel::card_layout(card, rect.x, rect.y, rect.w);
                }
                Some(Slot { key, rect, what })
            })
            .collect()
    }

    /// Where the items go, by index: see [`Room::arrange`]. The one being
    /// dragged goes to the free place on the grid nearest the pointer.
    fn dashboard_rects(&self, room: &Room, keys: &[String], shown: &[Shown]) -> Vec<Option<Rect>> {
        let items: Vec<(Option<Spot>, f64)> =
            keys.iter().zip(shown).map(|(key, what)| (self.dashboard.spots.get(key).copied(), what.height())).collect();
        let Some((drag, index)) = self
            .dashboard
            .drag
            .as_ref()
            .filter(|drag| drag.dragging)
            .and_then(|drag| Some((drag, keys.iter().position(|key| *key == drag.key)?)))
        else {
            return room.arrange(&items);
        };
        let mut rest = items.clone();
        let (spot, height) = rest.remove(index);
        let mut rects = room.arrange(&rest);
        let others: Vec<Rect> = rects.iter().flatten().copied().collect();
        let wanted = Rect::new(self.pointer.x - drag.grab.x, self.pointer.y - drag.grab.y, room.width, height);
        let rect =
            room.nearest_free(wanted, &others).or_else(|| spot.map(|spot| spot.rect(room.bounds, room.width, height)));
        rects.insert(index, rect);
        rects
    }

    /// Every item's key and what it shows, in the plugins' order.
    fn dashboard_items(&self, width: f64) -> (Vec<String>, Vec<Shown>) {
        let mut keys = Vec::new();
        let mut shown = Vec::new();
        let count = self.notification_count();
        if count > 0 {
            keys.push("notifications".to_string());
            shown.push(Shown::Notifications { count, latest: self.latest_notification().unwrap_or_default() });
        }
        let offered = self.plugins.as_ref().map(crate::plugins::Host::dashboard_items).unwrap_or_default();
        for (plugin, item) in offered {
            let (item_id, what) = match item {
                Item::Card(card) => {
                    let laid = crate::panel::card_layout(card, 0.0, 0.0, width);
                    (&card.key, Shown::Card { plugin: plugin.to_string(), card: card.clone(), laid })
                }
                Item::Image(picture) => {
                    (&picture.key, Shown::Picture { plugin: plugin.to_string(), picture: picture.light() })
                }
                Item::Surface(surface) => {
                    (&surface.key, Shown::Surface { plugin: plugin.to_string(), surface: surface.clone() })
                }
            };
            let mut key = format!("{plugin}/{item_id}");
            // Keys must tell items apart, even ones with the same id.
            while keys.contains(&key) {
                key.push('+');
            }
            keys.push(key);
            shown.push(what);
        }
        (keys, shown)
    }

    /// A press on the dashboard of the welcome screen. Its controls (a
    /// card's buttons, a surface's inside) keep it; anywhere else the item
    /// is held, to drag it elsewhere or, let go where it is, to click it.
    pub(crate) fn dashboard_press(&mut self, slots: &[Slot], point: Point) -> bool {
        let Some(slot) = slots.iter().find(|slot| slot.rect.contains(point)) else { return false };
        match &slot.what {
            Shown::Card { card, laid, .. } if crate::panel::card_action_at(card, laid, point).is_some() => {
                return false;
            }
            Shown::Surface { .. } if inner(slot.rect).contains(point) => return false,
            _ => {}
        }
        let click = Self::dashboard_click_at(slots, point).map(|(_, click)| click);
        let grab = Point::new(point.x - slot.rect.x, point.y - slot.rect.y);
        self.dashboard.drag = Some(Drag { key: slot.key.clone(), origin: point, grab, dragging: false, click });
        true
    }

    /// The pointer moved while an item is held. True when one is.
    pub(crate) fn dashboard_drag_motion(&mut self, point: Point) -> bool {
        let Some(drag) = &self.dashboard.drag else { return false };
        let starts =
            !drag.dragging && (point.x - drag.origin.x).hypot(point.y - drag.origin.y) > crate::input::DRAG_THRESHOLD;
        if starts {
            // The others stay where they are now while this one moves.
            self.dashboard_pin();
            if let Some(drag) = &mut self.dashboard.drag {
                drag.dragging = true;
            }
        }
        if self.dashboard_dragging() {
            self.schedule_render();
        }
        true
    }

    /// Notes where every item is now as where the user put it.
    fn dashboard_pin(&mut self) {
        let Some(room) = Room::beside(self.screen_room(), self.home_panel()) else { return };
        for slot in self.dashboard_slots(self.home_panel()) {
            self.dashboard.spots.insert(slot.key, Spot::from_rect(slot.rect, room.bounds));
        }
    }

    /// Whether an item is being dragged elsewhere.
    pub(crate) fn dashboard_dragging(&self) -> bool {
        self.dashboard.drag.as_ref().is_some_and(|drag| drag.dragging)
    }

    /// Whether a press here would hold an item of the welcome screen's
    /// dashboard to drag it: on an item, but not on a card's controls or
    /// inside a plugin's own surface.
    pub(crate) fn dashboard_grabbable(&self, point: Point) -> bool {
        if !(self.launcher.open && self.launcher.home) {
            return false;
        }
        let slots = self.dashboard_slots(self.home_panel());
        slots.iter().find(|slot| slot.rect.contains(point)).is_some_and(|slot| match &slot.what {
            Shown::Card { card, laid, .. } => crate::panel::card_action_at(card, laid, point).is_none(),
            Shown::Surface { .. } => !inner(slot.rect).contains(point),
            _ => true,
        })
    }

    /// Whether an item is held, so the plugins' surfaces do not get the
    /// pointer meanwhile.
    pub(crate) fn dashboard_holding(&self) -> bool {
        self.dashboard.drag.is_some()
    }

    /// Lets go of a held item: a click clicks it, a drag puts it where it
    /// was dropped and saves where every item is. True when an item was held.
    pub(crate) fn dashboard_release(&mut self) -> bool {
        if self.dashboard.drag.is_none() {
            return false;
        }
        let point = Point::new(self.pointer.x, self.pointer.y);
        let home = self.launcher.open && self.launcher.home;
        // Where everything shows now, the dragged item included.
        let slots = if home { self.dashboard_slots(self.home_panel()) } else { Vec::new() };
        let Some(drag) = self.dashboard.drag.take() else { return false };
        if !home {
            return true;
        }
        if !drag.dragging {
            let same = slots.iter().any(|slot| slot.key == drag.key && slot.rect.contains(point));
            if let (true, Some(click)) = (same, drag.click) {
                self.dashboard_click(click);
            }
            return true;
        }
        if let Some(room) = Room::beside(self.screen_room(), self.home_panel())
            && let Some(slot) = slots.iter().find(|slot| slot.key == drag.key)
        {
            self.dashboard.spots.insert(drag.key, Spot::from_rect(slot.rect, room.bounds));
            self.dashboard.save();
        }
        self.schedule_render();
        true
    }

    /// A card's control here, which is a button of its own, and its rectangle.
    pub(crate) fn dashboard_control_at(slots: &[Slot], point: Point) -> Option<(Rect, Click)> {
        let slot = slots.iter().find(|slot| slot.rect.contains(point))?;
        let Shown::Card { plugin, card, laid } = &slot.what else { return None };
        let (rect, action) = crate::panel::card_action_at(card, laid, point)?;
        Some((rect, Click::Plugin(plugin.clone(), action)))
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
        let dragged = self.dashboard.drag.as_ref().filter(|drag| drag.dragging).map(|drag| drag.key.as_str());
        // Nothing lights up under the pointer while an item is dragged.
        let pointer = match dragged {
            Some(_) => Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
            None => Point::new(self.pointer.x, self.pointer.y),
        };
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
                    if !self.dashboard_surface_drawn(plugin, &surface.key) {
                        let style = Style::new(theme.font_size * 0.9, theme.muted).align(Align::Centre);
                        painter.text("Starting…", inner, style);
                    }
                }
            }
        }
        // Where the dragged item will go, outlined.
        if let Some(slot) = slots.iter().find(|slot| Some(slot.key.as_str()) == dragged) {
            painter.shape(
                slot.rect,
                Shape {
                    border_width: 2.0,
                    border: theme.accent,
                    glow_radius: 16.0,
                    glow: theme.accent.alpha(0.35),
                    ..Shape::filled(Colour::TRANSPARENT, theme.popup_radius)
                },
            );
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

    fn room() -> Room {
        // 1920 by 1080 with a 620 wide panel in the middle.
        Room::beside(Rect::new(0.0, 0.0, 1920.0, 1080.0), Rect::new(650.0, 300.0, 620.0, 400.0)).unwrap()
    }

    #[test]
    fn a_fresh_dashboard_fills_beside_the_panel_left_then_right() {
        let room = room();
        let rects = room.arrange(&[(None, 140.0), (None, 140.0), (None, 900.0)]);
        let first = rects[0].unwrap();
        assert!((first.right() - (650.0 - MARGIN)).abs() < 1e-9 && first.y == MARGIN, "{first:?}");
        assert_eq!(rects[1].unwrap().y, MARGIN + 140.0 + GAP, "below the first");
        assert_eq!(rects[2].unwrap().x, 1270.0 + MARGIN, "too tall for what is left: on the right");
    }

    #[test]
    fn items_stay_where_they_were_put_and_others_go_around_them() {
        let room = room();
        let spot = Spot { across: 0.0, down: 0.0, right: false, bottom: true };
        let rects = room.arrange(&[(None, 140.0), (Some(spot), 140.0)]);
        let put = rects[1].unwrap();
        assert_eq!((put.x, put.bottom()), (MARGIN, 1080.0 - MARGIN), "bottom left corner");
        assert!(!rects[0].unwrap().intersects(&put));
        // A spot on top of another item's gives way.
        let clash = room.arrange(&[(Some(spot), 140.0), (Some(spot), 140.0)]);
        assert!(!clash[0].unwrap().intersects(&clash[1].unwrap()));
    }

    #[test]
    fn spots_keep_to_the_nearest_corner() {
        let bounds = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let rect = Rect::new(900.0, 700.0, 80.0, 60.0);
        let spot = Spot::from_rect(rect, bounds);
        assert!(spot.right && spot.bottom);
        assert!(spot.rect(bounds, 80.0, 60.0).approx_eq(&rect, 1e-9));
        // On a larger screen it stays as far from that corner.
        let larger = spot.rect(Rect::new(0.0, 0.0, 2000.0, 1200.0), 80.0, 60.0);
        assert!(larger.approx_eq(&Rect::new(1900.0, 1100.0, 80.0, 60.0), 1e-9));
    }

    #[test]
    fn a_dragged_item_snaps_to_the_grid_and_to_a_free_place() {
        let room = room();
        let other = Rect::new(300.0, 200.0, room.width, 140.0);
        let wanted = Rect::new(103.0, 499.0, room.width, 140.0);
        let snapped = room.nearest_free(wanted, &[other]).unwrap();
        assert_eq!(((snapped.x - room.bounds.x) % GRID, (snapped.y - room.bounds.y) % GRID), (0.0, 0.0));
        assert!((snapped.x - 103.0).abs() <= GRID && (snapped.y - 499.0).abs() <= GRID);
        // Onto the other item: the nearest free place instead.
        let onto = room.nearest_free(Rect::new(300.0, 210.0, room.width, 140.0), &[other]).unwrap();
        assert!(room.free(&onto, &[other]));
        // Over the panel: into the zone on that side.
        let left = room.nearest_free(Rect::new(700.0, 100.0, room.width, 140.0), &[]).unwrap();
        assert!(left.right() <= 650.0 - MARGIN + 1e-9, "{left:?}");
        let right = room.nearest_free(Rect::new(900.0, 100.0, room.width, 140.0), &[]).unwrap();
        assert!(right.x >= 1270.0 + MARGIN - 1e-9, "{right:?}");
    }

    #[test]
    fn no_room_on_a_narrow_area() {
        assert!(Room::beside(Rect::new(0.0, 0.0, 800.0, 600.0), Rect::new(100.0, 100.0, 600.0, 400.0)).is_none());
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
        assert!(
            matches!(&items.items[1], Item::Image(picture) if picture.size == Size::Medium && picture.key == "lamp")
        );
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
