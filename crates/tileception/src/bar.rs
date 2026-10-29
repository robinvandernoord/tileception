//! The bar along the top or bottom: where the camera is in the tree, and a few things
//! worth a glance. Left to right: the doctor with a health light, back and
//! forward, a breadcrumb of the camera's path, the saved views, chips that
//! only show when relevant, the clock (which zooms into a calendar) and the
//! notifications.
//!
//! The bar reserves its edge like a layer-shell bar. While one window is
//! framed on its own it shrinks to a hairline and gives the window the
//! space; touching its edge slides it back over the window. A
//! fullscreen window covers it.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::pointer::MotionEvent,
    utils::{Logical, Point as SPoint, SERIAL_COUNTER},
};
use tileception_core::{Node, NodeId, Point, Rect, WindowId};

use crate::{
    Tileception,
    input::Action,
    render::OutputElement,
    shapes::Shape,
    shell::{BarPosition, Part},
    theme::Colour,
    ui::{Align, LocalTime, Painter, Style, ease_out, glyph},
};

/// The bar's height while it reserves its edge.
pub const BAR_H: f64 = 28.0;
/// Pixels at the configured edge that bring a hairline bar back.
const EDGE: f64 = 2.0;
/// Pixels below the bar the pointer may stray before a revealed bar hides.
const SLACK: f64 = 18.0;
/// Seconds the bar takes to slide in or out over a framed window.
const SLIDE: f64 = 0.16;
/// How long a capture keeps its chip after the last frame was taken.
const CAPTURE_LINGER: Duration = Duration::from_millis(1500);
/// The widest a crumb's title gets: a share of the bar, within these bounds.
const CRUMB_MIN: f64 = 220.0;
const CRUMB_MAX: f64 = 420.0;
const SLOT_W: f64 = 20.0;
const ICON: u32 = 16;
const CALENDAR_W: f64 = 392.0;
const CALENDAR_H: f64 = 380.0;

const BACK: &str = r#"<path d="M15 6l-6 6 6 6"/>"#;
const FORWARD: &str = r#"<path d="M9 6l6 6-6 6"/>"#;
const STETHOSCOPE: &str =
    r#"<path d="M6 3v6a5 5 0 0 0 10 0V3"/><path d="M11 14v2a4 4 0 0 0 8 0v-3"/><circle cx="19" cy="11" r="2"/>"#;
const LAYERS: &str = r#"<path d="M12 3l9 5-9 5-9-5z"/><path d="M3 13l9 5 9-5"/>"#;
const SCREEN: &str = r#"<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>"#;
const BOLT: &str = r#"<path d="M13 3L6 13h5l-1 8 7-10h-5z"/>"#;

/// The last error in the log, for the health light. Filled from any thread
/// by [`ErrorWatch`].
static LAST_ERROR: Mutex<Option<(u64, String)>> = Mutex::new(None);

/// A tracing layer that remembers errors, so the bar can show that
/// something went wrong (a config that did not load, a service that did not
/// start) and offer to ask the doctor about it.
pub struct ErrorWatch;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ErrorWatch {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        if *event.metadata().level() != tracing::Level::ERROR {
            return;
        }
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.0 = value.to_string();
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        if let Ok(mut last) = LAST_ERROR.lock() {
            let seq = last.as_ref().map_or(1, |(seq, _)| seq + 1);
            *last = Some((seq, message.0.chars().take(400).collect()));
        }
    }
}

/// An error not yet asked about: its number and text.
fn unseen_error(seen: u64) -> Option<(u64, String)> {
    LAST_ERROR.lock().ok()?.clone().filter(|(seq, _)| *seq > seen)
}

pub struct Bar {
    /// Revealed over a framed window (hairline mode only).
    revealed: bool,
    changed: Instant,
    level_then: f64,
    /// What the bar takes off its edge of the layout now.
    reserved: f64,
    /// A press on the bar, so its release does not reach anything else.
    pressed: bool,
    /// Scrolling on the bar not yet turned into steps.
    scroll: f64,
    /// The last error the doctor was asked about.
    seen_error: u64,
    /// When screencopy last took a frame, and whether it was for sharing.
    pub capture: Option<(Instant, bool)>,
    calendar: Calendar,
    /// The process of an outside bar (`bar = "waybar"`).
    command: Option<(String, u32)>,
}

impl Bar {
    /// Visual state belongs to an output; the external bar process belongs to
    /// the session and must survive switching or temporarily rendering outputs.
    pub fn exchange_output(&mut self, other: &mut Self) {
        std::mem::swap(self, other);
        std::mem::swap(&mut self.command, &mut other.command);
    }
}

impl Default for Bar {
    fn default() -> Self {
        Bar {
            revealed: false,
            changed: Instant::now(),
            level_then: 0.0,
            reserved: 0.0,
            pressed: false,
            scroll: 0.0,
            seen_error: 0,
            capture: None,
            calendar: Calendar::default(),
            command: None,
        }
    }
}

impl Bar {
    fn reveal_level(&self) -> f64 {
        let progress = (self.changed.elapsed().as_secs_f64() / SLIDE).clamp(0.0, 1.0);
        if self.revealed {
            self.level_then + (1.0 - self.level_then) * progress
        } else {
            self.level_then * (1.0 - progress)
        }
    }

    fn set_revealed(&mut self, revealed: bool) {
        if revealed != self.revealed {
            self.level_then = self.reveal_level();
            self.revealed = revealed;
            self.changed = Instant::now();
        }
    }
}

/// The clock zoomed into a month.
struct Calendar {
    open: bool,
    changed: Instant,
    level_then: f64,
    /// Months away from the current one.
    offset: i32,
}

impl Default for Calendar {
    fn default() -> Self {
        Calendar { open: false, changed: Instant::now(), level_then: 0.0, offset: 0 }
    }
}

impl Calendar {
    fn level(&self, duration: f64) -> f64 {
        let progress =
            if duration <= 0.0 { 1.0 } else { (self.changed.elapsed().as_secs_f64() / duration).clamp(0.0, 1.0) };
        if self.open {
            self.level_then + (1.0 - self.level_then) * progress
        } else {
            self.level_then * (1.0 - progress)
        }
    }
}

/// How the bar shows right now.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Switched off or replaced in `[shell]`.
    Off,
    /// A fullscreen window covers it.
    Covered,
    /// One window is framed: a line along the bar's edge that slides open on touch.
    Hairline,
    Full,
}

/// What is under the pointer on the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Launcher,
    Doctor,
    Back,
    Forward,
    Crumb(NodeId),
    Slot(usize),
    Media,
    Dnd,
    Clock,
    Battery,
    Notifications,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CalendarHit {
    Previous,
    Next,
    Today,
    Card,
}

/// What one crumb shows.
#[derive(Debug, Clone)]
enum CrumbLook {
    /// The root: "Everything", or "Overview" with the number of windows.
    Root(String),
    /// A framed window: its icon and title.
    Window { app: String, title: String },
    /// A space: the icons of what is in it, most windows first.
    Apps { apps: Vec<String>, more: usize },
    /// Crumbs left out in the middle of a long path.
    Gap,
}

