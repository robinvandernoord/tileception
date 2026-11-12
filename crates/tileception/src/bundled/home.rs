//! The `home` plugin: Tuya lights, plugs, sensors and air conditioners on
//! the LAN through `tileception-home`. It owns the device connections, so
//! everything that controls them goes through it: the panel block, `home`
//! in the launcher, `tileception msg home` and key bindings. The lock screen
//! gets only the indoor temperature, to read.
//!
//! It follows the device file: when an import in Settings or
//! `tileception home import` writes it, the plugin connects again. Once at
//! start it refreshes the inventory from the cloud, when the file allows.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use serde_json::{Value, json};
use tileception_home::{Change, Device, HomeService, Kind, Options, Reach, Request, store};

use super::{Notifier, Plugin, tool};
use crate::{
    home::{HomeCommand, Wanted},
    panel::block::{
        Bar, BarChip, Block, Card, Choice, Choices, Footer, FooterAction, Group, Item, Level, LevelLook, Line, Lock,
        Notice, Reading, Row, Stepper, Switch, Tint, Trend, View,
    },
    plugins::{ActionKind, BAR, LOCK, PANEL},
    ui::ago,
};

/// The device list for Settings: names, places, how each answers, and the
/// addresses heard on the network.
pub const DEVICES: &str = "tileception://home/devices";
/// How often the device file is checked for changes.
const WATCH: Duration = Duration::from_secs(2);

/// Why there is nothing to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Problem {
    NoConfigDir,
    NoDevices,
    Unreadable,
}

impl Problem {
    fn text(self) -> &'static str {
        match self {
            Problem::NoConfigDir => "No config directory for the device file",
            Problem::NoDevices => "Not set up yet. Click to set it up in Settings.",
            Problem::Unreadable => "The device file could not be read",
        }
    }
}

pub struct HomePlugin {
    path: Option<PathBuf>,
    service: Option<HomeService>,
    problem: Option<Problem>,
    /// The device file's modification time when last loaded.
    loaded: Option<SystemTime>,
    /// The home shown: the one whose devices answer on this network.
    place: Option<String>,
    /// The climate device the card shows, when there are several.
    climate: Option<String>,
    notifier: Option<Notifier>,
    /// Ends the startup refresh when the file changes meanwhile.
    refreshing: Option<Arc<AtomicBool>>,
}

impl HomePlugin {
    pub fn new(args: &[String]) -> Result<Self, String> {
        let devices = args.iter().position(|arg| arg == "--devices").and_then(|index| args.get(index + 1));
        let path = match devices {
            Some(path) => Some(crate::wallpaper::expand_home(path)),
            None => store::default_path(),
        };
        Ok(Self {
            path,
            service: None,
            problem: None,
            loaded: None,
            place: None,
            climate: None,
            notifier: None,
            refreshing: None,
        })
    }

    /// Connects to the devices in the file, as it is now.
    fn connect(&mut self) {
        self.service = None;
        self.problem = None;
        let Some(path) = self.path.clone() else {
            self.problem = Some(Problem::NoConfigDir);
            return;
        };
        self.loaded = modified(&path);
        let file = match store::load(&path) {
            Ok(file) => file,
            Err(_) if !path.exists() => {
                self.problem = Some(Problem::NoDevices);
                return;
            }
            Err(error) => {
                eprintln!("home: {error}");
                self.problem = Some(Problem::Unreadable);
                return;
            }
        };
        let options =
            Options { discovery: file.source != "simulated", history: tileception_home::history::default_path() };
        let notifier = self.notifier.clone();
        self.service = Some(HomeService::start(&file, options, move || {
            if let Some(notifier) = &notifier {
                notifier.changed(PANEL);
                notifier.changed(BAR);
                notifier.changed(LOCK);
                notifier.changed(DEVICES);
            }
        }));
    }

    /// Connects again when the device file changed since it was read.
    fn follow_file(&mut self) {
        let Some(path) = &self.path else { return };
        if modified(path) != self.loaded {
            if let Some(refreshing) = self.refreshing.take() {
                refreshing.store(true, Ordering::Relaxed);
            }
            self.connect();
        }
    }

