//! The optional smart home block in the notification panel: Tuya lights,
//! plugs and air conditioners controlled on the LAN by `tileception-home`,
//! with its `[home]` settings, `tileception msg home` commands and the
//! `tileception home` command line that imports the devices.
//!
//! The block sits between the sound controls and Do not disturb. It shows
//! the home whose devices answer on this network, a climate widget, and the
//! devices by room. It starts folded while nothing is on and unfolds when
//! something turns on; folding it by hand holds until that changes.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::{RegistrationToken, channel, ping::make_ping};
use tileception_core::{Point, Rect};
use tileception_home::{Change, Device, HomeService, Kind, Options, Reach, Request, Snapshot, store};

use crate::{
    Tileception,
    controls::level_bar,
    shapes::Shape,
    ui::{Align, Painter, Style, ago, glyph},
};

/// The `[home]` section.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HomeConfig {
    /// Show the smart home block in the notification panel.
    pub enabled: bool,
    /// The device file; `~/.config/tileception/home-devices.json` by default.
    pub devices: Option<String>,
}

impl HomeConfig {
    pub(crate) fn path(&self) -> Option<PathBuf> {
        match &self.devices {
            Some(path) => Some(crate::wallpaper::expand_home(path)),
            None => store::default_path(),
        }
    }
}

/// Which slider of the selected light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dial {
    Brightness,
    ColourTemp,
}

#[derive(Default)]
pub struct Home {
    service: Option<HomeService>,
    source: Option<RegistrationToken>,
    refresh: Option<(RegistrationToken, Arc<AtomicBool>)>,
    pub snapshot: Snapshot,
    /// Why there is nothing to show, while enabled.
    problem: Option<Problem>,
    /// Folded or unfolded by hand, and how many devices were on then.
    fold: Option<(bool, usize)>,
    /// The light whose sliders show.
    selected: Option<String>,
    /// One list, devices that are on first, instead of rooms.
    all: bool,
    /// The climate device the widget shows, when there are several.
    climate: Option<String>,
    /// The home shown: the one whose devices answer on this network.
    place: Option<String>,
    /// A slider held: the device, which one, when its value was last sent
    /// and where it is.
    pub drag: Option<(String, Dial, Instant, u32)>,
}

/// Why the block has nothing to show while `[home]` is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// No config directory for the device file.
    NoConfigDir,
    /// No device file yet: nothing was imported.
    NoDevices,
    /// The device file exists but could not be read.
    Unreadable,
    /// The service did not start.
    Service,
}

/// Whether the panel's notice can lead to setting up in Settings.
fn state_needs_setup(problem: Option<Problem>) -> bool {
    problem == Some(Problem::NoDevices)
}

impl Problem {
    fn text(self) -> &'static str {
        match self {
            Problem::NoConfigDir => "No config directory for the device file",
            Problem::NoDevices => "Not set up yet. Click to set it up in Settings.",
            Problem::Unreadable => "The device file could not be read",
            Problem::Service => "The smart home service did not start",
        }
    }
}

const ROW: f64 = 40.0;
const SLIDER: f64 = 28.0;
const HEADING: f64 = 24.0;
const HEADER: f64 = 32.0;
const CLIMATE: f64 = 64.0;
const CHOICES: f64 = 28.0;
const FOOTER: f64 = 28.0;
/// The trend line under the climate widget.
const TREND: f64 = 30.0;
const SEND_INTERVAL: f64 = 1.0 / 8.0;

pub struct LightRow {
    pub rect: Rect,
    pub device_id: String,
    pub switch: Option<Rect>,
}

pub struct DialRow {
    pub device_id: String,
    pub dial: Dial,
    pub track: Rect,
    pub label: Rect,
    pub value: u32,
}

pub struct ClimateBlock {
    pub rect: Rect,
    pub device_id: String,
    pub name: Rect,
    pub minus: Rect,
    pub plus: Rect,
    pub switch: Rect,
    pub modes: Vec<(Rect, String)>,
    pub fans: Vec<(Rect, String)>,
    /// The trend line under the widget, when there is a history to draw.
    pub trend: Option<Rect>,
}

/// The block's layout, worked out afresh each frame.
pub struct HomeBlock {
    pub rect: Rect,
    pub header: Rect,
    pub view: Option<(Rect, Rect)>,
    pub folded: bool,
    pub climate: Option<ClimateBlock>,
    pub headings: Vec<(Rect, String, Option<String>)>,
    pub rows: Vec<LightRow>,
    pub dials: Vec<DialRow>,
    pub footer: Option<(Rect, Rect)>,
    /// A line instead of devices: setup needed, or why it failed.
    pub notice: Option<(Rect, String)>,
}

#[derive(Clone, PartialEq)]
enum PointerAction {
    View(bool),
    Fold,
    AllOff,
    /// Opens Settings on the Smart home page, to set it up.
    SetUp,
    Select(String),
    NextClimate(String),
    Change {
        device: String,
        change: Change,
    },
    Dial {
        device: String,
        dial: Dial,
        track: Rect,
    },
}

impl crate::ui::PointerAction for PointerAction {
    fn activate(&self, state: &mut Tileception, _rect: Rect, point: Point) {
        match self {
            Self::View(all) => state.home.all = *all,
            Self::SetUp => state.open_settings_page(5),
            Self::Fold => state.home.fold = Some((!state.panel_controls().home.folded, state.home_lit())),
            Self::AllOff => {
                if let Some(service) = &state.home.service {
                    service.send(Request::AllOff);
                }
            }
            Self::Select(device) => {
                state.home.selected = (state.home.selected.as_ref() != Some(device)).then(|| device.clone())
            }
            Self::Change { device, change } => state.home_send(device.clone(), change.clone()),
            Self::Dial { device, dial, track } => {
                let value = (((point.x - track.x) / track.w).clamp(0., 1.) * 100.).round() as u32;
                state.home.drag = Some((device.clone(), *dial, Instant::now(), value));
                state.send_dial(device, *dial, value);
            }
            Self::NextClimate(device) => {
                let climates: Vec<String> = state
                    .home_devices()
                    .into_iter()
                    .filter(|candidate| candidate.kind == Kind::Climate)
                    .map(|candidate| candidate.device_id.clone())
                    .collect();
                if let Some(index) = climates.iter().position(|candidate| candidate == device) {
                    state.home.climate = climates.get((index + 1) % climates.len()).cloned();
                }
            }
        }
        state.schedule_render();
    }
}

impl HomeBlock {
    fn empty(left: f64, top: f64, width: f64) -> HomeBlock {
        HomeBlock {
            rect: Rect::new(left, top, width, 0.0),
            header: Rect::new(left, top, width, 0.0),
            view: None,
            folded: true,
            climate: None,
            headings: Vec::new(),
            rows: Vec::new(),
            dials: Vec::new(),
            footer: None,
            notice: None,
        }
    }
}

fn reachable(device: &Device) -> bool {
    matches!(device.reach, Reach::Online | Reach::Cloud { updated: Some(_) })
}

fn is_on(device: &Device) -> bool {
    reachable(device) && device.state.is_on == Some(true)
}

