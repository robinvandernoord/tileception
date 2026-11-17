//! The Updates window: a real toplevel drawn by the compositor, like
//! Settings. Its pages list the pending updates by source package, app and
//! device, the kernels with a queue of installs and removals, the next
//! release with what to look at first, the apt history, and the mirror and
//! automatic updates.

use std::collections::{BTreeMap, BTreeSet};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    input::{keyboard::keysyms, pointer::CursorIcon},
};
use tileception_core::{Point, Rect, WindowId};
use tileception_updates::{self as backend, Snapshot, apt, kernels, release};

use super::Task;
use crate::{
    Tileception,
    config::Mods,
    render::OutputElement,
    settings::client::{self, Client},
    shapes::Shape,
    theme::Colour,
    ui::{Align, Button, Look, Painter, PointerEvent, PointerTarget, Style, glyph},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Available,
    Kernels,
    Release,
    History,
    Settings,
}

impl Page {
    const ALL: [Page; 5] = [Page::Available, Page::Kernels, Page::Release, Page::History, Page::Settings];

    pub fn parse(name: &str) -> Option<Page> {
        match name {
            "available" | "updates" => Some(Page::Available),
            "kernels" => Some(Page::Kernels),
            "release" => Some(Page::Release),
            "history" => Some(Page::History),
            "settings" | "mirror" | "mirrors" | "automatic" => Some(Page::Settings),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Available => "Available",
            Page::Kernels => "Kernels",
            Page::Release => "Release",
            Page::History => "History",
            Page::Settings => "Settings",
        }
    }
}

pub(crate) struct Window {
    pub(crate) open: bool,
    window: Option<WindowId>,
    client: Option<Client>,
    pub(crate) size: (f64, f64),
    pointer: (f64, f64),
    pressed: Option<PointerTarget>,
    page: Page,
    scroll: f64,
    /// The highlighted row of the page's list.
    selected: usize,
    /// Updates left out of the next install, by source and name.
    skipped: BTreeSet<apt::Key>,
    /// Firmware chosen for the next install: firmware is never ticked by
    /// default.
    firmware: BTreeSet<apt::Key>,
    /// Kernels queued to install or remove, by image package.
    queue: BTreeMap<String, kernels::Queued>,
    /// The leftover headers, and the leftover modules and tools, are queued too.
    headers: bool,
    support: bool,
    /// Removals also drop the packages' configuration.
    purge: bool,
    /// Every variant is listed: other flavours, unsigned and debug builds.
    all_variants: bool,
    /// Kernel series whose builds are folded away.
    folded: BTreeSet<String>,
    /// A sheet of text shown in place of the list: what apt says a
    /// transaction would do (empty while it is being asked), or a log.
    preview: Option<String>,
    /// Identifies the simulation requested for the current preview.
    preview_request: u64,
    /// What the sheet is about, while there is one.
    sheet: Sheet,
}

/// What the sheet of text in place of the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Sheet {
    /// What the kernel queue would do.
    #[default]
    Queue,
    /// What installing the ticked updates would do.
    Install,
    /// The same review, with a restart after the confirmed install.
    InstallRestart,
    /// What removing the unneeded dependencies would do.
    Autoremove,
    /// What settling broken dependencies would do.
    Repair,
    /// What the last task printed.
    Log,
    /// What changed in an update.
    Notes,
}

impl Sheet {
    fn title(self) -> &'static str {
        match self {
            Sheet::Queue => "What the queue would do",
            Sheet::Install => "What installing would do",
            Sheet::InstallRestart => "What installing before restarting would do",
            Sheet::Autoremove => "Clean-up preview",
            Sheet::Repair => "What a repair would do",
            Sheet::Log => "The last task",
            Sheet::Notes => "Release notes",
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Window {
            open: false,
            window: None,
            client: None,
            size: (880.0, 580.0),
            pointer: (-1.0, -1.0),
            pressed: None,
            page: Page::Available,
            scroll: 0.0,
            selected: 0,
            skipped: BTreeSet::new(),
            firmware: BTreeSet::new(),
            queue: BTreeMap::new(),
            headers: false,
            support: false,
            purge: true,
            all_variants: false,
            folded: BTreeSet::new(),
            preview: None,
            preview_request: 0,
            sheet: Sheet::Queue,
        }
    }
}

impl Window {
    pub(super) fn invalidate(&self) {
        if let Some(client) = &self.client {
            client.invalidate();
        }
    }

    pub(super) fn set_preview(&mut self, request: u64, text: String) {
        if request == self.preview_request && self.preview.is_some() && self.sheet != Sheet::Log {
            let empty =
                if self.sheet == Sheet::Notes { "No release notes were found." } else { "apt-get printed nothing." };
            self.preview = Some(if text.trim().is_empty() { empty.into() } else { text });
            self.scroll = 0.0;
        }
    }

    pub(super) fn all_variants(&self) -> bool {
        self.all_variants
    }

    /// Forgets choices about packages that are gone after a new look.
    pub(super) fn settle(&mut self, snapshot: &Snapshot, found: &[kernels::Kernel]) {
        let listed = |key: &apt::Key| snapshot.updates.iter().any(|update| update.key() == *key);
        self.skipped.retain(listed);
        self.firmware.retain(listed);
        self.queue.retain(|package, queued| {
            found.iter().find(|kernel| kernel.package == *package).is_some_and(|kernel| match queued {
                kernels::Queued::Install => !kernel.installed,
                kernels::Queued::Remove => kernel.installed && !kernel.running,
            })
        });
        self.headers &= !kernels::unused_headers(&snapshot.packages).is_empty();
        self.support &= !kernels::unused_support(&snapshot.packages).is_empty();
    }

    /// Whether an update is ticked for the next install.
    pub(super) fn ticked(&self, update: &apt::Update) -> bool {
        if update.source == apt::Source::Firmware {
            self.firmware.contains(&update.key())
        } else {
            !self.skipped.contains(&update.key())
        }
    }

    fn toggle(&mut self, update: &apt::Update, tick: bool) {
        let (set, add) = if update.source == apt::Source::Firmware {
            (&mut self.firmware, tick)
        } else {
            (&mut self.skipped, !tick)
        };
        if add {
            set.insert(update.key());
        } else {
            set.remove(&update.key());
        }
    }

    pub(super) fn task_done(&mut self, task: &Task) {
        if matches!(task, Task::Remove(_) | Task::Kernels { .. }) {
            self.queue.clear();
            self.headers = false;
            self.support = false;
        }
        // A simulation of what has now been done says nothing any more.
        if self.sheet != Sheet::Log {
            self.preview = None;
        }
    }
}

/// The two kinds of leftover kernel package, cleaned up apart as in kerntop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leftover {
    Headers,
    Support,
}

/// One line of a page's list.
#[derive(Debug, Clone, PartialEq)]
enum Row {
    Heading(String, String),
    /// A source package's updates, an app or a device: the first update
    /// and how many follow it.
    Group(usize, usize),
    /// An update that waits until it is let through: a skipped version,
    /// then the held packages.
    Muted(usize),
    /// Automatic updates, with the switch.
    Automatic,
    /// A tested mirror.
    Mirror(usize),
    /// Tests the mirrors, with its button.
    TestMirrors,
    /// A mirror the running test has yet to reach; the first is being tested.
    Waiting(usize),
    Warning(String),
    /// A kernel series: its name, and how many builds are installed and available.
    Series(String, usize, usize),
    Kernel(usize),
    Leftovers(Leftover),
    Check(usize),
    Entry(usize),
    /// The dependencies that are no longer needed, with what removes them.
    Unneeded,
    Text(String),
}

impl Row {
    fn height(&self) -> f64 {
        match self {
            Row::Heading(..) | Row::Series(..) => 32.0,
            Row::Entry(_) | Row::Muted(_) | Row::Warning(_) => 30.0,
            Row::Group(..) => 44.0,
            Row::Kernel(_)
            | Row::Leftovers(_)
            | Row::Unneeded
            | Row::Mirror(_)
            | Row::TestMirrors
            | Row::Waiting(_) => 34.0,
            Row::Check(_) | Row::Automatic => 48.0,
            Row::Text(_) => 18.0,
        }
    }