    /// Refreshes devices, homes and rooms from the cloud once, when the
    /// file keeps the credentials and asks for it; the watcher then picks
    /// up the new file.
    fn refresh_at_startup(&mut self) {
        let Some(path) = self.path.clone() else { return };
        let cached = match store::load(&path) {
            Ok(file) if file.source == "tuya" => file,
            _ => return,
        };
        if !cached.cloud.as_ref().is_some_and(|access| access.startup_refresh) {
            if cached.cloud.is_none() {
                eprintln!("home: skipping startup refresh: no saved Tuya credentials; keeping cached devices");
            }
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.refreshing = Some(cancelled.clone());
        let started = std::thread::Builder::new().name("home-refresh".into()).spawn(move || {
            let result = (|| {
                let refreshed = tileception_home::cloud::refresh(&cached)?;
                if cancelled.load(Ordering::Relaxed) || refreshed == cached {
                    return Ok(false);
                }
                // Do not overwrite an import made during the request.
                if store::load(&path)? != cached {
                    return Err("the device file changed during refresh".to_string());
                }
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                store::save(&path, &refreshed)?;
                Ok(true)
            })();
            match result {
                Ok(true) => eprintln!("home: refreshed devices, homes and rooms at startup"),
                Ok(false) => {}
                Err(error) => eprintln!("home: startup refresh failed; keeping cached devices: {error}"),
            }
        });
        if let Err(error) = started {
            eprintln!("home: could not start startup refresh: {error}");
        }
    }

    fn snapshot(&self) -> Vec<Device> {
        self.service.as_ref().map(|service| service.snapshot().devices).unwrap_or_default()
    }

    /// Follows the network: the home with the most devices answering, the
    /// last one while none do, else the one with the most devices.
    fn follow_place(&mut self, devices: &[Device]) {
        let mut counts: Vec<(Option<String>, usize, usize)> = Vec::new();
        for device in devices {
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
        let known = counts.iter().any(|(home, ..)| *home == self.place);
        self.place = match best_online {
            Some((home, ..)) => home.clone(),
            None if known => self.place.clone(),
            None => counts.iter().max_by_key(|(_, _, total)| *total).and_then(|(home, ..)| home.clone()),
        };
    }

    /// The devices of the home shown, in the file's order.
    fn shown(&mut self) -> Vec<Device> {
        let devices = self.snapshot();
        self.follow_place(&devices);
        let place = self.place.clone();
        devices.into_iter().filter(|device| device.home == place || place.is_none()).collect()
    }

    fn climate<'a>(&self, devices: &'a [Device]) -> Option<&'a Device> {
        let climates: Vec<&Device> = devices.iter().filter(|device| device.kind == Kind::Climate).collect();
        climates
            .iter()
            .find(|device| Some(&device.device_id) == self.climate.as_ref())
            .or_else(|| climates.first())
            .copied()
    }

    fn send(&self, device_id: String, change: Change) -> Result<Value, String> {
        let service = self.service.as_ref().ok_or_else(|| self.unavailable())?;
        service.send(Request::Set { device_id, change });
        Ok(json!({ "ok": true }))
    }

    fn unavailable(&self) -> String {
        self.problem.map_or("The smart home is not connected", Problem::text).to_string()
    }

    fn device(&self, name: &str) -> Result<Device, String> {
        let wanted = name.trim().to_lowercase();
        let devices = self.snapshot();
        devices
            .iter()
            .find(|device| device.name.to_lowercase() == wanted)
            .or_else(|| devices.iter().find(|device| device.device_id == name.trim()))
            .cloned()
            .ok_or_else(|| format!("no device called {name:?}"))
    }