fn mode_label(mode: &str) -> String {
    match mode {
        "auto" => "Auto".into(),
        "cold" => "Cool".into(),
        "hot" => "Heat".into(),
        "wet" => "Dry".into(),
        "wind" => "Fan".into(),
        "middle" => "Mid".into(),
        "mute" => "Quiet".into(),
        other => {
            let mut chars = other.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}

fn degrees(value: f64) -> String {
    if value.fract().abs() < 0.05 { format!("{value:.0}\u{b0}") } else { format!("{value:.1}\u{b0}") }
}

impl Tileception {
    pub(crate) fn settings_home_update_addresses(&self, file: &mut store::DeviceFile) {
        let Some(service) = &self.home.service else { return };
        for (device_id, host, version) in service.discovered() {
            if let Some(device) = file.devices.iter_mut().find(|device| device.device_id == device_id) {
                device.host = Some(host.to_string());
                if let Some(version) = version {
                    device.version = Some(version.as_str().into());
                }
            }
        }
    }

    pub(crate) fn settings_home_status(&self) -> String {
        if let Some(problem) = self.home.problem {
            problem.text().into()
        } else if self.home.service.is_some() {
            format!("Connected: {} devices", self.home.snapshot.devices.len())
        } else {
            "Disconnected".into()
        }
    }

    pub(crate) fn settings_home_cloud_label(&self, sensors: bool) -> String {
        let enabled = self
            .config
            .file
            .home
            .path()
            .and_then(|path| store::load(&path).ok())
            .and_then(|file| file.cloud)
            .is_some_and(|cloud| if sensors { cloud.poll_sensors } else { cloud.startup_refresh });
        format!("{}: {}", if sensors { "Cloud sensors" } else { "Startup refresh" }, if enabled { "on" } else { "off" })
    }

    pub(crate) fn settings_home_cloud(&mut self, sensors: bool) -> Result<(), String> {
        let path = self.config.file.home.path().ok_or("No config directory")?;
        let mut file = store::load(&path)?;
        let access = file.cloud.as_mut().ok_or("Import devices with saved cloud access first")?;
        if sensors {
            access.poll_sensors = !access.poll_sensors;
        } else {
            access.startup_refresh = !access.startup_refresh;
        }
        store::save(&path, &file)?;
        if self.home.service.is_some() {
            self.apply_home();
        }
        Ok(())
    }

    pub(crate) fn settings_home_connected(&self) -> bool {
        self.home.service.is_some()
    }

    /// Starts, restarts or stops the service to match `[home]`.
    pub fn apply_home(&mut self) {
        self.stop_home();
        let config = self.config.file.home.clone();
        if !config.enabled {
            return;
        }
        let Some(path) = config.path() else {
            self.home.problem = Some(Problem::NoConfigDir);
            return;
        };
        let file = match store::load(&path) {
            Ok(file) => file,
            Err(_) if !path.exists() => {
                self.home.problem = Some(Problem::NoDevices);
                return;
            }
            Err(err) => {
                tracing::warn!("home: {err}");
                self.home.problem = Some(Problem::Unreadable);
                return;
            }
        };
        let result = (|| {
            let (ping, source) = make_ping().map_err(|err| err.to_string())?;
            let options =
                Options { discovery: file.source != "simulated", history: tileception_home::history::default_path() };
            let service = HomeService::start(&file, options, move || ping.ping());
            let token = self
                .loop_handle
                .insert_source(source, |_, _, state| state.home_changed())
                .map_err(|err| err.to_string())?;
            Ok::<_, String>((service, token))
        })();
        match result {
            Ok((service, token)) => {
                self.home.snapshot = service.snapshot();
                self.home.service = Some(service);
                self.home.source = Some(token);
                self.home.problem = None;
                self.home_changed();
            }
            Err(err) => {
                tracing::error!("could not start the smart home service: {err}");
                self.home.problem = Some(Problem::Service);
            }
        }
    }

    /// Refreshes the saved inventory once at startup while cached controls run.
    pub fn refresh_home_at_startup(&mut self) {
        if self.home.service.is_none() {
            return;
        }
        let Some(path) = self.config.file.home.path() else { return };
        let cached = match store::load(&path) {
            Ok(file) if file.source == "tuya" => file,
            _ => return,
        };
        if cached.cloud.is_none() {
            tracing::info!("home: skipping startup refresh: no saved Tuya credentials; keeping cached devices");
            return;
        }
        if !cached.cloud.as_ref().is_some_and(|access| access.startup_refresh) {
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = channel::channel::<Result<bool, String>>();
        let token = match self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(result) = event {
                if let Some((token, cancelled)) = state.home.refresh.take() {
                    cancelled.store(true, Ordering::Relaxed);
                    state.loop_handle.remove(token);
                }
                match result {
                    Ok(true) => {
                        tracing::info!("home: refreshed devices, homes and rooms at startup");
                        state.apply_home();
                    }
                    Ok(false) => {}
                    Err(err) => tracing::warn!("home: startup refresh failed; keeping cached devices: {err}"),
                }
            }
        }) {
            Ok(token) => token,
            Err(err) => {
                tracing::warn!("home: could not wait for startup refresh: {err}");
                return;
            }
        };
        self.home.refresh = Some((token, cancelled.clone()));
        let started = std::thread::Builder::new().name("home-refresh".into()).spawn(move || {
            let result = (|| {
                let refreshed = tileception_home::cloud::refresh(&cached)?;
                if cancelled.load(Ordering::Relaxed) || refreshed == cached {
                    return Ok(false);
                }
                // Do not overwrite a manual import made during the request.
                if store::load(&path)? != cached {
                    return Err("the device file changed during refresh".into());
                }
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                store::save(&path, &refreshed)?;
                Ok(true)
            })();
            let _ = sender.send(result);
        });
        if let Err(err) = started {
            if let Some((token, cancelled)) = self.home.refresh.take() {
                cancelled.store(true, Ordering::Relaxed);
                self.loop_handle.remove(token);
            }
            tracing::warn!("home: could not start startup refresh: {err}");
        }
    }

    pub fn stop_home(&mut self) {
        if let Some((token, cancelled)) = self.home.refresh.take() {
            cancelled.store(true, Ordering::Relaxed);
            self.loop_handle.remove(token);
        }
        if let Some(token) = self.home.source.take() {
            self.loop_handle.remove(token);
        }
        self.home.service = None;
        self.home.snapshot = Snapshot::default();
        self.home.problem = None;
        self.home.drag = None;
    }

    fn home_changed(&mut self) {
        let Some(service) = &self.home.service else { return };
        self.home.snapshot = service.snapshot();
        // Follow the network: the home with the most devices answering,
        // the last one while none do, else the one with the most devices.
        let mut counts: Vec<(Option<String>, usize, usize)> = Vec::new();
        for device in &self.home.snapshot.devices {
            let answering = usize::from(device.reach == Reach::Online);
            match counts.iter_mut().find(|(home, ..)| *home == device.home) {
                Some((_, online, total)) => {
                    *online += answering;
                    *total += 1;
                }
                None => counts.push((device.home.clone(), answering, 1)),
            }
        }
        let best_online = counts.iter().max_by_key(|(_, online, _)| *online).filter(|(_, online, _)| *online > 0);
        let known = counts.iter().any(|(home, ..)| *home == self.home.place);
        self.home.place = match best_online {
            Some((home, ..)) => home.clone(),
            None if known => self.home.place.clone(),
            None => counts.iter().max_by_key(|(_, _, total)| *total).and_then(|(home, ..)| home.clone()),
        };
        if self.notifications.open {
            self.schedule_render();
        }
    }

    /// The devices of the home shown, in the file's order.
    fn home_devices(&self) -> Vec<&Device> {
        let place = &self.home.place;
        self.home.snapshot.devices.iter().filter(|device| device.home == *place || place.is_none()).collect()
    }

    fn home_lit(&self) -> usize {
        self.home_devices()
            .iter()
            .filter(|device| matches!(device.kind, Kind::Light | Kind::Plug | Kind::Climate) && is_on(device))
            .count()
    }

    fn home_folded(&self) -> bool {
        let lit = self.home_lit();
        match self.home.fold {
            Some((folded, lit_then)) if lit_then == lit => folded,
            _ => lit == 0,
        }
    }

    fn home_climate(&self) -> Option<&Device> {
        let climates: Vec<&Device> =
            self.home_devices().into_iter().filter(|device| device.kind == Kind::Climate).collect();
        climates
            .iter()
            .find(|device| Some(&device.device_id) == self.home.climate.as_ref())
            .or_else(|| climates.first())
            .copied()
    }

    /// The block with its top left at (x, y); zero height when switched off.
    pub fn home_layout(&self, left: f64, top: f64, width: f64) -> HomeBlock {
        let mut block = HomeBlock::empty(left, top, width);
        if !self.config.file.home.enabled {
            return block;
        }
        let inner = Rect::new(left + 10.0, top, width - 20.0, 0.0);
        let mut cursor = top + 4.0;
        block.header = Rect::new(left, cursor, width, HEADER);
        cursor += HEADER;
        if let Some(problem) = self.home.problem {
            block.notice = Some((Rect::new(inner.x, cursor, inner.w, 24.0), problem.text().into()));
            cursor += 30.0;
            block.rect = Rect::new(left, top, width, cursor - top);
            return block;
        }
        block.folded = self.home_folded();
        if block.folded {
            block.rect = Rect::new(left, top, width, cursor + 4.0 - top);
            return block;
        }
        let view_w = 104.0;
        let view = Rect::new(block.header.right() - 40.0 - view_w, block.header.y + 4.0, view_w, HEADER - 8.0);
        block.view = Some((
            Rect::new(view.x, view.y, view.w / 2.0, view.h),
            Rect::new(view.x + view.w / 2.0, view.y, view.w / 2.0, view.h),
        ));

        if let Some(climate) = self.home_climate() {
            let rect_top = cursor + 2.0;
            let mut inside = rect_top + 8.0;
            let main = Rect::new(inner.x, inside, inner.w, CLIMATE - 16.0);
            let switch = Rect::new(main.right() - 46.0, main.y + (main.h - 22.0) / 2.0, 38.0, 22.0);
            let plus = Rect::new(switch.x - 40.0, main.y + (main.h - 28.0) / 2.0, 28.0, 28.0);
            let minus = Rect::new(plus.x - 78.0, plus.y, 28.0, 28.0);
            let name = Rect::new(main.x + 124.0, main.y + 22.0, (minus.x - main.x - 128.0).max(20.0), 20.0);
            inside = main.bottom() + 4.0;
            let choices = |inside: &mut f64, values: &[String]| {
                let chosen: Vec<(Rect, String)> = if values.is_empty() {
                    Vec::new()
                } else {
                    let row = Rect::new(inner.x + 8.0, *inside, inner.w - 16.0, CHOICES - 4.0);
                    let label_w = 44.0;
                    let each = (row.w - label_w) / values.len() as f64;
                    let placed = values
                        .iter()
                        .enumerate()
                        .map(|(index, value)| {
                            (Rect::new(row.x + label_w + each * index as f64, row.y, each, row.h), value.clone())
                        })
                        .collect();
                    *inside += CHOICES;
                    placed
                };
                chosen
            };
            let modes = choices(&mut inside, &climate.modes);
            let fans = choices(&mut inside, &climate.fan_speeds);
            // The trend line waits for a history to draw.
            let trend = (climate.history.len() >= 2).then(|| {
                let rect = Rect::new(inner.x + 8.0, inside, inner.w - 16.0, TREND);
                inside += TREND + 2.0;
                rect
            });
            let rect = Rect::new(inner.x, rect_top, inner.w, inside + 4.0 - rect_top);
            cursor = rect.bottom() + 6.0;
            block.climate = Some(ClimateBlock {
                rect,
                device_id: climate.device_id.clone(),
                name,
                minus,
                plus,
                switch,
                modes,
                fans,
                trend,
            });
        }

        // Lights, plugs and sensors: by room, reachable first, or one list
        // with what is on first.
        let mut devices: Vec<&Device> =
            self.home_devices().into_iter().filter(|device| device.kind != Kind::Climate).collect();
        let rank = |device: &Device| match () {
            _ if !reachable(device) && !matches!(device.reach, Reach::Connecting | Reach::Cloud { .. }) => 2,
            _ if self.home.all && !is_on(device) => 1,
            _ => 0,
        };
        let mut rooms: Vec<Option<String>> = Vec::new();
        for device in &devices {
            if !rooms.contains(&device.room) {
                rooms.push(device.room.clone());
            }
        }
        if self.home.all {
            devices.sort_by_key(|device| rank(device));
            rooms = vec![None];
        } else {
            // Unnamed rooms last.
            rooms.sort_by_key(Option::is_none);
        }
        let climate_room = self.home_climate().and_then(|device| device.room.clone());
        for room in rooms {
            let mut members: Vec<&&Device> =
                devices.iter().filter(|device| self.home.all || device.room == room).collect();
            if !self.home.all {
                members.sort_by_key(|device| rank(device));
            }
            if members.is_empty() {
                continue;
            }
            if !self.home.all {
                let title = room.clone().unwrap_or_else(|| "Other".into());
                let detail = (room.is_some() && room == climate_room)
                    .then(|| self.home_climate().and_then(|device| device.state.temperature).map(degrees))
                    .flatten();
                block.headings.push((
                    Rect::new(inner.x + 2.0, cursor, inner.w - 4.0, HEADING),
                    title.to_uppercase(),
                    detail,
                ));
                cursor += HEADING;
            }
            for device in members {
                let rect = Rect::new(inner.x, cursor, inner.w, ROW);
                let switch = (device.kind != Kind::Sensor)
                    .then(|| Rect::new(rect.right() - 46.0, rect.y + (ROW - 22.0) / 2.0, 38.0, 22.0));
                block.rows.push(LightRow { rect, device_id: device.device_id.clone(), switch });
                cursor += ROW;
                let selected = self.home.selected.as_ref() == Some(&device.device_id) && device.reach == Reach::Online;
                if selected {
                    let held = |dial: Dial, value: u32| match &self.home.drag {
                        Some((held_id, held_dial, _, held_value))
                            if *held_id == device.device_id && *held_dial == dial =>
                        {
                            *held_value
                        }
                        _ => value,
                    };
                    let dial_row = |dial: Dial, value: u32, top: f64| DialRow {
                        device_id: device.device_id.clone(),
                        dial,
                        track: Rect::new(inner.x + 118.0, top, inner.w - 118.0 - 58.0, SLIDER),
                        label: Rect::new(inner.right() - 54.0, top, 48.0, SLIDER),
                        value: held(dial, value),
                    };
                    if device.dimmable {
                        block.dials.push(dial_row(
                            Dial::Brightness,
                            device.state.shown_brightness().unwrap_or(100),
                            cursor,
                        ));
                        cursor += SLIDER;
                    }
                    if device.tunable && !device.state.in_colour() {
                        block.dials.push(dial_row(Dial::ColourTemp, device.state.colour_temp.unwrap_or(0), cursor));
                        cursor += SLIDER;
                    }
                    cursor += 4.0;
                }
            }
            cursor += 4.0;
        }
        let footer = Rect::new(inner.x, cursor, inner.w, FOOTER);
        let all_off = Rect::new(footer.right() - 72.0, footer.y + 2.0, 72.0, footer.h - 4.0);
        block.footer = Some((footer, all_off));
        cursor = footer.bottom() + 6.0;
        block.rect = Rect::new(left, top, width, cursor - top);
        block
    }

    pub(crate) fn home_pointer_target(&self, block: &HomeBlock, point: Point) -> Option<crate::ui::PointerTarget> {
        use crate::ui::PointerTarget;
        if !block.rect.contains(point) {
            return None;
        }
        let mut buttons = Vec::new();
        if let Some((rooms, all)) = block.view {
            buttons.extend([(rooms, PointerAction::View(false)), (all, PointerAction::View(true))]);
        }
        if block.notice.is_none() {
            buttons.push((block.header, PointerAction::Fold));
        } else if state_needs_setup(self.home.problem)
            && let Some((rect, _)) = &block.notice
        {
            buttons.push((
                Rect::new(block.rect.x, block.header.y, block.rect.w, rect.bottom() - block.header.y),
                PointerAction::SetUp,
            ));
        }
        if let Some((_, all_off)) = block.footer {
            buttons.push((all_off, PointerAction::AllOff));
        }
        if let Some(climate) = &block.climate
            && climate.rect.contains(point)
        {
            return self.climate_pointer_target(climate, point);
        }
        for (rect, action) in buttons {
            if rect.contains(point) {
                return Some(PointerTarget::button(rect, action));
            }
        }
        for dial in &block.dials {
            let rect = Rect::new(dial.track.x - 8., dial.track.y, dial.track.w + 16., dial.track.h);
            if rect.contains(point) {
                return Some(PointerTarget::new(
                    rect,
                    PointerAction::Dial { device: dial.device_id.clone(), dial: dial.dial, track: dial.track },
                    smithay::input::pointer::CursorIcon::EwResize,
                ));
            }
        }
        for row in &block.rows {
            let Some(device) = self
                .home
                .snapshot
                .devices
                .iter()
                .find(|device| device.device_id == row.device_id && device.reach == Reach::Online)
            else {
                continue;
            };
            if !row.rect.contains(point) {
                continue;
            }
            if let Some(switch) = row.switch {
                let switch = Rect::new(switch.x - 8., row.rect.y, switch.w + 16., row.rect.h);
                if switch.contains(point) {
                    return Some(PointerTarget::button(
                        switch,
                        PointerAction::Change {
                            device: row.device_id.clone(),
                            change: Change::Switch(device.state.is_on != Some(true)),
                        },
                    ));
                }
            }
            if device.dimmable || device.tunable {
                return Some(PointerTarget::button(row.rect, PointerAction::Select(row.device_id.clone())));
            }
        }
        None
    }

    fn climate_pointer_target(&self, climate: &ClimateBlock, point: Point) -> Option<crate::ui::PointerTarget> {
        let device = self.home.snapshot.devices.iter().find(|device| device.device_id == climate.device_id).cloned()?;
        if climate.name.contains(point) {
            return Some(crate::ui::PointerTarget::button(
                climate.name,
                PointerAction::NextClimate(device.device_id.clone()),
            ));
        }
        if device.reach != Reach::Online {
            return None;
        }
        let step = device.target_range.map_or(0.5, |(_, _, step)| step.max(0.1));
        let target = device.state.target.unwrap_or(21.0);
        let switch =
            Rect::new(climate.switch.x - 6., climate.switch.y - 8., climate.switch.w + 12., climate.switch.h + 16.);
        let controls = [
            (switch, Change::Switch(device.state.is_on != Some(true))),
            (climate.minus, Change::Target(target - step)),
            (climate.plus, Change::Target(target + step)),
        ]
        .into_iter()
        .chain(climate.modes.iter().map(|(rect, mode)| (*rect, Change::Mode(mode.clone()))))
        .chain(climate.fans.iter().map(|(rect, fan)| (*rect, Change::Fan(fan.clone()))));
        controls.into_iter().find_map(|(rect, change)| {
            rect.contains(point).then(|| {
                crate::ui::PointerTarget::button(
                    rect,
                    PointerAction::Change { device: device.device_id.clone(), change },
                )
            })
        })
    }

    fn home_send(&mut self, device_id: String, change: Change) {
        if let Some(service) = &self.home.service {
            service.send(Request::Set { device_id, change });
        }
        self.schedule_render();
    }

    fn send_dial(&mut self, device_id: &str, dial: Dial, value: u32) {
        let change = match dial {
            Dial::Brightness => Change::Brightness(value.max(1)),
            Dial::ColourTemp => Change::ColourTemp(value),
        };
        self.home_send(device_id.to_string(), change);
    }

    /// The pointer moved while a slider is held.
    pub fn home_drag(&mut self, pointer_x: f64) {
        let Some((device_id, dial, sent, _)) = self.home.drag.clone() else { return };
        let block = self.panel_controls().home;
        let Some(row) = block.dials.iter().find(|row| row.device_id == device_id && row.dial == dial) else { return };
        let value = dial_value(row, pointer_x);
        if sent.elapsed().as_secs_f64() >= SEND_INTERVAL {
            self.send_dial(&device_id, dial, value);
            self.home.drag = Some((device_id, dial, Instant::now(), value));
        } else {
            self.home.drag = Some((device_id, dial, sent, value));
        }
        self.schedule_render();
    }

    /// Lets go of a slider, sending where it ended.
    pub fn home_release(&mut self) -> bool {
        let Some((device_id, dial, _, value)) = self.home.drag.take() else { return false };
        self.send_dial(&device_id, dial, value);
        true
    }

    pub fn draw_home(&self, painter: &mut Painter, block: &HomeBlock, pointer: Point) {
        if block.rect.h <= 0.0 {
            return;
        }
        let theme = &self.theme;
        painter.shape(
            block.rect,
            Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.surface, theme.radius) },
        );
        let header = block.header;
        if header.contains(pointer)
            && block.view.is_none_or(|(rooms, all)| !rooms.contains(pointer) && !all.contains(pointer))
        {
            painter.fill(header, theme.surface_raised, theme.radius);
        }
        painter.glyph(glyph::HOME, Rect::new(header.x + 8.0, header.y, 24.0, header.h), 16, theme.accent);
        let title = Style::new(theme.font_size, theme.text).bold();
        let (title_w, _) = painter.text("Home", Rect::new(header.x + 38.0, header.y, 80.0, header.h), title);
        let summary = self.home_summary(block.folded);
        let summary_w = if block.view.is_some() { header.w - title_w - 200.0 } else { header.w - title_w - 80.0 };
        painter.text(
            &summary,
            Rect::new(header.x + 46.0 + title_w, header.y, summary_w.max(10.0), header.h),
            Style::new(theme.font_size * 0.88, theme.muted),
        );
        let chevron = if block.folded { glyph::CHEVRON_RIGHT } else { glyph::CHEVRON_DOWN };
        if block.notice.is_none() {
            painter.glyph(chevron, Rect::new(header.right() - 34.0, header.y, 28.0, header.h), 13, theme.muted);
        }
        if let Some((rooms, all)) = block.view {
            painter.shape(
                Rect::new(rooms.x, rooms.y, rooms.w + all.w, rooms.h),
                Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background.alpha(0.4), 6.0) },
            );
            for (rect, label, chosen) in [(rooms, "Rooms", !self.home.all), (all, "All", self.home.all)] {
                if chosen {
                    painter.fill(
                        Rect::new(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, rect.h - 4.0),
                        theme.surface_raised,
                        4.0,
                    );
                    painter.fill(Rect::new(rect.x + 8.0, rect.bottom() - 4.0, rect.w - 16.0, 2.0), theme.accent, 1.0);
                }
                let colour = if chosen { theme.text } else { theme.muted };
                painter.text(label, rect, Style::new(theme.font_size * 0.82, colour).align(Align::Centre));
            }
        }
        if let Some((rect, text)) = &block.notice {
            painter.text(text, *rect, Style::new(theme.font_size * 0.9, theme.muted));
            return;
        }
        if let Some(climate) = &block.climate {
            self.draw_climate(painter, climate, pointer);
        }
        for (rect, title, detail) in &block.headings {
            painter.text(title, *rect, Style::new(theme.font_size * 0.78, theme.muted).bold());
            if let Some(detail) = detail {
                let style = Style::new(theme.font_size * 0.78, theme.muted);
                let (width, _) = painter.measure(detail, 120.0, style);
                painter.text(detail, Rect::new(rect.right() - width - 4.0, rect.y, width + 4.0, rect.h), style);
            }
        }
        for row in &block.rows {
            let Some(device) = self.home.snapshot.devices.iter().find(|device| device.device_id == row.device_id)
            else {
                continue;
            };
            self.draw_device_row(painter, row, device, pointer);
        }
        for dial in &block.dials {
            let label = match dial.dial {
                Dial::Brightness => "Brightness",
                Dial::ColourTemp => "Colour",
            };
            painter.text(
                label,
                Rect::new(dial.track.x - 74.0, dial.track.y, 72.0, dial.track.h),
                Style::new(theme.font_size * 0.82, theme.muted),
            );
            let held = self
                .home
                .drag
                .as_ref()
                .is_some_and(|(held_id, held_dial, ..)| *held_id == dial.device_id && *held_dial == dial.dial);
            let active = held
                || Rect::new(dial.track.x - 8.0, dial.track.y, dial.track.w + 16.0, dial.track.h).contains(pointer);
            if dial.dial == Dial::ColourTemp {
                // Warm to cool, under the knob.
                let bar = Rect::new(dial.track.x, dial.track.y + dial.track.h / 2.0 - 2.0, dial.track.w, 4.0);
                painter.shape(
                    bar,
                    Shape {
                        radius: 2.0,
                        fill: (
                            crate::theme::Colour([1.0, 0.70, 0.42, 1.0]),
                            crate::theme::Colour([0.81, 0.89, 1.0, 1.0]),
                        ),
                        ..Default::default()
                    },
                );
                let centre = bar.x + bar.w * f64::from(dial.value.min(100)) / 100.0;
                let radius = if active { 8.0 } else { 6.0 };
                painter.fill(
                    Rect::new(centre - radius, bar.y + 2.0 - radius, radius * 2.0, radius * 2.0),
                    theme.text,
                    radius as f32,
                );
            } else {
                level_bar(painter, dial.track, dial.value, 100, false, Some(active));
            }
            let text = match dial.dial {
                Dial::Brightness => format!("{}%", dial.value),
                Dial::ColourTemp => warmth(dial.value).to_string(),
            };
            painter.text(&text, dial.label, Style::new(theme.font_size * 0.82, theme.text).align(Align::Centre));
        }
        if let Some((footer, all_off)) = block.footer {
            let devices = self.home_devices();
            let local: Vec<&&Device> = devices
                .iter()
                .filter(|device| !matches!(device.reach, Reach::Cloud { .. } | Reach::Unsupported(_)))
                .collect();
            let answering = local.iter().filter(|device| device.reach == Reach::Online).count();
            let source = if self.home.snapshot.devices.is_empty() { "No devices" } else { "Local" };
            let line = format!("{source}, {answering} of {} reachable", local.len());
            let dot = Rect::new(footer.x + 6.0, footer.y + footer.h / 2.0 - 3.0, 6.0, 6.0);
            painter.fill(dot, if answering > 0 { theme.accent } else { theme.muted }, 3.0);
            painter.text(
                &line,
                Rect::new(footer.x + 18.0, footer.y, footer.w - 100.0, footer.h),
                Style::new(theme.font_size * 0.8, theme.muted),
            );
            let hover = all_off.contains(pointer);
            if hover {
                painter.fill(all_off, theme.surface_raised, theme.radius);
            }
            let colour =
                if self.home_lit() > 0 { if hover { theme.text } else { theme.muted } } else { theme.muted.alpha(0.5) };
            painter.text("All off", all_off, Style::new(theme.font_size * 0.82, colour).align(Align::Centre));
        }
    }

    fn home_summary(&self, folded: bool) -> String {
        if self.home.problem.is_some() {
            return String::new();
        }
        let lit = self
            .home_devices()
            .iter()
            .filter(|device| matches!(device.kind, Kind::Light | Kind::Plug) && is_on(device))
            .count();
        let lights = if lit == 0 { "all off".to_string() } else { format!("{lit} on") };
        let place = self.home.place.clone();
        if !folded {
            return match place {
                Some(place) if self.home_places() > 1 => format!("{place}, {lights}"),
                _ => lights,
            };
        }
        match self.home_climate() {
            Some(climate) if is_on(climate) => {
                let now = climate.state.temperature.map(degrees).unwrap_or_default();
                let mode = climate.state.mode.as_deref().map(mode_label).unwrap_or_default();
                let target = climate.state.target.map(degrees).unwrap_or_default();
                format!("{now}, {mode} to {target}, {lights}")
            }
            Some(climate) => match climate.state.temperature {
                Some(now) => format!("{}, {lights}", degrees(now)),
                None => lights,
            },
            None => lights,
        }
    }

    fn home_places(&self) -> usize {
        let mut places: Vec<&Option<String>> = self.home.snapshot.devices.iter().map(|device| &device.home).collect();
        places.dedup();
        places.sort();
        places.dedup();
        places.len()
    }

    fn draw_climate(&self, painter: &mut Painter, climate: &ClimateBlock, pointer: Point) {
        let theme = &self.theme;
        let Some(device) = self.home.snapshot.devices.iter().find(|device| device.device_id == climate.device_id)
        else {
            return;
        };
        painter.shape(
            climate.rect,
            Shape {
                border_width: 1.0,
                border: theme.border,
                ..Shape::filled(theme.surface_raised.alpha(0.5), theme.radius)
            },
        );
        let lit = is_on(device);
        let main = Rect::new(climate.rect.x, climate.name.y - 22.0, climate.rect.w, CLIMATE - 16.0);
        let (icon, tint) = match device.state.mode.as_deref() {
            _ if !lit => (glyph::THERMOMETER, theme.muted),
            Some("cold") => (glyph::SNOWFLAKE, theme.accent_end),
            Some("hot") => (glyph::FLAME, theme.danger),
            Some("wet") => (glyph::DROP, theme.accent),
            Some("wind") => (glyph::FAN, theme.accent),
            _ => (glyph::THERMOMETER, theme.accent),
        };
        painter.glyph(icon, Rect::new(main.x + 6.0, main.y, 28.0, main.h), 20, tint);
        let now = device.state.temperature.map(degrees).unwrap_or_else(|| "-".into());
        painter.text(
            &now,
            Rect::new(main.x + 38.0, main.y + 2.0, 84.0, 30.0),
            Style::new(theme.font_size * 1.8, theme.text).weight(300),
        );
        let status = if !reachable(device) {
            reach_text(device)
        } else if lit {
            device.state.mode.as_deref().map(mode_label).unwrap_or_else(|| "On".into())
        } else {
            "Off".into()
        };
        let hover_name = climate.name.contains(pointer)
            && self.home_devices().iter().filter(|other| other.kind == Kind::Climate).count() > 1;
        let name_colour = if hover_name { theme.text } else { theme.text.alpha(0.85) };
        painter.text(
            &format!("{}, {status}", device.name),
            Rect::new(climate.name.x, main.y + 4.0, climate.name.w, 18.0),
            Style::new(theme.font_size * 0.88, name_colour),
        );
        let humidity = self
            .home_devices()
            .iter()
            .filter(|other| other.kind == Kind::Sensor && reachable(other))
            .find_map(|sensor| sensor.state.humidity);
        if let Some(humidity) = humidity {
            painter.text(
                &format!("{humidity:.0}% humidity"),
                climate.name,
                Style::new(theme.font_size * 0.8, theme.muted),
            );
        }
        let usable = device.reach == Reach::Online;
        for (rect, icon) in [(climate.minus, glyph::MINUS), (climate.plus, glyph::PLUS)] {
            let hover = usable && rect.contains(pointer);
            painter.shape(
                rect,
                Shape {
                    border_width: 1.0,
                    border: theme.border,
                    ..Shape::filled(if hover { theme.surface_raised } else { theme.surface }, 6.0)
                },
            );
            painter.glyph(icon, rect, 13, if usable { theme.text } else { theme.muted });
        }
        let target = device.state.target.map(degrees).unwrap_or_else(|| "-".into());
        let between =
            Rect::new(climate.minus.right(), climate.minus.y, climate.plus.x - climate.minus.right(), climate.minus.h);
        painter.text(
            &target,
            between,
            Style::new(theme.font_size, if lit { theme.accent } else { theme.muted }).bold().align(Align::Centre),
        );
        draw_switch(painter, climate.switch, device.state.is_on == Some(true), device.pending, usable);
        for (choices, chosen, label) in [
            (&climate.modes, device.state.mode.as_deref(), "Mode"),
            (&climate.fans, device.state.fan.as_deref(), "Fan"),
        ] {
            let Some((first, _)) = choices.first() else { continue };
            painter.text(
                label,
                Rect::new(first.x - 44.0, first.y, 42.0, first.h),
                Style::new(theme.font_size * 0.78, theme.muted),
            );
            let strip = Rect::new(
                first.x,
                first.y,
                choices.last().map_or(first.w, |(last, _)| last.right() - first.x),
                first.h,
            );
            painter.shape(
                strip,
                Shape { border_width: 1.0, border: theme.border, ..Shape::filled(theme.background.alpha(0.4), 6.0) },
            );
            for (rect, value) in choices {
                let selected = chosen == Some(value.as_str());
                if selected {
                    painter.fill(
                        Rect::new(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, rect.h - 4.0),
                        theme.surface_raised,
                        4.0,
                    );
                    painter.fill(Rect::new(rect.x + 6.0, rect.bottom() - 4.0, rect.w - 12.0, 2.0), theme.accent, 1.0);
                } else if usable && rect.contains(pointer) {
                    painter.fill(
                        Rect::new(rect.x + 2.0, rect.y + 2.0, rect.w - 4.0, rect.h - 4.0),
                        theme.surface_raised.alpha(0.5),
                        4.0,
                    );
                }
                let colour = if selected && lit { theme.text } else { theme.muted };
                painter.text(
                    &mode_label(value),
                    *rect,
                    Style::new(theme.font_size * 0.76, colour).align(Align::Centre),
                );
            }
        }
        if let Some(trend) = &climate.trend {
            self.draw_trend(painter, *trend, device);
        }
    }

    /// The last six hours of the climate device's temperature and target, as
    /// two lines: the target dashed, the temperature solid.
    fn draw_trend(&self, painter: &mut Painter, rect: Rect, device: &Device) {
        use tileception_home::history::WINDOW;
        let theme = &self.theme;
        let now = std::time::SystemTime::now();
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for sample in &device.history {
            for value in [sample.temperature, sample.target].into_iter().flatten() {
                min = min.min(value);
                max = max.max(value);
            }
        }
        if !min.is_finite() || !max.is_finite() {
            return;
        }
        if max - min < 1.0 {
            let middle = (min + max) / 2.0;
            min = middle - 0.5;
            max = middle + 0.5;
        }
        let span = WINDOW.as_secs_f64();
        // Newest at the right, oldest six hours back at the left.
        let point = |when: std::time::SystemTime, value: f64| {
            let age = now.duration_since(when).map_or(span, |age| age.as_secs_f64()).min(span);
            let pos_x = rect.right() - age / span * rect.w;
            let pos_y = rect.bottom() - 2.0 - (value - min) / (max - min) * (rect.h - 4.0);
            (pos_x.clamp(rect.x, rect.right()), pos_y.clamp(rect.y, rect.bottom()))
        };
        // Hold each reading until the next sample, then join vertically.
        let mut line = |values: &[(f64, f64)], colour: crate::theme::Colour, thickness: f64, dashed: bool| {
            for (index, &(left, value)) in values.iter().enumerate() {
                let next = values.get(index + 1).copied();
                let right = next.map_or(rect.right(), |(next_x, _)| next_x);
                if dashed {
                    // Anchor the dash phase to the chart, independent of samples.
                    let mut dash = rect.x + ((left - rect.x) / 8.0).floor() * 8.0;
                    while dash < right {
                        let start = dash.max(left);
                        let end = (dash + 4.0).min(right);
                        if end > start {
                            painter.fill(
                                Rect::new(start, value - thickness / 2.0, end - start, thickness),
                                colour,
                                0.0,
                            );
                        }
                        dash += 8.0;
                    }
                } else if right > left {
                    painter.fill(Rect::new(left, value - thickness / 2.0, right - left, thickness), colour, 0.0);
                }
                if let Some((next_x, next_value)) = next {
                    painter.fill(
                        Rect::new(
                            next_x - thickness / 2.0,
                            value.min(next_value) - thickness / 2.0,
                            thickness,
                            (next_value - value).abs() + thickness,
                        ),
                        colour,
                        0.0,
                    );
                }
            }
        };
        let target: Vec<(f64, f64)> =
            device.history.iter().filter_map(|sample| sample.target.map(|value| point(sample.when, value))).collect();
        let temperature: Vec<(f64, f64)> = device
            .history
            .iter()
            .filter_map(|sample| sample.temperature.map(|value| point(sample.when, value)))
            .collect();
        line(&target, theme.muted.alpha(0.9), 1.0, true);
        line(&temperature, theme.accent, 2.0, false);
    }

    fn draw_device_row(&self, painter: &mut Painter, row: &LightRow, device: &Device, pointer: Point) {
        let theme = &self.theme;
        let selected = self.home.selected.as_ref() == Some(&device.device_id) && device.reach == Reach::Online;
        let usable = device.reach == Reach::Online;
        if selected {
            painter.fill(row.rect, theme.accent.alpha(0.12), theme.radius);
        } else if usable && row.rect.contains(pointer) {
            painter.fill(row.rect, theme.surface_raised, theme.radius);
        }
        let lit = is_on(device);
        let icon = match device.kind {
            Kind::Light => glyph::BULB,
            Kind::Plug => glyph::PLUG,
            Kind::Sensor => glyph::DROP,
            _ if !reachable(device) => glyph::WIFI_OFF,
            _ => glyph::HOME,
        };
        let badge = Rect::new(row.rect.x + 6.0, row.rect.y + 6.0, 28.0, 28.0);
        let warm = crate::theme::Colour([0.95, 0.71, 0.36, 1.0]);
        let (fill, tint) = match device.kind {
            _ if !reachable(device) && !matches!(device.reach, Reach::Connecting | Reach::Cloud { .. }) => {
                (theme.surface_raised.alpha(0.4), theme.muted.alpha(0.6))
            }
            Kind::Light if lit => (warm.alpha(0.14), warm),
            Kind::Plug if lit => (theme.accent.alpha(0.14), theme.accent),
            Kind::Sensor => (theme.accent.alpha(0.1), theme.accent),
            _ => (theme.surface_raised.alpha(0.6), theme.muted),
        };
        painter.fill(badge, fill, 6.0);
        painter.glyph(
            if reachable(device) || matches!(device.reach, Reach::Connecting | Reach::Cloud { .. }) {
                icon
            } else {
                glyph::WIFI_OFF
            },
            badge,
            15,
            tint,
        );
        let text_x = badge.right() + 10.0;
        let text_w = row.switch.map_or(row.rect.right(), |switch| switch.x) - text_x - 8.0;
        let name_colour = if lit || device.kind == Kind::Sensor { theme.text } else { theme.text.alpha(0.72) };
        painter.text(
            &device.name,
            Rect::new(text_x, row.rect.y + 3.0, text_w, 18.0),
            Style::new(theme.font_size * 0.92, name_colour),
        );
        let detail = device_detail(device);
        let detail_colour = match device.reach {
            Reach::Unreachable { .. } | Reach::Unsupported(_) => theme.danger.alpha(0.85),
            _ => theme.muted,
        };
        painter.text(
            &detail,
            Rect::new(text_x, row.rect.y + 20.0, text_w, 16.0),
            Style::new(theme.font_size * 0.78, detail_colour),
        );
        if let Some(switch) = row.switch {
            draw_switch(painter, switch, device.state.is_on == Some(true) && reachable(device), device.pending, usable);
        }
    }

    /// `tileception msg home ...`.
    pub fn home_command(&mut self, command: HomeCommand) -> Result<String, String> {
        // Switching the block on or off works whatever the current state is.
        if !matches!(command, HomeCommand::Enable(_)) {
            if !self.config.file.home.enabled {
                return Err("the smart home block is switched off: set enabled = true under [home], \
                     or run `tileception msg home enable`"
                    .into());
            }
            if let Some(problem) = self.home.problem {
                return Err(problem.text().into());
            }
        }
        match command {
            HomeCommand::Status => return Ok(self.home_status()),
            HomeCommand::Why(name) => return self.home_why(&name),
            HomeCommand::Fold(folded) => self.home.fold = Some((folded, self.home_lit())),
            HomeCommand::View(all) => self.home.all = all,
            HomeCommand::AllOff => {
                if let Some(service) = &self.home.service {
                    service.send(Request::AllOff);
                }
            }
            HomeCommand::Select(name) => {
                let device = self.find_device(&name)?;
                self.home.selected = Some(device);
            }
            HomeCommand::Set(name, change) => {
                let device_id = self.find_device(&name)?;
                let change = match change {
                    Wanted::Toggle => {
                        let device = self.home.snapshot.devices.iter().find(|device| device.device_id == device_id);
                        Change::Switch(device.and_then(|device| device.state.is_on) != Some(true))
                    }
                    Wanted::Change(change) => change,
                };
                self.home_send(device_id, change);
            }
            HomeCommand::Enable(wanted) => {
                let value = if wanted { "true" } else { "false" };
                let path = crate::config::path();
                crate::config_edit::set_value(&path, "home.enabled", value)
                    .map_err(|err| format!("could not set home.enabled in {}: {err}", path.display()))?;
                // The config watcher reloads the file and applies the change.
                return Ok(format!("home.enabled = {value} in {}", path.display()));
            }
        }
        self.schedule_render();
        Ok(String::new())
    }

    /// Why a device is not answering, and what to check.
    fn home_why(&self, name: &str) -> Result<String, String> {
        let device_id = self.find_device(name)?;
        let device = self
            .home
            .snapshot
            .devices
            .iter()
            .find(|device| device.device_id == device_id)
            .ok_or_else(|| format!("no device called {name:?}"))?;
        Ok(match &device.reach {
            Reach::Online => format!("{} is online ({}).", device.name, device_detail(device)),
            Reach::Connecting => format!("{} is still connecting; give it a moment.", device.name),
            Reach::Unreachable { since, why } => format!(
                "{} has been unreachable for {} ({why}). Check that it is powered on and on this network; its \
                 address is followed from the announcements it broadcasts. A checksum or handshake error means \
                 the stored local key is wrong: run `tileception home import` again to fetch fresh keys.",
                device.name,
                ago(*since)
            ),
            Reach::Cloud { updated: Some(when) } => {
                format!("{} only reports to the cloud; last read {} ago.", device.name, ago(*when))
            }
            Reach::Cloud { updated: None } => {
                format!("{} only reports to the cloud; it has not been read yet.", device.name)
            }
            Reach::Unsupported(why) => {
                let fix = if why.contains("gateway") {
                    "Devices behind a gateway are not supported yet"
                } else {
                    "Re-run `tileception home import` to fetch its local key"
                };
                format!("{} is not available locally: {why}. {fix}.", device.name)
            }
        })
    }

    fn find_device(&self, name: &str) -> Result<String, String> {
        let wanted = name.trim().to_lowercase();
        let devices = &self.home.snapshot.devices;
        devices
            .iter()
            .find(|device| device.name.to_lowercase() == wanted)
            .or_else(|| devices.iter().find(|device| device.device_id == name.trim()))
            .map(|device| device.device_id.clone())
            .ok_or_else(|| format!("no device called {name:?}"))
    }

    fn home_status(&self) -> String {
        let mut out = String::new();
        let place = self.home.place.clone().unwrap_or_else(|| "-".into());
        out.push_str(&format!("home: {place} (folded: {})\n", self.home_folded()));
        for device in &self.home.snapshot.devices {
            let reach = match &device.reach {
                Reach::Online => "online".to_string(),
                Reach::Connecting => "connecting".to_string(),
                Reach::Cloud { updated: Some(when) } => format!("cloud, {}", ago(*when)),
                Reach::Cloud { updated: None } => "cloud, not read yet".to_string(),
                Reach::Unreachable { since, why } => format!("unreachable for {} ({why})", ago(*since)),
                Reach::Unsupported(why) => format!("unsupported ({why})"),
            };
            let home = device.home.clone().unwrap_or_default();
            let room = device.room.clone().unwrap_or_default();
            out.push_str(&format!(
                "{}\t{home}\t{room}\t{:?}\t{reach}\t{}\n",
                device.name,
                device.kind,
                device_detail(device)
            ));
        }
        out
    }
}