#[derive(Debug, Clone)]
struct Crumb {
    node: Option<NodeId>,
    look: CrumbLook,
    /// The saved view that frames this node.
    slot: Option<usize>,
    rect: Rect,
}

#[derive(Debug, Clone, Copy)]
enum ChipKind {
    Media,
    Capture,
    Dnd,
}

#[derive(Debug, Clone)]
struct Chip {
    kind: ChipKind,
    label: String,
    rect: Rect,
}

struct Layout {
    bar: Rect,
    launcher: Rect,
    doctor: Rect,
    back: Rect,
    forward: Rect,
    crumbs: Vec<Crumb>,
    slots: Vec<(usize, Rect)>,
    chips: Vec<Chip>,
    tray: Vec<(crate::tray::Slot, Rect)>,
    clock: Rect,
    clock_text: String,
    /// The charge, on a laptop.
    battery: Option<(Rect, String)>,
    notifications: Rect,
}

impl Tileception {
    fn bar_part(&self) -> Part {
        Part::parse(&self.shell().bar)
    }

    fn bar_mode(&self) -> Mode {
        if !self.bar_part().is_builtin() {
            return Mode::Off;
        }
        if self.fullscreen.is_some() {
            return Mode::Covered;
        }
        if self.single_framed() { Mode::Hairline } else { Mode::Full }
    }

    /// One window framed on its own, where the bar would only take space.
    fn single_framed(&self) -> bool {
        let tree = self.world.tree();
        let framed = self.world.framed();
        !self.world.in_overview() && tree.is_leaf(framed) && !tree.shown_windows_in(framed).is_empty()
    }

    pub(crate) fn bar_at_bottom(&self) -> bool {
        self.bar_part().is_builtin() && self.shell().bar_position == BarPosition::Bottom
    }

    /// Place a bar popover towards the centre of the screen.
    pub(crate) fn bar_popup_y(&self, anchor: Rect, height: f64) -> f64 {
        let screen = self.output_rect();
        let top = if self.bar_at_bottom() { anchor.y - height - 6.0 } else { anchor.bottom() + 6.0 };
        top.clamp(screen.y + 8.0, (screen.bottom() - height - 8.0).max(screen.y + 8.0))
    }

    /// What the bar takes off its edge of the layout area.
    pub fn bar_reserve(&self, usable: Rect) -> Rect {
        let reserved = self.bar.reserved;
        if reserved <= 0.0 || usable.h <= reserved {
            return usable;
        }
        let top = if self.bar_at_bottom() { usable.y } else { usable.y + reserved };
        Rect::new(usable.x, top, usable.w, usable.h - reserved)
    }

    /// Follows the camera and the pointer: reserves the edge or gives it to a
    /// framed window, and slides a hairline bar open while the pointer is at
    /// its edge. Called after each frame and on pointer motion.
    pub fn update_bar(&mut self) {
        let mode = self.bar_mode();
        // A fullscreen window keeps the layout as it was underneath.
        let reserved = match mode {
            Mode::Full => BAR_H,
            Mode::Covered => self.bar.reserved,
            Mode::Off | Mode::Hairline => 0.0,
        };
        if reserved != self.bar.reserved {
            self.bar.reserved = reserved;
            self.update_usable_area();
        }
        let point = Point::new(self.pointer.x, self.pointer.y);
        let out = self.output_rect();
        let distance = if self.bar_at_bottom() { out.bottom() - point.y } else { point.y - out.y };
        let reveal = mode == Mode::Hairline
            && self.is_active_output()
            && !self.is_locked()
            && self.move_drag.is_none()
            && (distance <= EDGE
                || (self.bar.revealed && distance <= BAR_H + SLACK)
                || self.bar.pressed
                || self.tray.open
                || self.bar.calendar.open);
        self.bar.set_revealed(reveal);
        if mode != Mode::Full && mode != Mode::Hairline && self.bar.calendar.open {
            self.close_calendar();
        }
        if !matches!(mode, Mode::Full | Mode::Hairline) || self.is_locked() {
            self.close_tray();
        }
    }

    /// How far the bar is in, from 0 (hairline or hidden) to 1.
    fn bar_level(&self) -> f64 {
        match self.bar_mode() {
            Mode::Full => 1.0,
            Mode::Hairline => ease_out(self.bar.reveal_level()),
            Mode::Off | Mode::Covered => 0.0,
        }
    }

    /// The bar's place while it shows, also when it is sliding.
    fn bar_rect(&self) -> Option<Rect> {
        let level = self.bar_level();
        if level <= 0.01 {
            return None;
        }
        let out = self.output_rect();
        let top = if self.bar_at_bottom() { out.bottom() - level * BAR_H } else { out.y - (1.0 - level) * BAR_H };
        Some(Rect::new(out.x, top, out.w, BAR_H))
    }

    /// Text width without drawing it.
    fn bar_measure(&self, text: &str, style: Style, max: f64) -> f64 {
        let theme = &self.theme;
        self.text
            .borrow_mut()
            .styled(text, &theme.font, style.size, style.colour, max.max(1.0) as f32, style.weight, style.lines)
            .map(|rendered| rendered.width)
            .unwrap_or(0.0)
    }

    fn bar_style(&self) -> Style {
        Style::new(self.theme.font_size * 0.86, self.theme.text.alpha(0.86))
    }