    /// The panel block.
    fn block(&mut self) -> Block {
        let mut block = Block { title: "Home".into(), icon: Some("glyph:home".into()), ..Block::default() };
        if let Some(problem) = self.problem {
            let action = (problem == Problem::NoDevices).then(|| ActionKind::Settings { page: "Smart home".into() });
            block.notice = Some(Notice { text: problem.text().into(), action });
            return block;
        }
        let devices = self.shown();
        let lit = devices
            .iter()
            .filter(|device| matches!(device.kind, Kind::Light | Kind::Plug | Kind::Climate) && is_on(device))
            .count();
        block.active = lit as u32;
        let lights =
            devices.iter().filter(|device| matches!(device.kind, Kind::Light | Kind::Plug) && is_on(device)).count();
        let lights = if lights == 0 { "all off".to_string() } else { format!("{lights} on") };
        let climate = self.climate(&devices).cloned();
        block.summary = match &climate {
            Some(climate) if is_on(climate) => {
                let now = climate.state.temperature.map(degrees).unwrap_or_default();
                let mode = climate.state.mode.as_deref().map(mode_label).unwrap_or_default();
                let target = climate.state.target.map(degrees).unwrap_or_default();
                format!("{now}, {mode} to {target}, {lights}")
            }
            Some(climate) => match climate.state.temperature {
                Some(now) => format!("{}, {lights}", degrees(now)),
                None => lights.clone(),
            },
            None => lights.clone(),
        };
        let places = {
            let mut places: Vec<Option<String>> = self.snapshot().into_iter().map(|device| device.home).collect();
            places.sort();
            places.dedup();
            places.len()
        };
        block.summary_open = Some(match &self.place {
            Some(place) if places > 1 => format!("{place}, {lights}"),
            _ => lights,
        });

        let card = climate.as_ref().map(|climate| Item::Card(Box::new(climate_card(climate, &devices))));
        let others: Vec<&Device> = devices.iter().filter(|device| device.kind != Kind::Climate).collect();
        let faded =
            |device: &Device| !reachable(device) && !matches!(device.reach, Reach::Connecting | Reach::Cloud { .. });

        // By room: named rooms first, devices that answer first in each.
        let mut rooms: Vec<Option<String>> = Vec::new();
        for device in &others {
            if !rooms.contains(&device.room) {
                rooms.push(device.room.clone());
            }
        }
        rooms.sort_by_key(Option::is_none);
        let climate_room = climate.as_ref().and_then(|device| device.room.clone());
        let mut by_room: Vec<Item> = card.iter().cloned().collect();
        for room in rooms {
            let mut members: Vec<&&Device> = others.iter().filter(|device| device.room == room).collect();
            members.sort_by_key(|device| faded(device));
            let detail = (room.is_some() && room == climate_room)
                .then(|| climate.as_ref().and_then(|device| device.state.temperature).map(degrees))
                .flatten();
            by_room.push(Item::Group(Group {
                title: room.unwrap_or_else(|| "Other".into()),
                detail,
                items: members.into_iter().map(|device| Item::Row(device_row(device))).collect(),
            }));
        }
        // All at once: what is on first, what does not answer last.
        let mut all = others.clone();
        all.sort_by_key(|device| match () {
            _ if faded(device) => 2,
            _ if !is_on(device) => 1,
            _ => 0,
        });
        let mut everything: Vec<Item> = card.into_iter().collect();
        everything.extend(all.into_iter().map(|device| Item::Row(device_row(device))));
        block.views =
            vec![View { title: "Rooms".into(), items: by_room }, View { title: "All".into(), items: everything }];

        let local: Vec<&Device> = devices
            .iter()
            .filter(|device| !matches!(device.reach, Reach::Cloud { .. } | Reach::Unsupported(_)))
            .collect();
        let answering = local.iter().filter(|device| device.reach == Reach::Online).count();
        let source = if devices.is_empty() { "No devices" } else { "Local" };
        block.footer = Some(Footer {
            text: format!("{source}, {answering} of {} reachable", local.len()),
            good: answering > 0,
            actions: vec![FooterAction {
                title: "All off".into(),
                enabled: lit > 0,
                action: Some(tool_action("all_off", json!({}))),
            }],
        });
        block
    }

    /// The bar chip, while a light, plug or climate device is on: the one
    /// light's name or how many are on, and what the climate does.
    fn bar(&mut self) -> Bar {
        if self.problem.is_some() {
            return Bar::default();
        }
        let devices = self.shown();
        let lit: Vec<&Device> =
            devices.iter().filter(|device| matches!(device.kind, Kind::Light | Kind::Plug) && is_on(device)).collect();
        let climate = self.climate(&devices).filter(|climate| is_on(climate));
        let mut parts = Vec::new();
        match lit.as_slice() {
            [] => {}
            [only] => parts.push(only.name.clone()),
            many => parts.push(format!("{} on", many.len())),
        }
        if let Some(climate) = climate {
            let mode = climate.state.mode.as_deref().map_or_else(|| "On".into(), mode_label);
            parts.push(match climate.state.target {
                Some(target) => format!("{mode} to {}", degrees(target)),
                None => mode,
            });
        }
        if parts.is_empty() {
            return Bar::default();
        }
        let (icon, tint) = match climate {
            Some(climate) if lit.is_empty() => {
                let card = climate_card(climate, &devices);
                (card.icon, card.tint)
            }
            _ => (Some("glyph:bulb".into()), Tint::Warm),
        };
        Bar { chips: vec![BarChip { label: parts.join(" \u{b7} "), icon, tint, action: None }] }
    }

    /// The lock screen's reading: the indoor temperature, from the climate
    /// device the card shows, else from a sensor. Nothing to act on, and no
    /// names of devices or rooms.
    fn lock(&mut self) -> Lock {
        if self.problem.is_some() {
            return Lock::default();
        }
        let devices = self.shown();
        let climate =
            self.climate(&devices).filter(|device| reachable(device)).and_then(|device| device.state.temperature);
        let sensor = || {
            devices
                .iter()
                .filter(|device| device.kind == Kind::Sensor && reachable(device))
                .find_map(|device| device.state.temperature)
        };
        let Some(temperature) = climate.or_else(sensor) else { return Lock::default() };
        Lock {
            readings: vec![Reading {
                title: "Indoors".into(),
                value: degrees(temperature),
                icon: Some("glyph:thermometer".into()),
            }],
        }
    }