/// What a row says under the name.
pub(crate) fn device_detail(device: &Device) -> String {
    if device.pending {
        return "Switching...".into();
    }
    match &device.reach {
        Reach::Connecting => return "Connecting...".into(),
        Reach::Unreachable { since, .. } => {
            let how_long = ago(*since);
            return if how_long == "now" { "Unreachable".into() } else { format!("Unreachable for {how_long}") };
        }
        Reach::Unsupported(why) => return format!("Not available: {why}"),
        Reach::Cloud { updated: None } => return "Waiting for the cloud...".into(),
        Reach::Online | Reach::Cloud { .. } => {}
    }
    let state = &device.state;
    match device.kind {
        Kind::Light if state.is_on == Some(true) => {
            let brightness = state.shown_brightness().map(|value| format!("{value}%")).unwrap_or_else(|| "On".into());
            if state.in_colour() {
                format!("{brightness}, colour")
            } else if device.tunable {
                format!("{brightness}, {}", warmth(state.colour_temp.unwrap_or(0)).to_lowercase())
            } else {
                brightness
            }
        }
        Kind::Plug if state.is_on == Some(true) => {
            state.power.map(|watts| format!("{watts:.0} W")).unwrap_or_else(|| "On".into())
        }
        Kind::Sensor => {
            let mut parts = Vec::new();
            if let Some(temperature) = state.temperature {
                parts.push(degrees(temperature));
            }
            if let Some(humidity) = state.humidity {
                parts.push(format!("{humidity:.0}%"));
            }
            if let Reach::Cloud { updated: Some(when) } = device.reach {
                parts.push(format!("{} ago", ago(when)).replace("now ago", "just now"));
            }
            parts.join(", ")
        }
        Kind::Climate if state.is_on == Some(true) => {
            state.mode.as_deref().map(mode_label).unwrap_or_else(|| "On".into())
        }
        _ => "Off".into(),
    }
}