    fn selectable(&self) -> bool {
        matches!(
            self,
            Row::Group(..)
                | Row::Muted(_)
                | Row::Series(..)
                | Row::Kernel(_)
                | Row::Entry(_)
                | Row::Leftovers(_)
                | Row::Unneeded
                | Row::Automatic
                | Row::Mirror(_)
                | Row::TestMirrors
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Page(Page),
    Refresh,
    Select(usize),
    Tick(usize),
    Kernel(usize),
    /// Folds or unfolds the series at this row.
    Fold(usize),
    Leftovers(Leftover),
    Variants,
    Purge,
    Clear,
    Install,
    Security,
    Suggest,
    Preview,
    /// Shows a sheet: a transaction before it runs, or the last log.
    Sheet(Sheet),
    /// Leaves the sheet.
    Back,
    /// Does what the sheet shows.
    Run,
    /// Puts a failure away.
    Dismiss,
    Apply,
    Upgrade,
    Copy,
    Doctor,
    Move,
    /// The release notes of the highlighted update.
    Notes,
    /// Skips the highlighted update's version, or stops skipping it.
    Skip(bool),
    /// Holds the highlighted package, or releases it.
    Hold(bool),
    /// Turns automatic updates on or off.
    Automatic(bool),
    TestMirrors,
    UseMirror(usize),
}

impl crate::ui::PointerAction for Hit {
    fn activate(&self, state: &mut Tileception, _rect: Rect, _point: Point) {
        state.updates_window_activate(*self);
    }
}

struct Frame {
    panel: Rect,
    side: Rect,
    nav: Vec<(Rect, Page)>,
    header: Rect,
    list: Rect,
    detail: Option<Rect>,
    footer: Rect,
    /// The rows where they are drawn, scrolled.
    rows: Vec<(Rect, Row)>,
    max_scroll: f64,
}

const SIDE: f64 = 184.0;
const FOOTER: f64 = 54.0;
const DETAIL: f64 = 58.0;

/// Narrow headers put their actions below the title, wrapping at button widths.
fn header_button_rows(width: f64, count: usize) -> usize {
    if count == 0 || width >= 160.0 + count as f64 * 112.0 {
        return 0;
    }
    count.div_ceil(header_button_columns(width, count))
}

fn header_button_columns(width: f64, count: usize) -> usize {
    (((width - 32.0 + 8.0) / 112.0).floor().max(1.0) as usize).min(count.max(1))
}

/// The width of each of `count` buttons in a footer: `widest`, or less when
/// the window is narrow (a split screen's area), so they stay inside it.
fn footer_button_width(footer: Rect, count: usize, widest: f64) -> f64 {
    let count = count.max(1) as f64;
    let room = footer.w - 32.0 - (count - 1.0) * 8.0;
    widest.min(room / count).max(1.0)
}

/// Button `slot` of `count`, counted from the right.
fn footer_button(footer: Rect, slot: usize, count: usize, widest: f64) -> Rect {
    let width = footer_button_width(footer, count, widest);
    let right = footer.right() - 16.0 - slot as f64 * (width + 8.0);
    Rect::new(right - width, footer.y + (footer.h - 32.0) / 2.0, width, 32.0)
}

/// What is left of a footer for text beside its buttons.
fn footer_text_room(footer: Rect, count: usize, widest: f64) -> f64 {
    footer.w - 36.0 - count as f64 * (footer_button_width(footer, count, widest) + 8.0)
}

fn row_button(row: Rect) -> Rect {
    Rect::new(row.right() - 86.0, row.y + (row.h - 26.0) / 2.0, 78.0, 26.0)
}

fn tick_box(row: Rect) -> Rect {
    Rect::new(row.x + 8.0, row.y + (row.h - 16.0) / 2.0, 16.0, 16.0)
}

/// Draws text ending at the right edge of `rect`.
fn text_right(painter: &mut Painter, label: &str, rect: Rect, style: Style) {
    let (width, _) = painter.measure(label, rect.w, style);
    painter.text(label, Rect::new(rect.right() - width, rect.y, width + 1.0, rect.h), style);
}

impl Tileception {
    /// Opens the Updates window on a page, or turns the open one to it.
    pub fn open_updates(&mut self, page: Page) {
        if self.is_locked() || !self.updates_available() {
            return;
        }
        let window = &mut self.updates.window;
        if window.page != page {
            window.page = page;
            window.scroll = 0.0;
            window.selected = 0;
            window.preview = None;
        }
        if self.updates.window.open {
            self.updates_select_first();
            self.updates_changed();
            return;
        }
        self.power_menu = None;
        self.close_tray();
        self.close_launcher();
        let screen = self.world.screen();
        // Never wider than the screen, or on a split screen its area.
        self.updates.window.size = (
            880.0_f64.min(screen.w - 40.0).max(480.0).min(screen.w),
            580.0_f64.min(screen.h - 40.0).max(360.0).min(screen.h),
        );
        self.updates.window.open = true;
        // What the window lists should be as it is now, not hours old.
        self.updates_request(super::Request::Scan { offer: false, apps: true });
        match Client::start(self, client::Kind::Updates) {
            Ok(client) => self.updates.window.client = Some(client),
            Err(error) => {
                self.updates.window.open = false;
                tracing::warn!("could not open the Updates window: {error}");
            }
        }
        self.updates_select_first();
        self.schedule_render();
    }

    pub(crate) fn close_updates(&mut self) {
        let window = &mut self.updates.window;
        window.open = false;
        window.pressed = None;
        window.window = None;
        window.preview = None;
        if let Some(client) = window.client.take() {
            self.loop_handle.remove(client.source);
        }
        self.schedule_render();
    }

    pub(crate) fn is_updates_window(&self, window: WindowId) -> bool {
        self.updates.window.window == Some(window)
    }

    /// Whether keys go to the Updates window.
    pub(crate) fn updates_window_focused(&self) -> bool {
        self.updates.window.window.is_some()
            && self.world.focus() == self.updates.window.window
            && self.is_active_output()
            && !self.world.in_overview()
    }

    pub(crate) fn updates_client_window(
        &mut self,
        surface: &smithay::wayland::shell::xdg::ToplevelSurface,
        window: WindowId,
    ) {
        use smithay::reexports::wayland_server::Resource;
        let size = self.updates.window.size;
        let ours =
            self.updates.window.client.as_ref().is_some_and(|client| {
                surface.wl_surface().client().is_some_and(|owner| owner.id() == client.client_id)
            });
        if ours {
            self.updates.window.window = Some(window);
            surface.with_pending_state(|pending| pending.size = Some((size.0 as i32, size.1 as i32).into()));
        }
    }

    pub(crate) fn updates_client_event(
        &mut self,
        client_id: &smithay::reexports::wayland_server::backend::ClientId,
        event: client::Event,
    ) {
        let window = &self.updates.window;
        if !window.open || window.client.as_ref().is_none_or(|client| client.client_id != *client_id) {
            return;
        }
        window.invalidate();
        if let Some(area) = window.window.and_then(|window| self.area_for_window(window))
            && self.context_area().as_ref() != Some(&area)
        {
            let client_id = client_id.clone();
            self.with_area(&area, move |state| state.updates_client_event(&client_id, event));
            return;
        }
        match event {
            client::Event::Resize(width, height) => {
                self.updates.window.pressed = None;
                self.updates.window.size = (f64::from(width), f64::from(height));
            }
            client::Event::Motion(point) => self.updates.window.pointer = (point.x, point.y),
            client::Event::Leave => {
                self.updates.window.pointer = (-1.0, -1.0);
                self.updates.window.pressed = None;
            }
            client::Event::Button(pressed) => self.updates_window_button(pressed),
            // The updates window has no text to paste into.
            client::Event::MiddleClick => {}
            client::Event::Scroll(amount) => {
                self.updates.window.scroll += amount * 3.0;
                self.updates_clamp_scroll();
            }
            client::Event::Key { sym, mods, pressed, code } => {
                if pressed {
                    self.updates_window_key(sym, mods);
                    if crate::key_repeat::edits(sym, mods) {
                        self.start_key_repeat(
                            code,
                            crate::key_repeat::Repeat::Commands(vec![crate::commands::Command::UpdatesKey(sym, mods)]),
                        );
                    }
                } else {
                    self.key_released(code, crate::config::modifier_of(sym).is_some());
                }
            }
            client::Event::Unfocus => {
                self.stop_key_repeat();
                self.updates.window.pressed = None;
            }
            // Nothing in the updates window is lost by closing it.
            client::Event::Closed | client::Event::CloseRequested => self.close_updates(),
        }
        let point = Point::new(self.updates.window.pointer.0, self.updates.window.pointer.1);
        let cursor = self.updates_window_target(point).map_or(CursorIcon::Default, |target| target.cursor);
        if let Some(client) = &self.updates.window.client {
            client.cursor(cursor);
        }
        self.schedule_render();
    }

    /// Draws the window's buffer when it changed, in the context of the
    /// screen that shows it.
    pub(crate) fn update_updates_client(&mut self, renderer: &mut GlesRenderer) {
        if !self.updates.window.open {
            return;
        }
        if let Some(area) = self.updates.window.window.and_then(|window| self.area_for_window(window))
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.render_updates_client(renderer));
        } else {
            self.render_updates_client(renderer);
        }
    }

    fn render_updates_client(&self, renderer: &mut GlesRenderer) {
        use smithay::{
            backend::{
                allocator::Fourcc,
                renderer::{Bind, Offscreen, damage::OutputDamageTracker, gles::GlesTexture},
            },
            utils::{Rectangle, Transform},
        };
        let window = &self.updates.window;
        let Some(client) = &window.client else { return };
        if !client.needs_frame(self.scale) {
            return;
        }
        let logical = (window.size.0 as i32, window.size.1 as i32);
        let size = ((window.size.0 * self.scale).round() as i32, (window.size.1 * self.scale).round() as i32);
        let result = (|| -> Result<Vec<u8>, String> {
            let elements = self.updates_window_elements(renderer);
            let mut texture: GlesTexture =
                renderer.create_buffer(Fourcc::Abgr8888, size.into()).map_err(|error| error.to_string())?;
            {
                let mut target = renderer.bind(&mut texture).map_err(|error| error.to_string())?;
                let mut damage = OutputDamageTracker::new(size, 1.0, Transform::Normal);
                let result = damage
                    .render_output(renderer, &mut target, 0, &elements, self.theme.background.0)
                    .map_err(|error| format!("{error:?}"))?;
                result.sync.wait().map_err(|error| format!("{error:?}"))?;
            }
            crate::capture::read_region(renderer, &texture, Rectangle::from_size(size.into()))
        })();
        match result {
            Ok(pixels) => client.frame(client::Frame { pixels, width: size.0, height: size.1, logical }),
            Err(error) => tracing::warn!("could not draw the Updates window: {error}"),
        }
    }

    fn updates_rows(&self) -> Vec<Row> {
        let updates = &self.updates;
        let snapshot = &updates.snapshot;
        let mut rows = Vec::new();
        if let Some(preview) = &updates.window.preview {
            if preview.is_empty() {
                let waiting = match updates.window.sheet {
                    Sheet::Notes => "Reading the release notes...",
                    _ => "Asking apt-get what this would do...",
                };
                rows.push(Row::Text(waiting.into()));
            }
            if updates.window.sheet == Sheet::Repair && !preview.is_empty() {
                rows.push(Row::Text("First `dpkg --configure -a` finishes what was left half installed. Then:".into()));
                rows.push(Row::Text(String::new()));
            }
            rows.extend(preview.lines().map(|line| Row::Text(line.to_string())));
            return rows;
        }
        match updates.window.page {
            Page::Available => {
                if let Some(error) = &snapshot.error {
                    rows.push(Row::Text(error.clone()));
                }
                let mut heading = "";
                for (start, count) in groups(&snapshot.updates) {
                    let update = &snapshot.updates[start];
                    let (title, note) = match update.source {
                        _ if update.security => ("Security", ""),
                        apt::Source::Apt => ("Software", "by source package"),
                        apt::Source::Firmware => ("Firmware", "installed only when you tick it"),
                        _ => ("Apps", "Flatpak and Snap"),
                    };
                    if title != heading {
                        heading = title;
                        rows.push(Row::Heading(title.into(), note.into()));
                    }
                    rows.push(Row::Group(start, count));
                }
                if snapshot.updates.is_empty() && snapshot.error.is_none() {
                    let text = if updates.scanned { "Everything is up to date." } else { "Looking for updates..." };
                    rows.push(Row::Text(text.into()));
                }
                // What installing everything does besides, and what apt
                // leaves out of it.
                let plan = &snapshot.plan;
                let columns = ((updates.window.size.0 - SIDE - 60.0) / (f64::from(self.theme.font_size) * 0.5))
                    .max(24.0) as usize;
                for (title, note, names) in [
                    ("Removed", "removed when you install everything", &plan.removed),
                    ("Kept back", "held back by apt; needs a full upgrade", &plan.kept_back),
                    ("Phased", "rolling out gradually; arrives later", &plan.phased),
                ] {
                    if !names.is_empty() {
                        rows.push(Row::Heading(title.into(), note.into()));
                        rows.extend(wrap_names(names, columns).into_iter().map(Row::Text));
                    }
                }
                if !plan.unneeded.is_empty() {
                    rows.push(Row::Heading("Housekeeping".into(), String::new()));
                    rows.push(Row::Unneeded);
                }
                let muted = updates.skipped.len() + snapshot.held.len();
                if muted > 0 {
                    rows.push(Row::Heading("Skipped and held".into(), "wait until you let them through".into()));
                    rows.extend((0..muted).map(Row::Muted));
                }
            }
            Page::Settings => {
                let tool = updates.automatic.tool.name();
                rows.push(Row::Heading("Automatic updates".into(), format!("through {tool}")));
                if updates.automatic.available {
                    rows.push(Row::Automatic);
                } else {
                    rows.push(Row::Text(format!("{tool} is not installed, so nothing updates by itself.")));
                }
                let mirrors = &updates.mirrors;
                let note = mirrors.current.clone().unwrap_or_else(|| "none found in the package sources".into());
                rows.push(Row::Heading("Mirror".into(), note));
                if let Some(warning) = mirrors.warning() {
                    rows.push(Row::Warning(warning));
                }
                rows.extend((0..mirrors.candidates.len()).map(Row::Mirror));
                rows.extend((0..mirrors.waiting.len()).map(Row::Waiting));
                if mirrors.testing && mirrors.test_progress().is_none() {
                    rows.push(Row::Text("Finding the mirrors near you...".into()));
                } else if !mirrors.testing {
                    rows.push(Row::TestMirrors);
                }
            }
            Page::Kernels => {
                let found = &updates.kernels;
                let mut start = 0;
                while start < found.len() {
                    let series = found[start].series();
                    let end = start + found[start..].iter().take_while(|kernel| kernel.series() == series).count();
                    let installed = found[start..end].iter().filter(|kernel| kernel.installed).count();
                    let folded = updates.window.folded.contains(&series);
                    rows.push(Row::Series(series, installed, end - start - installed));
                    if !folded {
                        rows.extend((start..end).map(Row::Kernel));
                    }
                    start = end;
                }
                let note = "for kernels that are no longer installed";
                if !kernels::leftovers(&snapshot.packages).is_empty() {
                    rows.push(Row::Heading("Leftovers".into(), note.into()));
                }
                if !kernels::unused_headers(&snapshot.packages).is_empty() {
                    rows.push(Row::Leftovers(Leftover::Headers));
                }
                if !kernels::unused_support(&snapshot.packages).is_empty() {
                    rows.push(Row::Leftovers(Leftover::Support));
                }
                if found.is_empty() {
                    rows.push(Row::Text(format!(
                        "No kernel packages are installed here. The running kernel is {}.",
                        snapshot.running
                    )));
                }
            }
            Page::Release => {
                rows.push(Row::Heading("Before you start".into(), String::new()));
                rows.extend((0..self.updates_release_checks().len()).map(Row::Check));
            }
            Page::History => {
                rows.extend((0..snapshot.history.len()).map(Row::Entry));
                if snapshot.history.is_empty() {
                    rows.push(Row::Text("No apt history could be read.".into()));
                }
            }
        }
        rows
    }