    /// Every device for Settings, with the addresses heard on the network.
    fn devices(&self) -> Value {
        let devices: Vec<Value> = self
            .snapshot()
            .iter()
            .map(|device| {
                let place =
                    [device.home.clone(), device.room.clone()].into_iter().flatten().collect::<Vec<_>>().join(", ");
                json!({
                    "name": device.name,
                    "place": place,
                    "status": device_detail(device),
                    "answering": reachable(device),
                })
            })
            .collect();
        let discovered: Vec<Value> = self
            .service
            .as_ref()
            .map(HomeService::discovered)
            .unwrap_or_default()
            .into_iter()
            .map(|(device_id, host, version)| {
                json!({ "device_id": device_id, "host": host.to_string(), "version": version.map(|version| version.as_str().to_string()) })
            })
            .collect();
        json!({
            "connected": self.service.is_some(),
            "problem": self.problem.map(Problem::text),
            "devices": devices,
            "discovered": discovered,
        })
    }

    /// `home words` in the launcher.
    fn search(&mut self, query: &str) -> Value {
        if let Some(problem) = self.problem {
            let actions = if problem == Problem::NoDevices {
                vec![json!({ "title": "Set up", "type": "settings", "page": "Smart home" })]
            } else {
                Vec::new()
            };
            return json!({ "rows": [{ "title": problem.text(), "icon": "glyph:home", "actions": actions }] });
        }
        let devices = self.shown();
        let mut words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let number = words.last().and_then(|word| word.trim_end_matches(['%', '\u{b0}']).parse::<f64>().ok());
        if number.is_some() {
            words.pop();
        }
        let mut rows = Vec::new();
        let lit: Vec<&Device> =
            devices.iter().filter(|device| matches!(device.kind, Kind::Light | Kind::Plug) && is_on(device)).collect();
        let asks_off = words.is_empty() || words.iter().all(|word| ["all", "lights", "off"].contains(&word.as_str()));
        if asks_off && number.is_none() && !lit.is_empty() {
            rows.push(json!({
                "title": "All lights off",
                "detail": format!("{} on now", lit.len()),
                "icon": "glyph:bulb",
                "actions": [{ "title": "Switch off", "type": "tool", "name": "all_off", "arguments": {} }],
            }));
        }
        let matches = |text: &str| {
            let text = text.to_lowercase();
            words
                .iter()
                .all(|word| text.contains(word.as_str()) || ["all", "lights", "off", "on"].contains(&word.as_str()))
        };
        // Rooms, to switch everything in them.
        let mut rooms: Vec<String> = devices.iter().filter_map(|device| device.room.clone()).collect();
        rooms.sort();
        rooms.dedup();
        if !words.is_empty() && number.is_none() {
            for room in rooms.iter().filter(|room| matches(room)) {
                let switchable: Vec<&Device> = devices
                    .iter()
                    .filter(|device| {
                        device.room.as_ref() == Some(room) && matches!(device.kind, Kind::Light | Kind::Plug)
                    })
                    .collect();
                if switchable.is_empty() {
                    continue;
                }
                let any_on = switchable.iter().any(|device| is_on(device));
                rows.push(json!({
                    "title": format!("{room}: {}", if any_on { "switch off" } else { "switch on" }),
                    "detail": format!("{} lights and plugs", switchable.len()),
                    "icon": "glyph:home",
                    "actions": [{ "title": if any_on { "Switch off" } else { "Switch on" }, "type": "tool", "name": "room",
                                  "arguments": { "room": room, "on": !any_on } }],
                }));
            }
        }
        for device in devices.iter().filter(|device| device.kind != Kind::Sensor) {
            let place = device.room.clone().unwrap_or_default();
            if !matches(&format!("{} {place}", device.name)) {
                continue;
            }
            let icon = device_icon(device);
            let detail = device_detail(device);
            // What does not answer is shown, without an action that cannot work.
            if device.reach != Reach::Online {
                rows.push(json!({ "title": device.name, "detail": detail, "icon": icon }));
                continue;
            }
            match (number, device.kind) {
                (Some(value), Kind::Climate) => rows.push(json!({
                    "title": format!("Set {} to {}", device.name, degrees(value)),
                    "detail": detail, "icon": icon,
                    "actions": [{ "title": "Set", "type": "tool", "name": "set",
                                  "arguments": { "device": device.device_id, "change": "target", "value": value } }],
                })),
                (Some(value), Kind::Light) if device.dimmable && (1.0..=100.0).contains(&value) => rows.push(json!({
                    "title": format!("Set {} to {value:.0}%", device.name),
                    "detail": detail, "icon": icon,
                    "actions": [{ "title": "Set", "type": "tool", "name": "set",
                                  "arguments": { "device": device.device_id, "change": "brightness", "value": value.round() } }],
                })),
                (Some(_), _) => {}
                (None, _) => {
                    let lit = device.state.is_on == Some(true);
                    rows.push(json!({
                        "title": format!("{}: {}", device.name, if lit { "switch off" } else { "switch on" }),
                        "detail": detail, "icon": icon,
                        "actions": [{ "title": if lit { "Switch off" } else { "Switch on" }, "type": "tool", "name": "set",
                                      "arguments": { "device": device.device_id, "change": "switch", "value": !lit } }],
                    }));
                }
            }
        }
        json!({ "rows": rows })
    }