fn reach_text(device: &Device) -> String {
    device_detail(device)
}

fn warmth(percent: u32) -> &'static str {
    match percent {
        0..=33 => "Warm",
        34..=66 => "Neutral",
        _ => "Cool",
    }
}

fn dial_value(row: &DialRow, pointer_x: f64) -> u32 {
    (((pointer_x - row.track.x) / row.track.w).clamp(0.0, 1.0) * 100.0).round() as u32
}

pub(crate) fn draw_switch(painter: &mut Painter, track: Rect, lit: bool, pending: bool, usable: bool) {
    let theme = painter.theme();
    let radius = (track.h / 2.0) as f32;
    let alpha = if usable { 1.0 } else { 0.4 };
    if lit {
        painter.shape(
            track,
            Shape {
                radius,
                fill: (theme.accent.alpha(alpha), theme.accent_end.alpha(alpha)),
                glow_radius: if pending || !usable { 0.0 } else { 8.0 },
                glow: theme.accent.alpha(0.25),
                ..Default::default()
            },
        );
    } else {
        painter.shape(
            track,
            Shape {
                border_width: 1.0,
                border: theme.border.alpha(alpha),
                ..Shape::filled(theme.background.alpha(alpha), radius)
            },
        );
    }
    let knob = track.h - 6.0;
    let knob_x = if lit { track.right() - knob - 3.0 } else { track.x + 3.0 };
    let colour = if pending {
        theme.muted
    } else if lit {
        theme.on_accent
    } else {
        theme.muted
    };
    painter.fill(Rect::new(knob_x, track.y + 3.0, knob, knob), colour.alpha(alpha), (knob / 2.0) as f32);
}