    fn updates_frame(&self) -> Frame {
        let window = &self.updates.window;
        let panel = Rect::new(0.0, 0.0, window.size.0, window.size.1);
        let side = Rect::new(panel.x, panel.y, SIDE.min(panel.w * 0.34), panel.h);
        let nav = Page::ALL
            .iter()
            .enumerate()
            .map(|(index, page)| {
                (Rect::new(side.x + 8.0, side.y + 60.0 + index as f64 * 36.0, side.w - 16.0, 32.0), *page)
            })
            .collect();
        let body = Rect::new(side.right(), panel.y, panel.w - side.w, panel.h);
        let header_h = if window.page == Page::Release { 132.0 } else { 56.0 }
            + header_button_rows(body.w, self.updates_header_buttons().len()) as f64 * 38.0;
        let header = Rect::new(body.x, body.y, body.w, header_h);
        let footer = Rect::new(body.x, body.bottom() - FOOTER, body.w, FOOTER);
        let has_detail = window.preview.is_none() && window.page != Page::Release;
        let detail = has_detail.then(|| Rect::new(body.x, footer.y - DETAIL, body.w, DETAIL));
        let list_bottom = detail.map_or(footer.y, |rect| rect.y);
        let list = Rect::new(body.x, header.bottom(), body.w, (list_bottom - header.bottom()).max(1.0));
        let rows = self.updates_rows();
        let content: f64 = rows.iter().map(Row::height).sum::<f64>() + 12.0;
        let max_scroll = (content - list.h).max(0.0);
        let mut top = list.y + 6.0 - window.scroll.clamp(0.0, max_scroll);
        let rows = rows
            .into_iter()
            .map(|row| {
                let rect = Rect::new(list.x + 10.0, top, list.w - 20.0, row.height());
                top += rect.h;
                (rect, row)
            })
            .collect();
        Frame { panel, side, nav, header, list, detail, footer, rows, max_scroll }
    }

    fn updates_clamp_scroll(&mut self) {
        let max = self.updates_frame().max_scroll;
        let window = &mut self.updates.window;
        window.scroll = window.scroll.clamp(0.0, max);
    }

    fn updates_select_first(&mut self) {
        let rows = self.updates_rows();
        let window = &mut self.updates.window;
        // The first row means nothing was chosen yet.
        let chosen = window.selected > 0 && rows.get(window.selected).is_some_and(Row::selectable);
        if chosen {
            return;
        }
        window.selected = rows.iter().position(Row::selectable).unwrap_or(0);
        if window.page != Page::Kernels {
            return;
        }
        // Apt offers many kernels: start at the running one, with the newer
        // ones above it within reach.
        let found = &self.updates.kernels;
        let running = rows.iter().position(
            |row| matches!(row, Row::Kernel(index) if found.get(*index).is_some_and(|kernel| kernel.running)),
        );
        if let Some(running) = running {
            let above: f64 = rows[..running].iter().map(Row::height).sum();
            self.updates.window.selected = running;
            self.updates.window.scroll = (above - 120.0).max(0.0);
            self.updates_clamp_scroll();
        }
    }

    /// Moves the highlight by rows, or to an end, and keeps it in view.
    fn updates_move_selection(&mut self, delta: isize, to_end: Option<bool>) {
        let rows = self.updates_rows();
        let choices: Vec<usize> = (0..rows.len()).filter(|index| rows[*index].selectable()).collect();
        if choices.is_empty() {
            self.updates.window.scroll += delta as f64 * 30.0;
            return self.updates_clamp_scroll();
        }
        let current = choices.iter().position(|index| *index >= self.updates.window.selected).unwrap_or(0);
        let next = match to_end {
            Some(false) => 0,
            Some(true) => choices.len() - 1,
            None => (current as isize + delta).clamp(0, choices.len() as isize - 1) as usize,
        };
        self.updates.window.selected = choices[next];
        let frame = self.updates_frame();
        if let Some((rect, _)) = frame.rows.get(choices[next]) {
            if rect.y < frame.list.y + 6.0 {
                self.updates.window.scroll -= frame.list.y + 6.0 - rect.y;
            } else if rect.bottom() > frame.list.bottom() - 6.0 {
                self.updates.window.scroll += rect.bottom() - (frame.list.bottom() - 6.0);
            }
        }
        self.updates_clamp_scroll();
    }

    fn updates_selected_row(&self) -> Option<Row> {
        self.updates_rows().get(self.updates.window.selected).cloned()
    }

    /// What the kernel queue would install and remove: the queued kernels
    /// with their headers and modules, and the ticked leftovers.
    fn updates_queue_plan(&self) -> (Vec<String>, Vec<String>) {
        let snapshot = &self.updates.snapshot;
        let window = &self.updates.window;
        let mut install: Vec<String> = Vec::new();
        let mut images = Vec::new();
        for (package, queued) in &window.queue {
            match queued {
                kernels::Queued::Remove => images.push(package.clone()),
                kernels::Queued::Install => {
                    let targets = self
                        .updates
                        .kernels
                        .iter()
                        .find(|kernel| kernel.package == *package)
                        .map(kernels::install_targets)
                        .unwrap_or_default();
                    install.extend(targets.into_iter().filter(|name| !install.contains(name)).collect::<Vec<_>>());
                }
            }
        }
        let mut remove = kernels::removal(&snapshot.packages, &snapshot.running, &images).unwrap_or_default();
        let headers = if window.headers { kernels::unused_headers(&snapshot.packages) } else { Vec::new() };
        let support = if window.support { kernels::unused_support(&snapshot.packages) } else { Vec::new() };
        for package in headers.into_iter().chain(support) {
            if !remove.contains(&package.name) {
                remove.push(package.name.clone());
            }
        }
        (install, remove)
    }

    /// Queues what clean-up suggests: old kernels and leftovers.
    pub(crate) fn updates_queue_suggested(&mut self) {
        let snapshot = &self.updates.snapshot;
        let old: BTreeMap<String, kernels::Queued> = kernels::removable(&self.updates.kernels)
            .iter()
            .map(|kernel| (kernel.package.clone(), kernels::Queued::Remove))
            .collect();
        let headers = !kernels::unused_headers(&snapshot.packages).is_empty();
        let support = !kernels::unused_support(&snapshot.packages).is_empty();
        let window = &mut self.updates.window;
        window.queue = old;
        window.headers = headers;
        window.support = support;
        window.preview = None;
        self.updates_changed();
    }

    /// Switches between kerntop's recommended view and every variant. The
    /// long list of variants starts with only the installed series unfolded.
    fn updates_toggle_variants(&mut self) {
        let all = !self.updates.window.all_variants;
        self.updates.window.all_variants = all;
        self.updates_refresh_kernels();
        let mut folded = BTreeSet::new();
        if all {
            let found = &self.updates.kernels;
            let installed: BTreeSet<String> =
                found.iter().filter(|kernel| kernel.installed).map(kernels::Kernel::series).collect();
            folded = found.iter().map(kernels::Kernel::series).filter(|series| !installed.contains(series)).collect();
        }
        let window = &mut self.updates.window;
        window.folded = folded;
        window.selected = 0;
        window.scroll = 0.0;
        self.updates_select_first();
    }

    /// What the ticks choose: how many, how much to download, the APT
    /// names (none when every APT update is ticked and none is skipped) and the
    /// tasks.
    fn updates_ticked(&self) -> (usize, u64, Vec<String>, Vec<Task>) {
        let window = &self.updates.window;
        let pending = &self.updates.snapshot.updates;
        let ticked: Vec<&apt::Update> = pending.iter().filter(|update| window.ticked(update)).collect();
        let bytes = ticked.iter().map(|update| update.size).sum();
        // A full upgrade would install skipped versions too.
        let all_apt = !self.updates.skips_apt()
            && pending.iter().filter(|update| update.source == apt::Source::Apt).all(|update| window.ticked(update));
        let names = if all_apt {
            Vec::new()
        } else {
            ticked.iter().filter(|update| update.source == apt::Source::Apt).map(|update| update.name.clone()).collect()
        };
        (ticked.len(), bytes, names, super::install_tasks(&ticked, all_apt))
    }

    /// The updates of the highlighted row: a group's, or a muted one with
    /// whether it is held.
    fn updates_selected_updates(&self) -> (Vec<apt::Update>, Option<bool>) {
        let updates = &self.updates;
        match self.updates_selected_row() {
            Some(Row::Group(start, count)) => {
                (updates.snapshot.updates.iter().skip(start).take(count).cloned().collect(), None)
            }
            Some(Row::Muted(index)) => match updates.muted(index) {
                Some((update, held)) => (vec![update.clone()], Some(held)),
                None => (Vec::new(), None),
            },
            _ => (Vec::new(), None),
        }
    }

    /// Opens the release notes of an update.
    pub(crate) fn updates_show_notes(&mut self, name: &str) -> Result<(), String> {
        let update = self.updates.find(name)?.clone();
        self.updates_show_notes_of(update);
        Ok(())
    }

    fn updates_show_notes_of(&mut self, update: apt::Update) {
        let window = &mut self.updates.window;
        window.sheet = Sheet::Notes;
        window.preview = Some(String::new());
        window.scroll = 0.0;
        window.preview_request = window.preview_request.wrapping_add(1);
        let request = window.preview_request;
        self.updates_request_notes(request, update);
    }

