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
pub(crate) const ICON: u32 = 16;
const CALENDAR_W: f64 = 392.0;
/// The notification count's badge on the bell.
const BADGE_H: f64 = 14.0;
const CALENDAR_H: f64 = 380.0;
/// A day's row in the month.
const CALENDAR_CELL_H: f64 = 34.0;
/// Below the month: the events heading, then a row per event.
const AGENDA_HEAD: f64 = 34.0;
const AGENDA_ROW: f64 = 40.0;
/// The most events the calendar lists.
const AGENDA_MAX: usize = 6;

const BACK: &str = r#"<path d="M15 6l-6 6 6 6"/>"#;
const FORWARD: &str = r#"<path d="M9 6l6 6-6 6"/>"#;
const STETHOSCOPE: &str =
    r#"<path d="M6 3v6a5 5 0 0 0 10 0V3"/><path d="M11 14v2a4 4 0 0 0 8 0v-3"/><circle cx="19" cy="11" r="2"/>"#;
const LAYERS: &str = r#"<path d="M12 3l9 5-9 5-9-5z"/><path d="M3 13l9 5 9-5"/>"#;
const SCREEN: &str = r#"<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>"#;
const UPDATES: &str = r#"<path d="M12 19V5M5 12l7-7 7 7"/>"#;
const BOLT: &str = r#"<path d="M13 3L6 13h5l-1 8 7-10h-5z"/>"#;

/// The last error in the log, for the health light. Filled from any thread
/// by [`ErrorWatch`].
static LAST_ERROR: Mutex<Option<(u64, String)>> = Mutex::new(None);