/// A change written as a command.
#[derive(Debug, Clone, PartialEq)]
pub enum Wanted {
    Toggle,
    Change(Change),
}

/// `tileception msg home ...`.
#[derive(Debug, Clone, PartialEq)]
pub enum HomeCommand {
    Status,
    /// Why a device is unreachable, and what to check.
    Why(String),
    Set(String, Wanted),
    Select(String),
    Fold(bool),
    /// `true` for one list, `false` for rooms.
    View(bool),
    AllOff,
    /// Writes `enabled` under `[home]` in config.toml.
    Enable(bool),
}

/// Parses what follows `home`. Device names may have spaces; a value comes
/// last.
pub fn parse(rest: &str) -> Result<HomeCommand, String> {
    let rest = rest.trim();
    let (sub, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let tail = tail.trim();
    let named = || if tail.is_empty() { Err(format!("home {sub} needs a device name")) } else { Ok(tail.to_string()) };
    let valued = || -> Result<(String, String), String> {
        let (name, value) = tail
            .rsplit_once(char::is_whitespace)
            .ok_or_else(|| format!("home {sub} needs a device name and a value"))?;
        Ok((name.trim().to_string(), value.trim().to_string()))
    };
    let percent = |value: &str| {
        value
            .trim_end_matches('%')
            .parse::<u32>()
            .ok()
            .filter(|number| *number <= 100)
            .ok_or_else(|| format!("{value:?} is not a percentage"))
    };
    Ok(match sub {
        "" | "status" => HomeCommand::Status,
        "why" => HomeCommand::Why(named()?),
        "toggle" => HomeCommand::Set(named()?, Wanted::Toggle),
        "on" => HomeCommand::Set(named()?, Wanted::Change(Change::Switch(true))),
        "off" => HomeCommand::Set(named()?, Wanted::Change(Change::Switch(false))),
        "select" => HomeCommand::Select(named()?),
        "brightness" => {
            let (name, value) = valued()?;
            HomeCommand::Set(name, Wanted::Change(Change::Brightness(percent(&value)?.max(1))))
        }
        "colour" | "color" | "warmth" => {
            let (name, value) = valued()?;
            HomeCommand::Set(name, Wanted::Change(Change::ColourTemp(percent(&value)?)))
        }
        "target" => {
            let (name, value) = valued()?;
            let degrees = value
                .trim_end_matches('\u{b0}')
                .parse::<f64>()
                .map_err(|_| format!("{value:?} is not a temperature"))?;
            HomeCommand::Set(name, Wanted::Change(Change::Target(degrees)))
        }
        "mode" => {
            let (name, value) = valued()?;
            let mode = match value.to_lowercase().as_str() {
                "cool" => "cold".to_string(),
                "heat" => "hot".to_string(),
                "dry" => "wet".to_string(),
                "fan" => "wind".to_string(),
                other => other.to_string(),
            };
            HomeCommand::Set(name, Wanted::Change(Change::Mode(mode)))
        }
        "fan" => {
            let (name, value) = valued()?;
            let speed = match value.to_lowercase().as_str() {
                "mid" => "middle".to_string(),
                "quiet" => "mute".to_string(),
                other => other.to_string(),
            };
            HomeCommand::Set(name, Wanted::Change(Change::Fan(speed)))
        }
        "all-off" => HomeCommand::AllOff,
        "fold" => HomeCommand::Fold(true),
        "unfold" => HomeCommand::Fold(false),
        "rooms" => HomeCommand::View(false),
        "all" => HomeCommand::View(true),
        "enable" => HomeCommand::Enable(true),
        "disable" => HomeCommand::Enable(false),
        other => return Err(format!("unknown home command {other:?}")),
    })
}

pub const CLI_USAGE: &str = "\
Usage: tileception home <command>

  import      fetch your Tuya devices (names, rooms, local keys) from the
              Tuya IoT Platform once; afterwards they are controlled on the
              LAN only. Asks for the project's Access ID, data center and
              Access Secret. Options: --client-id ID, --region eu|we|us|ue|in|cn|sg,
              --secret-file PATH, --cloud-sensors / --no-cloud-sensors,
              --startup-refresh / --no-startup-refresh,
              --file PATH
  status      connect to the imported devices for a few seconds and list them
              (--file PATH)
  simulate    start simulated devices on localhost and write a device file
              for them (--file PATH); runs until interrupted
";

/// `tileception home ...`.
pub fn run(args: &[String]) -> Result<(), String> {
    let flag = |name: &str| args.iter().position(|arg| arg == name).and_then(|index| args.get(index + 1)).cloned();
    let has = |name: &str| args.iter().any(|arg| arg == name);
    let file = flag("--file").map(PathBuf::from).or_else(store::default_path).ok_or("no config directory")?;
    match args.first().map(String::as_str) {
        Some("import") => import(
            &file,
            flag("--client-id"),
            flag("--region"),
            flag("--secret-file"),
            match () {
                _ if has("--cloud-sensors") => Some(true),
                _ if has("--no-cloud-sensors") => Some(false),
                _ => None,
            },
            match () {
                _ if has("--startup-refresh") => Some(true),
                _ if has("--no-startup-refresh") => Some(false),
                _ => None,
            },
        ),
        Some("status") => status(&file),
        Some("simulate") => simulate(&file),
        None | Some("-h" | "--help" | "help") => {
            print!("{CLI_USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other:?}\n\n{CLI_USAGE}")),
    }
}

fn ask(question: &str) -> Result<String, String> {
    use std::io::Write;
    print!("{question}");
    std::io::stdout().flush().map_err(|err| err.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|err| err.to_string())?;
    Ok(line.trim().to_string())
}

/// Reads a line showing `*` for each character typed: the terminal stops
/// echoing and hands over every key at once; Backspace, Ctrl+U and Ctrl+C
/// work, and the terminal is always put back.
/// Without a terminal (input from a pipe) it reads a plain line.
fn ask_hidden(question: &str) -> Result<zeroize::Zeroizing<String>, String> {
    use std::io::{Read, Write};
    let stty = |arguments: &[&str]| {
        std::process::Command::new("stty")
            .args(arguments)
            .stdin(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let Some(saved) = stty(&["-g"]) else {
        return ask(question).map(zeroize::Zeroizing::new);
    };
    if stty(&["-echo", "-icanon", "-isig", "min", "1"]).is_none() {
        return ask(question).map(zeroize::Zeroizing::new);
    }
    let mut out = std::io::stdout();
    let mut answer = zeroize::Zeroizing::new(String::new());
    let result = (|| {
        write!(out, "{question}").and_then(|()| out.flush()).map_err(|err| err.to_string())?;
        let mut byte = [0u8; 1];
        loop {
            if std::io::stdin().read(&mut byte).map_err(|err| err.to_string())? == 0 {
                break;
            }
            match byte[0] {
                b'\n' | b'\r' => break,
                // Ctrl+C or Ctrl+D: the terminal is put back before leaving.
                0x03 | 0x04 => return Err("cancelled".to_string()),
                // Backspace or Delete.
                0x08 | 0x7f => {
                    if answer.pop().is_some() {
                        write!(out, "\x08 \x08").map_err(|err| err.to_string())?;
                    }
                }
                // Ctrl+U clears the line.
                0x15 => {
                    for _ in 0..answer.chars().count() {
                        write!(out, "\x08 \x08").map_err(|err| err.to_string())?;
                    }
                    answer.clear();
                }
                printable if printable >= 0x20 => {
                    answer.push(char::from(printable));
                    write!(out, "*").map_err(|err| err.to_string())?;
                }
                _ => {}
            }
            out.flush().map_err(|err| err.to_string())?;
        }
        Ok::<(), String>(())
    })();
    stty(&[saved.as_str()]);
    println!();
    result.map(|()| answer)
}

/// Fetch inventory and merge discovered or cached LAN addresses. Callers
/// decide whether to retain credentials and when to save the result.
pub(crate) fn import_devices(
    file: &std::path::Path,
    credentials: tileception_home::cloud::Credentials,
    scan: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<tileception_home::store::DeviceFile, String> {
    let mut imported = tileception_home::cloud::import(credentials, progress)?;
    let heard = if scan {
        progress("Looking for devices on this network (15 seconds)...");
        tileception_home::discovery::scan(Duration::from_secs(15))
    } else {
        progress("Using discovery from the running connection...");
        Vec::new()
    };
    let previous = store::load(file).ok();
    for device in &mut imported.devices {
        let found = heard.iter().find(|(device_id, ..)| *device_id == device.device_id);
        let earlier =
            previous.as_ref().and_then(|old| old.devices.iter().find(|old| old.device_id == device.device_id));
        if let Some((_, host, version)) = found {
            device.host = Some(host.to_string());
            device.version = version.map(|version| version.as_str().to_string());
        } else if let Some(earlier) = earlier {
            device.host = earlier.host.clone();
            device.version = earlier.version.clone();
        }
    }
    Ok(imported)
}

fn import(
    file: &std::path::Path,
    client_id: Option<String>,
    region: Option<String>,
    secret_file: Option<String>,
    cloud_sensors: Option<bool>,
    startup_refresh: Option<bool>,
) -> Result<(), String> {
    println!("Importing devices from the Tuya IoT Platform (iot.tuya.com, Cloud > Development > your project).");
    let client_id = match client_id {
        Some(given) => given,
        None => ask("Access ID: ")?,
    };
    let secret = match secret_file {
        Some(path) => zeroize::Zeroizing::new(
            std::fs::read_to_string(&path).map_err(|err| format!("{path}: {err}"))?.trim().to_string(),
        ),
        None => ask_hidden("Access Secret: ")?,
    };
    if client_id.is_empty() || secret.is_empty() {
        return Err("the Access ID and the Access Secret are both needed".into());
    }
    let region = match region {
        Some(region) => region,
        None => {
            println!("Data centers: {}", tileception_home::cloud::REGIONS);
            ask("Data center (shown on the project's overview): ")?
        }
    };
    if tileception_home::cloud::host(&region).is_none() {
        return Err(format!("unknown data center {region:?}; use {}", tileception_home::cloud::REGIONS));
    }
    let credentials = tileception_home::cloud::Credentials {
        client_id: client_id.clone(),
        secret: secret.clone(),
        region: region.clone(),
    };
    let mut imported = import_devices(file, credentials, true, &mut |line| println!("{line}"))?;
    let cloud_only: Vec<String> = imported
        .devices
        .iter()
        .filter(|device| {
            device.host.is_none() && tileception_home::model::Model::for_entry(device).kind == Kind::Sensor
        })
        .map(|device| device.name.clone())
        .collect();
    let poll_sensors = if cloud_only.is_empty() {
        false
    } else {
        match cloud_sensors {
            Some(choice) => choice,
            None => {
                let (names, verb, unheard, them) = match cloud_only.as_slice() {
                    [one] => (one.clone(), "is a sensor that only reports", "it was", "it"),
                    _ => (cloud_only.join(", "), "are sensors that only report", "they were", "them"),
                };
                println!(
                    "\n{names} {verb} to the cloud ({unheard} not heard on this network). Reading {them} every 10 minutes needs the Access Secret stored in {} (readable by you only).",
                    file.display()
                );
                matches!(ask(&format!("Read {them} from the cloud? [y/N] "))?.to_lowercase().as_str(), "y" | "yes")
            }
        }
    };
    let startup_refresh = match startup_refresh {
        Some(choice) => choice,
        None => {
            if !poll_sensors {
                println!(
                    "\nRefreshing devices, homes and rooms at startup needs the Access Secret stored in {} (readable by you only).",
                    file.display()
                );
            }
            matches!(ask("Refresh devices, homes and rooms at startup? [y/N] ")?.to_lowercase().as_str(), "y" | "yes")
        }
    };
    if poll_sensors || startup_refresh {
        imported.cloud =
            Some(store::CloudAccess { client_id, region, secret: secret.to_string(), poll_sensors, startup_refresh });
    }
    store::save(file, &imported)?;

    println!("\nSaved {} devices to {}:", imported.devices.len(), file.display());
    for device in &imported.devices {
        let place = [device.home.clone(), device.room.clone()].into_iter().flatten().collect::<Vec<_>>().join(", ");
        let found = match (&device.host, &device.version) {
            (Some(host), Some(version)) => format!("on this network at {host}, protocol {version}"),
            (Some(host), None) => format!("last seen at {host}"),
            _ if cloud_only.contains(&device.name) && poll_sensors => "read from the cloud".to_string(),
            _ => "not heard on this network (off, or elsewhere)".to_string(),
        };
        println!("  {}  [{}]  {found}", device.name, if place.is_empty() { "-".into() } else { place });
    }
    println!("\nSet enabled = true under [home] in config.toml to show them in the notification panel (Super+N).");
    Ok(())
}

fn status(file: &std::path::Path) -> Result<(), String> {
    let devices = store::load(file)?;
    let options = Options { discovery: devices.source != "simulated", history: None };
    let service = HomeService::start(&devices, options, || {});
    let until = Instant::now() + Duration::from_secs(12);
    while Instant::now() < until {
        let snapshot = service.snapshot();
        if snapshot.devices.iter().all(|device| !matches!(device.reach, Reach::Connecting)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    for device in service.snapshot().devices {
        let place = [device.home.clone(), device.room.clone()].into_iter().flatten().collect::<Vec<_>>().join(", ");
        let reach = match &device.reach {
            Reach::Online => "online".to_string(),
            Reach::Connecting => "still connecting".to_string(),
            Reach::Cloud { .. } => "cloud".to_string(),
            Reach::Unreachable { why, .. } => format!("unreachable ({why})"),
            Reach::Unsupported(why) => format!("not available ({why})"),
        };
        println!("{}  [{place}]  {reach}  {}", device.name, device_detail(&device));
    }
    Ok(())
}

fn simulate(file: &std::path::Path) -> Result<(), String> {
    let household = tileception_home::sim::household().map_err(|err| err.to_string())?;
    store::save(file, &household.file)?;
    println!("Simulating {} devices; device file: {}", household.file.devices.len(), file.display());
    println!("Point [home] devices at it in a test config. Interrupt to stop.");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_home_commands() {
        assert_eq!(parse(""), Ok(HomeCommand::Status));
        assert_eq!(parse("why Hallway spot"), Ok(HomeCommand::Why("Hallway spot".into())));
        assert_eq!(parse("enable"), Ok(HomeCommand::Enable(true)));
        assert_eq!(parse("disable"), Ok(HomeCommand::Enable(false)));
        assert_eq!(parse("toggle Couch 2"), Ok(HomeCommand::Set("Couch 2".into(), Wanted::Toggle)));
        assert_eq!(
            parse("brightness Couch 2 40%"),
            Ok(HomeCommand::Set("Couch 2".into(), Wanted::Change(Change::Brightness(40))))
        );
        assert_eq!(
            parse("mode Airco cool"),
            Ok(HomeCommand::Set("Airco".into(), Wanted::Change(Change::Mode("cold".into()))))
        );
        assert_eq!(
            parse("target Airco 21.5"),
            Ok(HomeCommand::Set("Airco".into(), Wanted::Change(Change::Target(21.5))))
        );
        assert!(parse("brightness Couch 200").is_err());
        assert!(parse("toggle").is_err());
    }
}