    fn updates_window_activate(&mut self, hit: Hit) {
        let result = match hit {
            Hit::Move => {
                if let Some(client) = &self.updates.window.client {
                    client.start_move();
                }
                Ok(())
            }
            Hit::Page(page) => {
                let window = &mut self.updates.window;
                window.page = page;
                window.scroll = 0.0;
                window.selected = 0;
                window.preview = None;
                self.updates_select_first();
                Ok(())
            }
            Hit::Refresh => self.updates_start(Task::Refresh),
            Hit::Select(row) => {
                self.updates.window.selected = row;
                Ok(())
            }
            Hit::Tick(row) => {
                if let Some(Row::Group(start, count)) = self.updates_rows().get(row).cloned() {
                    let members: Vec<apt::Update> =
                        self.updates.snapshot.updates.iter().skip(start).take(count).cloned().collect();
                    let window = &mut self.updates.window;
                    // A group partly ticked becomes wholly ticked.
                    let tick = !members.iter().all(|update| window.ticked(update));
                    for update in &members {
                        window.toggle(update, tick);
                    }
                    window.selected = row;
                }
                Ok(())
            }
            Hit::Notes => {
                if let Some(update) = self.updates_selected_updates().0.into_iter().next() {
                    self.updates_show_notes_of(update);
                }
                Ok(())
            }
            Hit::Skip(skip) => {
                let (chosen, _) = self.updates_selected_updates();
                chosen.iter().try_for_each(|update| self.updates_skip_update(update, skip))
            }
            Hit::Hold(hold) => {
                let (chosen, _) = self.updates_selected_updates();
                let names: Vec<String> = chosen
                    .iter()
                    .filter(|update| update.source == apt::Source::Apt)
                    .map(|update| update.name.clone())
                    .collect();
                if names.is_empty() { Ok(()) } else { self.updates_hold(names, hold) }
            }
            Hit::Automatic(enable) => self.updates_start(Task::Automatic(enable)),
            Hit::TestMirrors => {
                self.updates_test_mirrors();
                Ok(())
            }
            // While a test runs the comparison is incomplete, so no mirror
            // is chosen from it, by pointer or key.
            Hit::UseMirror(_) if self.updates.mirrors.testing => Ok(()),
            Hit::UseMirror(index) => match self.updates.mirrors.candidates.get(index) {
                Some(mirror) if mirror.error.is_some() => Err("That mirror cannot be reached.".into()),
                Some(mirror) => {
                    let url = mirror.url.clone();
                    self.updates_start(Task::Mirror(url))
                }
                None => Ok(()),
            },
            Hit::Kernel(index) => self.updates_kernel_action(index),
            Hit::Fold(row) => {
                if let Some(Row::Series(series, ..)) = self.updates_rows().get(row).cloned() {
                    let folded = &mut self.updates.window.folded;
                    if !folded.remove(&series) {
                        folded.insert(series);
                    }
                    self.updates.window.selected = row;
                }
                Ok(())
            }
            Hit::Leftovers(kind) => {
                let window = &mut self.updates.window;
                match kind {
                    Leftover::Headers => window.headers = !window.headers,
                    Leftover::Support => window.support = !window.support,
                }
                window.preview = None;
                Ok(())
            }
            Hit::Variants => {
                self.updates_toggle_variants();
                Ok(())
            }
            Hit::Purge => {
                self.updates.window.purge = !self.updates.window.purge;
                self.updates.window.preview = None;
                Ok(())
            }
            Hit::Clear => {
                let window = &mut self.updates.window;
                window.queue.clear();
                window.headers = false;
                window.support = false;
                window.preview = None;
                Ok(())
            }
            Hit::Install => {
                if self.updates.window.preview.as_ref().is_some_and(String::is_empty) {
                    return;
                }
                let (count, _, _, tasks) = self.updates_ticked();
                // Every APT update is ticked: a full upgrade, or with a skip
                // the install that keeps it, which can take packages away too.
                let full = matches!(tasks.first(), Some(Task::Install(_)))
                    && self
                        .updates
                        .snapshot
                        .updates
                        .iter()
                        .filter(|update| update.source == apt::Source::Apt)
                        .all(|update| self.updates.window.ticked(update));
                if count == 0 {
                    Err("Tick at least one update.".into())
                } else if self.updates.window.preview.is_none() && full && self.updates.snapshot.plan.removes() {
                    // Never take packages away unseen.
                    self.updates_open_sheet(Sheet::Install)
                } else {
                    let restart =
                        self.updates.window.preview.is_some() && self.updates.window.sheet == Sheet::InstallRestart;
                    self.updates_start_all(tasks).map(|()| {
                        if restart {
                            self.updates.after = Some(crate::power::PowerAction::Restart);
                        }
                    })
                }
            }
            Hit::Sheet(sheet) => self.updates_open_sheet(sheet),
            Hit::Back => {
                self.updates.window.preview = None;
                self.updates.window.scroll = 0.0;
                Ok(())
            }
            Hit::Run => match self.updates.window.sheet {
                _ if self.updates.window.preview.as_ref().is_none_or(String::is_empty) => Ok(()),
                Sheet::Queue => return self.updates_window_activate(Hit::Apply),
                Sheet::Install | Sheet::InstallRestart => return self.updates_window_activate(Hit::Install),
                Sheet::Autoremove => self.updates_start(Task::Autoremove),
                Sheet::Repair => self.updates_start(Task::Repair),
                Sheet::Log | Sheet::Notes => Ok(()),
            },
            Hit::Dismiss => {
                self.updates.outcome = None;
                self.updates.failed = false;
                Ok(())
            }
            Hit::Security => self.updates_install(true),
            Hit::Suggest => {
                self.updates_queue_suggested();
                Ok(())
            }
            Hit::Preview => {
                if self.updates.window.preview.take().is_some() {
                    Ok(())
                } else {
                    self.updates_open_sheet(Sheet::Queue)
                }
            }
            Hit::Apply => {
                let (install, remove) = self.updates_queue_plan();
                if install.is_empty() && remove.is_empty() {
                    Err("Queue a kernel first.".into())
                } else {
                    self.updates_start(Task::Kernels { install, remove, purge: self.updates.window.purge })
                }
            }
            Hit::Upgrade => self.updates_upgrade_release(),
            Hit::Copy if self.updates.window.preview.is_some() => {
                let text = self.updates.window.preview.clone().unwrap_or_default();
                self.copy_text(text);
                Ok(())
            }
            Hit::Copy | Hit::Doctor => {
                if let Some(Row::Entry(index)) = self.updates_selected_row()
                    && let Some(entry) = self.updates.snapshot.history.get(index).cloned()
                {
                    let changes: Vec<String> = entry
                        .upgraded
                        .iter()
                        .chain(&entry.installed)
                        .chain(&entry.removed)
                        .map(|change| format!("{} {}", change.name, change.version))
                        .collect();
                    if hit == Hit::Copy {
                        self.copy_text(changes.join("\n"));
                    } else {
                        self.ask_doctor(&format!(
                            "On {} apt ran `{}` and changed these packages: {}. What changed in them, and could any of it cause problems on this desktop?",
                            entry.date,
                            entry.command,
                            changes.join(", ")
                        ));
                    }
                }
                Ok(())
            }
        };
        if let Err(error) = result {
            self.updates.outcome = Some(Err(error));
        }
        self.updates_clamp_scroll();
        self.updates_changed();
    }

    /// Shows a sheet in place of the list: what `apt-get` says a transaction
    /// would do, asked from the worker, or the last task's log.
    fn updates_open_sheet(&mut self, sheet: Sheet) -> Result<(), String> {
        let args = match sheet {
            Sheet::Queue => {
                let (install, remove) = self.updates_queue_plan();
                apt::transaction_args(&install, &remove, self.updates.window.purge, true)?
            }
            Sheet::Install | Sheet::InstallRestart => {
                let (count, _, _, tasks) = self.updates_ticked();
                if count == 0 {
                    return Err("Tick at least one update.".into());
                }
                let task = tasks
                    .into_iter()
                    .find(|task| matches!(task, Task::Install(_) | Task::InstallKeeping { .. }))
                    .ok_or("Only APT updates can be previewed.")?;
                let task = self.updates_constrained_task(task)?;
                let command = task.commands()?.remove(0);
                let mut args: Vec<String> = command.into_iter().skip(1).collect();
                args.insert(0, "-s".into());
                args
            }
            Sheet::Notes => return Err("Choose an update for its release notes.".into()),
            Sheet::Autoremove => apt::autoremove_args(true),
            Sheet::Repair => apt::repair_args(true),
            Sheet::Log => {
                let text = self.updates.log().map(|log| log.text.clone()).ok_or("No task has run yet.")?;
                let window = &mut self.updates.window;
                window.sheet = sheet;
                window.preview = Some(if text.trim().is_empty() { "The task printed nothing.".into() } else { text });
                window.scroll = 0.0;
                return Ok(());
            }
        };
        let window = &mut self.updates.window;
        window.sheet = sheet;
        window.preview = Some(String::new());
        window.scroll = 0.0;
        window.preview_request = window.preview_request.wrapping_add(1);
        let request = window.preview_request;
        self.updates_preview(request, args);
        Ok(())
    }

    /// Installs every update from outside the window. When that would take
    /// packages away, the window opens on what it does instead.
    pub(crate) fn updates_install_reviewed(&mut self) -> Result<(), String> {
        if !self.updates.snapshot.plan.removes() || self.updates.snapshot.updates.is_empty() {
            return self.updates_install(false);
        }
        self.updates_review_install(false)
    }

    pub(super) fn updates_review_install(&mut self, restart: bool) -> Result<(), String> {
        self.open_updates(Page::Available);
        self.updates.window.skipped.clear();
        if restart {
            self.updates.window.firmware = self
                .updates
                .snapshot
                .updates
                .iter()
                .filter(|update| update.source == apt::Source::Firmware)
                .map(apt::Update::key)
                .collect();
        }
        self.updates_open_sheet(if restart { Sheet::InstallRestart } else { Sheet::Install })
    }

    /// A kernel's button: queues an offered one to install or an installed
    /// one to remove, or takes it off the queue. The running kernel has none.
    fn updates_kernel_action(&mut self, index: usize) -> Result<(), String> {
        let Some(kernel) = self.updates.kernels.get(index) else { return Ok(()) };
        if kernel.running {
            return Err("The running kernel cannot be removed.".into());
        }
        let queued = if kernel.installed { kernels::Queued::Remove } else { kernels::Queued::Install };
        let package = kernel.package.clone();
        let queue = &mut self.updates.window.queue;
        if queue.remove(&package).is_none() {
            queue.insert(package, queued);
        }
        self.updates.window.preview = None;
        Ok(())
    }