    /// The apps in a space, most windows first, then by name.
    fn apps_in(&self, node: NodeId) -> Vec<String> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for window_id in self.world.tree().windows_in(node) {
            let Some(window) = self.windows.get(&window_id) else { continue };
            let app = crate::state::app_id(window);
            match counts.iter_mut().find(|(name, _)| *name == app) {
                Some((_, count)) => *count += 1,
                None => counts.push((app, 1)),
            }
        }
        counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        counts.into_iter().map(|(app, _)| app).collect()
    }

    /// The crumbs from the root to the framed node, before they are placed:
    /// only levels the camera stops at, which are the root, the framed node,
    /// spaces framed before and spaces a saved view frames. Splits made only
    /// to split along the longer side are skipped.
    fn crumb_path(&self) -> Vec<Crumb> {
        let tree = self.world.tree();
        let root = tree.root();
        let saved: Vec<(usize, NodeId)> =
            (1..=9).filter_map(|slot| self.world.saved_view_node(slot).map(|saved_node| (slot, saved_node))).collect();
        let slot_of = |node: NodeId| saved.iter().find(|(_, saved_node)| *saved_node == node).map(|(slot, _)| *slot);
        if self.world.in_overview() {
            let windows = tree.windows_in(root).len();
            let label = format!("Overview \u{b7} {windows} window{}", if windows == 1 { "" } else { "s" });
            return vec![Crumb { node: Some(root), look: CrumbLook::Root(label), slot: None, rect: Rect::UNIT }];
        }
        let framed = self.world.framed();
        let mut path = tree.ancestors(framed);
        path.reverse();
        path.into_iter()
            .filter(|ancestor| {
                *ancestor == root
                    || *ancestor == framed
                    || slot_of(*ancestor).is_some()
                    || self.world.visited(*ancestor)
            })
            .map(|node| {
                let look = if node == root {
                    CrumbLook::Root("Everything".to_string())
                } else if let Node::Tile { views, selected } = tree.node(node) {
                    let window_id = views[*selected];
                    let window = self.windows.get(&window_id);
                    CrumbLook::Window {
                        app: window.map(crate::state::app_id).unwrap_or_default(),
                        title: window.map(crate::state::window_title).unwrap_or_default(),
                    }
                } else {
                    let apps = self.apps_in(node);
                    let more = apps.len().saturating_sub(3);
                    CrumbLook::Apps { apps: apps.into_iter().take(3).collect(), more }
                };
                Crumb { node: Some(node), look, slot: slot_of(node), rect: Rect::UNIT }
            })
            .collect()
    }

    /// The widest a crumb's title gets on this screen.
    fn crumb_max(&self) -> f64 {
        (self.output_rect().w * 0.18).clamp(CRUMB_MIN, CRUMB_MAX)
    }

    /// A crumb's width; the `current` one is drawn bold, which is wider.
    fn crumb_width(&self, crumb: &Crumb, current: bool) -> f64 {
        let style = if current { self.bar_style().bold() } else { self.bar_style() };
        let max = self.crumb_max();
        let slot = if crumb.slot.is_some() { 18.0 } else { 0.0 };
        let content = match &crumb.look {
            CrumbLook::Root(label) => 18.0 + self.bar_measure(label, style, max),
            CrumbLook::Window { title, .. } => 22.0 + self.bar_measure(title, style, max).min(max),
            CrumbLook::Apps { apps, more } => {
                let icons = apps.len().max(1) as f64 * 18.0;
                let more = if *more > 0 { 4.0 + self.bar_measure(&format!("+{more}"), style, 60.0) } else { 0.0 };
                icons + more
            }
            CrumbLook::Gap => self.bar_measure("\u{2026}", style, 40.0),
        };
        content + slot + 14.0
    }

    fn bar_layout(&self) -> Option<Layout> {
        let bar = self.bar_rect()?;
        let style = self.bar_style();
        let top = bar.y + 2.0;
        let height = BAR_H - 4.0;
        let mut cursor_x = bar.x + 6.0;
        let doctor = Rect::new(cursor_x, top, 36.0, height);
        cursor_x = doctor.right() + 4.0;
        let launcher = Rect::new(cursor_x, top, 28.0, height);
        cursor_x = launcher.right() + 4.0;
        let back = Rect::new(cursor_x, top, 22.0, height);
        let forward = Rect::new(back.right(), top, 22.0, height);
        let left = forward.right() + 8.0;

        // The right side, from the edge inwards.
        let mut right = bar.right() - 6.0;
        let count = self.notification_status().map_or(0, |(unread, _)| unread);
        let badge = if count > 0 { 6.0 + self.bar_measure(&count.to_string(), style.bold(), 40.0) } else { 0.0 };
        let notifications = Rect::new(right - 28.0 - badge, top, 28.0 + badge, height);
        right = notifications.x - 4.0;
        let now = LocalTime::now();
        let clock_text = now.as_ref().map(|time| format!("{}  {}", short_date(time), time.clock())).unwrap_or_default();
        let clock_w = self.bar_measure(&clock_text, style.bold(), 240.0) + 16.0;
        let clock = Rect::new(right - clock_w, top, clock_w, height);
        right = clock.x - 6.0;
        let battery = self.battery.reading.map(|reading| {
            let label = format!("{}%", reading.percent);
            let bolt = if reading.charge == crate::battery::Charge::Charging { 14.0 } else { 0.0 };
            let width = 34.0 + bolt + self.bar_measure(&label, style, 60.0);
            let rect = Rect::new(right - width, top, width, height);
            right = rect.x - 4.0;
            (rect, label)
        });
        let tray = self.tray_slots(&mut right, top, height);
        let mut chips = Vec::new();
        for (kind, label) in self.chip_list() {
            let width = (24.0 + self.bar_measure(&label, style, 200.0) + 10.0).min(230.0);
            if right - width < left + 120.0 {
                break;
            }
            let rect = Rect::new(right - width, top + 1.0, width, height - 2.0);
            right = rect.x - 4.0;
            chips.push(Chip { kind, label, rect });
        }

        // Saved views in the middle, or just left of the chips when crowded.
        let slots_w = 9.0 * SLOT_W;
        let mut slots_x = bar.x + (bar.w - slots_w) / 2.0;
        if slots_x + slots_w > right - 8.0 {
            slots_x = right - 8.0 - slots_w;
        }
        let show_slots = slots_x > left + 80.0;
        let slots: Vec<(usize, Rect)> = if show_slots {
            (1..=9)
                .map(|slot| {
                    (slot, Rect::new(slots_x + (slot - 1) as f64 * SLOT_W, top + 2.0, SLOT_W - 2.0, height - 4.0))
                })
                .collect()
        } else {
            Vec::new()
        };
        let crumbs_end = if show_slots { slots_x - 12.0 } else { right - 8.0 };
        let crumbs = self.place_crumbs(left, crumbs_end, top, height);
        Some(Layout {
            bar,
            launcher,
            doctor,
            back,
            forward,
            crumbs,
            slots,
            chips,
            tray,
            clock,
            clock_text,
            battery,
            notifications,
        })
    }

    /// Lays the crumbs out from `left`, collapsing the middle of a long path.
    fn place_crumbs(&self, left: f64, end: f64, top: f64, height: f64) -> Vec<Crumb> {
        let mut crumbs = self.crumb_path();
        let separator = 14.0;
        let total = |crumbs: &[Crumb]| -> f64 {
            let last = crumbs.len().saturating_sub(1);
            crumbs.iter().enumerate().map(|(index, crumb)| self.crumb_width(crumb, index == last)).sum::<f64>()
                + separator * last as f64
        };
        let room = end - left;
        // Drop crumbs after the root until it fits, keeping the last one.
        while crumbs.len() > 2 && total(&crumbs) > room {
            let gap_at = crumbs.iter().position(|crumb| matches!(crumb.look, CrumbLook::Gap));
            match gap_at {
                Some(index) if index + 2 < crumbs.len() => {
                    crumbs.remove(index + 1);
                }
                Some(_) => break,
                None => {
                    crumbs[1] = Crumb { node: None, look: CrumbLook::Gap, slot: None, rect: Rect::UNIT };
                }
            }
        }
        let mut cursor_x = left;
        let last = crumbs.len().saturating_sub(1);
        for (index, crumb) in crumbs.iter_mut().enumerate() {
            let width = self.crumb_width(crumb, index == last).min((end - cursor_x).max(24.0));
            crumb.rect = Rect::new(cursor_x, top, width, height);
            cursor_x += width + separator;
        }
        crumbs
    }

    /// Chips that only show when there is something to say, nearest the
    /// clock first.
    fn chip_list(&self) -> Vec<(ChipKind, String)> {
        let mut chips = Vec::new();
        if let Some((captured, sharing)) = self.bar.capture
            && captured.elapsed() < CAPTURE_LINGER
        {
            let label = if sharing { "Sharing screen" } else { "Screen captured" };
            chips.push((ChipKind::Capture, label.to_string()));
        }
        if self.notification_status().is_some_and(|(_, dnd)| dnd) {
            chips.push((ChipKind::Dnd, "Do not disturb".to_string()));
        }
        if let Some(player) = self.media.snapshot.media.active() {
            let label = match (player.title.trim(), player.artist.trim()) {
                ("", _) => player.identity.clone(),
                (title, "") => title.to_string(),
                (title, artist) => format!("{title} \u{b7} {artist}"),
            };
            chips.push((ChipKind::Media, label));
        }
        chips
    }

    fn bar_hit(&self, layout: &Layout, point: Point) -> Option<Hit> {
        if !layout.bar.contains(point) {
            return None;
        }
        let hits = [
            (layout.launcher, Hit::Launcher),
            (layout.doctor, Hit::Doctor),
            (layout.back, Hit::Back),
            (layout.forward, Hit::Forward),
            (layout.clock, Hit::Clock),
            (layout.notifications, Hit::Notifications),
        ];
        if let Some((_, hit)) = hits.iter().find(|(rect, _)| rect.contains(point)) {
            return Some(*hit);
        }
        if layout.battery.as_ref().is_some_and(|(rect, _)| rect.contains(point)) {
            return Some(Hit::Battery);
        }
        if let Some(crumb) = layout.crumbs.iter().find(|crumb| crumb.rect.contains(point))
            && let Some(node) = crumb.node
        {
            return Some(Hit::Crumb(node));
        }
        if let Some((slot, _)) = layout.slots.iter().find(|(_, rect)| rect.contains(point)) {
            return Some(Hit::Slot(*slot));
        }
        if let Some(chip) = layout.chips.iter().find(|chip| chip.rect.contains(point)) {
            return match chip.kind {
                ChipKind::Media => Some(Hit::Media),
                ChipKind::Dnd => Some(Hit::Dnd),
                ChipKind::Capture => Some(Hit::Bar),
            };
        }
        Some(Hit::Bar)
    }

    /// Whether the bar (or the open calendar) takes the pointer here.
    pub fn bar_takes(&self, point: Point) -> bool {
        if self.overlay_under(point) {
            return false;
        }
        self.tray.open || self.bar.calendar.open || self.bar_rect().is_some_and(|rect| rect.contains(point))
    }

    /// Pointer motion over the bar. Returns whether the bar took it.
    pub fn bar_motion(&mut self, pos: SPoint<f64, Logical>, time: u32) -> bool {
        self.update_bar();
        let point = Point::new(pos.x, pos.y);
        let busy = self.gap_drag.is_some()
            || self.resize_drag.is_some()
            || self.move_drag.is_some()
            || self.selection.anchor.is_some()
            || self.pending_press.is_some();
        if busy || !self.bar_takes(point) {
            return false;
        }
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        pointer.motion(self, None, &MotionEvent { location: pos, serial: SERIAL_COUNTER.next_serial(), time });
        pointer.frame(self);
        true
    }

    /// A button on the bar. Returns whether the bar took it.
    pub fn bar_button(&mut self, button: u32, pressed: bool) -> bool {
        let point = Point::new(self.pointer.x, self.pointer.y);
        if !pressed {
            return std::mem::take(&mut self.bar.pressed);
        }
        if self.overlay_under(point) {
            return false;
        }
        if let Some(layout) = self.bar_layout() {
            if let Some((slot, _)) = layout.tray.iter().find(|(_, rect)| rect.contains(point)) {
                self.bar.pressed = true;
                self.tray_slot_button(*slot, button);
                return true;
            }
            if self.tray.open {
                self.bar.pressed = true;
                let anchor = layout.tray.last().map(|(_, rect)| *rect).unwrap_or(layout.clock);
                self.tray_popup_button(anchor, button);
                return true;
            }
        }
        if self.bar.calendar.open {
            self.bar.pressed = true;
            if button == crate::input::BTN_LEFT {
                match self.calendar_hit(point) {
                    Some(CalendarHit::Previous) => self.bar.calendar.offset -= 1,
                    Some(CalendarHit::Next) => self.bar.calendar.offset += 1,
                    Some(CalendarHit::Today) => self.bar.calendar.offset = 0,
                    Some(CalendarHit::Card) => {}
                    // The clock itself, or anywhere outside: zoom back.
                    None => {
                        self.close_calendar();
                    }
                }
            }
            return true;
        }
        if !self.bar_takes(point) {
            return false;
        }
        self.bar.pressed = true;
        if button != crate::input::BTN_LEFT {
            return true;
        }
        let Some(layout) = self.bar_layout() else { return true };
        let Some(hit) = self.bar_hit(&layout, point) else { return true };
        match hit {
            Hit::Launcher => self.shell_launcher(),
            Hit::Doctor => match unseen_error(self.bar.seen_error) {
                Some((seq, message)) => {
                    self.bar.seen_error = seq;
                    self.ask_doctor(&format!(
                        "Tileception logged this error: \u{201c}{message}\u{201d}. What does it mean, and how do I fix it?"
                    ));
                }
                None => self.toggle_doctor(),
            },
            Hit::Back => self.run(Action::Back),
            Hit::Forward => self.run(Action::Forward),
            Hit::Crumb(node) => {
                let now = self.now();
                self.world.frame(node, now);
                self.apply_layout();
            }
            Hit::Slot(slot) => {
                if self.world.has_saved_view(slot) {
                    self.run(Action::RecallView(slot));
                }
            }
            Hit::Media => {
                let play_pause = crate::media::Control::Player(tileception_media::PlayerCommand::PlayPause);
                if let Err(error) = self.execute(crate::commands::Command::Media(play_pause)) {
                    tracing::warn!("{error}");
                }
            }
            Hit::Dnd => self.toggle_dnd(),
            Hit::Clock => self.open_calendar(),
            Hit::Notifications => self.shell_notifications(),
            Hit::Battery | Hit::Bar => {}
        }
        true
    }

    /// Scrolling on the bar steps in and out of the tree, a notch at a time;
    /// over the calendar it turns the months. `amount` is in wheel notches
    /// (positive is down). Returns whether the bar took it.
    pub fn bar_scroll(&mut self, amount: f64) -> bool {
        let point = Point::new(self.pointer.x, self.pointer.y);
        if !self.bar_takes(point) {
            return false;
        }
        if let Some(layout) = self.bar_layout() {
            let anchor = layout.tray.last().map(|(_, rect)| *rect).unwrap_or(layout.clock);
            if self.tray_scroll(&layout.tray, anchor, amount) {
                return true;
            }
        }
        self.bar.scroll += amount;
        while self.bar.scroll.abs() >= 1.0 {
            let down = self.bar.scroll > 0.0;
            self.bar.scroll -= self.bar.scroll.signum();
            if self.bar.calendar.open {
                self.bar.calendar.offset += if down { 1 } else { -1 };
            } else {
                self.run(if down { Action::StepOut } else { Action::StepIn });
            }
        }
        true
    }

    pub fn open_calendar(&mut self) {
        self.close_tray();
        let calendar = &mut self.bar.calendar;
        if !calendar.open {
            let duration = self.world.settings.animation;
            calendar.level_then = calendar.level(duration);
            calendar.open = true;
            calendar.changed = Instant::now();
            calendar.offset = 0;
        }
    }

    /// Zooms the calendar back into the clock. Returns whether it was open.
    pub fn close_calendar(&mut self) -> bool {
        let duration = self.world.settings.animation;
        let calendar = &mut self.bar.calendar;
        if !calendar.open {
            return false;
        }
        calendar.level_then = calendar.level(duration);
        calendar.open = false;
        calendar.changed = Instant::now();
        true
    }

    /// The calendar card at its full size: centred under the bar.
    fn calendar_card(&self) -> Rect {
        let screen = self.world.screen();
        let width = CALENDAR_W.min(screen.w - 32.0).max(1.0);
        let height = CALENDAR_H.min(screen.h - 32.0).max(1.0);
        Rect::new(screen.x + (screen.w - width) / 2.0, screen.y + ((screen.h - height) * 0.3).max(16.0), width, height)
    }

    fn calendar_parts(&self, card: Rect) -> (Rect, Rect, Rect) {
        let month = Rect::new(card.x + 16.0, card.y + 92.0, card.w - 32.0, 32.0);
        let previous = Rect::new(month.right() - 64.0, month.y, 30.0, month.h);
        let next = Rect::new(month.right() - 30.0, month.y, 30.0, month.h);
        (month, previous, next)
    }

    fn calendar_hit(&self, point: Point) -> Option<CalendarHit> {
        let card = self.calendar_card();
        if !card.contains(point) {
            return None;
        }
        let (month, previous, next) = self.calendar_parts(card);
        if previous.contains(point) {
            Some(CalendarHit::Previous)
        } else if next.contains(point) {
            Some(CalendarHit::Next)
        } else if month.contains(point) && point.x < previous.x {
            Some(CalendarHit::Today)
        } else {
            Some(CalendarHit::Card)
        }
    }

    pub fn bar_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let mode = self.bar_mode();
        if matches!(mode, Mode::Off | Mode::Covered) || self.is_locked() {
            return Vec::new();
        }
        let theme = &self.theme;
        let mut painter = Painter::new(self, renderer);
        let layout = self.bar_layout();
        if mode == Mode::Hairline {
            let out = self.output_rect();
            let top = if self.bar_at_bottom() { out.bottom() - 2.0 } else { out.y };
            painter.fill(Rect::new(out.x, top, out.w, 2.0), theme.accent.alpha(0.35), 0.0);
        }
        if let Some(layout) = &layout {
            self.draw_bar(&mut painter, layout, mode);
        }
        let duration = self.world.settings.animation;
        let level = ease_out(self.bar.calendar.level(duration));
        if level > 0.001 {
            let clock = layout.as_ref().map(|placed| placed.clock).unwrap_or_else(|| {
                let out = self.output_rect();
                let top = if self.bar_at_bottom() { out.bottom() - BAR_H } else { out.y };
                Rect::new(out.right() - 120.0, top, 110.0, BAR_H)
            });
            self.draw_calendar(&mut painter, clock, level);
        }
        painter.finish()
    }

    /// Tray popovers float above the shell panels, below overlay clients.
    pub fn tray_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        if matches!(self.bar_mode(), Mode::Off | Mode::Covered) || self.is_locked() {
            return Vec::new();
        }
        let Some(layout) = self.bar_layout() else {
            return Vec::new();
        };
        let mut painter = Painter::new(self, renderer);
        let anchor = layout.tray.last().map(|(_, rect)| *rect).unwrap_or(layout.clock);
        self.draw_tray_popup(&mut painter, &layout.tray, anchor);
        painter.finish()
    }

    fn draw_bar(&self, painter: &mut Painter, layout: &Layout, mode: Mode) {
        let theme = &self.theme;
        let bar = layout.bar;
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let hit = self.bar_hit(layout, pointer);
        let style = self.bar_style();
        if mode == Mode::Hairline {
            // Slid over a window: a shadow towards the layout separates it.
            painter.shape(
                Rect::new(bar.x, bar.y + if self.bar_at_bottom() { 0.0 } else { 4.0 }, bar.w, bar.h - 4.0),
                Shape { glow_radius: 16.0, glow: Colour([0.0, 0.0, 0.0, 0.4]), ..Default::default() },
            );
        }
        painter.fill(bar, theme.surface, 0.0);
        let border_y = if self.bar_at_bottom() { bar.y } else { bar.bottom() - 1.0 };
        painter.fill(Rect::new(bar.x, border_y, bar.w, 1.0), theme.border, 0.0);
        let hover_fill = |painter: &mut Painter, rect: Rect, hovered: bool| {
            if hovered {
                painter.fill(rect, theme.surface_raised, (theme.radius - 1.0).max(2.0));
            }
        };

        // Tide's tile mark is also the mouse entry point to the launcher.
        hover_fill(painter, layout.launcher, hit == Some(Hit::Launcher));
        let mark_x = layout.launcher.x + 6.0;
        let mark_y = layout.launcher.y + (layout.launcher.h - 16.0) / 2.0;
        painter.fill(Rect::new(mark_x, mark_y, 7.0, 16.0), theme.accent, 1.5);
        painter.fill(Rect::new(mark_x + 9.0, mark_y, 7.0, 7.0), theme.accent, 1.5);
        painter.fill(Rect::new(mark_x + 9.0, mark_y + 9.0, 7.0, 7.0), theme.accent_end, 1.5);

        // The doctor, with its health light.
        let error = unseen_error(self.bar.seen_error);
        hover_fill(painter, layout.doctor, hit == Some(Hit::Doctor));
        painter.glyph(
            STETHOSCOPE,
            Rect::new(layout.doctor.x + 2.0, layout.doctor.y, 22.0, layout.doctor.h),
            ICON,
            theme.text,
        );
        let light = Rect::new(layout.doctor.right() - 9.0, layout.doctor.y + layout.doctor.h / 2.0 - 3.0, 6.0, 6.0);
        let (colour, glow) = if error.is_some() {
            (theme.warning, theme.warning.alpha(0.6))
        } else {
            (theme.accent, theme.accent.alpha(0.3))
        };
        painter.shape(light, Shape { glow_radius: 4.0, glow, ..Shape::filled(colour, 3.0) });

        for (rect, glyph_, which, enabled) in [
            (layout.back, BACK, Hit::Back, self.world.can_travel(-1)),
            (layout.forward, FORWARD, Hit::Forward, self.world.can_travel(1)),
        ] {
            hover_fill(painter, rect, enabled && hit == Some(which));
            painter.glyph(glyph_, rect, ICON, if enabled { theme.text.alpha(0.86) } else { theme.muted.alpha(0.5) });
        }

        self.draw_crumbs(painter, layout, hit);
        self.draw_tray_slots(painter, &layout.tray);

        let current = (!self.world.in_overview()).then(|| self.world.framed());
        for (slot, rect) in &layout.slots {
            let node = self.world.saved_view_node(*slot);
            let here = node.is_some() && node == current;
            let label = slot.to_string();
            match node {
                Some(_) => {
                    let lit = here || hit == Some(Hit::Slot(*slot));
                    painter.shape(
                        *rect,
                        Shape {
                            border_width: if here { 1.0 } else { 0.0 },
                            border: theme.accent,
                            ..Shape::filled(if lit { theme.accent.alpha(0.32) } else { theme.accent.alpha(0.16) }, 4.0)
                        },
                    );
                    painter.text(
                        &label,
                        *rect,
                        Style::new(theme.font_size * 0.78, theme.text).bold().align(Align::Centre),
                    );
                }
                None => {
                    painter.text(
                        &label,
                        *rect,
                        Style::new(theme.font_size * 0.74, theme.muted.alpha(0.45)).align(Align::Centre),
                    );
                }
            }
        }

        for chip in &layout.chips {
            let lit = matches!((chip.kind, hit), (ChipKind::Media, Some(Hit::Media)) | (ChipKind::Dnd, Some(Hit::Dnd)));
            let (icon, tint) = match chip.kind {
                ChipKind::Media => (
                    match self.media.snapshot.media.active().map(|player| player.status) {
                        Some(tileception_media::Status::Playing) => glyph::MUSIC,
                        _ => glyph::PAUSE,
                    },
                    theme.accent,
                ),
                ChipKind::Capture => (SCREEN, theme.danger),
                ChipKind::Dnd => (glyph::MOON, theme.warning),
            };
            painter.shape(
                chip.rect,
                Shape {
                    border_width: 1.0,
                    border: tint.alpha(if lit { 0.7 } else { 0.35 }),
                    ..Shape::filled(if lit { tint.alpha(0.22) } else { tint.alpha(0.1) }, chip.rect.h as f32 / 2.0)
                },
            );
            painter.glyph(icon, Rect::new(chip.rect.x + 4.0, chip.rect.y, 18.0, chip.rect.h), 13, tint);
            let text = Rect::new(chip.rect.x + 24.0, chip.rect.y, chip.rect.w - 30.0, chip.rect.h);
            painter.text(&chip.label, text, Style::new(theme.font_size * 0.8, theme.text.alpha(0.9)));
        }

        let calendar = self.bar.calendar.open;
        hover_fill(painter, layout.clock, calendar || hit == Some(Hit::Clock));
        painter.text(&layout.clock_text, layout.clock, style.bold().colour(theme.text).align(Align::Centre));

        if let (Some((rect, label)), Some(reading)) = (&layout.battery, &self.battery.reading) {
            hover_fill(painter, *rect, hit == Some(Hit::Battery));
            self.draw_battery(painter, *rect, label, reading);
        }

        hover_fill(painter, layout.notifications, self.notifications.open || hit == Some(Hit::Notifications));
        let dnd = self.notification_status().is_some_and(|(_, dnd)| dnd);
        let bell = Rect::new(layout.notifications.x + 2.0, layout.notifications.y, 24.0, layout.notifications.h);
        painter.glyph(glyph::BELL, bell, ICON, if dnd { theme.muted } else { theme.text.alpha(0.86) });
        if let Some((count, _)) = self.notification_status().filter(|(unread, _)| *unread > 0) {
            let label = count.to_string();
            let width = layout.notifications.right() - bell.right() - 2.0;
            let badge =
                Rect::new(bell.right() - 4.0, layout.notifications.y + 4.0, width + 2.0, layout.notifications.h - 8.0);
            painter.fill(badge, if dnd { theme.muted.alpha(0.4) } else { theme.accent }, badge.h as f32 / 2.0);
            painter.text(
                &label,
                badge,
                Style::new(theme.font_size * 0.72, theme.on_accent).bold().align(Align::Centre),
            );
        }

        // Tooltips and previews open towards the layout.
        if hit == Some(Hit::Launcher) {
            self.bar_tooltip(painter, layout.launcher.x, bar, "Launcher", "Search apps and commands (Super)");
        }
        if hit == Some(Hit::Doctor) {
            let (title, body) = match &error {
                Some((_, message)) => ("Something went wrong \u{b7} click to ask about it", message.as_str()),
                None => ("Doctor", "Ask about settings, shortcuts and apps (Super+/)"),
            };
            self.bar_tooltip(painter, layout.doctor.x, bar, title, body);
        }
        if let Some(Hit::Slot(slot)) = hit {
            self.draw_slot_preview(painter, layout, slot);
        }
        if let (Some(Hit::Battery), Some((rect, _)), Some(reading)) = (hit, &layout.battery, &self.battery.reading) {
            let left = (rect.x - 80.0).max(bar.x + 6.0);
            self.bar_tooltip(painter, left, bar, "Battery", &reading.describe());
        }
    }

    /// A small battery filled to the charge, in the border's colour when the
    /// charge gives it one, then the percentage.
    fn draw_battery(&self, painter: &mut Painter, rect: Rect, label: &str, reading: &crate::battery::Reading) {
        let theme = &self.theme;
        let tint = self.battery_border().unwrap_or(theme.text.alpha(0.86));
        let mid = rect.y + rect.h / 2.0;
        let body = Rect::new(rect.x + 6.0, mid - 5.5, 20.0, 11.0);
        painter.shape(body, Shape { border_width: 1.5, border: tint, radius: 3.0, ..Default::default() });
        painter.fill(Rect::new(body.right() + 1.0, mid - 2.5, 2.0, 5.0), tint, 1.0);
        let level = f64::from(reading.percent.clamp(4, 100)) / 100.0;
        painter.fill(Rect::new(body.x + 2.5, body.y + 2.5, (body.w - 5.0) * level, body.h - 5.0), tint, 1.0);
        let mut cursor_x = body.right() + 6.0;
        if reading.charge == crate::battery::Charge::Charging {
            painter.glyph(BOLT, Rect::new(cursor_x - 2.0, rect.y, 14.0, rect.h), 12, tint);
            cursor_x += 13.0;
        }
        painter.text(
            label,
            Rect::new(cursor_x, rect.y, rect.right() - cursor_x, rect.h),
            self.bar_style().colour(theme.text.alpha(0.9)),
        );
    }

    fn draw_crumbs(&self, painter: &mut Painter, layout: &Layout, hit: Option<Hit>) {
        let theme = &self.theme;
        let style = self.bar_style();
        let last = layout.crumbs.len().saturating_sub(1);
        for (index, crumb) in layout.crumbs.iter().enumerate() {
            let rect = crumb.rect;
            if index > 0 {
                let sep = Rect::new(rect.x - 14.0, rect.y, 14.0, rect.h);
                painter.glyph(glyph::CHEVRON_RIGHT, sep, 11, theme.muted.alpha(0.7));
            }
            let lit = crumb.node.is_some_and(|node| hit == Some(Hit::Crumb(node)));
            if lit {
                painter.fill(rect, theme.surface_raised, (theme.radius - 1.0).max(2.0));
            }
            let current = index == last;
            let colour = if current { theme.text } else { theme.text.alpha(0.72) };
            let mut cursor_x = rect.x + 7.0;
            let icon_y = rect.y + (rect.h - ICON as f64) / 2.0;
            let end = rect.right() - 7.0 - if crumb.slot.is_some() { 18.0 } else { 0.0 };
            match &crumb.look {
                CrumbLook::Root(label) => {
                    painter.glyph(LAYERS, Rect::new(cursor_x - 2.0, rect.y, 18.0, rect.h), 14, colour);
                    cursor_x += 18.0;
                    let text_style = if current { style.bold() } else { style };
                    painter.text(label, Rect::new(cursor_x, rect.y, end - cursor_x, rect.h), text_style.colour(colour));
                }
                CrumbLook::Window { app, title } => {
                    self.app_mark(painter, app, (cursor_x, icon_y));
                    cursor_x += 22.0;
                    let text_style = if current { style.bold() } else { style };
                    painter.text(
                        title,
                        Rect::new(cursor_x, rect.y, (end - cursor_x).max(1.0), rect.h),
                        text_style.colour(colour),
                    );
                }
                CrumbLook::Apps { apps, more } => {
                    for app in apps {
                        self.app_mark(painter, app, (cursor_x, icon_y));
                        cursor_x += 18.0;
                    }
                    if *more > 0 {
                        painter.text(
                            &format!("+{more}"),
                            Rect::new(cursor_x + 2.0, rect.y, end - cursor_x, rect.h),
                            style.colour(theme.muted),
                        );
                    }
                }
                CrumbLook::Gap => {
                    painter.text("\u{2026}", rect, style.colour(theme.muted).align(Align::Centre));
                }
            }
            if let Some(slot) = crumb.slot {
                let badge = Rect::new(rect.right() - 22.0, rect.y + 5.0, 15.0, rect.h - 10.0);
                painter.fill(badge, theme.accent.alpha(0.25), 3.0);
                painter.text(
                    &slot.to_string(),
                    badge,
                    Style::new(theme.font_size * 0.7, theme.text).bold().align(Align::Centre),
                );
            }
        }
    }

    /// An app's icon, or its initial in an accent square.
    fn app_mark(&self, painter: &mut Painter, app: &str, origin: (f64, f64)) {
        let theme = &self.theme;
        if let Some(icon) = self.launcher.app_icon(app, ICON) {
            painter.image(&icon, origin);
            return;
        }
        let rect = Rect::new(origin.0, origin.1, ICON as f64, ICON as f64);
        // Without an icon, apps still tell apart by colour.
        let fills = [
            (theme.accent, theme.accent_end),
            (theme.warning, theme.warning),
            (theme.danger, theme.danger),
            (theme.accent_end, theme.accent),
        ];
        let hash = app.bytes().fold(0usize, |running, byte| running.wrapping_mul(31).wrapping_add(byte as usize));
        painter.shape(rect, Shape { radius: 4.0, fill: fills[hash % fills.len()], ..Default::default() });
        let initial: String = app.chars().next().map(|letter| letter.to_uppercase().collect()).unwrap_or_default();
        painter.text(&initial, rect, Style::new(theme.font_size * 0.66, theme.on_accent).bold().align(Align::Centre));
    }

    pub(crate) fn bar_tooltip(&self, painter: &mut Painter, preferred_x: f64, anchor: Rect, title: &str, body: &str) {
        let theme = &self.theme;
        let width = 320.0_f64.min(self.output_rect().w - 16.0);
        let left = preferred_x.min(self.output_rect().right() - width - 8.0).max(8.0);
        let body_style = Style::new(theme.font_size * 0.9, theme.muted).lines(4);
        let (_, body_h) = (0.0, self.bar_measure_height(body, body_style, width - 24.0));
        let top = self.bar_popup_y(anchor, 40.0 + body_h);
        let rect = Rect::new(left, top, width, 40.0 + body_h);
        painter.popup(rect);
        painter.text(
            title,
            Rect::new(left + 12.0, top + 8.0, width - 24.0, 20.0),
            Style::new(theme.font_size * 0.86, theme.text).bold(),
        );
        painter.text(body, Rect::new(left + 12.0, top + 30.0, width - 24.0, body_h), body_style);
    }

    fn bar_measure_height(&self, text: &str, style: Style, max: f64) -> f64 {
        let theme = &self.theme;
        self.text
            .borrow_mut()
            .styled(text, &theme.font, style.size, style.colour, max.max(1.0) as f32, style.weight, style.lines)
            .map(|rendered| rendered.height)
            .unwrap_or(0.0)
    }

    /// What a saved view frames, drawn small beside its slot.
    fn draw_slot_preview(&self, painter: &mut Painter, layout: &Layout, slot_number: usize) {
        let Some(node) = self.world.saved_view_node(slot_number) else { return };
        let Some(unit) = self.world.layout().rect(node) else { return };
        let Some((_, slot)) = layout.slots.iter().find(|(number, _)| *number == slot_number) else { return };
        // The space as it looks framed from the root, at most 260 by 180.
        let screen = self.world.screen();
        let aspect = (unit.w * screen.w) / (unit.h * screen.h).max(1.0);
        let (preview_width, preview_height) =
            if aspect >= 260.0 / 180.0 { (260.0, (260.0 / aspect).round()) } else { ((180.0 * aspect).round(), 180.0) };
        let left =
            (slot.x + slot.w / 2.0 - preview_width / 2.0).clamp(8.0, self.output_rect().right() - preview_width - 8.0);
        let height = preview_height + 16.0 + 24.0;
        let card = Rect::new(left, self.bar_popup_y(layout.bar, height), preview_width + 16.0, height);
        painter.popup(card);
        let area = Rect::new(card.x + 8.0, card.y + 8.0, preview_width, preview_height);
        painter.fill(area, self.theme.background, 4.0);
        let tree = self.world.tree();
        for leaf in tree.leaves(node) {
            let Some(leaf_rect) = self.world.layout().rect(leaf) else { continue };
            let Some(window) = tree.shown_windows_in(leaf).first().copied() else { continue };
            let rect = Rect::new(
                area.x + (leaf_rect.x - unit.x) / unit.w * area.w,
                area.y + (leaf_rect.y - unit.y) / unit.h * area.h,
                leaf_rect.w / unit.w * area.w,
                leaf_rect.h / unit.h * area.h,
            )
            .inset(2.0);
            painter.fill(rect, self.theme.surface, 3.0);
            if let Some(fitted) = self.fit_into(window, rect) {
                painter.window(window, fitted);
            }
        }
        let windows = tree.windows_in(node).len();
        let label = format!("View {slot_number} \u{b7} {windows} window{}", if windows == 1 { "" } else { "s" });
        let theme = &self.theme;
        painter.text(
            &label,
            Rect::new(area.x, area.bottom() + 2.0, area.w, 22.0),
            Style::new(theme.font_size * 0.8, theme.muted),
        );
    }

    fn fit_into(&self, window_id: WindowId, area: Rect) -> Option<Rect> {
        let geo = self.windows.get(&window_id)?.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            return None;
        }
        let (width, height) = (geo.size.w as f64, geo.size.h as f64);
        let scale = (area.w / width).min(area.h / height).min(1.0);
        Some(Rect::new(
            area.x + (area.w - width * scale) / 2.0,
            area.y + (area.h - height * scale) / 2.0,
            width * scale,
            height * scale,
        ))
    }

    /// The clock grown into a month, along the same curve the camera uses.
    fn draw_calendar(&self, painter: &mut Painter, clock: Rect, level: f64) {
        let theme = &self.theme;
        let card = self.calendar_card();
        let rect = clock.zoom_lerp(&card, level);
        let (alpha, offset) = (painter.alpha, painter.offset);
        painter.fill(self.world.screen(), Colour([0.0, 0.0, 0.0, 0.28 * level as f32]), 0.0);
        painter.popup(rect);
        // The month fades in once the card is nearly there, and moves with it.
        painter.alpha = alpha * (((level - 0.85) / 0.15).clamp(0.0, 1.0) as f32);
        let (from, target) = (card.center(), rect.center());
        painter.offset = (offset.0 + target.x - from.x, offset.1 + target.y - from.y);
        if painter.alpha <= 0.0 {
            (painter.alpha, painter.offset) = (alpha, offset);
            return;
        }
        let Some(now) = LocalTime::now() else { return };
        let pointer = Point::new(self.pointer.x, self.pointer.y);
        let over = self.calendar_hit(pointer);

        painter.text(
            &now.clock(),
            Rect::new(card.x + 20.0, card.y + 14.0, card.w - 40.0, 46.0),
            Style::new(theme.font_size * 2.6, theme.text).weight(300),
        );
        painter.text(
            &now.date(),
            Rect::new(card.x + 22.0, card.y + 58.0, card.w - 40.0, 24.0),
            Style::new(theme.font_size, theme.muted),
        );

        let (year, month) = add_months(now.year, now.month as i32 + 1, self.bar.calendar.offset);
        let (header, previous, next) = self.calendar_parts(card);
        let title = format!("{} {year}", MONTHS[(month - 1) as usize]);
        let title_style = Style::new(theme.font_size * 1.05, theme.text).bold();
        painter.text(&title, Rect::new(header.x + 4.0, header.y, header.w - 80.0, header.h), title_style);
        for (rect, icon, which) in [(previous, BACK, CalendarHit::Previous), (next, FORWARD, CalendarHit::Next)] {
            if over == Some(which) {
                painter.fill(rect, theme.surface_raised, theme.radius);
            }
            painter.glyph(icon, rect, ICON, theme.text.alpha(0.86));
        }

        // Week numbers, then Monday to Sunday.
        let grid_x = card.x + 16.0;
        let cell_w = (card.w - 32.0) / 8.0;
        let cell_h = 34.0;
        let days_y = header.bottom() + 8.0;
        let small = Style::new(theme.font_size * 0.74, theme.muted).align(Align::Centre);
        for (index, name) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].iter().enumerate() {
            let cell = Rect::new(grid_x + (index + 1) as f64 * cell_w, days_y, cell_w, 22.0);
            painter.text(name, cell, small.bold());
        }
        let first = days_from_civil(year, month, 1);
        let start = first - monday_index(first);
        let today = days_from_civil(now.year, now.month as i32 + 1, now.day);
        for week in 0..6 {
            let top = days_y + 24.0 + week as f64 * cell_h;
            let monday = start + week * 7;
            let number = Rect::new(grid_x, top, cell_w, cell_h);
            painter.text(&iso_week(monday).to_string(), number, small.colour(theme.muted.alpha(0.6)));
            for weekday in 0..7 {
                let day = monday + weekday;
                let (_, cell_month, dom) = civil_from_days(day);
                let cell = Rect::new(grid_x + (weekday + 1) as f64 * cell_w, top, cell_w, cell_h);
                let inside = cell_month == month;
                let style =
                    Style::new(theme.font_size * 0.92, if inside { theme.text } else { theme.muted.alpha(0.5) })
                        .align(Align::Centre);
                if day == today {
                    let mark = Rect::new(cell.x + (cell.w - 30.0) / 2.0, cell.y + 2.0, 30.0, 30.0);
                    painter.shape(
                        mark,
                        Shape {
                            radius: 15.0,
                            fill: (theme.accent, theme.accent_end),
                            glow_radius: theme.glow * 0.5,
                            glow: theme.accent.alpha(0.3),
                            ..Default::default()
                        },
                    );
                    painter.text(&dom.to_string(), cell, style.colour(theme.on_accent).bold());
                } else {
                    painter.text(
                        &dom.to_string(),
                        cell,
                        if weekday >= 5 && inside { style.colour(theme.text.alpha(0.7)) } else { style },
                    );
                }
            }
        }
        (painter.alpha, painter.offset) = (alpha, offset);
    }

    /// Starts or stops an outside bar after `[shell] bar` changed, or at startup.
    pub fn apply_bar_command(&mut self) {
        let wanted = match self.bar_part() {
            Part::Command(line) => Some(line),
            Part::Builtin | Part::Off => None,
        };
        if self.bar.command.as_ref().map(|(line, _)| line) == wanted.as_ref() {
            return;
        }
        if let Some((line, pid)) = self.bar.command.take() {
            tracing::info!("stopping the bar {line:?}");
            #[allow(unsafe_code, reason = "signals a process group this process started")]
            // SAFETY: kill only sends a signal. The group is the one the bar's
            // shell leads; a watcher thread waits on the shell, so its id is
            // not reused while it runs.
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
            }
        }
        if let Some(line) = wanted
            && let Some(pid) = self.spawn_child("sh", &["-c".into(), line.clone()], true)
        {
            self.bar.command = Some((line, pid));
        }
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// "Fri 16 Oct"
fn short_date(now: &LocalTime) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    format!("{} {} {}", DAYS[now.weekday], now.day, &MONTHS[now.month][..3])
}