thread_local! {
    /// Errors logged now are expected and handled: see [`expecting_errors`].
    static EXPECTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `probe`, whose errors (from a library trying something that may not
/// work here) are handled by the caller: they stay in the log, but do not
/// light up the bar.
pub fn expecting_errors<T>(probe: impl FnOnce() -> T) -> T {
    let before = EXPECTED.replace(true);
    let result = probe();
    EXPECTED.set(before);
    result
}

/// A tracing layer that remembers errors, so the bar can show that
/// something went wrong (a config that did not load, a service that did not
/// start) and offer to ask the doctor about it.
pub struct ErrorWatch;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ErrorWatch {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        if *event.metadata().level() != tracing::Level::ERROR || EXPECTED.get() {
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
        if from_gl_driver(event.metadata().target(), &message.0) {
            return;
        }
        if let Ok(mut last) = LAST_ERROR.lock() {
            let seq = last.as_ref().map_or(1, |(seq, _)| seq + 1);
            *last = Some((seq, message.0.chars().take(400).collect()));
        }
    }
}

/// Whether an error is the GL driver talking through smithay's renderer
/// (`[GL] ...` from its debug callback or a shader log). Those stay in the
/// log, but the doctor can do nothing about them, so the bar stays calm.
fn from_gl_driver(target: &str, message: &str) -> bool {
    target.starts_with("smithay::backend::renderer::gles") && message.starts_with("[GL]")
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
    pub(crate) minimap: crate::minimap::Minimap,
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
            minimap: Default::default(),
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

/// A card that zooms out of a bar element and back into it.
pub(crate) struct Zoom {
    pub(crate) open: bool,
    changed: Instant,
    level_then: f64,
}

impl Default for Zoom {
    fn default() -> Self {
        Zoom { open: false, changed: Instant::now(), level_then: 0.0 }
    }
}

impl Zoom {
    /// How far open, from 0 to 1, for a zoom taking `duration` seconds.
    pub(crate) fn level(&self, duration: f64) -> f64 {
        let progress =
            if duration <= 0.0 { 1.0 } else { (self.changed.elapsed().as_secs_f64() / duration).clamp(0.0, 1.0) };
        if self.open {
            self.level_then + (1.0 - self.level_then) * progress
        } else {
            self.level_then * (1.0 - progress)
        }
    }

    /// Starts zooming open or closed. Returns whether that changed anything.
    pub(crate) fn set(&mut self, open: bool, duration: f64) -> bool {
        if open == self.open {
            return false;
        }
        self.level_then = self.level(duration);
        self.open = open;
        self.changed = Instant::now();
        true
    }
}

/// The clock zoomed into a month.
#[derive(Default)]
struct Calendar {
    zoom: Zoom,
    /// Months away from the current one.
    offset: i32,
    /// The day clicked in the month (days since 1970), whose events show
    /// below it in place of what is coming.
    selected: Option<i64>,
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
    Minimap,
    Crumb(NodeId),
    Slot(usize),
    Media,
    Dnd,
    Plugin(usize),
    Updates,
    Network,
    /// The calendar event's chip: the one on now, or the next today.
    Event,
    Clock,
    Battery,
    Notifications,
    Tray(crate::tray::Slot),
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CalendarHit {
    Previous,
    Next,
    Today,
    /// A day in the month, in days since 1970.
    Day(i64),
    Card,
}

impl crate::ui::PointerAction for Hit {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: Point) {
        state.bar_activate_hit(*self);
    }
}

impl crate::ui::PointerAction for CalendarHit {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: Point) {
        match self {
            Self::Previous => state.bar.calendar.offset -= 1,
            Self::Next => state.bar.calendar.offset += 1,
            Self::Today => {
                state.bar.calendar.offset = 0;
                state.bar.calendar.selected = None;
            }
            Self::Day(day) => state.select_calendar_day(*day),
            Self::Card => {}
        }
    }
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
    /// Pending updates: opens the updates card.
    Updates,
    Media,
    Capture,
    Dnd,
    /// A plugin's chip, by its place in [`Tileception::plugin_chips`].
    Plugin(usize),
    Network,
    /// The event on now, or else the next today, from `[calendar]`.
    Event,
}

#[derive(Debug, Clone)]
struct Chip {
    kind: ChipKind,
    label: String,
    rect: Rect,
}

pub(crate) struct Layout {
    bar: Rect,
    launcher: Rect,
    doctor: Rect,
    back: Rect,
    forward: Rect,
    /// The minimap chip, unless `[shell] bar_minimap` is off.
    pub(crate) minimap: Option<Rect>,
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

    /// Whether the built-in bar is on screen or can slide out.
    pub(crate) fn bar_shows(&self) -> bool {
        matches!(self.bar_mode(), Mode::Full | Mode::Hairline)
    }

    /// One window framed on its own, where the bar would only take space.
    /// On a split screen, in every area, so no area moves under the bar.
    fn single_framed(&self) -> bool {
        self.every_area(|world| {
            let tree = world.tree();
            let framed = world.framed();
            !world.in_overview() && tree.is_leaf(framed) && !tree.shown_windows_in(framed).is_empty()
        })
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
                || self.bar.calendar.zoom.open
                || self.updates.popover.zoom.open
                || self.bar.minimap.zoom.open
                || self.network.zoom.open);
        self.bar.set_revealed(reveal);
        if (!matches!(mode, Mode::Full | Mode::Hairline) || self.is_locked()) && self.updates.popover.zoom.open {
            self.close_updates_popover();
        }
        if mode != Mode::Full && mode != Mode::Hairline && self.bar.calendar.zoom.open {
            self.close_calendar();
        }
        if (!matches!(mode, Mode::Full | Mode::Hairline) || !self.shell().bar_minimap || self.is_locked())
            && self.bar.minimap.zoom.open
        {
            self.close_minimap();
        }
        if !matches!(mode, Mode::Full | Mode::Hairline) || self.is_locked() {
            self.close_tray();
        }
        let network_unwanted = !matches!(mode, Mode::Full | Mode::Hairline)
            || self.is_locked()
            // The welcome screen is the launcher too, but it is the desktop.
            || (self.launcher.open && !self.launcher.home)
            || self.power_menu.is_some()
            || !self.shell().bar_network;
        if network_unwanted && self.network.zoom.open {
            self.close_network();
        }
        let blocked = self.tray.open
            || self.bar.calendar.zoom.open
            || self.updates.popover.zoom.open
            || self.network.zoom.open
            || self.is_locked()
            || !self.is_active_output()
            || self.launcher.open
            || self.power_menu.is_some()
            || self.move_drag.is_some()
            || self.gap_drag.is_some()
            || self.resize_drag.is_some()
            || self.selection.anchor.is_some()
            || self.pending_press.is_some();
        self.update_minimap_hover(blocked);
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
    pub(crate) fn bar_rect(&self) -> Option<Rect> {
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

    pub(crate) fn bar_layout(&self) -> Option<Layout> {
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
        let mut left = forward.right() + 8.0;
        let minimap = self.minimap_chip(left, top, height);
        if let Some(chip) = minimap {
            left = chip.right() + 6.0;
        }

        // The right side, from the edge inwards.
        let mut right = bar.right() - 6.0;
        let count = self.notification_status().map_or(0, |(unread, _)| unread);
        // The badge sits on the bell; only a count wider than the bell's
        // corner widens the button.
        let badge = if count > 0 { self.badge_width(&count.to_string()) } else { 0.0 };
        let notifications =
            Rect::new(right - 28.0 - (badge - 16.0).max(0.0), top, 28.0 + (badge - 16.0).max(0.0), height);
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
            // The network chip is only its icon; hovering it says the rest.
            let width = if matches!(kind, ChipKind::Network) {
                30.0
            } else {
                (24.0 + self.bar_measure(&label, style, 200.0) + 10.0).min(230.0)
            };
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
            minimap,
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
        // Always there, so it stays in one place.
        if let Some((label, ..)) = self.network_chip() {
            chips.push((ChipKind::Network, label));
        }
        if let Some((label, _)) = self.updates.chip().filter(|_| self.updates_available()) {
            chips.push((ChipKind::Updates, label));
        }
        if let Some((event, ongoing)) = self.calendar_chip()
            && let (Some(start), Some(end), Some(now)) =
                (LocalTime::at(event.start), LocalTime::at(event.end), LocalTime::now())
        {
            chips.push((ChipKind::Event, event_chip(&event.summary, ongoing, &start, &end, &now)));
        }
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
        for (index, (_, chip)) in self.plugin_chips().into_iter().enumerate() {
            chips.push((ChipKind::Plugin(index), chip.label.clone()));
        }
        chips
    }

    /// Plugins' chips, after the built-in ones.
    fn plugin_chips(&self) -> Vec<(&str, &crate::panel::block::BarChip)> {
        self.plugins.as_ref().map(crate::plugins::Host::bar_chips).unwrap_or_default()
    }

    /// A plugin's chip was clicked: its action, else the panel with the
    /// plugin's block unfolded.
    fn plugin_chip_clicked(&mut self, index: usize) {
        let Some((plugin, action)) =
            self.plugin_chips().get(index).map(|(plugin, chip)| ((*plugin).to_string(), chip.action.clone()))
        else {
            return;
        };
        if let Some(action) = action {
            self.panel_chip_action(&plugin, action);
            return;
        }
        if !self.notifications.open {
            self.shell_notifications();
        }
        let _ = self.panel_fold(&plugin, false);
    }

    fn bar_hit(&self, layout: &Layout, point: Point) -> Option<Hit> {
        if !layout.bar.contains(point) {
            return None;
        }
        self.bar_controls(layout)
            .into_iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, hit)| hit)
            .or(Some(Hit::Bar))
    }

    /// One binding table for painting hover state, hit-testing and activation.
    fn bar_controls(&self, layout: &Layout) -> Vec<(Rect, Hit)> {
        let mut hits = vec![
            (layout.launcher, Hit::Launcher),
            (layout.doctor, Hit::Doctor),
            (layout.back, Hit::Back),
            (layout.forward, Hit::Forward),
            (layout.clock, Hit::Clock),
            (layout.notifications, Hit::Notifications),
        ];
        hits.extend(layout.battery.as_ref().map(|(rect, _)| (*rect, Hit::Battery)));
        hits.extend(layout.minimap.map(|rect| (rect, Hit::Minimap)));
        hits.extend(layout.crumbs.iter().filter_map(|crumb| crumb.node.map(|node| (crumb.rect, Hit::Crumb(node)))));
        hits.extend(layout.slots.iter().map(|(slot, rect)| (*rect, Hit::Slot(*slot))));
        hits.extend(layout.tray.iter().map(|(slot, rect)| (*rect, Hit::Tray(*slot))));
        hits.extend(layout.chips.iter().map(|chip| {
            (
                chip.rect,
                match chip.kind {
                    ChipKind::Updates => Hit::Updates,
                    ChipKind::Media => Hit::Media,
                    ChipKind::Dnd => Hit::Dnd,
                    ChipKind::Plugin(index) => Hit::Plugin(index),
                    ChipKind::Network => Hit::Network,
                    ChipKind::Event => Hit::Event,
                    ChipKind::Capture => Hit::Bar,
                },
            )
        }));
        hits
    }

    pub(crate) fn bar_pointer_target(&self, point: Point) -> Option<crate::ui::PointerTarget> {
        use crate::ui::PointerTarget;
        if self.network.zoom.open {
            return self.network_pointer_target(point);
        }
        if self.bar.calendar.zoom.open {
            return self
                .calendar_controls()
                .into_iter()
                .find_map(|(rect, hit)| rect.contains(point).then(|| PointerTarget::button(rect, hit)));
        }
        if self.updates.popover.zoom.open {
            return self.updates_popover_target(point);
        }
        if self.bar.minimap.zoom.open {
            return None;
        }
        let layout = self.bar_layout()?;
        self.bar_controls(&layout).into_iter().filter(|(_, hit)| !matches!(hit, Hit::Battery | Hit::Bar)).find_map(
            |(rect, hit)| {
                if !rect.contains(point) {
                    return None;
                }
                match hit {
                    Hit::Tray(slot) => self.tray_slot_target(slot, rect),
                    _ => Some(PointerTarget::button(rect, hit)),
                }
            },
        )
    }

    /// Whether the bar (or the open calendar) takes the pointer here.
    pub fn bar_takes(&self, point: Point) -> bool {
        if self.overlay_under(point) {
            return false;
        }
        self.tray.open
            || self.bar.calendar.zoom.open
            || self.bar.minimap.zoom.open
            || self.updates.popover.zoom.open
            || self.network.zoom.open
            || self.bar_rect().is_some_and(|rect| rect.contains(point))
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
            if button == crate::input::BTN_LEFT {
                self.minimap_release(point);
            }
            return std::mem::take(&mut self.bar.pressed);
        }
        if self.overlay_under(point) {
            return false;
        }
        if let Some(layout) = self.bar_layout() {
            if let Some(Hit::Tray(slot)) = self.bar_hit(&layout, point) {
                self.bar.pressed = true;
                self.tray_slot_button(slot, button);
                return true;
            }
            if self.tray.open {
                self.bar.pressed = true;
                let anchor = layout.tray.last().map(|(_, rect)| *rect).unwrap_or(layout.clock);
                self.tray_popup_button(anchor, button);
                return true;
            }
        }
        if self.bar.minimap.zoom.open {
            self.bar.pressed = true;
            if button == crate::input::BTN_LEFT {
                self.minimap_press(point);
            }
            return true;
        }
        if self.updates.popover.zoom.open {
            self.bar.pressed = true;
            // Its buttons are pointer targets; a press beside the card closes it.
            if !self.updates_popover_contains(point) {
                self.close_updates_popover();
            }
            return true;
        }
        if self.network.zoom.open {
            self.bar.pressed = true;
            self.network_press(point);
            return true;
        }
        if self.bar.calendar.zoom.open {
            self.bar.pressed = true;
            if button == crate::input::BTN_LEFT {
                match self.calendar_hit(point) {
                    Some(hit) => crate::ui::PointerAction::activate(&hit, self, self.calendar_card(), point),
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
        self.bar_activate_hit(hit);
        true
    }

    fn bar_activate_hit(&mut self, hit: Hit) {
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
            Hit::Minimap => self.toggle_minimap(),
            Hit::Crumb(node) => {
                let now = self.now();
                self.world.frame(node, now);
                self.apply_layout();
            }
            Hit::Slot(slot) => {
                if self.has_saved_view_anywhere(slot) {
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
            Hit::Plugin(index) => self.plugin_chip_clicked(index),
            Hit::Updates => self.toggle_updates_popover(),
            Hit::Network => self.toggle_network(),
            Hit::Clock | Hit::Event => self.open_calendar(),
            Hit::Notifications => self.shell_notifications(),
            Hit::Tray(slot) => self.tray_slot_button(slot, crate::input::BTN_LEFT),
            Hit::Battery | Hit::Bar => {}
        }
    }

    /// Scrolling on the bar steps in and out of the tree, a notch at a time;
    /// over the calendar it turns the months. `amount` is in wheel notches
    /// (positive is down). Returns whether the bar took it.
    pub fn bar_scroll(&mut self, amount: f64) -> bool {
        let point = Point::new(self.pointer.x, self.pointer.y);
        if !self.bar_takes(point) {
            return false;
        }
        if self.network.zoom.open {
            self.network_scroll(amount);
            return true;
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
            if self.bar.calendar.zoom.open {
                self.bar.calendar.offset += if down { 1 } else { -1 };
            } else if self.updates.popover.zoom.open {
                // The card does not scroll, and the tree stays put under it.
            } else {
                self.run(if down { Action::StepOut } else { Action::StepIn });
            }
        }
        true
    }

    pub fn open_calendar(&mut self) {
        self.close_updates_popover();
        self.close_tray();
        self.close_minimap();
        self.close_network();
        let duration = self.world.settings.animation;
        if self.bar.calendar.zoom.set(true, duration) {
            self.bar.calendar.offset = 0;
            self.bar.calendar.selected = None;
        }
    }

    /// Shows a clicked day's events below the month, turning to its month
    /// when it is a neighbouring month's; clicking it again goes back to
    /// what is coming.
    fn select_calendar_day(&mut self, day: i64) {
        let calendar = &mut self.bar.calendar;
        if calendar.selected == Some(day) {
            calendar.selected = None;
            return;
        }
        calendar.selected = Some(day);
        if let Some(now) = LocalTime::now() {
            let (year, month, _) = civil_from_days(day);
            calendar.offset = (year - now.year) * 12 + month - (now.month as i32 + 1);
        }
    }

    /// Zooms the calendar back into the clock. Returns whether it was open.
    pub fn close_calendar(&mut self) -> bool {
        let duration = self.world.settings.animation;
        self.bar.calendar.zoom.set(false, duration)
    }

    /// Where the updates chip is, or the clock while the chip is not shown.
    pub(crate) fn updates_chip_rect(&self) -> Rect {
        let layout = self.bar_layout();
        layout
            .as_ref()
            .and_then(|placed| placed.chips.iter().find(|chip| matches!(chip.kind, ChipKind::Updates)))
            .map(|chip| chip.rect)
            .unwrap_or_else(|| self.calendar_clock(layout.as_ref()))
    }

    /// A card of this size opening from a bar element: centred on it, kept
    /// on the screen, below the bar (above a bottom bar).
    pub(crate) fn bar_card(&self, anchor: Rect, width: f64, height: f64) -> Rect {
        let out = self.output_rect();
        let width = width.min(out.w - 16.0).max(1.0);
        let height = height.min(out.h - BAR_H - 16.0).max(1.0);
        let left = (anchor.center().x - width / 2.0).min(out.right() - width - 8.0).max(out.x + 8.0);
        let edge = if self.bar_at_bottom() { out.bottom() - BAR_H } else { out.y };
        Rect::new(left, self.bar_popup_y(Rect::new(out.x, edge, out.w, BAR_H), height), width, height)
    }

    /// The network chip, or where it would be: the clock, when the chip did
    /// not fit or the bar is hidden.
    pub(crate) fn network_anchor(&self) -> Rect {
        self.network_anchor_in(self.bar_layout().as_ref())
    }

    fn network_anchor_in(&self, layout: Option<&Layout>) -> Rect {
        layout
            .and_then(|placed| placed.chips.iter().find(|chip| matches!(chip.kind, ChipKind::Network)))
            .map(|chip| chip.rect)
            .unwrap_or_else(|| self.calendar_clock(layout))
    }

    /// The calendar card at its full size, below the clock (above it for a
    /// bottom bar).
    fn calendar_card(&self) -> Rect {
        self.calendar_card_for(self.calendar_clock(self.bar_layout().as_ref()))
    }

    /// Where the clock is, or would be while the bar is hidden.
    fn calendar_clock(&self, layout: Option<&Layout>) -> Rect {
        layout.map(|placed| placed.clock).unwrap_or_else(|| {
            let out = self.output_rect();
            let top = if self.bar_at_bottom() { out.bottom() - BAR_H } else { out.y };
            Rect::new(out.right() - 120.0, top, 110.0, BAR_H)
        })
    }

    /// `calendar_card` for a clock already placed: centred on it, and kept
    /// on the screen.
    fn calendar_card_for(&self, clock: Rect) -> Rect {
        let out = self.output_rect();
        let width = CALENDAR_W.min(out.w - 16.0).max(1.0);
        let height = (CALENDAR_H + self.agenda_height()).min(out.h - BAR_H - 16.0).max(1.0);
        let left = (clock.center().x - width / 2.0).min(out.right() - width - 8.0).max(out.x + 8.0);
        let edge = if self.bar_at_bottom() { out.bottom() - BAR_H } else { out.y };
        Rect::new(left, self.bar_popup_y(Rect::new(out.x, edge, out.w, BAR_H), height), width, height)
    }

    /// The events the calendar lists, while `[calendar]` has sources: the
    /// clicked day's, or else the coming week's.
    fn agenda(&self) -> Vec<&crate::ics::Occurrence> {
        if let Some(day) = self.bar.calendar.selected {
            return crate::calendar::on_day(&self.calendar.events, day, AGENDA_MAX);
        }
        let now = crate::calendar::now();
        let until = crate::calendar::local_midnight(now, crate::calendar::DAYS_AHEAD + 1);
        crate::ics::upcoming(&self.calendar.events, now, until, AGENDA_MAX)
    }

    /// For each of `count` days from `first` (days since 1970), whether
    /// an event is on it.
    fn busy_days(&self, first: i64, count: usize) -> Vec<bool> {
        let bounds: Vec<i64> = (0..=count as i64).map(|day| crate::calendar::day_start(first + day)).collect();
        let mut busy = vec![false; count];
        let (from, until) = (bounds[0], bounds[count]);
        for event in &self.calendar.events {
            let end = event.end.max(event.start + 1);
            if end <= from || event.start >= until {
                continue;
            }
            for (index, day) in busy.iter_mut().enumerate() {
                *day |= event.start < bounds[index + 1] && end > bounds[index];
            }
        }
        busy
    }

    /// Room for the events below the month: a row for each, or for saying
    /// there are none, and one per source that could not be read.
    fn agenda_height(&self) -> f64 {
        if self.config.file.calendar.sources.is_empty() {
            return 0.0;
        }
        let rows = self.agenda().len().max(1) + self.calendar.errors.len().min(2);
        AGENDA_HEAD + rows as f64 * AGENDA_ROW
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
        self.calendar_controls()
            .into_iter()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, hit)| hit)
            .or(Some(CalendarHit::Card))
    }

    fn calendar_controls(&self) -> Vec<(Rect, CalendarHit)> {
        let card = self.calendar_card();
        let (month, previous, next) = self.calendar_parts(card);
        let mut controls = vec![
            (previous, CalendarHit::Previous),
            (next, CalendarHit::Next),
            (Rect::new(month.x, month.y, (previous.x - month.x).max(0.), month.h), CalendarHit::Today),
        ];
        // Days only have something to show while there are calendars.
        if !self.config.file.calendar.sources.is_empty() {
            controls.extend(self.calendar_days(card).into_iter().map(|(cell, day)| (cell, CalendarHit::Day(day))));
        }
        controls
    }

    /// The month the calendar shows: its year, and its month from 1 to 12.
    fn calendar_month(&self) -> Option<(i32, i32)> {
        let now = LocalTime::now()?;
        Some(add_months(now.year, now.month as i32 + 1, self.bar.calendar.offset))
    }

    /// The month's grid: six weeks from the Monday on or before its first,
    /// each day's cell with the day, in days since 1970.
    fn calendar_days(&self, card: Rect) -> Vec<(Rect, i64)> {
        let Some((year, month)) = self.calendar_month() else { return Vec::new() };
        let (header, _, _) = self.calendar_parts(card);
        let grid_x = card.x + 16.0;
        let cell_w = (card.w - 32.0) / 8.0;
        let days_y = header.bottom() + 8.0;
        let first = days_from_civil(year, month, 1);
        let start = first - monday_index(first);
        (0..42)
            .map(|index| {
                let (week, weekday) = (index / 7, index % 7);
                let cell = Rect::new(
                    grid_x + (weekday + 1) as f64 * cell_w,
                    days_y + 24.0 + week as f64 * CALENDAR_CELL_H,
                    cell_w,
                    CALENDAR_CELL_H,
                );
                (cell, start + index)
            })
            .collect()
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
        let level = ease_out(self.bar.calendar.zoom.level(duration));
        if level > 0.001 {
            let clock = self.calendar_clock(layout.as_ref());
            self.draw_calendar(&mut painter, clock, level);
        }
        let level = ease_out(self.updates.popover.zoom.level(duration));
        if level > 0.001 {
            self.draw_updates_popover(&mut painter, level);
        }
        let level = ease_out(self.bar.minimap.zoom.level(duration));
        if level > 0.001 {
            let chip = layout.as_ref().and_then(|placed| placed.minimap).unwrap_or_else(|| {
                let out = self.output_rect();
                let top = if self.bar_at_bottom() { out.bottom() - BAR_H } else { out.y };
                Rect::new(out.x + 100.0, top, 40.0, BAR_H)
            });
            self.draw_minimap_card(&mut painter, chip, level);
        }
        let level = ease_out(self.network.zoom.level(duration));
        if level > 0.001 {
            let chip = self.network_anchor_in(layout.as_ref());
            self.draw_network_card(&mut painter, chip, level);
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

    pub(crate) fn tray_pointer_anchor(&self) -> Option<Rect> {
        let layout = self.bar_layout()?;
        Some(layout.tray.last().map(|(_, rect)| *rect).unwrap_or(layout.clock))
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
        painter.frosted(bar, Shape::filled(theme.surface, 0.0));
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

        if let Some(chip) = layout.minimap {
            hover_fill(painter, chip, self.bar.minimap.zoom.open || hit == Some(Hit::Minimap));
            self.draw_minimap_chip(painter, chip);
        }
        self.draw_crumbs(painter, layout, hit);
        self.draw_tray_slots(painter, &layout.tray);

        let worlds = self.area_worlds();
        for (slot, rect) in &layout.slots {
            // Saved in any area; shown now when every area that has it is there.
            let saved: Vec<_> =
                worlds.iter().filter_map(|(_, world)| world.saved_view_node(*slot).map(|node| (world, node))).collect();
            let here =
                !saved.is_empty() && saved.iter().all(|(world, node)| !world.in_overview() && world.framed() == *node);
            let label = slot.to_string();
            match saved.first() {
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

        let plugin_chips = self.plugin_chips();
        for chip in &layout.chips {
            if matches!(chip.kind, ChipKind::Network) {
                // A plain icon like the bell's, not a pill.
                hover_fill(painter, chip.rect, self.network.zoom.open || hit == Some(Hit::Network));
                self.draw_network_icon(painter, chip.rect);
                continue;
            }
            let lit = match (chip.kind, hit) {
                (ChipKind::Updates, hit) => hit == Some(Hit::Updates) || self.updates.popover.zoom.open,
                (ChipKind::Media, Some(Hit::Media)) | (ChipKind::Dnd, Some(Hit::Dnd)) => true,
                (ChipKind::Network, _) if self.network.zoom.open => true,
                (ChipKind::Network, Some(Hit::Network)) => true,
                (ChipKind::Event, Some(Hit::Event)) => true,
                (ChipKind::Plugin(index), Some(Hit::Plugin(hovered))) => index == hovered,
                _ => false,
            };
            let (icon, tint) = match chip.kind {
                ChipKind::Updates => match self.updates.chip().map(|(_, tone)| tone) {
                    Some(crate::updates::Tone::Warning) => (glyph::ALERT, theme.warning),
                    Some(crate::updates::Tone::Busy) => (glyph::RESTART, theme.accent),
                    _ => (UPDATES, theme.accent),
                },
                ChipKind::Plugin(index) => {
                    let chip = plugin_chips.get(index).map(|(_, chip)| *chip);
                    let icon = chip.and_then(|chip| chip.icon.as_deref()).and_then(glyph::named).unwrap_or(glyph::PLUG);
                    let tint = chip.map_or(crate::panel::block::Tint::Accent, |chip| chip.tint);
                    (icon, crate::panel::tint(theme, tint).1)
                }
                ChipKind::Media => (
                    match self.media.snapshot.media.active().map(|player| player.status) {
                        Some(tileception_media::Status::Playing) => glyph::MUSIC,
                        _ => glyph::PAUSE,
                    },
                    theme.accent,
                ),
                ChipKind::Network => (glyph::WIFI_DISABLED, theme.muted),
                ChipKind::Capture => (SCREEN, theme.danger),
                ChipKind::Dnd => (glyph::MOON, theme.warning),
                ChipKind::Event => (glyph::CALENDAR, theme.accent),
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

        let calendar = self.bar.calendar.zoom.open;
        hover_fill(painter, layout.clock, calendar || hit == Some(Hit::Clock));
        painter.text(&layout.clock_text, layout.clock, style.bold().colour(theme.text).align(Align::Centre));

        if let (Some((rect, label)), Some(reading)) = (&layout.battery, &self.battery.reading) {
            hover_fill(painter, *rect, hit == Some(Hit::Battery));
            self.draw_battery(painter, *rect, label, reading);
        }

        let bell_lit = self.notifications.open || hit == Some(Hit::Notifications);
        hover_fill(painter, layout.notifications, bell_lit);
        let dnd = self.notification_status().is_some_and(|(_, dnd)| dnd);
        let bell = Rect::new(layout.notifications.x + 2.0, layout.notifications.y, 24.0, layout.notifications.h);
        painter.glyph(glyph::BELL, bell, ICON, if dnd { theme.muted } else { theme.text.alpha(0.86) });
        if let Some((count, _)) = self.notification_status().filter(|(unread, _)| *unread > 0) {
            let label = count.to_string();
            let width = self.badge_width(&label);
            // Over the bell's top right corner, pulled back in for wide counts.
            let left = (bell.x + 13.0).min(layout.notifications.right() + 2.0 - width);
            let badge = Rect::new(left, bell.y + 1.0, width, BADGE_H);
            // A ring in the bar's colour keeps the badge apart from the bell.
            // Frosted, the bar is see-through and a solid ring would show.
            if !theme.frost {
                let behind = if bell_lit { theme.surface_raised } else { theme.surface };
                painter.fill(badge.inset(-1.5), behind, (BADGE_H / 2.0 + 1.5) as f32);
            }
            let fill = if dnd { (theme.muted, theme.muted) } else { theme.accent_fill() };
            painter.shape(badge, Shape { fill, ..Shape::filled(Colour::TRANSPARENT, (BADGE_H / 2.0) as f32) });
            let ink = if dnd { theme.surface } else { theme.on_accent };
            painter.text_on_ink(&label, badge, badge_style(theme, ink));
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
        if hit == Some(Hit::Network)
            && !self.network.zoom.open
            && let Some(chip) = layout.chips.iter().find(|chip| matches!(chip.kind, ChipKind::Network))
        {
            let (title, body) = self.network_tooltip();
            self.bar_tooltip(painter, (chip.rect.x - 120.0).max(bar.x + 6.0), bar, &title, &body);
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
    pub(crate) fn app_mark(&self, painter: &mut Painter, app: &str, origin: (f64, f64)) {
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

    /// How wide the bell's badge is for `label`: round for one digit, a
    /// pill for more.
    fn badge_width(&self, label: &str) -> f64 {
        (self.bar_measure(label, badge_style(&self.theme, self.theme.on_accent), 40.0) + 7.0).max(BADGE_H)
    }

    fn bar_measure_height(&self, text: &str, style: Style, max: f64) -> f64 {
        let theme = &self.theme;
        self.text
            .borrow_mut()
            .styled(text, &theme.font, style.size, style.colour, max.max(1.0) as f32, style.weight, style.lines)
            .map(|rendered| rendered.height)
            .unwrap_or(0.0)
    }

    /// What a saved view frames, drawn small beside its slot: on a split
    /// screen, each area's part side by side.
    fn draw_slot_preview(&self, painter: &mut Painter, layout: &Layout, slot_number: usize) {
        // Each area holding the view: its world, the node and its shape on screen.
        let parts: Vec<_> = self
            .area_worlds()
            .into_iter()
            .filter_map(|(_, world)| {
                let node = world.saved_view_node(slot_number)?;
                let unit = world.layout().rect(node)?;
                let screen = world.screen();
                Some((world, node, unit, (unit.w * screen.w) / (unit.h * screen.h).max(1.0)))
            })
            .collect();
        if parts.is_empty() {
            return;
        }
        let Some((_, slot)) = layout.slots.iter().find(|(number, _)| *number == slot_number) else { return };
        // The spaces as they look framed from the root, side by side, at most
        // 260 (or 400 for several areas) by 180.
        const PART_GAP: f64 = 6.0;
        let gaps = PART_GAP * (parts.len() - 1) as f64;
        let max_width = if parts.len() > 1 { 400.0 } else { 260.0 };
        let total_aspect: f64 = parts.iter().map(|part| part.3).sum();
        let preview_height = (180.0_f64).min(((max_width - gaps) / total_aspect.max(0.01)).round()).max(1.0);
        let preview_width = (total_aspect * preview_height + gaps).round();
        let left =
            (slot.x + slot.w / 2.0 - preview_width / 2.0).clamp(8.0, self.output_rect().right() - preview_width - 8.0);
        let height = preview_height + 16.0 + 24.0;
        let card = Rect::new(left, self.bar_popup_y(layout.bar, height), preview_width + 16.0, height);
        painter.popup(card);
        let mut x_pos = card.x + 8.0;
        let mut windows = 0;
        for (world, node, unit, aspect) in &parts {
            let area = Rect::new(x_pos, card.y + 8.0, aspect * preview_height, preview_height);
            x_pos = area.right() + PART_GAP;
            painter.fill(area, self.theme.background, 4.0);
            let tree = world.tree();
            windows += tree.windows_in(*node).len();
            for leaf in tree.leaves(*node) {
                let Some(leaf_rect) = world.layout().rect(leaf) else { continue };
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
        }
        let label = format!("View {slot_number} \u{b7} {windows} window{}", if windows == 1 { "" } else { "s" });
        let theme = &self.theme;
        painter.text(
            &label,
            Rect::new(card.x + 8.0, card.y + 8.0 + preview_height + 2.0, preview_width, 22.0),
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
        let card = self.calendar_card_for(clock);
        let rect = clock.zoom_lerp(&card, level);
        let (alpha, offset) = (painter.alpha, painter.offset);
        painter.popup(rect);
        // The month fades in once the card is nearly there, and moves with it.
        painter.alpha = alpha * card_contents(level);
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
            &now.clock_seconds(),
            Rect::new(card.x + 20.0, card.y + 14.0, card.w - 40.0, 46.0),
            Style::new(theme.font_size * 2.6, theme.text).weight(300),
        );
        painter.text(
            &now.date(),
            Rect::new(card.x + 22.0, card.y + 58.0, card.w - 40.0, 24.0),
            Style::new(theme.font_size, theme.muted),
        );

        let Some((year, month)) = self.calendar_month() else { return };
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
        let days_y = header.bottom() + 8.0;
        let small = Style::new(theme.font_size * 0.74, theme.muted).align(Align::Centre);
        for (index, name) in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].iter().enumerate() {
            let cell = Rect::new(grid_x + (index + 1) as f64 * cell_w, days_y, cell_w, 22.0);
            painter.text(name, cell, small.bold());
        }
        let today = days_from_civil(now.year, now.month as i32 + 1, now.day);
        let days = self.calendar_days(card);
        let busy = self.busy_days(days.first().map_or(0, |(_, day)| *day), days.len());
        for (index, (cell, day)) in days.into_iter().enumerate() {
            let weekday = index % 7;
            if weekday == 0 {
                let number = Rect::new(grid_x, cell.y, cell_w, cell.h);
                painter.text(&iso_week(day).to_string(), number, small.colour(theme.muted.alpha(0.6)));
            }
            let (_, cell_month, dom) = civil_from_days(day);
            let inside = cell_month == month;
            let style = Style::new(theme.font_size * 0.92, if inside { theme.text } else { theme.muted.alpha(0.5) })
                .align(Align::Centre);
            let mark = Rect::new(cell.x + (cell.w - 30.0) / 2.0, cell.y + 2.0, 30.0, 30.0);
            // Hovering is shown by the pointer feedback every button gets.
            if self.bar.calendar.selected == Some(day) {
                let ring = Rect::new(mark.x - 2.0, mark.y - 2.0, mark.w + 4.0, mark.h + 4.0);
                painter.shape(
                    ring,
                    Shape {
                        border_width: 1.5,
                        border: theme.accent.alpha(0.9),
                        ..Shape::filled(Colour::TRANSPARENT, 17.0)
                    },
                );
            }
            if day == today {
                painter.shape(
                    mark,
                    Shape {
                        radius: 15.0,
                        fill: theme.accent_fill(),
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
            // A dot below the number for a day with events.
            if busy[index] {
                let colour =
                    if day == today { theme.on_accent } else { theme.accent.alpha(if inside { 0.9 } else { 0.4 }) };
                painter.fill(Rect::new(cell.center().x - 2.0, cell.y + 25.0, 4.0, 4.0), colour, 2.0);
            }
        }
        if self.agenda_height() > 0.0 {
            self.draw_agenda(painter, card, today);
        }
        (painter.alpha, painter.offset) = (alpha, offset);
    }

    /// Today's events and the coming week's below the month, or the
    /// clicked day's.
    fn draw_agenda(&self, painter: &mut Painter, card: Rect, today: i64) {
        let theme = &self.theme;
        let left = card.x + 20.0;
        let width = card.w - 40.0;
        let top = card.y + CALENDAR_H - 8.0;
        let selected = self.bar.calendar.selected;
        let heading = match selected {
            None => "COMING UP".to_string(),
            Some(day) => day_heading(day, today).to_uppercase(),
        };
        painter.fill(Rect::new(left, top, width, 1.0), theme.text.alpha(0.08), 0.0);
        painter.text(
            &heading,
            Rect::new(left, top + 8.0, width, 20.0),
            Style::new(theme.font_size * 0.7, theme.muted).bold(),
        );
        let now = crate::calendar::now();
        let mut row_y = top + AGENDA_HEAD;
        let title = Style::new(theme.font_size * 0.9, theme.text);
        let detail = Style::new(theme.font_size * 0.74, theme.muted);
        let events = self.agenda();
        if events.is_empty() {
            let none = if selected.is_some() { "Nothing on this day" } else { "Nothing in the coming week" };
            painter.text(none, Rect::new(left, row_y, width, 22.0), detail);
            row_y += AGENDA_ROW;
        }
        for event in events {
            let Some(start) = LocalTime::at(event.start) else { continue };
            let day = days_from_civil(start.year, start.month as i32 + 1, start.day);
            let ongoing = event.start <= now && now < event.end.max(event.start + 1);
            let lit = ongoing || (!event.all_day && day == today);
            // A mark in the accent colour, faint for later days.
            let mark = Rect::new(left, row_y + 3.0, 3.0, AGENDA_ROW - 12.0);
            painter.fill(mark, theme.accent.alpha(if lit { 0.9 } else { 0.35 }), 1.5);
            let summary = event.summary.split_whitespace().collect::<Vec<_>>().join(" ");
            let summary = if summary.is_empty() { "Event".to_string() } else { summary };
            let text_left = left + 12.0;
            painter.text(&summary, Rect::new(text_left, row_y, width - 12.0, 20.0), title);
            // The heading names a clicked day, so its own events need only their times.
            let mut when = if selected == Some(day) {
                String::new()
            } else if day == today || (selected.is_none() && day < today) {
                "Today".to_string()
            } else if day == today + 1 {
                "Tomorrow".to_string()
            } else {
                short_date(&start)
            };
            if event.all_day {
                when = if when.is_empty() { "All day".to_string() } else { format!("{when}, all day") };
            } else if ongoing {
                let end = LocalTime::at(event.end)
                    .zip(LocalTime::at(now))
                    .map(|(end, here)| ends_at(&end, &here))
                    .unwrap_or_default();
                when = format!("Now, until {end}");
            } else {
                let end = LocalTime::at(event.end).filter(|_| event.end > event.start);
                let times = match end {
                    Some(end) => format!("{} \u{2013} {}", start.clock(), end.clock()),
                    None => start.clock(),
                };
                when = if when.is_empty() { times } else { format!("{when}, {times}") };
            }
            if !event.location.trim().is_empty() {
                let place = event.location.split_whitespace().collect::<Vec<_>>().join(" ");
                when = format!("{when} \u{b7} {place}");
            }
            painter.text(&when, Rect::new(text_left, row_y + 18.0, width - 12.0, 18.0), detail);
            row_y += AGENDA_ROW;
        }
        for error in self.calendar.errors.iter().take(2) {
            let rect = Rect::new(left, row_y, width, 34.0);
            painter.text(error, rect, detail.colour(theme.danger).lines(2));
            row_y += AGENDA_ROW;
        }
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

/// The event chip's text: "14:00  Review" for the next event, "until
/// 14:30  Review" for one on now, with the weekday when it ends on a later
/// day.
fn event_chip(summary: &str, ongoing: bool, start: &LocalTime, end: &LocalTime, now: &LocalTime) -> String {
    let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let summary = if summary.is_empty() { "Event".to_string() } else { summary };
    if ongoing { format!("until {}  {summary}", ends_at(end, now)) } else { format!("{}  {summary}", start.clock()) }
}

/// When an event on now ends: its time today, with the weekday later this
/// week, and with the date after that.
fn ends_at(end: &LocalTime, now: &LocalTime) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let day = |time: &LocalTime| days_from_civil(time.year, time.month as i32 + 1, time.day);
    match day(end) - day(now) {
        ..=0 => end.clock(),
        1..=6 => format!("{} {}", DAYS[end.weekday], end.clock()),
        _ => format!("{} {}", short_date(end), end.clock()),
    }
}

/// How the calendar heads a clicked day's events: "Today", "Tomorrow",
/// or "Thursday 26 November", with the year when it is not this year's.
fn day_heading(day: i64, today: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    match day - today {
        0 => return "Today".to_string(),
        1 => return "Tomorrow".to_string(),
        _ => {}
    }
    let (year, month, dom) = civil_from_days(day);
    let (this_year, _, _) = civil_from_days(today);
    let name = format!("{} {dom} {}", WEEKDAYS[monday_index(day) as usize], MONTHS[(month - 1) as usize]);
    if year == this_year { name } else { format!("{name} {year}") }
}

/// A month `delta` months after `month` (1 to 12) of `year`.
pub(crate) fn add_months(year: i32, month: i32, delta: i32) -> (i32, i32) {
    let index = year * 12 + (month - 1) + delta;
    (index.div_euclid(12), index.rem_euclid(12) + 1)
}

/// How visible a bar card's contents are while the card zooms out of the bar
/// to `level`: they fade in over the last stretch, the same for every card.
pub(crate) fn card_contents(level: f64) -> f32 {
    ease_out(((level - 0.7) / 0.3).clamp(0.0, 1.0)) as f32
}

/// The notification count's lettering on the bell's badge.
fn badge_style(theme: &crate::theme::Theme, colour: Colour) -> Style {
    Style::new(theme.font_size * 0.66, colour).bold()
}

/// Days since 1970-01-01 for a date in the proleptic Gregorian calendar.
pub(crate) fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let adjusted_year = i64::from(if month <= 2 { year - 1 } else { year });
    let era = adjusted_year.div_euclid(400);
    let yoe = adjusted_year - era * 400;
    let month_num = i64::from(month);
    let doy = (153 * (if month_num > 2 { month_num - 3 } else { month_num + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date for a day number from [`days_from_civil`].
pub(crate) fn civil_from_days(days: i64) -> (i32, i32, i32) {
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
pub(crate) fn monday_index(days: i64) -> i64 {
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
    fn gl_driver_errors_stay_off_the_bar() {
        assert!(from_gl_driver("smithay::backend::renderer::gles", "[GL] GL_INVALID_OPERATION in glDrawArrays"));
        assert!(from_gl_driver("smithay::backend::renderer::gles::shaders", "[GL] 0:12: syntax error"));
        assert!(!from_gl_driver("smithay::backend::renderer::gles", "Failed to create EGL context"));
        assert!(!from_gl_driver("tileception::config", "[GL] not really"));
    }

    #[test]
    fn a_clicked_day_is_named_in_its_heading() {
        let today = days_from_civil(2026, 11, 13);
        assert_eq!(day_heading(today, today), "Today");
        assert_eq!(day_heading(today + 1, today), "Tomorrow");
        assert_eq!(day_heading(days_from_civil(2026, 11, 26), today), "Thursday 26 November");
        assert_eq!(day_heading(today - 1, today), "Thursday 12 November");
        assert_eq!(day_heading(days_from_civil(2027, 1, 4), today), "Monday 4 January 2027");
    }

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

    #[test]
    fn the_event_chip_says_when() {
        // Monday 9 November 2026 and the days after.
        let local = |day: i32, weekday: usize, hour: i32, minute: i32| LocalTime {
            year: 2026,
            hour,
            minute,
            second: 0,
            weekday,
            day,
            month: 10,
        };
        let now = local(9, 1, 14, 5);
        let (start, end) = (local(9, 1, 14, 0), local(9, 1, 14, 30));
        assert_eq!(event_chip("Review", false, &start, &end, &now), "14:00  Review");
        assert_eq!(event_chip(" Design\n review ", true, &start, &end, &now), "until 14:30  Design review");
        assert_eq!(event_chip("", true, &start, &end, &now), "until 14:30  Event");
        // One ending on a later day names the day.
        assert_eq!(event_chip("Trip", true, &start, &local(10, 2, 9, 0), &now), "until Tue 09:00  Trip");
        // The calendar names the date of one ending a week or more later.
        assert_eq!(ends_at(&local(16, 1, 9, 0), &now), "Mon 16 Nov 09:00");
    }
}