    /// kerntop's keys on the Kernels page. Returns whether the key was one.
    fn updates_kernel_key(&mut self, sym: u32) -> bool {
        let selected = self.updates_selected_row();
        let kernel = match &selected {
            Some(Row::Kernel(index)) => self.updates.kernels.get(*index).map(|kernel| (*index, kernel.installed)),
            _ => None,
        };
        let busy = self.updates.busy();
        match (sym, kernel) {
            (keysyms::KEY_a, _) => self.updates_window_activate(Hit::Variants),
            (keysyms::KEY_p, _) => self.updates_window_activate(Hit::Preview),
            (keysyms::KEY_c, _) => self.updates_window_activate(Hit::Clear),
            (keysyms::KEY_r, _) if !busy => self.updates_window_activate(Hit::Refresh),
            (keysyms::KEY_u, _) if !busy => {
                let snapshot = &self.updates.snapshot;
                let headers = !kernels::unused_headers(&snapshot.packages).is_empty();
                let support = !kernels::unused_support(&snapshot.packages).is_empty();
                let window = &mut self.updates.window;
                let all = window.headers == headers && window.support == support;
                window.headers = headers && !all;
                window.support = support && !all;
                window.preview = None;
            }
            (keysyms::KEY_q, Some((index, _)))
            | (keysyms::KEY_i, Some((index, false)))
            | (keysyms::KEY_d, Some((index, true)))
                if !busy =>
            {
                self.updates_window_activate(Hit::Kernel(index));
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn updates_window_key(&mut self, sym: u32, mods: Mods) {
        if !self.updates.window.open {
            return;
        }
        let plain = !mods.ctrl && !mods.alt && !mods.logo;
        if plain && self.updates.window.page == Page::Kernels && self.updates_kernel_key(sym) {
            return self.updates_changed();
        }
        let selected = self.updates.window.selected;
        match sym {
            keysyms::KEY_Escape if self.updates.window.preview.is_some() => self.updates.window.preview = None,
            keysyms::KEY_Escape => return self.close_updates(),
            keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => {
                let back = mods.shift || sym == keysyms::KEY_ISO_Left_Tab;
                let current = Page::ALL.iter().position(|page| *page == self.updates.window.page).unwrap_or(0);
                let next = (current + if back { Page::ALL.len() - 1 } else { 1 }) % Page::ALL.len();
                self.updates_window_activate(Hit::Page(Page::ALL[next]));
            }
            keysyms::KEY_Up => self.updates_move_selection(-1, None),
            keysyms::KEY_Down => self.updates_move_selection(1, None),
            keysyms::KEY_Page_Up => self.updates_move_selection(-8, None),
            keysyms::KEY_Page_Down => self.updates_move_selection(8, None),
            keysyms::KEY_Home => self.updates_move_selection(0, Some(false)),
            keysyms::KEY_End => self.updates_move_selection(0, Some(true)),
            keysyms::KEY_space => match self.updates_selected_row() {
                Some(Row::Group(..)) => self.updates_window_activate(Hit::Tick(selected)),
                Some(Row::Automatic) if !self.updates.busy() => {
                    let enabled = self.updates.automatic.enabled;
                    self.updates_window_activate(Hit::Automatic(!enabled));
                }
                Some(Row::Kernel(index)) => self.updates_window_activate(Hit::Kernel(index)),
                Some(Row::Leftovers(kind)) => self.updates_window_activate(Hit::Leftovers(kind)),
                Some(Row::TestMirrors) if !self.updates.busy() => self.updates_window_activate(Hit::TestMirrors),
                Some(Row::Series(..)) => self.updates_window_activate(Hit::Fold(selected)),
                Some(Row::Unneeded) if !self.updates.busy() => {
                    self.updates_window_activate(Hit::Sheet(Sheet::Autoremove));
                }
                _ => {}
            },
            keysyms::KEY_Return | keysyms::KEY_KP_Enter
                if matches!(self.updates_selected_row(), Some(Row::Series(..))) =>
            {
                self.updates_window_activate(Hit::Fold(selected));
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter
                if !self.updates.busy() && self.updates.window.preview.is_some() =>
            {
                self.updates_window_activate(Hit::Run);
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter
                if !self.updates.busy() && matches!(self.updates_selected_row(), Some(Row::Unneeded)) =>
            {
                self.updates_window_activate(Hit::Sheet(Sheet::Autoremove));
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if !self.updates.busy() => match self.updates.window.page {
                Page::Available => self.updates_window_activate(Hit::Install),
                Page::Kernels => self.updates_window_activate(Hit::Apply),
                Page::Release => self.updates_window_activate(Hit::Upgrade),
                Page::Settings => match self.updates_selected_row() {
                    Some(Row::Mirror(index)) => self.updates_window_activate(Hit::UseMirror(index)),
                    Some(Row::TestMirrors) => self.updates_window_activate(Hit::TestMirrors),
                    Some(Row::Automatic) => {
                        let enabled = self.updates.automatic.enabled;
                        self.updates_window_activate(Hit::Automatic(!enabled));
                    }
                    _ => {}
                },
                Page::History => {}
            },
            keysyms::KEY_n if plain && self.updates.window.preview.is_none() => {
                if matches!(self.updates_selected_row(), Some(Row::Group(..) | Row::Muted(_))) {
                    self.updates_window_activate(Hit::Notes);
                }
            }
            keysyms::KEY_F5 if !self.updates.busy() => self.updates_window_activate(Hit::Refresh),
            _ => {}
        }
        self.updates_changed();
    }

    /// The footer's buttons, rightmost first, and whether each is the main one.
    fn updates_footer_buttons(&self) -> Vec<(String, Hit, bool)> {
        let updates = &self.updates;
        if updates.busy() {
            return Vec::new();
        }
        let back = ("Back".to_string(), Hit::Back, false);
        match (updates.window.preview.as_ref(), updates.window.sheet) {
            (None, _) | (Some(_), Sheet::Queue) => {}
            (Some(preview), Sheet::Install | Sheet::InstallRestart) => {
                if preview.is_empty() {
                    return vec![back];
                }
                let (count, ..) = self.updates_ticked();
                let label = if updates.window.sheet == Sheet::InstallRestart {
                    format!("Install {count} and restart")
                } else {
                    format!("Install {count}")
                };
                return vec![(label, Hit::Install, true), back];
            }
            (Some(preview), Sheet::Autoremove) => {
                let count = apt::parse_plan(preview).removed.len();
                return if count == 0 { vec![back] } else { vec![(format!("Remove {count}"), Hit::Run, true), back] };
            }
            (Some(_), Sheet::Repair) => return vec![("Repair".into(), Hit::Run, true), back],
            (Some(_), Sheet::Log | Sheet::Notes) => return vec![back, ("Copy".into(), Hit::Copy, false)],
        }
        // A task that failed: what went wrong comes before anything else.
        if updates.failed() {
            let mut buttons = Vec::new();
            if updates.repairable() {
                buttons.push(("Repair".to_string(), Hit::Sheet(Sheet::Repair), true));
            }
            if updates.log().is_some() {
                buttons.push(("Show log".to_string(), Hit::Sheet(Sheet::Log), false));
            }
            buttons.push(("Dismiss".to_string(), Hit::Dismiss, false));
            return buttons;
        }
        match updates.window.page {
            Page::Available => {
                let (count, ..) = self.updates_ticked();
                if updates.snapshot.updates.is_empty() {
                    let mut buttons = Vec::new();
                    if updates.repairable() {
                        buttons.push(("Repair".to_string(), Hit::Sheet(Sheet::Repair), true));
                    }
                    buttons.push(("Check now".into(), Hit::Refresh, false));
                    return buttons;
                }
                let totals = updates.snapshot.totals();
                let mut buttons = vec![(format!("Install {count}"), Hit::Install, true)];
                if totals.security > 0 && totals.security < totals.count {
                    buttons.push(("Security only".into(), Hit::Security, false));
                }
                if count > 0 {
                    buttons.push(("Preview".into(), Hit::Sheet(Sheet::Install), false));
                }
                buttons
            }
            Page::Kernels => {
                let (install, remove) = self.updates_queue_plan();
                let mut buttons = Vec::new();
                if !install.is_empty() || !remove.is_empty() {
                    buttons.push(("Apply queue".to_string(), Hit::Apply, true));
                    let label = if updates.window.preview.is_some() { "Back" } else { "Preview" };
                    buttons.push((label.to_string(), Hit::Preview, false));
                    buttons.push(("Clear".to_string(), Hit::Clear, false));
                } else if updates.cleanup_summary().is_some() {
                    buttons.push(("Queue old kernels".to_string(), Hit::Suggest, false));
                }
                buttons
            }
            Page::Release => {
                if updates.release().is_some() {
                    vec![("Upgrade".into(), Hit::Upgrade, true)]
                } else {
                    Vec::new()
                }
            }
            Page::History if updates.log().is_some() => vec![("Last task log".into(), Hit::Sheet(Sheet::Log), false)],
            Page::History | Page::Settings => Vec::new(),
        }
    }

    fn updates_detail_buttons(&self) -> Vec<(&'static str, Hit)> {
        if self.updates.window.page == Page::Available && !self.updates.busy() {
            let (chosen, muted) = self.updates_selected_updates();
            let Some(first) = chosen.first() else { return Vec::new() };
            let apt = first.source == apt::Source::Apt;
            // Rightmost first.
            return match muted {
                Some(true) => vec![("Release", Hit::Hold(false)), ("Notes", Hit::Notes)],
                Some(false) => vec![("Unskip", Hit::Skip(false)), ("Notes", Hit::Notes)],
                None if apt => {
                    vec![("Hold", Hit::Hold(true)), ("Skip version", Hit::Skip(true)), ("Notes", Hit::Notes)]
                }
                None => vec![("Skip version", Hit::Skip(true)), ("Notes", Hit::Notes)],
            };
        }
        if self.updates.window.page != Page::History || !matches!(self.updates_selected_row(), Some(Row::Entry(_))) {
            return Vec::new();
        }
        let mut buttons = Vec::new();
        if self.shell().doctor() {
            buttons.push(("Ask Doctor", Hit::Doctor));
        }
        buttons.push(("Copy list", Hit::Copy));
        buttons
    }

    /// A release check that another page settles.
    fn updates_check_page(check: &release::Check) -> Option<(&'static str, Page)> {
        if check.state != release::State::Attention {
            None
        } else if check.title.starts_with("Install current") {
            Some(("Show", Page::Available))
        } else if check.title.starts_with("Not enough disk") {
            Some(("Kernels", Page::Kernels))
        } else {
            None
        }
    }

    fn updates_window_controls(&self, frame: &Frame) -> Vec<(Rect, Hit)> {
        let updates = &self.updates;
        let mut controls: Vec<(Rect, Hit)> = frame.nav.iter().map(|(rect, page)| (*rect, Hit::Page(*page))).collect();
        let header_buttons = self.updates_header_buttons();
        let count = header_buttons.len();
        for (slot, (_, hit)) in header_buttons.into_iter().enumerate() {
            controls.push((Self::updates_header_button(frame.header, slot, count), hit));
        }
        let footer_buttons = self.updates_footer_buttons();
        let count = footer_buttons.len();
        for (slot, (_, hit, _)) in footer_buttons.into_iter().enumerate() {
            controls.push((footer_button(frame.footer, slot, count, 124.0), hit));
        }
        if let Some(detail) = frame.detail {
            let detail_buttons = self.updates_detail_buttons();
            let count = detail_buttons.len();
            for (slot, (_, hit)) in detail_buttons.into_iter().enumerate() {
                controls.push((footer_button(detail, slot, count, 104.0), hit));
            }
        }
        let checks = self.updates_release_checks();
        let found = &updates.kernels;
        for (index, (rect, row)) in frame.rows.iter().enumerate() {
            if rect.bottom() <= frame.list.y || rect.y >= frame.list.bottom() {
                continue;
            }
            let Some(visible) = crate::settings::intersect(*rect, frame.list) else { continue };
            match row {
                Row::Group(..) if !updates.busy() => {
                    controls.push((Rect::new(rect.x, visible.y, 32.0, visible.h), Hit::Tick(index)));
                }
                Row::Automatic if !updates.busy() => {
                    controls.push((row_button(*rect), Hit::Automatic(!updates.automatic.enabled)));
                }
                Row::Mirror(mirror) if !updates.busy() && !updates.mirrors.testing => {
                    let usable = updates.mirrors.candidates.get(*mirror).is_some_and(|mirror| {
                        mirror.error.is_none() && Some(&mirror.url) != updates.mirrors.current.as_ref()
                    });
                    if usable {
                        controls.push((row_button(*rect), Hit::UseMirror(*mirror)));
                    }
                }
                Row::Kernel(kernel) if !updates.busy() => {
                    if found.get(*kernel).is_some_and(|kernel| !kernel.running) {
                        controls.push((row_button(*rect), Hit::Kernel(*kernel)));
                    }
                }
                Row::Leftovers(kind) if !updates.busy() => controls.push((row_button(*rect), Hit::Leftovers(*kind))),
                Row::TestMirrors if !updates.busy() => controls.push((row_button(*rect), Hit::TestMirrors)),
                Row::Unneeded if !updates.busy() => {
                    controls.push((row_button(*rect), Hit::Sheet(Sheet::Autoremove)));
                }
                Row::Series(..) => controls.push((visible, Hit::Fold(index))),
                Row::Check(check) => {
                    if let Some((_, page)) = checks.get(*check).and_then(Self::updates_check_page) {
                        controls.push((row_button(*rect), Hit::Page(page)));
                    }
                }
                _ => {}
            }
            if row.selectable() {
                controls.push((visible, Hit::Select(index)));
            }
        }
        controls
    }

    /// The header's buttons, rightmost first.
    fn updates_header_buttons(&self) -> Vec<(&'static str, Hit)> {
        let window = &self.updates.window;
        if self.updates.busy() || window.preview.is_some() {
            return Vec::new();
        }
        match window.page {
            Page::Available => vec![("Check", Hit::Refresh)],
            Page::Kernels => vec![
                ("Check", Hit::Refresh),
                (if window.all_variants { "Recommended" } else { "All variants" }, Hit::Variants),
                (if window.purge { "Purge: on" } else { "Purge: off" }, Hit::Purge),
            ],
            _ => Vec::new(),
        }
    }

    fn updates_header_button(header: Rect, slot: usize, count: usize) -> Rect {
        let wrapped = header_button_rows(header.w, count) > 0;
        let columns = if wrapped { header_button_columns(header.w, count) } else { count };
        let width = footer_button_width(header, columns, 104.0);
        let offset_y = if wrapped { 56.0 + (slot / columns) as f64 * 38.0 } else { 13.0 };
        Rect::new(
            header.right() - 16.0 - width - (slot % columns) as f64 * (width + 8.0),
            header.y + offset_y,
            width,
            30.0,
        )
    }

    fn updates_window_target(&self, point: Point) -> Option<PointerTarget> {
        let frame = self.updates_frame();
        if !frame.panel.contains(point) {
            return None;
        }
        if let Some((rect, hit)) =
            self.updates_window_controls(&frame).into_iter().find(|(rect, _)| rect.contains(point))
        {
            return Some(PointerTarget::button(rect, hit));
        }
        let title = Rect::new(frame.panel.x, frame.panel.y, frame.panel.w, 56.0);
        title.contains(point).then(|| PointerTarget::new(title, Hit::Move, CursorIcon::Grab))
    }

    fn updates_window_button(&mut self, pressed: bool) {
        let point = Point::new(self.updates.window.pointer.0, self.updates.window.pointer.1);
        let target = self.updates_window_target(point);
        match PointerTarget::button_event(&mut self.updates.window.pressed, target, pressed) {
            PointerEvent::Press(target) | PointerEvent::Release(target) => target.activate(self, point),
            _ => {}
        }
    }

    fn updates_window_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement> {
        let theme = &self.theme;
        let updates = &self.updates;
        let window = &updates.window;
        let snapshot = &updates.snapshot;
        let frame = self.updates_frame();
        let pointer = Point::new(window.pointer.0, window.pointer.1);
        let size = theme.font_size;
        let muted = Style::new(size * 0.86, theme.muted);
        let body = Style::new(size * 0.94, theme.text);
        let mut painter = Painter::new(self, renderer);
        painter.shape(
            frame.panel,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background, theme.radius) },
        );
        painter.fill(
            Rect::new(frame.side.x + 1.0, frame.side.y + 1.0, frame.side.w - 1.0, frame.side.h - 2.0),
            theme.surface,
            0.0,
        );
        painter.fill(Rect::new(frame.side.right(), frame.panel.y, 1.0, frame.panel.h), theme.border, 0.0);

        // The sidebar: the pages with what waits on each, then the system.
        painter.glyph(ARROW_UP, Rect::new(frame.side.x + 16.0, frame.side.y + 16.0, 22.0, 24.0), 18, theme.accent);
        painter.text(
            "Updates",
            Rect::new(frame.side.x + 46.0, frame.side.y + 16.0, frame.side.w - 54.0, 24.0),
            Style::new(size * 1.15, theme.text).bold(),
        );
        let totals = snapshot.totals();
        for (rect, page) in &frame.nav {
            let current = *page == window.page;
            if current {
                painter.shape(
                    *rect,
                    Shape {
                        radius: theme.radius,
                        fill: (theme.accent.alpha(0.20), theme.accent_end.alpha(0.06)),
                        ..Default::default()
                    },
                );
            } else if rect.contains(pointer) {
                painter.fill(*rect, theme.surface_raised, theme.radius);
            }
            let style = if current { body.bold() } else { body.colour(theme.text.alpha(0.8)) };
            painter.text(page.title(), Rect::new(rect.x + 12.0, rect.y, rect.w - 24.0, rect.h), style);
            let badge = match page {
                Page::Available if totals.count > 0 => totals.count.to_string(),
                Page::Kernels => updates
                    .cleanup_summary()
                    .and_then(|summary| summary.rsplit(", ").next().map(str::to_string))
                    .unwrap_or_default(),
                Page::Release => updates.release().unwrap_or_default().to_string(),
                Page::Settings if updates.mirrors.warning().is_some() => "Mirror".to_string(),
                _ => String::new(),
            };
            if !badge.is_empty() {
                let warn = (*page == Page::Available && totals.security > 0) || *page == Page::Settings;
                let tint = if warn { theme.warning } else { theme.accent };
                text_right(
                    &mut painter,
                    &badge,
                    Rect::new(rect.x, rect.y, rect.w - 10.0, rect.h),
                    muted.colour(tint).bold(),
                );
            }
        }
        let system = [snapshot.system.pretty.clone(), format!("Kernel {}", snapshot.running), updates.checked()];
        for (index, line) in system.iter().enumerate() {
            let top = frame.side.bottom() - 70.0 + index as f64 * 18.0;
            painter.text(
                line,
                Rect::new(frame.side.x + 16.0, top, frame.side.w - 28.0, 18.0),
                Style::new(size * 0.78, theme.muted.alpha(0.85)),
            );
        }

        // The header.
        let header = frame.header;
        let (title, note) = match window.page {
            _ if window.preview.is_some() => (
                window.sheet.title().to_string(),
                match (window.sheet, updates.log()) {
                    (Sheet::Log, Some(log)) => {
                        format!("{} \u{b7} {}", log.title, if log.succeeded { "finished" } else { "failed" })
                    }
                    (Sheet::Log, None) => String::new(),
                    _ => "nothing has changed yet".to_string(),
                },
            ),
            Page::Available => (
                match (totals.count, totals.security) {
                    (0, _) if snapshot.error.is_some() => "Updates could not be listed".to_string(),
                    (0, _) => "Up to date".to_string(),
                    (1, 0) => "1 update".to_string(),
                    (count, 0) => format!("{count} updates"),
                    (count, security) => format!("{count} updates, {security} for security"),
                },
                String::new(),
            ),
            Page::Kernels => {
                let installed: u64 = snapshot.packages.iter().map(|package| package.size * 1024).sum();
                (
                    "Kernels".to_string(),
                    if window.all_variants {
                        format!("{} installed \u{b7} all variants", backend::human_size(installed))
                    } else {
                        format!("{} installed", backend::human_size(installed))
                    },
                )
            }
            Page::Release => ("Release".to_string(), format!("You're on {}", snapshot.system.pretty)),
            Page::History => ("History".to_string(), "from the apt log".to_string()),
            Page::Settings => (
                "Settings".to_string(),
                match updates.mirrors.test_progress() {
                    Some((done, total)) => format!("testing mirror {} of {total}", (done + 1).min(total)),
                    None if updates.mirrors.testing => "finding the mirrors near you".to_string(),
                    None => String::new(),
                },
            ),
        };
        let header_buttons = self.updates_header_buttons();
        let count = header_buttons.len();
        let room = if header_button_rows(header.w, count) > 0 {
            header.w - 36.0
        } else {
            footer_text_room(header, count, 104.0)
        };
        let (width, _) = painter.text(
            &title,
            Rect::new(header.x + 18.0, header.y, room.max(1.0), 56.0),
            Style::new(size * 1.12, theme.text).bold(),
        );
        painter.text(
            &note,
            Rect::new(header.x + 30.0 + width, header.y + 1.0, (room - width - 12.0).max(1.0), 56.0),
            muted,
        );
        for (slot, (label, _)) in header_buttons.into_iter().enumerate() {
            draw_button(&mut painter, Self::updates_header_button(header, slot, count), label, Look::Plain, pointer);
        }
        if window.page == Page::Release {
            let hero = Rect::new(header.x + 16.0, header.y + 56.0, header.w - 32.0, 64.0);
            let offered = updates.release();
            let tint = if offered.is_some() { theme.accent } else { theme.muted };
            painter.shape(
                hero,
                Shape {
                    radius: theme.radius + 2.0,
                    border_width: 1.0,
                    border: tint.alpha(0.45),
                    fill: (tint.alpha(0.12), theme.accent_end.alpha(if offered.is_some() { 0.05 } else { 0.0 })),
                    ..Default::default()
                },
            );
            let brand = snapshot.system.pretty.split_whitespace().next().unwrap_or("Release");
            let (headline, detail) = match updates.offer() {
                release::Offer::Release(name) => (
                    format!("{brand} {name} is available"),
                    "Takes about an hour. Your files and apps stay.".to_string(),
                ),
                release::Offer::None => {
                    ("No newer release".to_string(), format!("{} is the newest on this track.", snapshot.system.pretty))
                }
                release::Offer::Unsupported => (
                    "Release upgrades are not available here".to_string(),
                    "This system has no do-release-upgrade.".to_string(),
                ),
                release::Offer::Unknown => ("Looking for a newer release...".to_string(), String::new()),
            };
            painter.text(
                &headline,
                Rect::new(hero.x + 16.0, hero.y + 10.0, hero.w - 150.0, 24.0),
                Style::new(size * 1.1, theme.text).bold(),
            );
            painter.text(&detail, Rect::new(hero.x + 16.0, hero.y + 34.0, hero.w - 150.0, 20.0), muted);
            let track = match updates.track() {
                release::Track::Lts => "LTS releases only",
                release::Track::Normal => "Every release",
                release::Track::Never => "Offers switched off",
            };
            text_right(&mut painter, track, Rect::new(hero.x, hero.y, hero.w - 16.0, hero.h), muted);
        }
        painter.fill(Rect::new(header.x, header.bottom() - 1.0, header.w, 1.0), theme.border, 0.0);

        // The list.
        let found = &updates.kernels;
        let keep = kernels::fallback(found).map(|kernel| kernel.package.clone());
        let checks = self.updates_release_checks();
        painter.clipped(frame.list, |painter| {
            for (index, (rect, row)) in frame.rows.iter().enumerate() {
                let rect = *rect;
                if rect.bottom() <= frame.list.y || rect.y >= frame.list.bottom() {
                    continue;
                }
                if row.selectable() && index == window.selected {
                    painter.shape(
                        rect,
                        Shape {
                            radius: theme.radius,
                            fill: (theme.accent.alpha(0.18), theme.accent_end.alpha(0.05)),
                            ..Default::default()
                        },
                    );
                } else if row.selectable() && rect.contains(pointer) && frame.list.contains(pointer) {
                    painter.fill(rect, theme.text.alpha(0.04), theme.radius);
                }
                match row {
                    Row::Heading(title, note) => {
                        let line = Rect::new(rect.x + 8.0, rect.y + 8.0, rect.w - 16.0, rect.h - 8.0);
                        let (width, _) =
                            painter.text(&title.to_uppercase(), line, Style::new(size * 0.72, theme.muted).bold());
                        painter.text(
                            note,
                            Rect::new(line.x + width + 10.0, line.y, line.w - width - 10.0, line.h),
                            Style::new(size * 0.78, theme.muted.alpha(0.7)),
                        );
                    }
                    Row::Text(text) => {
                        painter.text_font(
                            text,
                            Rect::new(rect.x + 8.0, rect.y, rect.w - 16.0, rect.h),
                            Style::new(size * 0.84, theme.muted),
                            if window.preview.is_some() { "monospace" } else { &theme.font },
                        );
                    }
                    Row::Group(start, count) => {
                        let members: Vec<&apt::Update> = snapshot.updates.iter().skip(*start).take(*count).collect();
                        let Some(update) = members.first().copied() else { continue };
                        let ticked = members.iter().filter(|update| window.ticked(update)).count();
                        draw_tick(painter, tick_box(Rect::new(rect.x, rect.y, rect.w, 30.0)), ticked == members.len());
                        if ticked > 0 && ticked < members.len() {
                            painter.glyph(
                                glyph::MINUS,
                                tick_box(Rect::new(rect.x, rect.y, rect.w, 30.0)),
                                12,
                                theme.accent,
                            );
                        }
                        let (label, tint) = badge(update, theme);
                        draw_badge(painter, Rect::new(rect.x + 34.0, rect.y + 7.0, 62.0, 16.0), label, tint);
                        let name_w = ((rect.w - 190.0) * 0.42).max(80.0);
                        let title = if update.source == apt::Source::Apt { update.group() } else { update.label() };
                        let top = Rect::new(rect.x + 106.0, rect.y, name_w - 8.0, 30.0);
                        painter.text(title, top, body);
                        let versions = match &update.old {
                            Some(old) => format!("{old} \u{2192} {}", update.new),
                            None => format!("new \u{2192} {}", update.new),
                        };
                        painter.text_font(
                            &versions,
                            Rect::new(rect.x + 106.0 + name_w, rect.y, rect.w - 190.0 - name_w, 30.0),
                            Style::new(size * 0.8, theme.muted),
                            "monospace",
                        );
                        let total: u64 = members.iter().map(|update| update.size).sum();
                        let bytes = if total > 0 { backend::human_size(total) } else { "cached".into() };
                        text_right(painter, &bytes, Rect::new(rect.right() - 80.0, rect.y, 72.0, 30.0), muted);
                        // What it is, and the packages it brings.
                        let mut about = Vec::new();
                        if members.len() > 1 || update.name != title {
                            about
                                .push(members.iter().map(|update| update.name.as_str()).collect::<Vec<_>>().join(", "));
                        }
                        if !update.summary.is_empty() {
                            about.push(update.summary.clone());
                        }
                        painter.text(
                            &about.join(" \u{b7} "),
                            Rect::new(rect.x + 106.0, rect.y + 24.0, rect.w - 196.0, 18.0),
                            Style::new(size * 0.78, theme.muted.alpha(0.85)),
                        );
                    }
                    Row::Muted(index) => {
                        let Some((update, held)) = updates.muted(*index) else { continue };
                        painter.glyph(
                            if held { glyph::LOCK } else { glyph::MINUS },
                            tick_box(rect),
                            13,
                            theme.muted.alpha(0.7),
                        );
                        let faint = Style::new(size * 0.88, theme.muted.alpha(0.75));
                        let name_w = ((rect.w - 190.0) * 0.42).max(80.0);
                        painter.text(update.label(), Rect::new(rect.x + 106.0, rect.y, name_w - 8.0, rect.h), faint);
                        draw_badge(
                            painter,
                            Rect::new(rect.x + 34.0, rect.y + 7.0, 62.0, 16.0),
                            if held { "Held" } else { "Skipped" },
                            theme.muted,
                        );
                        let versions = format!("{} \u{2192} {}", update.old.as_deref().unwrap_or("new"), update.new);
                        painter.text_font(
                            &versions,
                            Rect::new(rect.x + 106.0 + name_w, rect.y, rect.w - 190.0 - name_w, rect.h),
                            Style::new(size * 0.8, theme.muted.alpha(0.7)),
                            "monospace",
                        );
                    }
                    Row::Automatic => {
                        let automatic = &updates.automatic;
                        let state = if automatic.enabled { "On" } else { "Off" };
                        painter.glyph(
                            if automatic.enabled { glyph::CHECK } else { glyph::MINUS },
                            Rect::new(rect.x + 6.0, rect.y + 6.0, 20.0, 20.0),
                            15,
                            if automatic.enabled { theme.accent } else { theme.muted },
                        );
                        painter.text(
                            &format!("{state}: installs updates by itself, once a day"),
                            Rect::new(rect.x + 34.0, rect.y + 5.0, rect.w - 130.0, 20.0),
                            body,
                        );
                        let covers = if automatic.covers.is_empty() {
                            "It is set to install nothing.".to_string()
                        } else {
                            format!("Covers {}", automatic.covers.join(", "))
                        };
                        painter.text(&covers, Rect::new(rect.x + 34.0, rect.y + 24.0, rect.w - 130.0, 18.0), muted);
                        if !updates.busy() {
                            let label = if automatic.enabled { "Turn off" } else { "Turn on" };
                            draw_button(painter, row_button(rect), label, Look::Plain, pointer);
                        }
                    }
                    Row::Mirror(index) => {
                        let mirrors = &updates.mirrors;
                        let Some(mirror) = mirrors.candidates.get(*index) else { continue };
                        let current = Some(&mirror.url) == mirrors.current.as_ref();
                        let ink = if mirror.stale(mirrors.reference) { theme.muted } else { theme.text };
                        let url_w = ((rect.w - 120.0) * 0.55).max(160.0);
                        painter.text_font(
                            &mirror.url,
                            Rect::new(rect.x + 10.0, rect.y, url_w, rect.h),
                            Style::new(size * 0.84, ink),
                            "monospace",
                        );
                        let tint = if mirror.stale(mirrors.reference) { theme.warning } else { theme.muted };
                        painter.text(
                            &super::describe_mirror(mirror, mirrors.reference),
                            Rect::new(rect.x + 20.0 + url_w, rect.y, rect.w - url_w - 120.0, rect.h),
                            muted.colour(tint),
                        );
                        if current {
                            text_right(
                                painter,
                                "in use",
                                Rect::new(rect.x, rect.y, rect.w - 20.0, rect.h),
                                muted.colour(theme.accent),
                            );
                        } else if mirror.error.is_none() && !updates.busy() && !mirrors.testing {
                            draw_button(painter, row_button(rect), "Use", Look::Plain, pointer);
                        }
                    }
                    Row::Waiting(index) => {
                        let Some(url) = updates.mirrors.waiting.get(*index) else { continue };
                        let url_w = ((rect.w - 120.0) * 0.55).max(160.0);
                        painter.text_font(
                            url,
                            Rect::new(rect.x + 10.0, rect.y, url_w, rect.h),
                            Style::new(size * 0.84, theme.muted),
                            "monospace",
                        );
                        let (state, tint) = if *index == 0 {
                            ("testing\u{2026}", theme.accent)
                        } else {
                            ("waiting", theme.muted.alpha(0.7))
                        };
                        painter.text(
                            state,
                            Rect::new(rect.x + 20.0 + url_w, rect.y, rect.w - url_w - 120.0, rect.h),
                            muted.colour(tint),
                        );
                    }
                    Row::Warning(text) => {
                        painter.glyph(
                            glyph::ALERT,
                            Rect::new(rect.x + 6.0, rect.y + 5.0, 20.0, 20.0),
                            15,
                            theme.warning,
                        );
                        painter.text(
                            text,
                            Rect::new(rect.x + 34.0, rect.y, rect.w - 40.0, rect.h),
                            body.colour(theme.warning),
                        );
                    }
                    Row::Series(series, installed, available) => {
                        let folded = window.folded.contains(series);
                        let chevron = if folded { glyph::CHEVRON_RIGHT } else { glyph::CHEVRON_DOWN };
                        painter.glyph(
                            chevron,
                            Rect::new(rect.x + 6.0, rect.y + 6.0, 20.0, rect.h - 6.0),
                            13,
                            theme.muted,
                        );
                        let line = Rect::new(rect.x + 30.0, rect.y + 6.0, rect.w - 38.0, rect.h - 6.0);
                        let (width, _) = painter.text(
                            &format!("LINUX {series}"),
                            line,
                            Style::new(size * 0.72, if *installed > 0 { theme.text } else { theme.muted }).bold(),
                        );
                        let note = match (*installed, *available) {
                            (0, available) => format!("{available} available"),
                            (installed, 0) => format!("{installed} installed"),
                            (installed, available) => format!("{installed} installed \u{b7} {available} available"),
                        };
                        painter.text(
                            &note,
                            Rect::new(line.x + width + 10.0, line.y, line.w - width - 10.0, line.h),
                            Style::new(size * 0.78, theme.muted.alpha(0.7)),
                        );
                    }
                    Row::Kernel(kernel) => {
                        let Some(kernel) = found.get(*kernel) else { continue };
                        let queued = window.queue.get(&kernel.package).copied();
                        let mark = tick_box(rect);
                        if kernel.running {
                            painter.glyph(glyph::LOCK, mark, 14, theme.accent);
                        } else if kernel.installed || queued.is_some() {
                            draw_tick(painter, mark, queued.is_some());
                        } else {
                            painter.glyph(glyph::PLUS, mark, 14, theme.muted);
                        }
                        let name_w = ((rect.w - 220.0) * 0.5).max(120.0);
                        let ink = if queued == Some(kernels::Queued::Remove) { theme.muted } else { theme.text };
                        let name = if kernel.unsigned() {
                            format!("{} (unsigned)", kernel.identifier)
                        } else {
                            kernel.identifier.clone()
                        };
                        painter.text_font(
                            &name,
                            Rect::new(rect.x + 34.0, rect.y, name_w, rect.h),
                            Style::new(size * 0.88, ink),
                            "monospace",
                        );
                        let (state, tint) = match (kernel.running, kernel.installed, queued) {
                            (true, ..) => ("running \u{b7} protected", theme.accent),
                            (_, false, Some(_)) => ("queued to install", theme.accent),
                            (_, false, None) => ("available", theme.accent_end),
                            (_, true, Some(_)) => ("queued to remove", theme.danger),
                            (_, true, None) if keep.as_deref() == Some(kernel.package.as_str()) => {
                                ("installed \u{b7} fallback", theme.muted)
                            }
                            (_, true, None) => ("installed", theme.muted),
                        };
                        painter.text(
                            state,
                            Rect::new(rect.x + 44.0 + name_w, rect.y, rect.w - 250.0 - name_w, rect.h),
                            muted.colour(tint),
                        );
                        if kernel.installed {
                            let bytes = backend::human_size(kernel.size * 1024);
                            text_right(painter, &bytes, Rect::new(rect.right() - 170.0, rect.y, 72.0, rect.h), muted);
                        }
                        if !kernel.running && !updates.busy() {
                            let label = match (kernel.installed, queued.is_some()) {
                                (false, false) => "Install",
                                (false, true) => "Skip",
                                (true, false) => "Remove",
                                (true, true) => "Keep",
                            };
                            draw_button(painter, row_button(rect), label, Look::Plain, pointer);
                        }
                    }
                    Row::Leftovers(kind) => {
                        let (title, ticked, packages) = match kind {
                            Leftover::Headers => {
                                ("Development headers", window.headers, kernels::unused_headers(&snapshot.packages))
                            }
                            Leftover::Support => {
                                ("Modules and tools", window.support, kernels::unused_support(&snapshot.packages))
                            }
                        };
                        draw_tick(painter, tick_box(rect), ticked);
                        let names: Vec<String> = packages.iter().map(|package| package.name.clone()).collect();
                        let label = format!("{title} ({})", names.len());
                        let (width, _) = painter.text(&label, Rect::new(rect.x + 34.0, rect.y, 220.0, rect.h), body);
                        painter.text(
                            &names.join(", "),
                            Rect::new(rect.x + 46.0 + width, rect.y, rect.w - 230.0 - width, rect.h),
                            muted,
                        );
                        let bytes = backend::human_size(kernels::size_of(&snapshot.packages, &names));
                        text_right(painter, &bytes, Rect::new(rect.right() - 170.0, rect.y, 72.0, rect.h), muted);
                        if !updates.busy() {
                            let label = if ticked { "Keep" } else { "Remove" };
                            draw_button(painter, row_button(rect), label, Look::Plain, pointer);
                        }
                    }
                    Row::Unneeded => {
                        let names = &snapshot.plan.unneeded;
                        painter.glyph(glyph::MINUS, tick_box(rect), 14, theme.muted);
                        let label = match names.len() {
                            1 => "1 package no longer needed".to_string(),
                            count => format!("{count} packages no longer needed"),
                        };
                        let (width, _) = painter.text(&label, Rect::new(rect.x + 34.0, rect.y, 320.0, rect.h), body);
                        painter.text(
                            &names.join(", "),
                            Rect::new(rect.x + 46.0 + width, rect.y, rect.w - 150.0 - width, rect.h),
                            muted,
                        );
                        if !updates.busy() {
                            draw_button(painter, row_button(rect), "Review", Look::Plain, pointer);
                        }
                    }
                    Row::TestMirrors => {
                        painter.glyph(glyph::RESTART, tick_box(rect), 14, theme.muted);
                        let (label, note) = if updates.mirrors.candidates.is_empty() {
                            ("Test the mirrors near you", "compares their speed and how far behind each is")
                        } else {
                            ("Test again", "the results above are from the last test")
                        };
                        let (width, _) = painter.text(label, Rect::new(rect.x + 34.0, rect.y, 320.0, rect.h), body);
                        painter.text(
                            note,
                            Rect::new(rect.x + 46.0 + width, rect.y, rect.w - 150.0 - width, rect.h),
                            muted,
                        );
                        if !updates.busy() {
                            draw_button(painter, row_button(rect), "Test", Look::Plain, pointer);
                        }
                    }
                    Row::Check(check) => {
                        let Some(check) = checks.get(*check) else { continue };
                        let (icon, tint) = match check.state {
                            release::State::Ready => (glyph::CHECK, theme.accent),
                            release::State::Attention => (glyph::ALERT, theme.warning),
                            release::State::Optional => (glyph::MINUS, theme.muted),
                        };
                        painter.glyph(icon, Rect::new(rect.x + 6.0, rect.y + 6.0, 20.0, 20.0), 15, tint);
                        painter.text(&check.title, Rect::new(rect.x + 34.0, rect.y + 5.0, rect.w - 130.0, 20.0), body);
                        painter.text(
                            &check.detail,
                            Rect::new(rect.x + 34.0, rect.y + 24.0, rect.w - 130.0, 18.0),
                            muted,
                        );
                        if let Some((label, _)) = Self::updates_check_page(check) {
                            draw_button(painter, row_button(rect), label, Look::Plain, pointer);
                        }
                    }
                    Row::Entry(entry) => {
                        let Some(entry) = snapshot.history.get(*entry) else { continue };
                        painter.text_font(
                            entry.date.get(..16).unwrap_or(&entry.date),
                            Rect::new(rect.x + 8.0, rect.y, 124.0, rect.h),
                            Style::new(size * 0.8, theme.muted),
                            "monospace",
                        );
                        let (width, _) = painter.text(
                            &entry.summary(),
                            Rect::new(rect.x + 140.0, rect.y, rect.w - 240.0, rect.h),
                            body,
                        );
                        painter.text(
                            &entry.names(3),
                            Rect::new(rect.x + 152.0 + width, rect.y, rect.w - 252.0 - width, rect.h),
                            muted,
                        );
                        let who = entry.who();
                        text_right(painter, who, Rect::new(rect.right() - 96.0, rect.y, 88.0, rect.h), muted);
                    }
                }
            }
        });

        // The strip about the highlighted row.
        if let Some(detail) = frame.detail {
            painter.fill(detail, theme.surface, 0.0);
            painter.fill(Rect::new(detail.x, detail.y, detail.w, 1.0), theme.border, 0.0);
            let buttons = self.updates_detail_buttons();
            let room = footer_text_room(detail, buttons.len(), 104.0);
            let lines = Rect::new(detail.x + 18.0, detail.y + 8.0, room, 20.0);
            let second = Rect::new(lines.x, lines.y + 20.0, lines.w, 20.0);
            match (window.page, self.updates_selected_row()) {
                (Page::Available, Some(Row::Group(..) | Row::Muted(_))) => {
                    let (chosen, waits) = self.updates_selected_updates();
                    if let Some(update) = chosen.first() {
                        let title = match update.source {
                            apt::Source::Apt if chosen.len() > 1 => {
                                format!("{} \u{b7} {} packages", update.group(), chosen.len())
                            }
                            _ => update.label().to_string(),
                        };
                        painter.text(&title, lines, body.bold());
                        let origin = if update.origin.is_empty() { "unknown source" } else { &update.origin };
                        let note = match (waits, update.source) {
                            (Some(true), _) => format!("Held with apt-mark \u{b7} from {origin}"),
                            (Some(false), _) => {
                                format!("Version {} skipped \u{b7} the next one shows again", update.new)
                            }
                            (None, apt::Source::Firmware) => {
                                format!("{} \u{b7} from {origin} \u{b7} tick it to install", update.summary)
                            }
                            (None, apt::Source::Apt) => format!("From {origin}"),
                            (None, source) => format!("{} \u{b7} from {origin}", source.label()),
                        };
                        painter.text(&note, second, muted);
                    }
                }
                (Page::Available, Some(Row::Unneeded)) => {
                    painter.text("Dependencies of packages that have been removed", lines, body.bold());
                    painter.text("Review lists what `apt-get autoremove` would remove.", second, muted);
                }
                (Page::History, Some(Row::Entry(index))) => {
                    if let Some(entry) = snapshot.history.get(index) {
                        painter.text(&format!("{} \u{b7} {}", entry.date, entry.summary()), lines, body.bold());
                        painter.text_font(&entry.command, second, Style::new(size * 0.8, theme.muted), "monospace");
                    }
                }
                (Page::Kernels, selected) => {
                    let kernel = match selected {
                        Some(Row::Kernel(index)) => found.get(index),
                        _ => None,
                    };
                    let (text, tint) = match kernel {
                        _ if kernels::fallback(found).is_none() && found.iter().any(|kernel| kernel.installed) => {
                            ("No fallback kernel is installed beside the running one.".to_string(), theme.warning)
                        }
                        Some(kernel) if kernel.installed => {
                            let headers = if kernel.headers.is_empty() {
                                "no headers".to_string()
                            } else {
                                format!("headers {}", kernel.headers.join(", ").replace("linux-headers-", ""))
                            };
                            let version = kernel.version.as_deref().unwrap_or_default();
                            (format!("{} {version} \u{b7} {headers}", kernel.package), theme.muted)
                        }
                        Some(kernel) => {
                            (format!("Installs {}", kernels::install_targets(kernel).join(", ")), theme.muted)
                        }
                        None => (
                            "Queue kernels to install or remove, preview, then apply as one transaction.".to_string(),
                            theme.muted,
                        ),
                    };
                    let muted = muted.colour(tint);
                    painter.text(&text, lines, muted);
                    let meter = Rect::new(lines.x, second.y + 6.0, detail.w - 36.0, 8.0);
                    painter.fill(meter, theme.border, 4.0);
                    let total: f64 =
                        found.iter().filter(|kernel| kernel.installed).map(|kernel| kernel.size as f64).sum();
                    let mut left = meter.x;
                    for kernel in found.iter().filter(|kernel| kernel.installed && total > 0.0) {
                        let width = meter.w * kernel.size as f64 / total;
                        let tint = if window.queue.contains_key(&kernel.package) {
                            theme.danger.alpha(0.6)
                        } else if kernel.running {
                            theme.accent
                        } else {
                            theme.accent_end
                        };
                        painter.fill(Rect::new(left, meter.y, (width - 2.0).max(1.0), meter.h), tint, 3.0);
                        left += width;
                    }
                }
                _ => {}
            }
            let count = buttons.len();
            for (slot, (label, _)) in buttons.into_iter().enumerate() {
                draw_button(&mut painter, footer_button(detail, slot, count, 104.0), label, Look::Plain, pointer);
            }
        }

        // The footer: what the buttons would do, or how the task is going.
        let footer = frame.footer;
        painter.fill(Rect::new(footer.x, footer.y, footer.w, 1.0), theme.border, 0.0);
        let buttons = self.updates_footer_buttons();
        let room = footer_text_room(footer, buttons.len(), 124.0);
        let line = Rect::new(footer.x + 18.0, footer.y, room, footer.h);
        // A mirror test shows its progress here too, when no task does.
        let mirror_test = updates.mirrors.test_progress().map(|(done, total)| {
            let next = updates.mirrors.waiting.first().map_or(String::new(), |url| format!(" \u{b7} {url}"));
            (format!("Testing mirrors \u{b7} {done} of {total} done{next}"), done as f64 / total as f64)
        });
        let progress = updates
            .progress()
            .map(|(title, label, fraction)| (format!("{title} \u{b7} {label}"), fraction))
            .or(mirror_test);
        if let Some((text, fraction)) = progress {
            painter.text(&text, Rect::new(line.x, line.y + 8.0, line.w, 22.0), body);
            let track = Rect::new(line.x, footer.y + 36.0, footer.w - 36.0, 4.0);
            painter.fill(track, theme.border, 2.0);
            painter.shape(
                Rect::new(track.x, track.y, (track.w * fraction.max(0.03)).min(track.w), track.h),
                Shape { fill: (theme.accent, theme.accent_end), ..Shape::filled(theme.accent, 2.0) },
            );
        } else {
            let sheet = window.preview.as_ref().map(|preview| (window.sheet, apt::parse_plan(preview)));
            let (text, tint) = match (updates.outcome(), window.page) {
                (Some(Err(reason)), _) => (reason.clone(), theme.danger),
                _ if matches!(
                    sheet,
                    Some((Sheet::Install | Sheet::InstallRestart | Sheet::Autoremove | Sheet::Repair | Sheet::Log, _))
                ) =>
                {
                    match sheet {
                        Some((Sheet::Log, _)) => ("Kept until the next task ends.".to_string(), theme.muted),
                        Some((_, plan)) if window.preview.as_ref().is_some_and(String::is_empty) => {
                            let _ = plan;
                            ("Asking apt-get...".to_string(), theme.muted)
                        }
                        Some((_, plan)) if plan.removes() && window.sheet != Sheet::Autoremove => {
                            (format!("This removes {}: {}", plan.removed.len(), plan.removed.join(", ")), theme.warning)
                        }
                        Some((_, plan)) => (format!("This {}.", plan.summary()), theme.muted),
                        None => (String::new(), theme.muted),
                    }
                }
                (_, Page::Available) => {
                    let (count, bytes, ..) = self.updates_ticked();
                    let restart = if snapshot.restart { " \u{b7} a restart is waiting" } else { "" };
                    let text = if snapshot.updates.is_empty() {
                        match updates.outcome() {
                            Some(Ok(message)) => message.clone(),
                            _ => "Nothing to install.".to_string(),
                        }
                    } else {
                        format!("{count} selected \u{b7} {} to download{restart}", backend::human_size(bytes))
                    };
                    (text, theme.muted)
                }
                (_, Page::Kernels) => {
                    let (install, remove) = self.updates_queue_plan();
                    if install.is_empty() && remove.is_empty() {
                        (updates.cleanup_summary().unwrap_or_else(|| "Nothing to clean up.".into()), theme.muted)
                    } else if kernels::leaves_no_fallback(found, &window.queue) {
                        ("This leaves no fallback kernel if the running one fails to start.".into(), theme.warning)
                    } else {
                        let mut parts = Vec::new();
                        if !install.is_empty() {
                            parts.push(format!("install {}", install.len()));
                        }
                        if !remove.is_empty() {
                            let freed = backend::human_size(kernels::size_of(&snapshot.packages, &remove));
                            let verb = if window.purge { "purge" } else { "remove" };
                            parts.push(format!("{verb} {} \u{b7} frees {freed}", remove.len()));
                        }
                        (format!("Queue: {}", parts.join(", ")), theme.muted)
                    }
                }
                (_, Page::Release) => {
                    let text = if updates.release().is_none() {
                        "Nothing to upgrade to."
                    } else if release::ready(&checks) {
                        "Runs do-release-upgrade in a terminal, where it asks its questions."
                    } else {
                        "Fix the marked items first."
                    };
                    (text.to_string(), theme.muted)
                }
                (_, Page::History) => ("Read-only. To undo an update, use a terminal.".to_string(), theme.muted),
                (_, Page::Settings) => {
                    let text = match updates.mirrors.current.as_ref() {
                        Some(_) if updates.mirrors.candidates.is_empty() => {
                            "Testing asks each mirror for a small file and part of a larger one.".to_string()
                        }
                        Some(_) => "Using a mirror rewrites the Ubuntu sources and refreshes the lists.".to_string(),
                        None => "No Ubuntu mirror was found in the package sources.".to_string(),
                    };
                    (text, theme.muted)
                }
            };
            painter.text(&text, line, muted.colour(tint));
        }
        let ready = release::ready(&checks);
        let count = buttons.len();
        for (slot, (label, hit, primary)) in buttons.into_iter().enumerate() {
            let look = match (primary, hit) {
                (true, _) => Look::Primary,
                (false, Hit::Security) => Look::Warning,
                _ => Look::Plain,
            };
            let rect = footer_button(footer, slot, count, 124.0);
            let enabled = ready || hit != Hit::Upgrade || !primary;
            painter.button(rect, Button::new(&label, look).enabled(enabled), rect.contains(pointer));
        }
        painter.finish()
    }
}

/// Package names as lines of about `columns` characters.
fn wrap_names(names: &[String], columns: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for name in names {
        match lines.last_mut() {
            Some(line) if line.len() + name.len() + 2 <= columns => {
                line.push_str(", ");
                line.push_str(name);
            }
            _ => lines.push(name.clone()),
        }
    }
    lines
}

/// An arrow pointing up: updates.
const ARROW_UP: &str = r#"<path d="M12 19V5M5 12l7-7 7 7"/>"#;

fn draw_button(painter: &mut Painter, rect: Rect, label: &str, look: Look, pointer: Point) {
    painter.button(rect, Button::new(label, look), rect.contains(pointer));
}

fn draw_tick(painter: &mut Painter, rect: Rect, ticked: bool) {
    let theme = painter.theme();
    if ticked {
        painter.shape(rect, Shape { fill: (theme.accent, theme.accent_end), ..Shape::filled(theme.accent, 4.0) });
        painter.glyph(glyph::CHECK, rect, 12, theme.on_accent);
    } else {
        painter.shape(
            rect,
            Shape { border_width: 1.5, border: theme.muted.alpha(0.7), radius: 4.0, ..Default::default() },
        );
    }
}

/// The badge of an update: why it matters, or where it comes from.
fn badge(update: &apt::Update, theme: &crate::theme::Theme) -> (&'static str, Colour) {
    match (update.security, update.kind) {
        (true, _) => ("Security", theme.warning),
        (_, apt::Kind::Kernel) => ("Kernel", theme.accent_end),
        (_, apt::Kind::Desktop) => ("Desktop", theme.accent),
        (_, apt::Kind::System) => ("System", theme.muted),
        (_, apt::Kind::App) => (update.source.label(), theme.accent),
        (_, apt::Kind::Firmware) => ("Firmware", theme.danger),
    }
}

/// The updates as rows: a source package's binaries together, each app
/// and device alone. They must be sorted so that a group is contiguous.
fn groups(updates: &[apt::Update]) -> Vec<(usize, usize)> {
    let mut found: Vec<(usize, usize)> = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        let joins = found.last().is_some_and(|(start, _)| {
            let first = &updates[*start];
            update.source == apt::Source::Apt
                && first.source == apt::Source::Apt
                && first.security == update.security
                && first.group() == update.group()
        });
        match found.last_mut() {
            Some((_, count)) if joins => *count += 1,
            _ => found.push((index, 1)),
        }
    }
    found
}

fn draw_badge(painter: &mut Painter, rect: Rect, label: &str, tint: Colour) {
    let theme = painter.theme();
    painter.shape(rect, Shape { border_width: 1.0, border: tint.alpha(0.7), ..Shape::filled(tint.alpha(0.1), 4.0) });
    painter.text(&label.to_uppercase(), rect, Style::new(theme.font_size * 0.62, tint).bold().align(Align::Centre));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_name_in_two_sources_is_ticked_apart() {
        let system = apt::Update {
            name: "org.mozilla.firefox//stable".into(),
            source: apt::Source::Flatpak,
            ..apt::Update::default()
        };
        let user = apt::Update { source: apt::Source::FlatpakUser, ..system.clone() };
        let mut window = Window::default();
        window.toggle(&user, false);
        assert!(window.ticked(&system) && !window.ticked(&user));
        window.toggle(&user, true);
        assert!(window.ticked(&user));
    }

    #[test]
    fn a_source_packages_binaries_share_a_row() {
        let update = |name: &str, package: &str, source: apt::Source| apt::Update {
            name: name.into(),
            package: package.into(),
            source,
            ..apt::Update::default()
        };
        let updates = [
            update("libc-bin", "glibc", apt::Source::Apt),
            update("libc6", "glibc", apt::Source::Apt),
            update("curl", "", apt::Source::Apt),
            update("org.a//stable", "", apt::Source::Flatpak),
            update("org.b//stable", "", apt::Source::Flatpak),
        ];
        assert_eq!(groups(&updates), [(0, 2), (2, 1), (3, 1), (4, 1)]);
        assert!(groups(&[]).is_empty());
    }

    #[test]
    fn names_wrap_into_lines() {
        let names: Vec<String> = ["alpha", "beta", "gamma", "delta"].iter().map(|name| (*name).to_string()).collect();
        assert_eq!(wrap_names(&names, 13), ["alpha, beta", "gamma, delta"]);
        assert_eq!(wrap_names(&names, 3), ["alpha", "beta", "gamma", "delta"]);
        assert!(wrap_names(&[], 40).is_empty());
    }
}