/// A month `delta` months after `month` (1 to 12) of `year`.
fn add_months(year: i32, month: i32, delta: i32) -> (i32, i32) {
    let index = year * 12 + (month - 1) + delta;
    (index.div_euclid(12), index.rem_euclid(12) + 1)
}

/// Days since 1970-01-01 for a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let adjusted_year = i64::from(if month <= 2 { year - 1 } else { year });
    let era = adjusted_year.div_euclid(400);
    let yoe = adjusted_year - era * 400;
    let month_num = i64::from(month);
    let doy = (153 * (if month_num > 2 { month_num - 3 } else { month_num + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date for a day number from [`days_from_civil`].
fn civil_from_days(days: i64) -> (i32, i32, i32) {
    let shifted_days = days + 719_468;
    let era = shifted_days.div_euclid(146_097);
    let doe = shifted_days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let march_month = (5 * doy + 2) / 153;
    let day = doy - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 { march_month + 3 } else { march_month - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year as i32, month as i32, day as i32)
}

/// 0 for Monday up to 6 for Sunday.
fn monday_index(days: i64) -> i64 {
    // 1970-01-01 was a Thursday.
    (days + 3).rem_euclid(7)
}

/// The ISO 8601 week number of a day.
fn iso_week(days: i64) -> i64 {
    let thursday = days - monday_index(days) + 3;
    let (year, _, _) = civil_from_days(thursday);
    (thursday - days_from_civil(year, 1, 1)) / 7 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_arithmetic() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 10, 16)), (2026, 10, 16));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        // 16 October 2026 is a Friday.
        assert_eq!(monday_index(days_from_civil(2026, 10, 16)), 4);
        assert_eq!(iso_week(days_from_civil(2026, 10, 16)), 42);
        // 1 January 2021 belongs to week 53 of 2020; 4 January 2021 starts week 1.
        assert_eq!(iso_week(days_from_civil(2021, 1, 1)), 53);
        assert_eq!(iso_week(days_from_civil(2021, 1, 4)), 1);
        assert_eq!(add_months(2026, 10, 3), (2027, 1));
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 12, 0), (2026, 12));
    }
}