    /// `tileception msg home ...`, parsed by the compositor's rules.
    fn command(&mut self, text: &str) -> Result<String, String> {
        let command = crate::home::parse(text)?;
        if self.service.is_none() {
            return Err(self.unavailable());
        }
        match command {
            HomeCommand::Status => Ok(self.status()),
            HomeCommand::Why(name) => self.why(&name),
            HomeCommand::AllOff => {
                self.all_off()?;
                Ok(String::new())
            }
            HomeCommand::Set(name, wanted) => {
                let device = self.device(&name)?;
                let change = match wanted {
                    Wanted::Toggle => Change::Switch(device.state.is_on != Some(true)),
                    Wanted::Change(change) => change,
                };
                self.send(device.device_id, change)?;
                Ok(String::new())
            }
            HomeCommand::Select(_) | HomeCommand::Fold(_) | HomeCommand::View(_) | HomeCommand::Enable(_) => {
                Err("Tileception handles this command itself".into())
            }
        }
    }

    fn all_off(&self) -> Result<Value, String> {
        let service = self.service.as_ref().ok_or_else(|| self.unavailable())?;
        service.send(Request::AllOff);
        Ok(json!({ "ok": true }))
    }

    fn status(&mut self) -> String {
        let devices = self.snapshot();
        self.follow_place(&devices);
        let place = self.place.clone().unwrap_or_else(|| "-".into());
        let mut out = format!("home: {place}\n");
        for device in &devices {
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

    /// Why a device is not answering, and what to check.
    fn why(&self, name: &str) -> Result<String, String> {
        let device = self.device(name)?;
        Ok(match &device.reach {
            Reach::Online => format!("{} is online ({}).", device.name, device_detail(&device)),
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
}

impl Plugin for HomePlugin {
    fn tools(&self) -> Value {
        let mut set = tool(
            "set",
            "Change a device: switch (true/false), brightness or colour (0-100), target (degrees), mode or fan",
            &["device", "change"],
        );
        set["inputSchema"]["properties"]["value"] = json!({});
        let mut room = tool("room", "Switch the lights and plugs in a room on or off", &["room"]);
        room["inputSchema"]["properties"]["on"] = json!({ "type": "boolean" });
        let mut step = tool("step_target", "Raise or lower a climate device's target by steps", &["device"]);
        step["inputSchema"]["properties"]["steps"] = json!({ "type": "integer" });
        json!([
            tool("launcher_info", "Launcher metadata", &[]),
            tool("launcher_search", "Devices and rooms matching the words", &["prefix", "query"]),
            set,
            tool("toggle", "Switch a device on or off", &["device"]),
            step,
            room,
            tool("all_off", "Switch off every light and plug that is on, never climate devices", &[]),
            tool("next_climate", "Show the next climate device in the panel", &["device"]),
            tool("command", "Run a `tileception msg home` command, such as `status` or `why Lamp`", &["text"]),
        ])
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String> {
        self.follow_file();
        let text = |key: &str| arguments[key].as_str().unwrap_or_default().to_string();
        match name {
            "launcher_info" => Ok(json!({
                "name": "Home",
                "description": "Switch lights and plugs, and set the air conditioning",
                "icon": "glyph:home",
                "prefixes": ["home"],
            })),
            "launcher_search" => Ok(self.search(&text("query"))),
            "set" => {
                let device = self.device(&text("device"))?;
                let value = &arguments["value"];
                let percent = || {
                    value
                        .as_f64()
                        .map(|value| value.round().clamp(0.0, 100.0) as u32)
                        .ok_or("value must be a number from 0 to 100")
                };
                let change = match text("change").as_str() {
                    "switch" => Change::Switch(value.as_bool().ok_or("value must be true or false")?),
                    "brightness" => Change::Brightness(percent()?.max(1)),
                    "colour" | "color" | "warmth" => Change::ColourTemp(percent()?),
                    "target" => Change::Target(value.as_f64().ok_or("value must be a temperature")?),
                    "mode" => Change::Mode(value.as_str().ok_or("value must be a mode")?.to_string()),
                    "fan" => Change::Fan(value.as_str().ok_or("value must be a fan speed")?.to_string()),
                    other => return Err(format!("unknown change {other:?}")),
                };
                self.send(device.device_id, change)
            }
            "toggle" => {
                let device = self.device(&text("device"))?;
                let lit = device.state.is_on == Some(true);
                self.send(device.device_id, Change::Switch(!lit))
            }
            "step_target" => {
                let device = self.device(&text("device"))?;
                let step = device.target_range.map_or(0.5, |(_, _, step)| step.max(0.1));
                let steps = arguments["steps"].as_i64().unwrap_or(1) as f64;
                let target = device.state.target.unwrap_or(21.0) + step * steps;
                let target = device.target_range.map_or(target, |(low, high, _)| target.clamp(low, high));
                self.send(device.device_id, Change::Target(target))
            }
            "room" => {
                let room = text("room");
                let wanted = arguments["on"].as_bool().unwrap_or(false);
                let devices: Vec<Device> = self
                    .shown()
                    .into_iter()
                    .filter(|device| {
                        device.room.as_deref() == Some(room.as_str()) && matches!(device.kind, Kind::Light | Kind::Plug)
                    })
                    .collect();
                if devices.is_empty() {
                    return Err(format!("no lights or plugs in {room:?}"));
                }
                for device in devices.into_iter().filter(|device| device.reach == Reach::Online) {
                    self.send(device.device_id, Change::Switch(wanted))?;
                }
                Ok(json!({ "ok": true }))
            }
            "all_off" => self.all_off(),
            "next_climate" => {
                let devices = self.shown();
                let climates: Vec<String> = devices
                    .iter()
                    .filter(|device| device.kind == Kind::Climate)
                    .map(|device| device.device_id.clone())
                    .collect();
                let current = text("device");
                if let Some(index) = climates.iter().position(|candidate| *candidate == current) {
                    self.climate = climates.get((index + 1) % climates.len()).cloned();
                }
                if let Some(notifier) = &self.notifier {
                    notifier.changed(PANEL);
                    notifier.changed(BAR);
                    notifier.changed(LOCK);
                }
                Ok(json!({ "ok": true }))
            }
            "command" => self.command(&text("text")).map(|text| json!({ "text": text })),
            _ => Err(format!("Unknown tool {name}")),
        }
    }

    fn resources(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (PANEL, "Smart home block"),
            (BAR, "Smart home chip"),
            (LOCK, "Indoor temperature on the lock screen"),
            (DEVICES, "Smart home devices"),
        ]
    }

    fn read(&mut self, uri: &str) -> Result<Value, String> {
        self.follow_file();
        match uri {
            PANEL => serde_json::to_value(self.block()).map_err(|error| error.to_string()),
            BAR => serde_json::to_value(self.bar()).map_err(|error| error.to_string()),
            LOCK => serde_json::to_value(self.lock()).map_err(|error| error.to_string()),
            DEVICES => Ok(self.devices()),
            _ => Err(format!("no resource {uri}")),
        }
    }

    fn start(&mut self, notifier: Notifier) {
        self.notifier = Some(notifier.clone());
        self.connect();
        self.refresh_at_startup();
        // A changed device file shows as an update; reading it connects again.
        if let Some(path) = self.path.clone() {
            let mut seen = modified(&path);
            let _ = std::thread::Builder::new().name("home-watch".into()).spawn(move || {
                loop {
                    std::thread::sleep(WATCH);
                    let now = modified(&path);
                    if now != seen {
                        seen = now;
                        notifier.changed(PANEL);
                        notifier.changed(BAR);
                        notifier.changed(LOCK);
                        notifier.changed(DEVICES);
                    }
                }
            });
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok()
}

fn tool_action(name: &str, arguments: Value) -> ActionKind {
    ActionKind::Tool { name: name.into(), arguments }
}

fn set_action(device: &Device, change: &str, value: Value) -> ActionKind {
    tool_action("set", json!({ "device": device.device_id, "change": change, "value": value }))
}

fn device_icon(device: &Device) -> &'static str {
    if !reachable(device) && !matches!(device.reach, Reach::Connecting | Reach::Cloud { .. }) {
        return "glyph:wifi_off";
    }
    match device.kind {
        Kind::Light => "glyph:bulb",
        Kind::Plug => "glyph:plug",
        Kind::Sensor => "glyph:drop",
        Kind::Climate => "glyph:thermometer",
        _ => "glyph:home",
    }
}

/// A light, plug or sensor's row.
fn device_row(device: &Device) -> Row {
    let lit = is_on(device);
    let tint = match device.kind {
        _ if !reachable(device) && !matches!(device.reach, Reach::Connecting | Reach::Cloud { .. }) => Tint::Faded,
        Kind::Light if lit => Tint::Warm,
        Kind::Plug if lit => Tint::Accent,
        Kind::Sensor => Tint::Accent,
        _ => Tint::Muted,
    };
    let enabled = device.reach == Reach::Online;
    let mut levels = Vec::new();
    if device.dimmable {
        levels.push(Level {
            title: "Brightness".into(),
            value: device.state.shown_brightness().unwrap_or(100),
            unit: "%".into(),
            action: Some(tool_action("set", json!({ "device": device.device_id, "change": "brightness" }))),
            ..Level::default()
        });
    }
    if device.tunable && !device.state.in_colour() {
        levels.push(Level {
            title: "Colour".into(),
            value: device.state.colour_temp.unwrap_or(0),
            look: LevelLook::Warmth,
            steps: vec!["Warm".into(), "Neutral".into(), "Cool".into()],
            action: Some(tool_action("set", json!({ "device": device.device_id, "change": "colour" }))),
            ..Level::default()
        });
    }
    Row {
        key: device.device_id.clone(),
        title: device.name.clone(),
        detail: device_detail(device),
        icon: Some(device_icon(device).into()),
        tint,
        dim: !(lit || device.kind == Kind::Sensor),
        alert: matches!(device.reach, Reach::Unreachable { .. } | Reach::Unsupported(_)),
        enabled,
        switch: (device.kind != Kind::Sensor).then(|| Switch {
            checked: lit,
            pending: device.pending,
            action: Some(tool_action("toggle", json!({ "device": device.device_id }))),
        }),
        levels,
    }
}

/// The climate device's card: the room temperature, its target, mode and
/// fan, and the last hours as a trend line.
fn climate_card(device: &Device, devices: &[Device]) -> Card {
    let lit = is_on(device);
    let (icon, tint) = match device.state.mode.as_deref() {
        _ if !lit => ("glyph:thermometer", Tint::Muted),
        Some("cold") => ("glyph:snowflake", Tint::Cool),
        Some("hot") => ("glyph:flame", Tint::Danger),
        Some("wet") => ("glyph:drop", Tint::Accent),
        Some("wind") => ("glyph:fan", Tint::Accent),
        _ => ("glyph:thermometer", Tint::Accent),
    };
    let status = if !reachable(device) {
        device_detail(device)
    } else if lit {
        device.state.mode.as_deref().map(mode_label).unwrap_or_else(|| "On".into())
    } else {
        "Off".into()
    };
    let humidity = devices
        .iter()
        .filter(|other| other.kind == Kind::Sensor && reachable(other))
        .find_map(|sensor| sensor.state.humidity);
    let climates = devices.iter().filter(|other| other.kind == Kind::Climate).count();
    let choices = |title: &str, values: &[String], chosen: Option<&str>, change: &str| Choices {
        title: title.into(),
        options: values
            .iter()
            .map(|value| Choice {
                title: mode_label(value),
                selected: chosen == Some(value.as_str()),
                action: Some(set_action(device, change, json!(value))),
            })
            .collect(),
    };
    let now = SystemTime::now();
    let age = |when: SystemTime| now.duration_since(when).map_or(0.0, |age| age.as_secs_f64());
    let trend = (device.history.len() >= 2).then(|| Trend {
        span: tileception_home::history::WINDOW.as_secs_f64(),
        lines: vec![
            Line {
                dashed: true,
                points: device.history.iter().filter_map(|sample| Some((age(sample.when), sample.target?))).collect(),
            },
            Line {
                dashed: false,
                points: device
                    .history
                    .iter()
                    .filter_map(|sample| Some((age(sample.when), sample.temperature?)))
                    .collect(),
            },
        ],
    });
    Card {
        key: device.device_id.clone(),
        title: format!("{}, {status}", device.name),
        detail: humidity.map(|humidity| format!("{humidity:.0}% humidity")),
        value: device.state.temperature.map(degrees).unwrap_or_else(|| "-".into()),
        icon: Some(icon.into()),
        tint,
        lit,
        enabled: device.reach == Reach::Online,
        title_action: (climates > 1).then(|| tool_action("next_climate", json!({ "device": device.device_id }))),
        stepper: Some(Stepper {
            value: device.state.target.map(degrees).unwrap_or_else(|| "-".into()),
            minus: Some(tool_action("step_target", json!({ "device": device.device_id, "steps": -1 }))),
            plus: Some(tool_action("step_target", json!({ "device": device.device_id, "steps": 1 }))),
        }),
        switch: Some(Switch {
            checked: device.state.is_on == Some(true),
            pending: device.pending,
            action: Some(tool_action("toggle", json!({ "device": device.device_id }))),
        }),
        choices: vec![
            choices("Mode", &device.modes, device.state.mode.as_deref(), "mode"),
            choices("Fan", &device.fan_speeds, device.state.fan.as_deref(), "fan"),
        ],
        trend,
        buttons: Vec::new(),
        progress: None,
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

fn warmth(percent: u32) -> &'static str {
    match percent {
        0..=33 => "Warm",
        34..=66 => "Neutral",
        _ => "Cool",
    }
}

/// What a device's row says under its name.
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

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions")]
mod tests {
    use super::*;

    /// A plugin on the simulated household, connected until all answer.
    fn simulated() -> (HomePlugin, tileception_home::sim::Household) {
        let household = tileception_home::sim::household().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "tileception-home-plugin-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("devices.json");
        store::save(&path, &household.file).unwrap();
        let mut plugin = HomePlugin::new(&["--devices".into(), path.to_string_lossy().into_owned()]).unwrap();
        plugin.connect();
        for _ in 0..100 {
            if plugin.snapshot().iter().all(|device| device.reach != Reach::Connecting) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        (plugin, household)
    }

    #[test]
    fn a_missing_file_asks_to_set_up() {
        let mut plugin =
            HomePlugin::new(&["--devices".into(), "/nonexistent/tileception/devices.json".into()]).unwrap();
        plugin.connect();
        let block = plugin.block();
        let notice = block.notice.unwrap();
        assert_eq!(notice.action, Some(ActionKind::Settings { page: "Smart home".into() }));
        assert!(plugin.call("toggle", &json!({ "device": "Lamp" })).is_err());
        assert!(plugin.bar().chips.is_empty());
        assert!(plugin.lock().readings.is_empty());
    }

    #[test]
    fn the_lock_screen_gets_only_the_indoor_temperature() {
        let (mut plugin, _household) = simulated();
        let mut lock = plugin.lock();
        for _ in 0..100 {
            if !lock.readings.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            lock = plugin.lock();
        }
        assert_eq!(lock.readings.len(), 1, "{lock:?}");
        assert_eq!(lock.readings[0].title, "Indoors");
        assert_eq!(lock.readings[0].value, "23.5\u{b0}");
        // Read, as the compositor reads it: no device or room names, nothing
        // to act on.
        let value = plugin.read(LOCK).unwrap();
        let text = value.to_string();
        assert!(!text.contains("Airco") && !text.contains("Living room") && !text.contains("action"), "{text}");
        assert!(plugin.resources().iter().any(|(uri, _)| *uri == LOCK));
    }

    #[test]
    fn the_block_lists_the_simulated_household() {
        let (mut plugin, _household) = simulated();
        let block = plugin.block();
        assert!(block.notice.is_none());
        assert_eq!(block.views.len(), 2);
        let rooms = &block.views[0].items;
        assert!(rooms.iter().any(|item| matches!(item, Item::Group(_))), "rooms are groups");
        let mut rows = 0;
        for item in rooms {
            if let Item::Group(group) = item {
                rows += group.items.len();
            }
        }
        assert!(rows > 0);
        assert!(block.footer.is_some());
        // The block is what the compositor reads.
        let value = serde_json::to_value(&block).unwrap();
        let read: Block = serde_json::from_value(value).unwrap();
        assert_eq!(read, block);
    }

    #[test]
    fn toggling_a_light_turns_it_on() {
        let (mut plugin, _household) = simulated();
        let light = plugin
            .snapshot()
            .into_iter()
            .find(|device| {
                device.kind == Kind::Light && device.reach == Reach::Online && device.state.is_on != Some(true)
            })
            .expect("a simulated light that is off");
        plugin.call("toggle", &json!({ "device": light.name })).unwrap();
        let mut lit = false;
        for _ in 0..100 {
            lit = plugin.device(&light.name).unwrap().state.is_on == Some(true);
            if lit {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(lit);
        assert!(plugin.block().active > 0);
        let rows = plugin.search("");
        assert_eq!(rows["rows"][0]["title"], "All lights off");
        // The chip names what is on and opens the block when clicked.
        let chip = plugin.bar().chips.pop().expect("a chip while something is on");
        assert!(chip.label.contains(" on") || chip.label.contains(&light.name), "{}", chip.label);
        assert_eq!(chip.action, None);
        plugin.all_off().unwrap();
        for _ in 0..100 {
            if plugin.shown().iter().all(|device| device.kind != Kind::Light || !is_on(device)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let left = plugin.bar().chips;
        assert!(left.iter().all(|chip| !chip.label.contains(&light.name)), "{left:?}");
    }
}
