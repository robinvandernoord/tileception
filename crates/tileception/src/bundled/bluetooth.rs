//! The `bluetooth` plugin: paired devices through BlueZ in the panel block,
//! `bt` in the launcher and `tileception msg bluetooth`.
//!
//! It also reconnects devices that were connected before the computer slept
//! or the adapter was switched off, and says why when that fails. BlueZ
//! reconnects few devices itself: keyboards and mice usually come back on
//! their own, headphones and speakers usually do not.
//!
//! A watcher follows adapters and devices on the system bus; a thread of
//! its own decides what to reconnect; another makes the calls, which can
//! take seconds each.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zbus::blocking::{Connection, MessageIterator, fdo::DBusProxy};
use zbus::message::Type;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::{Notifier, Plugin, tool};
use crate::{
    panel::block::{
        Bar, BarChip, Block, Card, Choice, Choices, Footer, FooterAction, Group, Item, Notice, Row, Switch, Tint, View,
    },
    plugins::{ActionKind, BAR, PANEL},
};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const BATTERY: &str = "org.bluez.Battery1";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const ICON: &str = "glyph:bluetooth";
/// After waking or switching the adapter on, devices that come back by
/// themselves get this long first.
const SETTLE: Duration = if cfg!(test) { Duration::from_millis(200) } else { Duration::from_secs(3) };
/// A device that disconnects this shortly before sleep or the adapter going
/// off was taken down with it, not switched off by its owner.
const GRACE: Duration = Duration::from_secs(5);
/// How long searching for devices to pair lasts.
const SEARCH: Duration = Duration::from_secs(30);
/// A stand-in for BlueZ on another bus, for tests: its address.
const BUS_VARIABLE: &str = "TILECEPTION_BLUETOOTH_BUS";

/// The `[bluetooth]` section.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BluetoothConfig {
    /// Reconnect paired devices after sleep and when the adapter comes on.
    pub reconnect: bool,
}

impl Default for BluetoothConfig {
    fn default() -> Self {
        Self { reconnect: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Adapter {
    path: String,
    name: String,
    /// On and not on its way off.
    powered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Device {
    path: String,
    adapter: String,
    name: String,
    address: String,
    paired: bool,
    connected: bool,
    /// Percent, for devices that report it.
    battery: Option<u8>,
    /// It said its name; nameless devices are left out of the nearby list.
    named: bool,
    /// Signal strength in dBm while searching.
    rssi: Option<i16>,
}

/// What BlueZ has, as one value so changes compare easily.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct State {
    /// BlueZ is running.
    available: bool,
    adapters: Vec<Adapter>,
    devices: Vec<Device>,
}

impl State {
    fn powered(&self, adapter: &str) -> bool {
        self.adapters.iter().any(|candidate| candidate.path == adapter && candidate.powered)
    }

    fn device(&self, path: &str) -> Option<&Device> {
        self.devices.iter().find(|device| device.path == path)
    }

    /// Paired devices, connected first, then by name.
    fn paired(&self) -> Vec<&Device> {
        let mut paired: Vec<&Device> = self.devices.iter().filter(|device| device.paired).collect();
        paired.sort_by_key(|device| (!device.connected, device.name.to_lowercase()));
        paired
    }

    /// Named devices found while searching that are not paired, strongest
    /// signal first.
    fn nearby(&self) -> Vec<&Device> {
        let mut nearby: Vec<&Device> = self.devices.iter().filter(|device| !device.paired && device.named).collect();
        nearby.sort_by_key(|device| (std::cmp::Reverse(device.rssi.unwrap_or(i16::MIN)), device.name.to_lowercase()));
        nearby
    }

    /// A paired device by name, address or object path.
    fn find(&self, wanted: &str) -> Result<&Device, String> {
        pick(&self.paired(), wanted).ok_or_else(|| format!("no paired device called {:?}", wanted.trim()))
    }

    /// A nearby device that is not paired.
    fn find_nearby(&self, wanted: &str) -> Result<&Device, String> {
        pick(&self.nearby(), wanted).ok_or_else(|| format!("no device called {:?} nearby; search first", wanted.trim()))
    }
}

/// By object path or address, then the whole name, then part of it.
fn pick<'a>(devices: &[&'a Device], wanted: &str) -> Option<&'a Device> {
    let wanted = wanted.trim();
    let lower = wanted.to_lowercase();
    devices
        .iter()
        .find(|device| device.path == wanted || device.address.eq_ignore_ascii_case(wanted))
        .or_else(|| devices.iter().find(|device| device.name.eq_ignore_ascii_case(wanted)))
        .or_else(|| devices.iter().find(|device| device.name.to_lowercase().contains(&lower)))
        .copied()
}

/// A code to confirm or to type while pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Prompt {
    device: String,
    code: String,
    /// Asks whether the code matches; otherwise it is to be typed on the device.
    confirm: bool,
}

/// Why Bluetooth cannot be used now, and what clicking the notice does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Problem {
    NotRunning,
    NoAdapter,
    HardBlocked,
    SoftBlocked,
    Off,
}

impl Problem {
    fn find(state: &State, rfkill: Rfkill) -> Option<Self> {
        if !state.available {
            return Some(Problem::NotRunning);
        }
        if state.adapters.iter().any(|adapter| adapter.powered) {
            return None;
        }
        Some(match rfkill {
            Rfkill::Hard => Problem::HardBlocked,
            Rfkill::Soft => Problem::SoftBlocked,
            Rfkill::Clear if state.adapters.is_empty() => Problem::NoAdapter,
            Rfkill::Clear => Problem::Off,
        })
    }

    fn summary(self) -> &'static str {
        match self {
            Problem::NotRunning => "Not running",
            Problem::NoAdapter => "No adapter",
            Problem::HardBlocked | Problem::SoftBlocked => "Blocked",
            Problem::Off => "Off",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Problem::NotRunning => "The Bluetooth service is not running. Start it with `systemctl start bluetooth`.",
            Problem::NoAdapter => "No Bluetooth adapter found.",
            Problem::HardBlocked => "Switched off by a hardware switch or key.",
            Problem::SoftBlocked => "Blocked by rfkill. Click to unblock it and switch it on.",
            Problem::Off => "Bluetooth is off. Click to switch it on.",
        }
    }

    fn fixable(self) -> bool {
        matches!(self, Problem::SoftBlocked | Problem::Off)
    }
}

/// Which devices to bring back. A device is remembered while connected; when
/// it disconnects with its adapter on, the time is noted, and it is left
/// alone unless sleep or the adapter going off follows within [`GRACE`].
#[derive(Debug, Default)]
struct Memory {
    devices: BTreeMap<String, Option<Instant>>,
}

impl Memory {
    /// Follows connections from one state to the next. While `holding`
    /// (asleep, or waiting to reconnect), disconnects are expected.
    fn follow(&mut self, old: &State, new: &State, now: Instant, holding: bool) {
        for device in &new.devices {
            let was_connected = old.device(&device.path).is_some_and(|before| before.connected);
            if device.connected && device.paired {
                self.devices.insert(device.path.clone(), None);
            } else if was_connected
                && !device.connected
                && !holding
                && new.powered(&device.adapter)
                && let Some(entry) = self.devices.get_mut(&device.path)
            {
                *entry = Some(now);
            }
        }
        // Unpaired or removed devices are forgotten; while BlueZ restarts or
        // an adapter is unplugged, they are kept.
        if new.available {
            self.devices.retain(|path, _| match new.device(path) {
                Some(device) => device.paired,
                None => !new.adapters.iter().any(|adapter| path.starts_with(&format!("{}/", adapter.path))),
            });
        }
    }

    /// Sleep or an adapter going off: what disconnected just before went
    /// down with it.
    fn hold(&mut self, now: Instant) {
        for entry in self.devices.values_mut() {
            if entry.is_some_and(|since| now.saturating_duration_since(since) < GRACE) {
                *entry = None;
            }
        }
    }

    /// Remembered devices that are not connected, on adapters that are on.
    fn targets(&self, state: &State) -> Vec<(String, String)> {
        self.devices
            .iter()
            .filter(|(_, since)| since.is_none())
            .filter_map(|(path, _)| state.device(path))
            .filter(|device| device.paired && !device.connected && state.powered(&device.adapter))
            .map(|device| (device.path.clone(), device.name.clone()))
            .collect()
    }
}

/// For the thread that decides.
enum Event {
    State(State),
    /// True when the computer is about to sleep, false when it woke.
    Sleep(bool),
    /// Reconnect now; the names tried go back.
    Reconnect(mpsc::Sender<Vec<String>>),
    /// A reconnect finished: (path, name) that came back, and (path, name, why) that did not.
    Reconnected {
        connected: Vec<(String, String)>,
        failed: Vec<(String, String, String)>,
    },
    /// Something asked for by hand finished; on failure, a summary and why.
    Done {
        path: Option<String>,
        failure: Option<(String, String)>,
    },
    /// Start searching for devices to pair, or stop.
    Search(bool),
    /// Ask about pairing started on the other device (summary, body) in a
    /// notification with buttons, or close that notification.
    Ask(Option<(String, String)>),
}

/// For the thread that calls BlueZ.
enum Request {
    Reconnect(Vec<(String, String)>),
    Connect { path: String, name: String },
    Disconnect { path: String, name: String },
    Power { adapters: Vec<String>, powered: bool, unblock: bool },
    Discover { adapters: Vec<String>, start: bool },
    Pair { path: String, name: String },
    Forget { adapter: String, path: String, name: String },
}

/// What the threads and the tools share.
#[derive(Default)]
struct Shared {
    state: State,
    /// Devices being connected, shown as pending switches.
    connecting: HashSet<String>,
    /// Devices being paired.
    pairing: HashSet<String>,
    /// Searching for devices to pair.
    searching: bool,
    /// The code shown while pairing.
    prompt: Option<Prompt>,
    /// Why pairing cannot work, said by the agent before BlueZ fails it.
    pair_note: Option<String>,
    /// The notification asking about pairing started elsewhere.
    ask_id: Option<u32>,
    /// Could not reach the system bus.
    broken: Option<String>,
}

pub struct BluetoothPlugin {
    reconnect: bool,
    shared: Arc<Mutex<Shared>>,
    events: Option<mpsc::Sender<Event>>,
    requests: Option<mpsc::Sender<Request>>,
    notifier: Option<Notifier>,
    /// Answers to the code shown while pairing, for the agent.
    answers: Option<async_channel::Sender<bool>>,
}

impl BluetoothPlugin {
    pub fn new(args: &[String]) -> Self {
        Self {
            reconnect: !args.iter().any(|arg| arg == "--no-reconnect"),
            shared: Arc::default(),
            events: None,
            requests: None,
            notifier: None,
            answers: None,
        }
    }

    fn panel_changed(&self) {
        if let Some(notifier) = &self.notifier {
            notifier.changed(PANEL);
        }
    }

    fn search_devices(&self, start: bool) -> Result<Value, String> {
        let events = self.events.as_ref().ok_or_else(|| self.unavailable())?;
        if start && Problem::find(&self.shared().state, rfkill()).is_some() {
            return Err(self.unavailable());
        }
        events.send(Event::Search(start)).map_err(|_| "Bluetooth stopped".to_string())?;
        Ok(json!({ "ok": true }))
    }

    fn pair(&self, wanted: &str) -> Result<Value, String> {
        let (path, name) = {
            let mut shared = self.shared();
            let device = shared.state.find_nearby(wanted)?;
            let found = (device.path.clone(), device.name.clone());
            shared.pairing.insert(found.0.clone());
            found
        };
        self.panel_changed();
        self.request(Request::Pair { path, name })
    }

    /// Answers the code shown: it matches, or it does not.
    fn answer(&self, accept: bool) -> Result<Value, String> {
        if !self.shared().prompt.as_ref().is_some_and(|prompt| prompt.confirm) {
            return Err("no code is waiting for an answer".into());
        }
        let answers = self.answers.as_ref().ok_or_else(|| self.unavailable())?;
        answers.try_send(accept).map_err(|_| "Bluetooth stopped".to_string())?;
        Ok(json!({ "ok": true }))
    }

    fn forget(&self, wanted: &str) -> Result<Value, String> {
        let (adapter, path, name) = {
            let shared = self.shared();
            let device = shared.state.find(wanted)?;
            (device.adapter.clone(), device.path.clone(), device.name.clone())
        };
        self.request(Request::Forget { adapter, path, name })
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn request(&self, request: Request) -> Result<Value, String> {
        let requests = self.requests.as_ref().ok_or_else(|| self.unavailable())?;
        requests.send(request).map_err(|_| "Bluetooth stopped".to_string())?;
        Ok(json!({ "ok": true }))
    }

    fn unavailable(&self) -> String {
        let shared = self.shared();
        match &shared.broken {
            Some(error) => format!("Bluetooth is unavailable: {error}"),
            None => Problem::find(&shared.state, rfkill()).map_or("Bluetooth is not ready", Problem::text).into(),
        }
    }

    /// Connects (`Some(true)`), disconnects or, with `None`, toggles a device.
    fn connect(&self, wanted: &str, want: Option<bool>) -> Result<Value, String> {
        let (path, name, connected) = {
            let mut shared = self.shared();
            let device = shared.state.find(wanted)?;
            if !shared.state.powered(&device.adapter) {
                return Err("Bluetooth is off".into());
            }
            let found = (device.path.clone(), device.name.clone(), device.connected);
            if !found.2 && want != Some(false) {
                shared.connecting.insert(found.0.clone());
            }
            found
        };
        self.panel_changed();
        match want.unwrap_or(!connected) {
            true if connected => Ok(json!({ "ok": true })),
            true => self.request(Request::Connect { path, name }),
            false => self.request(Request::Disconnect { path, name }),
        }
    }

    fn power(&self, powered: bool) -> Result<Value, String> {
        let (adapters, unblock) = {
            let shared = self.shared();
            let adapters = shared.state.adapters.iter().map(|adapter| adapter.path.clone()).collect();
            (adapters, powered && rfkill() == Rfkill::Soft)
        };
        self.request(Request::Power { adapters, powered, unblock })
    }

    fn reconnect_now(&self) -> Result<Vec<String>, String> {
        let events = self.events.as_ref().ok_or_else(|| self.unavailable())?;
        let (reply, answer) = mpsc::channel();
        events.send(Event::Reconnect(reply)).map_err(|_| "Bluetooth stopped".to_string())?;
        answer.recv_timeout(Duration::from_secs(5)).map_err(|_| "Bluetooth did not answer".to_string())
    }

    /// The panel block.
    fn block(&self) -> Block {
        let shared = self.shared();
        let state = &shared.state;
        let mut block = Block { title: "Bluetooth".into(), icon: Some(ICON.into()), ..Block::default() };
        if let Some(error) = &shared.broken {
            block.summary = "Unavailable".into();
            block.notice = Some(Notice { text: format!("Bluetooth is unavailable: {error}"), action: None });
            return block;
        }
        if let Some(problem) = Problem::find(state, rfkill()) {
            block.summary = problem.summary().into();
            let action = problem.fixable().then(|| tool_action("power", json!({ "on": true })));
            block.notice = Some(Notice { text: problem.text().into(), action });
            return block;
        }
        let paired = state.paired();
        let connected: Vec<&&Device> = paired.iter().filter(|device| device.connected).collect();
        // Searching and a code to answer unfold the block too.
        block.active = connected.len() as u32 + u32::from(shared.searching) + u32::from(shared.prompt.is_some());
        block.summary = match connected.as_slice() {
            _ if shared.prompt.is_some() => "Pairing".into(),
            [] if shared.searching => "Searching".into(),
            [] => "Nothing connected".into(),
            [device] => match device.battery {
                Some(percent) => format!("{}, {percent}%", device.name),
                None => device.name.clone(),
            },
            several => format!("{} connected", several.len()),
        };
        let mut items: Vec<Item> = Vec::new();
        if let Some(prompt) = &shared.prompt {
            items.push(Item::Card(Box::new(prompt_card(prompt, state))));
        }
        items.extend(paired.iter().map(|device| Item::Row(row(device, shared.connecting.contains(&device.path)))));
        if shared.searching {
            let nearby = state.nearby();
            let detail = if nearby.is_empty() { "Searching\u{2026}" } else { "Click a switch to pair" };
            items.push(Item::Group(Group {
                title: "Nearby".into(),
                detail: Some(detail.into()),
                items: nearby
                    .into_iter()
                    .map(|device| Item::Row(nearby_row(device, shared.pairing.contains(&device.path))))
                    .collect(),
            }));
        } else if paired.is_empty() {
            block.notice = Some(Notice {
                text: "No paired devices yet. Click to search for one.".into(),
                action: Some(tool_action("search", json!({ "on": true }))),
            });
        }
        block.views = vec![View { title: "Devices".into(), items }];
        let adapter = state.adapters.iter().find(|adapter| adapter.powered).map(|adapter| adapter.name.clone());
        let (search, searching) = if shared.searching { ("Stop", false) } else { ("Pair device", true) };
        block.footer = Some(Footer {
            text: adapter.unwrap_or_default(),
            good: true,
            actions: vec![
                FooterAction {
                    title: search.into(),
                    enabled: true,
                    action: Some(tool_action("search", json!({ "on": searching }))),
                },
                FooterAction {
                    title: "Switch off".into(),
                    enabled: true,
                    action: Some(tool_action("power", json!({ "on": false }))),
                },
            ],
        });
        block
    }

    /// `bt words` in the launcher.
    fn search(&self, query: &str) -> Value {
        let shared = self.shared();
        let state = &shared.state;
        let mut words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        // `bt forget <name>` lists paired devices to unpair.
        let forgetting = words.first().is_some_and(|word| word == "forget");
        if forgetting {
            words.remove(0);
        }
        let matches = |text: &str| {
            let text = text.to_lowercase();
            words.iter().all(|word| text.contains(word.as_str()))
        };
        let mut rows = Vec::new();
        let problem = shared.broken.is_none().then(|| Problem::find(state, rfkill())).flatten();
        match problem {
            Some(problem) if problem.fixable() => rows.push(json!({
                "title": "Switch Bluetooth on", "detail": problem.text(), "icon": ICON,
                "actions": [{ "title": "Switch on", "type": "tool", "name": "power", "arguments": { "on": true } }],
            })),
            Some(problem) => rows.push(json!({ "title": problem.text(), "icon": ICON })),
            None if shared.broken.is_some() => {
                rows.push(json!({ "title": "Bluetooth is unavailable", "icon": ICON }));
            }
            None if forgetting => {
                for device in state.paired().into_iter().filter(|device| matches(&device.name)) {
                    rows.push(json!({
                        "title": format!("Forget {}", device.name),
                        "detail": "Unpair it; pair it again to use it",
                        "icon": ICON,
                        "actions": [{ "title": "Forget", "type": "tool", "name": "forget",
                                      "arguments": { "device": device.path } }],
                    }));
                }
            }
            None => {
                for device in state.paired().into_iter().filter(|device| matches(&device.name)) {
                    let verb = if device.connected { "Disconnect" } else { "Connect" };
                    rows.push(json!({
                        "title": format!("{verb} {}", device.name),
                        "detail": detail(device, shared.connecting.contains(&device.path)),
                        "icon": ICON,
                        "actions": [{ "title": verb, "type": "tool", "name": "toggle",
                                      "arguments": { "device": device.path } }],
                    }));
                }
                if matches("switch bluetooth off") || matches("off") {
                    rows.push(json!({
                        "title": "Switch Bluetooth off", "icon": ICON,
                        "actions": [{ "title": "Switch off", "type": "tool", "name": "power", "arguments": { "on": false } }],
                    }));
                }
            }
        }
        json!({ "rows": rows })
    }

    /// `tileception msg bluetooth ...`.
    fn command(&self, text: &str) -> Result<String, String> {
        let text = text.trim();
        let (verb, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
        let rest = rest.trim();
        match (verb, rest.is_empty()) {
            ("" | "status", true) => Ok(self.status()),
            ("reconnect", true) => Ok(match self.reconnect_now()?.as_slice() {
                [] => "nothing to reconnect\n".into(),
                names => format!("reconnecting {}\n", names.join(", ")),
            }),
            ("on" | "off", true) => self.power(verb == "on").map(|_| String::new()),
            ("connect", false) => self.connect(rest, Some(true)).map(|_| String::new()),
            ("disconnect", false) => self.connect(rest, Some(false)).map(|_| String::new()),
            ("search" | "stop", true) => self.search_devices(verb == "search").map(|_| String::new()),
            ("pair", false) => self.pair(rest).map(|_| String::new()),
            ("confirm" | "reject", true) => self.answer(verb == "confirm").map(|_| String::new()),
            ("forget", false) => self.forget(rest).map(|_| String::new()),
            _ => Err("bluetooth takes status, on, off, reconnect, connect <device>, disconnect <device>, search, \
                      stop, pair <device>, confirm, reject or forget <device>"
                .into()),
        }
    }

    fn status(&self) -> String {
        let shared = self.shared();
        if let Some(error) = &shared.broken {
            return format!("bluetooth: unavailable: {error}\n");
        }
        let state = &shared.state;
        let mut text = match Problem::find(state, rfkill()) {
            Some(problem) => format!("bluetooth: {}\n", problem.text()),
            None => String::new(),
        };
        for adapter in &state.adapters {
            let power = if adapter.powered { "on" } else { "off" };
            text += &format!("{} ({}): {power}\n", adapter.name, adapter.path);
            for device in state.paired().into_iter().filter(|device| device.adapter == adapter.path) {
                text += &format!(
                    "  {} [{}] {}\n",
                    device.name,
                    device.address,
                    detail(device, shared.connecting.contains(&device.path)).to_lowercase()
                );
            }
        }
        if shared.searching {
            text += "nearby:\n";
            for device in state.nearby() {
                let detail = nearby_detail(device, shared.pairing.contains(&device.path)).to_lowercase();
                text += &format!("  {} [{}] {detail}\n", device.name, device.address);
            }
        }
        if let Some(prompt) = &shared.prompt {
            let wanted = if prompt.confirm { "confirm or reject" } else { "type it on the device" };
            text += &format!("pairing code {}: {wanted}\n", prompt.code);
        }
        text
    }
}

impl Plugin for BluetoothPlugin {
    fn tools(&self) -> Value {
        let mut power = tool("power", "Switch Bluetooth on or off", &["on"]);
        power["inputSchema"]["properties"]["on"] = json!({ "type": "boolean" });
        let mut search = tool("search", "Search for devices to pair for 30 seconds, or stop", &["on"]);
        search["inputSchema"]["properties"]["on"] = json!({ "type": "boolean" });
        let mut answer = tool("answer", "Say whether the pairing code shown matches the device's", &["accept"]);
        answer["inputSchema"]["properties"]["accept"] = json!({ "type": "boolean" });
        json!([
            tool("launcher_info", "Launcher metadata", &[]),
            tool("launcher_search", "Paired devices matching the words", &["prefix", "query"]),
            tool("toggle", "Connect a paired device, or disconnect it when connected", &["device"]),
            tool("connect", "Connect a paired device by name or address", &["device"]),
            tool("disconnect", "Disconnect a device by name or address", &["device"]),
            power,
            search,
            tool("pair", "Pair, trust and connect a device found while searching", &["device"]),
            answer,
            tool("forget", "Unpair a device", &["device"]),
            tool("reconnect", "Reconnect devices that were connected before sleep or the adapter going off", &[]),
            tool("command", "Run a `tileception msg bluetooth` command, such as `status`", &["text"]),
        ])
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, String> {
        let text = |key: &str| arguments[key].as_str().unwrap_or_default().to_string();
        match name {
            "launcher_info" => Ok(json!({
                "name": "Bluetooth",
                "description": "Connect, disconnect and forget paired devices",
                "icon": ICON,
                "prefixes": ["bt", "bluetooth"],
            })),
            "launcher_search" => Ok(self.search(&text("query"))),
            "toggle" => self.connect(&text("device"), None),
            "connect" => self.connect(&text("device"), Some(true)),
            "disconnect" => self.connect(&text("device"), Some(false)),
            "power" => self.power(arguments["on"].as_bool().ok_or("on must be true or false")?),
            "search" => self.search_devices(arguments["on"].as_bool().unwrap_or(true)),
            "pair" => self.pair(&text("device")),
            "answer" => self.answer(arguments["accept"].as_bool().ok_or("accept must be true or false")?),
            "forget" => self.forget(&text("device")),
            "reconnect" => self.reconnect_now().map(|names| json!({ "reconnecting": names })),
            "command" => self.command(&text("text")).map(|text| json!({ "text": text })),
            _ => Err(format!("Unknown tool {name}")),
        }
    }

    fn resources(&self) -> Vec<(&'static str, &'static str)> {
        vec![(PANEL, "Bluetooth block"), (BAR, "Bluetooth chip")]
    }

    fn read(&mut self, uri: &str) -> Result<Value, String> {
        match uri {
            PANEL => serde_json::to_value(self.block()).map_err(|error| error.to_string()),
            BAR => serde_json::to_value(chips(&self.shared().state)).map_err(|error| error.to_string()),
            _ => Err(format!("no resource {uri}")),
        }
    }

    fn start(&mut self, notifier: Notifier) {
        let address = std::env::var(BUS_VARIABLE).ok();
        let bus = match address.as_deref() {
            Some(address) => zbus::blocking::connection::Builder::address(address).and_then(|builder| builder.build()),
            None => Connection::system(),
        };
        match bus {
            // A stand-in for BlueZ is never put to sleep.
            Ok(bus) => self.run(bus, None, address.is_none(), notifier),
            Err(error) => {
                eprintln!("bluetooth: no system bus: {error}");
                self.shared().broken = Some(error.to_string());
            }
        }
    }
}

impl BluetoothPlugin {
    /// Follows BlueZ on `bus`, notifying on `session` (the session bus when
    /// `None`), and sleep when `follow_sleep`.
    fn run(&mut self, bus: Connection, session: Option<Connection>, follow_sleep: bool, notifier: Notifier) {
        let (events, incoming) = mpsc::channel();
        let (requests, asked) = mpsc::channel();
        let spawn = |name: &str, work: Box<dyn FnOnce() + Send>| {
            if let Err(error) = std::thread::Builder::new().name(name.into()).spawn(work) {
                eprintln!("bluetooth: could not start {name}: {error}");
            }
        };
        // The agent answers BlueZ's questions while pairing. The watcher
        // registers it as the default whenever BlueZ starts, and each pairing
        // from here registers it again.
        let (answers, answered) = async_channel::bounded(4);
        let agent = Agent {
            shared: self.shared.clone(),
            notifier: notifier.clone(),
            events: events.clone(),
            answers: answered,
            cancel: answers.clone(),
        };
        if let Err(error) = bus.object_server().at(AGENT_PATH, agent) {
            eprintln!("bluetooth: could not offer the pairing agent: {error}");
        }
        match session.clone().map_or_else(Connection::session, Ok) {
            Ok(session) => {
                let (shared, answers) = (self.shared.clone(), answers.clone());
                spawn(
                    "bluez-answers",
                    Box::new(move || {
                        if let Err(error) = watch_answers(&session, &shared, &answers) {
                            eprintln!("bluetooth: not following notification buttons: {error}");
                        }
                    }),
                );
            }
            Err(error) => eprintln!("bluetooth: no session bus for notification buttons: {error}"),
        }
        self.answers = Some(answers);
        {
            let (bus, events) = (bus.clone(), events.clone());
            spawn(
                "bluez-watch",
                Box::new(move || {
                    if let Err(error) = watch(&bus, &events) {
                        eprintln!("bluetooth: stopped following BlueZ: {error}");
                    }
                }),
            );
        }
        if follow_sleep {
            let events = events.clone();
            spawn(
                "bluez-sleep",
                Box::new(move || {
                    if let Err(error) = watch_sleep(&events) {
                        eprintln!("bluetooth: not following sleep: {error}");
                    }
                }),
            );
        }
        {
            let (events, shared) = (events.clone(), self.shared.clone());
            spawn("bluez-calls", Box::new(move || serve(&bus, &asked, &events, &shared)));
        }
        {
            let (shared, requests, reconnect) = (self.shared.clone(), requests.clone(), self.reconnect);
            let notifier = notifier.clone();
            let notifications = Notifications { bus: session, last: 0 };
            spawn(
                "bluez-decide",
                Box::new(move || decide(&incoming, &requests, &shared, &notifier, notifications, reconnect)),
            );
        }
        self.notifier = Some(notifier);
        self.events = Some(events);
        self.requests = Some(requests);
    }
}

fn tool_action(name: &str, arguments: Value) -> ActionKind {
    ActionKind::Tool { name: name.into(), arguments }
}

/// The code to confirm or type, above the devices.
fn prompt_card(prompt: &Prompt, state: &State) -> Card {
    let name = state.device(&prompt.device).map_or("the device", |device| device.name.as_str());
    let mut card = Card {
        key: "pairing".into(),
        title: format!("Pairing {name}"),
        value: prompt.code.clone(),
        icon: Some(ICON.into()),
        tint: Tint::Accent,
        lit: true,
        enabled: true,
        ..Card::default()
    };
    if prompt.confirm {
        let question = if prompt.code.is_empty() {
            "It wants to pair with this computer"
        } else {
            "Does the device show this code?"
        };
        card.detail = Some(question.into());
        let choice = |title: &str, accept: bool| Choice {
            title: title.into(),
            selected: false,
            action: Some(tool_action("answer", json!({ "accept": accept }))),
        };
        card.choices =
            vec![Choices { title: String::new(), options: vec![choice("Pair", true), choice("Cancel", false)] }];
    } else {
        card.detail = Some("Type this code on the device, then press Enter".into());
    }
    card
}

fn nearby_detail(device: &Device, pairing: bool) -> &'static str {
    match device.rssi {
        _ if pairing => "Pairing\u{2026}",
        Some(rssi) if rssi >= -60 => "Close by",
        Some(rssi) if rssi >= -80 => "In range",
        Some(_) => "Far away",
        None => "Seen earlier",
    }
}

/// A device found while searching: the switch pairs and connects it.
fn nearby_row(device: &Device, pairing: bool) -> Row {
    Row {
        key: device.path.clone(),
        title: device.name.clone(),
        detail: nearby_detail(device, pairing).into(),
        icon: Some(ICON.into()),
        tint: Tint::Muted,
        enabled: true,
        switch: Some(Switch {
            checked: false,
            pending: pairing,
            action: Some(tool_action("pair", json!({ "device": device.path }))),
        }),
        ..Row::default()
    }
}

/// Where the agent is served on the bus.
const AGENT_PATH: &str = "/org/tileception/bluetooth/agent";

/// BlueZ's errors from the agent.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
}

/// Answers BlueZ while pairing: shows codes, waits for the answer to a code
/// to confirm, and turns down what needs typing here, as a PIN.
///
/// It is also the default agent, so pairing started on the other device
/// (a phone) asks here too. What was not started here is never accepted
/// without asking: the question shows in the block and as a notification
/// with buttons, since the panel may be closed.
struct Agent {
    shared: Arc<Mutex<Shared>>,
    notifier: Notifier,
    /// For the notification asking about pairing started elsewhere.
    events: mpsc::Sender<Event>,
    answers: async_channel::Receiver<bool>,
    /// Ends a wait for an answer when BlueZ cancels.
    cancel: async_channel::Sender<bool>,
}

impl Agent {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn prompt(&self, prompt: Option<Prompt>) {
        self.lock().prompt = prompt;
        self.notifier.changed(PANEL);
    }

    fn cannot(&self, why: &str) -> AgentError {
        self.lock().pair_note = Some(why.into());
        AgentError::Rejected(why.into())
    }

    /// Pairing with this device was started here.
    fn ours(&self, device: &str) -> bool {
        self.lock().pairing.contains(device)
    }

    fn name(&self, device: &str) -> String {
        self.lock().state.device(device).map_or_else(|| "A device".into(), |device| device.name.clone())
    }

    /// Shows the question and waits for the answer from the block or, for
    /// pairing started elsewhere, the notification.
    async fn ask(&self, prompt: Prompt) -> bool {
        // An answer left from an earlier question is not this one's.
        while self.answers.try_recv().is_ok() {}
        let elsewhere = !self.ours(&prompt.device);
        if elsewhere {
            let name = self.name(&prompt.device);
            let body = if prompt.code.is_empty() {
                format!("{name} wants to pair with this computer.")
            } else {
                format!("{name} wants to pair. Does it show {}?", prompt.code)
            };
            let _ = self.events.send(Event::Ask(Some((format!("Pair {name}?"), body))));
        }
        self.prompt(Some(prompt));
        let accepted = self.answers.recv().await.unwrap_or(false);
        self.prompt(None);
        if elsewhere {
            let _ = self.events.send(Event::Ask(None));
        }
        accepted
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}

    fn request_pin_code(&self, _device: OwnedObjectPath) -> Result<String, AgentError> {
        Err(self.cannot("It asks for a PIN code. Pair it with `bluetoothctl` instead."))
    }

    fn request_passkey(&self, _device: OwnedObjectPath) -> Result<u32, AgentError> {
        Err(self.cannot("It asks for a code to be typed here. Pair it with `bluetoothctl` instead."))
    }

    fn display_pin_code(&self, device: OwnedObjectPath, pincode: String) {
        self.prompt(Some(Prompt { device: device.to_string(), code: pincode, confirm: false }));
    }

    fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, _entered: u16) {
        self.prompt(Some(Prompt { device: device.to_string(), code: format!("{passkey:06}"), confirm: false }));
    }

    async fn request_confirmation(&self, device: OwnedObjectPath, passkey: u32) -> Result<(), AgentError> {
        let prompt = Prompt { device: device.to_string(), code: format!("{passkey:06}"), confirm: true };
        if self.ask(prompt).await { Ok(()) } else { Err(self.cannot("Pairing was cancelled.")) }
    }

    /// Pairing without a code: welcome when asked for here, else asked about.
    async fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), AgentError> {
        if self.ours(device.as_str()) {
            return Ok(());
        }
        let prompt = Prompt { device: device.to_string(), code: String::new(), confirm: true };
        if self.ask(prompt).await { Ok(()) } else { Err(self.cannot("Pairing was declined.")) }
    }

    /// A service of a device that is paired, or being paired from here.
    fn authorize_service(&self, device: OwnedObjectPath, _uuid: String) -> Result<(), AgentError> {
        let paired = self.lock().state.device(device.as_str()).is_some_and(|device| device.paired);
        if paired || self.ours(device.as_str()) { Ok(()) } else { Err(AgentError::Rejected("Not paired".into())) }
    }

    fn cancel(&self) {
        let _ = self.cancel.try_send(false);
        self.prompt(None);
    }
}

/// Registers the agent with BlueZ; as the default one, it also answers
/// pairing started on the other device.
fn register_agent(bus: &Connection, default: bool) {
    let path = zbus::zvariant::ObjectPath::from_static_str_unchecked(AGENT_PATH);
    let manager = "org.bluez.AgentManager1";
    let registered =
        bus.call_method(Some(BLUEZ), "/org/bluez", Some(manager), "RegisterAgent", &(&path, "KeyboardDisplay"));
    if let Some(error) = registered.as_ref().err().and_then(reason) {
        eprintln!("bluetooth: could not register the pairing agent: {error}");
    } else if default
        && let Err(error) = bus.call_method(Some(BLUEZ), "/org/bluez", Some(manager), "RequestDefaultAgent", &(&path,))
    {
        eprintln!("bluetooth: not the default pairing agent: {error}");
    }
}

/// Passes answers from the notification's buttons to the agent.
fn watch_answers(
    session: &Connection,
    shared: &Mutex<Shared>,
    answers: &async_channel::Sender<bool>,
) -> zbus::Result<()> {
    let messages = MessageIterator::from(session);
    let dbus = DBusProxy::new(session)?;
    for member in ["ActionInvoked", "NotificationClosed"] {
        let rule = zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .interface("org.freedesktop.Notifications")?
            .member(member)?
            .build();
        dbus.add_match_rule(rule)?;
    }
    for message in messages {
        let message = message?;
        let header = message.header();
        let member = header.member().map(|member| member.to_string()).unwrap_or_default();
        let answer = match member.as_str() {
            "ActionInvoked" => message
                .body()
                .deserialize::<(u32, String)>()
                .ok()
                .map(|(notification, key)| (notification, key == "pair")),
            // Dismissed by the user: no.
            "NotificationClosed" => message
                .body()
                .deserialize::<(u32, u32)>()
                .ok()
                .filter(|(_, why)| *why == 2)
                .map(|(notification, _)| (notification, false)),
            _ => None,
        };
        let Some((notification, accept)) = answer else { continue };
        let asking = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ask_id;
        if asking == Some(notification) {
            let _ = answers.try_send(accept);
        }
    }
    Ok(())
}

fn detail(device: &Device, connecting: bool) -> String {
    let status = match (device.connected, connecting) {
        (true, _) => "Connected",
        (false, true) => "Connecting\u{2026}",
        (false, false) => "Not connected",
    };
    match device.battery.filter(|_| device.connected) {
        Some(percent) => format!("{status}, battery {percent}%"),
        None => status.into(),
    }
}

/// The bar chip while something is connected: its name and charge, or how
/// many; clicking it opens the block.
fn chips(state: &State) -> Bar {
    let connected: Vec<&Device> = state.paired().into_iter().filter(|device| device.connected).collect();
    let low = connected.iter().filter_map(|device| device.battery).min();
    let label = match connected.as_slice() {
        [] => return Bar::default(),
        [device] => match device.battery {
            Some(percent) => format!("{} {percent}%", device.name),
            None => device.name.clone(),
        },
        several => format!("{} devices", several.len()),
    };
    let tint = match low {
        Some(percent) if percent <= 15 => Tint::Danger,
        Some(percent) if percent <= 30 => Tint::Warm,
        _ => Tint::Accent,
    };
    Bar { chips: vec![BarChip { label, icon: Some(ICON.into()), tint, action: None }] }
}

/// A paired device's row: the switch connects it.
fn row(device: &Device, connecting: bool) -> Row {
    Row {
        key: device.path.clone(),
        title: device.name.clone(),
        detail: detail(device, connecting),
        icon: Some(ICON.into()),
        tint: if device.connected { Tint::Accent } else { Tint::Muted },
        dim: !device.connected,
        enabled: true,
        switch: Some(Switch {
            checked: device.connected,
            pending: connecting && !device.connected,
            action: Some(tool_action("toggle", json!({ "device": device.path }))),
        }),
        ..Row::default()
    }
}

/// Follows BlueZ, sleep and the calls' results, and decides what to
/// reconnect and when.
fn decide(
    incoming: &mpsc::Receiver<Event>,
    requests: &mpsc::Sender<Request>,
    shared: &Mutex<Shared>,
    notifier: &Notifier,
    mut notifications: Notifications,
    reconnect: bool,
) {
    let lock = || shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut memory = Memory::default();
    let mut asleep = false;
    let mut due: Option<Instant> = None;
    let mut search_until: Option<Instant> = None;
    let search = |start: bool| {
        let adapters = {
            let mut shared = lock();
            shared.searching = start;
            shared.state.adapters.iter().filter(|adapter| adapter.powered).map(|adapter| adapter.path.clone()).collect()
        };
        notifier.changed(PANEL);
        let _ = requests.send(Request::Discover { adapters, start });
    };
    let start = |memory: &Memory, asleep: &mut bool| -> Vec<String> {
        *asleep = false;
        let mut shared = lock();
        let targets = memory.targets(&shared.state);
        let names = targets.iter().map(|(_, name)| name.clone()).collect();
        if !targets.is_empty() {
            shared.connecting.extend(targets.iter().map(|(path, _)| path.clone()));
            notifier.changed(PANEL);
            let _ = requests.send(Request::Reconnect(targets));
        }
        names
    };
    loop {
        let event = match [due, search_until].into_iter().flatten().min() {
            Some(when) => match incoming.recv_timeout(when.saturating_duration_since(Instant::now())) {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match incoming.recv() {
                Ok(event) => Some(event),
                Err(_) => return,
            },
        };
        let now = Instant::now();
        match event {
            None => {
                if search_until.is_some_and(|until| until <= now) {
                    search_until = None;
                    search(false);
                }
                if due.is_some_and(|when| when <= now) {
                    due = None;
                    start(&memory, &mut asleep);
                }
            }
            Some(Event::Search(true)) => {
                search_until = Some(now + SEARCH);
                search(true);
            }
            Some(Event::Search(false)) => {
                search_until = None;
                search(false);
            }
            Some(Event::Ask(Some((summary, body)))) => {
                let asking = notifications.ask(&summary, &body);
                lock().ask_id = asking;
            }
            Some(Event::Ask(None)) => {
                let asking = lock().ask_id.take();
                if let Some(notification) = asking {
                    notifications.close(notification);
                }
            }
            Some(Event::State(new)) => {
                let old = {
                    let mut shared = lock();
                    // A code to type goes once the device is paired.
                    let typed = shared.prompt.as_ref().is_some_and(|prompt| {
                        !prompt.confirm && new.device(&prompt.device).is_some_and(|device| device.paired)
                    });
                    if typed {
                        shared.prompt = None;
                    }
                    std::mem::replace(&mut shared.state, new.clone())
                };
                notifier.changed(PANEL);
                notifier.changed(BAR);
                memory.follow(&old, &new, now, asleep || due.is_some());
                if old.available != new.available {
                    eprintln!("bluetooth: {}", if new.available { "BlueZ is running" } else { "BlueZ stopped" });
                }
                if old.adapters.iter().any(|adapter| adapter.powered && !new.powered(&adapter.path)) {
                    memory.hold(now);
                    if search_until.take().is_some() {
                        lock().searching = false;
                    }
                }
                let came_on = new.adapters.iter().any(|adapter| {
                    adapter.powered && old.adapters.iter().any(|before| before.path == adapter.path && !before.powered)
                });
                if came_on && reconnect {
                    due = Some(now + SETTLE);
                }
            }
            Some(Event::Sleep(true)) => {
                asleep = true;
                memory.hold(now);
            }
            Some(Event::Sleep(false)) if reconnect => due = Some(now + SETTLE),
            Some(Event::Sleep(false)) => asleep = false,
            Some(Event::Reconnect(reply)) => {
                due = None;
                let _ = reply.send(start(&memory, &mut asleep));
            }
            Some(Event::Reconnected { connected, failed }) => {
                {
                    let mut shared = lock();
                    for path in connected.iter().map(|(path, _)| path).chain(failed.iter().map(|(path, ..)| path)) {
                        shared.connecting.remove(path);
                    }
                }
                notifier.changed(PANEL);
                for (_, name) in &connected {
                    eprintln!("bluetooth: reconnected {name}");
                }
                for (_, name, reason) in &failed {
                    eprintln!("bluetooth: could not reconnect {name}: {reason}");
                }
                let (summary, body) = match failed.as_slice() {
                    [] => continue,
                    [(_, name, reason)] => (format!("Could not reconnect {name}"), reason.clone()),
                    several => (
                        "Could not reconnect Bluetooth devices".into(),
                        several
                            .iter()
                            .map(|(_, name, reason)| format!("{name}: {reason}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                };
                notifications.show(&summary, &body);
            }
            Some(Event::Done { path, failure }) => {
                if let Some(path) = path {
                    lock().connecting.remove(&path);
                }
                notifier.changed(PANEL);
                if let Some((summary, reason)) = failure {
                    eprintln!("bluetooth: {summary}: {reason}");
                    notifications.show(&summary, &reason);
                }
            }
        }
    }
}

/// Makes the calls to BlueZ, one at a time, as asked.
fn serve(bus: &Connection, asked: &mpsc::Receiver<Request>, events: &mpsc::Sender<Event>, shared: &Mutex<Shared>) {
    let call = |path: &str, method: &str| bus.call_method(Some(BLUEZ), path, Some(DEVICE), method, &()).map(drop);
    let lock = || shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    while let Ok(request) = asked.recv() {
        let event = match request {
            Request::Reconnect(devices) => {
                let (mut connected, mut failed) = (Vec::new(), Vec::new());
                for (path, name) in devices {
                    match call(&path, "Connect").err().and_then(|error| reason(&error)) {
                        None => connected.push((path, name)),
                        Some(reason) => failed.push((path, name, reason)),
                    }
                }
                Event::Reconnected { connected, failed }
            }
            Request::Connect { path, name } => {
                let failure = call(&path, "Connect")
                    .err()
                    .and_then(|error| reason(&error))
                    .map(|reason| (format!("Could not connect {name}"), reason));
                Event::Done { path: Some(path), failure }
            }
            Request::Disconnect { path, name } => {
                let failure = call(&path, "Disconnect")
                    .err()
                    .and_then(|error| reason(&error))
                    .map(|reason| (format!("Could not disconnect {name}"), reason));
                Event::Done { path: Some(path), failure }
            }
            Request::Power { adapters, powered, unblock } => {
                let unblocked = if unblock { unblock_rfkill() } else { Ok(()) };
                let failure = unblocked.err().or_else(|| {
                    adapters.iter().find_map(|adapter| {
                        let value = zbus::zvariant::Value::from(powered);
                        bus.call_method(
                            Some(BLUEZ),
                            adapter.as_str(),
                            Some(PROPERTIES),
                            "Set",
                            &(ADAPTER, "Powered", value),
                        )
                        .err()
                        .and_then(|error| reason(&error))
                    })
                });
                let summary = if powered { "Could not switch Bluetooth on" } else { "Could not switch Bluetooth off" };
                Event::Done { path: None, failure: failure.map(|reason| (summary.into(), reason)) }
            }
            Request::Discover { adapters, start } => {
                let method = if start { "StartDiscovery" } else { "StopDiscovery" };
                let failure = adapters.iter().find_map(|adapter| {
                    let result = bus.call_method(Some(BLUEZ), adapter.as_str(), Some(ADAPTER), method, &());
                    // Stopping what already stopped is fine.
                    result.err().filter(|_| start).and_then(|error| reason(&error))
                });
                Event::Done {
                    path: None,
                    failure: failure.map(|reason| ("Could not search for devices".into(), reason)),
                }
            }
            Request::Pair { path, name } => {
                lock().pair_note = None;
                // Already registered unless BlueZ restarted meanwhile.
                register_agent(bus, false);
                // Trusted, so it may connect by itself and is reconnected after sleep.
                let trust = || {
                    let trusted = zbus::zvariant::Value::from(true);
                    bus.call_method(Some(BLUEZ), path.as_str(), Some(PROPERTIES), "Set", &(DEVICE, "Trusted", trusted))
                        .map(drop)
                };
                let result = call(&path, "Pair").and_then(|()| trust()).and_then(|()| call(&path, "Connect"));
                let failure = result.err().and_then(|error| reason(&error)).map(|why| {
                    let why = lock().pair_note.take().unwrap_or(why);
                    (format!("Could not pair {name}"), why)
                });
                if failure.is_none() {
                    eprintln!("bluetooth: paired {name}");
                }
                {
                    let mut shared = lock();
                    shared.pairing.remove(&path);
                    shared.prompt = None;
                }
                Event::Done { path: Some(path), failure }
            }
            Request::Forget { adapter, path, name } => {
                let device = zbus::zvariant::ObjectPath::try_from(path.as_str()).map_err(zbus::Error::from);
                let failure = device
                    .and_then(|device| {
                        bus.call_method(Some(BLUEZ), adapter.as_str(), Some(ADAPTER), "RemoveDevice", &(device,))
                    })
                    .err()
                    .and_then(|error| reason(&error))
                    .map(|reason| (format!("Could not forget {name}"), reason));
                Event::Done { path: Some(path), failure }
            }
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

/// Why a call failed, for people; `None` when it did not really fail.
fn reason(error: &zbus::Error) -> Option<String> {
    let zbus::Error::MethodError(name, detail, _) = error else { return Some(error.to_string()) };
    let detail = detail.as_deref().unwrap_or_default();
    let text = match name.as_str() {
        "org.bluez.Error.AlreadyConnected"
        | "org.bluez.Error.InProgress"
        | "org.bluez.Error.NotConnected"
        | "org.bluez.Error.AlreadyExists" => {
            return None;
        }
        "org.bluez.Error.AuthenticationRejected" | "org.bluez.Error.AuthenticationCanceled" => {
            "Pairing was declined or cancelled.".into()
        }
        "org.bluez.Error.AuthenticationFailed" => {
            "The codes did not match, or it is not in pairing mode any more.".into()
        }
        "org.bluez.Error.AuthenticationTimeout" => "It did not answer in time. Is it in pairing mode?".into(),
        "org.bluez.Error.ConnectionAttemptFailed" => "It did not answer. Is it switched on and nearby?".into(),
        "org.bluez.Error.NotReady" => "Bluetooth is off.".into(),
        "org.bluez.Error.DoesNotExist" => "It is no longer paired.".into(),
        "org.bluez.Error.Blocked" => "Bluetooth is blocked by rfkill.".into(),
        _ if detail.contains("page-timeout") || detail.contains("abort-by-local") || detail.contains("timeout") => {
            "It did not answer. Is it switched on and nearby?".into()
        }
        _ if detail.contains("profile-unavailable") => {
            "None of its services is available. For audio, is the sound server (PipeWire) running?".into()
        }
        _ if detail.contains("refused") => "It refused the connection. Try pairing it again.".into(),
        _ if !detail.is_empty() => format!("BlueZ said: {detail}"),
        _ => format!("BlueZ said: {name}"),
    };
    Some(text)
}

/// Notifications on the session bus; each replaces the one before.
#[derive(Default)]
struct Notifications {
    bus: Option<Connection>,
    last: u32,
}

impl Notifications {
    fn show(&mut self, summary: &str, body: &str) {
        if let Some(notification_id) = self.send(self.last, summary, body, &[], 1) {
            self.last = notification_id;
        }
    }

    /// A question with Pair and Cancel buttons that stays until answered.
    fn ask(&mut self, summary: &str, body: &str) -> Option<u32> {
        self.send(0, summary, body, &["pair", "Pair", "cancel", "Cancel"], 2)
    }

    fn close(&mut self, notification_id: u32) {
        let Some(bus) = self.connect() else { return };
        let closed = bus.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "CloseNotification",
            &(notification_id,),
        );
        if let Err(error) = closed {
            eprintln!("bluetooth: could not close a notification: {error}");
        }
    }

    fn connect(&mut self) -> Option<&Connection> {
        if self.bus.is_none() {
            self.bus = Connection::session().map_err(|error| eprintln!("bluetooth: no session bus: {error}")).ok();
        }
        self.bus.as_ref()
    }

    fn send(&mut self, replaces: u32, summary: &str, body: &str, actions: &[&str], urgency: u8) -> Option<u32> {
        let bus = self.connect()?;
        let hints: HashMap<&str, zbus::zvariant::Value<'_>> =
            HashMap::from([("urgency", zbus::zvariant::Value::U8(urgency))]);
        // A question waits for its answer; others go after the usual time.
        let timeout = if actions.is_empty() { -1i32 } else { 0 };
        let reply = bus.call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("Bluetooth", replaces, "bluetooth", summary, body, actions, hints, timeout),
        );
        match reply.and_then(|reply| reply.body().deserialize::<u32>()) {
            Ok(notification_id) => Some(notification_id),
            Err(error) => {
                eprintln!("bluetooth: could not notify: {error}");
                self.bus = None;
                None
            }
        }
    }
}

/// Whether rfkill blocks Bluetooth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rfkill {
    Clear,
    Soft,
    Hard,
}

fn rfkill() -> Rfkill {
    let Ok(entries) = std::fs::read_dir("/sys/class/rfkill") else { return Rfkill::Clear };
    let mut found = Rfkill::Clear;
    for entry in entries.flatten() {
        let read = |name: &str| std::fs::read_to_string(entry.path().join(name)).unwrap_or_default().trim().to_string();
        if read("type") != "bluetooth" {
            continue;
        }
        if read("hard") == "1" {
            return Rfkill::Hard;
        }
        if read("soft") == "1" {
            found = Rfkill::Soft;
        }
    }
    found
}

/// Lifts the soft block on every Bluetooth radio, as `rfkill unblock
/// bluetooth` does; the session's user may write `/dev/rfkill`.
fn unblock_rfkill() -> Result<(), String> {
    const TYPE_BLUETOOTH: u8 = 2;
    const OP_CHANGE_ALL: u8 = 3;
    // struct rfkill_event: idx (u32), type, op, soft, hard.
    let mut event = [0u8; 8];
    event[4] = TYPE_BLUETOOTH;
    event[5] = OP_CHANGE_ALL;
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/rfkill")
        .and_then(|mut device| device.write_all(&event))
        .map_err(|error| format!("Could not unblock rfkill ({error}). Try `rfkill unblock bluetooth`."))?;
    // BlueZ needs a moment to see the radio again.
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

/// Tells when the computer is about to sleep and when it woke.
fn watch_sleep(events: &mpsc::Sender<Event>) -> zbus::Result<()> {
    let bus = Connection::system()?;
    let manager = zbus::blocking::Proxy::new(
        &bus,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    for message in manager.receive_signal("PrepareForSleep")? {
        let starting: bool = message.body().deserialize()?;
        if events.send(Event::Sleep(starting)).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

/// Interfaces by object path, as BlueZ's object manager reports them.
type Objects = BTreeMap<String, HashMap<String, HashMap<String, OwnedValue>>>;

fn watch(bus: &Connection, events: &mpsc::Sender<Event>) -> zbus::Result<()> {
    // Listening first, so no change falls between it and the first state.
    let messages = MessageIterator::from(bus);
    let dbus = DBusProxy::new(bus)?;
    let rule = |interface: &'static str, member: &'static str| -> zbus::Result<zbus::MatchRule<'static>> {
        Ok(zbus::MatchRule::builder().msg_type(Type::Signal).interface(interface)?.member(member)?.build())
    };
    dbus.add_match_rule(rule("org.freedesktop.DBus", "NameOwnerChanged")?)?;
    dbus.add_match_rule(rule(OBJECT_MANAGER, "InterfacesAdded")?)?;
    dbus.add_match_rule(rule(OBJECT_MANAGER, "InterfacesRemoved")?)?;
    dbus.add_match_rule(
        zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(PROPERTIES)?
            .member("PropertiesChanged")?
            .path_namespace("/org/bluez")?
            .build(),
    )?;

    let fetch = |objects: &mut Objects| -> Option<String> {
        objects.clear();
        let owner = dbus.get_name_owner(BLUEZ.try_into().ok()?).ok()?.to_string();
        register_agent(bus, true);
        match managed_objects(bus) {
            Ok(fetched) => *objects = fetched,
            Err(error) => eprintln!("bluetooth: could not list BlueZ's objects: {error}"),
        }
        Some(owner)
    };
    let mut objects = Objects::new();
    let mut owner = fetch(&mut objects);
    let mut last = state(&objects, owner.is_some());
    if !last.available {
        eprintln!("bluetooth: BlueZ is not running");
    }
    if events.send(Event::State(last.clone())).is_err() {
        return Ok(());
    }

    for message in messages {
        let message = message?;
        let header = message.header();
        if header.message_type() != Type::Signal {
            continue;
        }
        let (Some(interface), Some(member)) = (header.interface(), header.member()) else { continue };
        let from_bluez = header.sender().is_some_and(|sender| Some(sender.as_str()) == owner.as_deref());
        match (interface.as_str(), member.as_str()) {
            ("org.freedesktop.DBus", "NameOwnerChanged") => {
                let Ok((name, _, new)) = message.body().deserialize::<(String, String, String)>() else { continue };
                if name != BLUEZ {
                    continue;
                }
                owner = if new.is_empty() {
                    objects.clear();
                    None
                } else {
                    fetch(&mut objects)
                };
            }
            (OBJECT_MANAGER, "InterfacesAdded") if from_bluez => {
                let Ok((path, added)) =
                    message.body().deserialize::<(OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>)>()
                else {
                    continue;
                };
                let interfaces = objects.entry(path.to_string()).or_default();
                for (interface, properties) in added {
                    interfaces.entry(interface).or_default().extend(properties);
                }
            }
            (OBJECT_MANAGER, "InterfacesRemoved") if from_bluez => {
                let Ok((path, removed)) = message.body().deserialize::<(OwnedObjectPath, Vec<String>)>() else {
                    continue;
                };
                if let Some(interfaces) = objects.get_mut(path.as_str()) {
                    interfaces.retain(|interface, _| !removed.contains(interface));
                    if interfaces.is_empty() {
                        objects.remove(path.as_str());
                    }
                }
            }
            (PROPERTIES, "PropertiesChanged") if from_bluez => {
                let Some(path) = header.path() else { continue };
                let Ok((interface, changed, invalidated)) =
                    message.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
                else {
                    continue;
                };
                let Some(properties) = objects.get_mut(path.as_str()).and_then(|object| object.get_mut(&interface))
                else {
                    continue;
                };
                properties.extend(changed);
                for name in invalidated {
                    properties.remove(&name);
                }
            }
            _ => continue,
        }
        let next = state(&objects, owner.is_some());
        if next != last {
            last = next;
            if events.send(Event::State(last.clone())).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn managed_objects(bus: &Connection) -> zbus::Result<Objects> {
    let reply = bus.call_method(Some(BLUEZ), "/", Some(OBJECT_MANAGER), "GetManagedObjects", &())?;
    let objects: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> = reply.body().deserialize()?;
    Ok(objects.into_iter().map(|(path, interfaces)| (path.to_string(), interfaces)).collect())
}

fn state(objects: &Objects, available: bool) -> State {
    let text = |properties: &HashMap<String, OwnedValue>, name: &str| {
        properties.get(name).and_then(|value| <&str>::try_from(value).ok()).unwrap_or_default().to_string()
    };
    let flag = |properties: &HashMap<String, OwnedValue>, name: &str| {
        properties.get(name).and_then(|value| bool::try_from(value).ok()).unwrap_or(false)
    };
    let mut state = State { available, ..State::default() };
    for (path, interfaces) in objects {
        if let Some(properties) = interfaces.get(ADAPTER) {
            // Newer BlueZ says "off-disabling" before devices go, while Powered is still true.
            let power_state = text(properties, "PowerState");
            let alias = text(properties, "Alias");
            state.adapters.push(Adapter {
                path: path.clone(),
                name: if alias.is_empty() { text(properties, "Name") } else { alias },
                powered: flag(properties, "Powered") && (power_state.is_empty() || power_state == "on"),
            });
        }
        if let Some(properties) = interfaces.get(DEVICE) {
            let address = text(properties, "Address");
            let alias = text(properties, "Alias");
            state.devices.push(Device {
                path: path.clone(),
                adapter: path.rsplit_once('/').map(|(parent, _)| parent.to_string()).unwrap_or_default(),
                name: if alias.is_empty() { address.clone() } else { alias },
                address,
                paired: flag(properties, "Paired"),
                connected: flag(properties, "Connected"),
                named: properties.contains_key("Name"),
                rssi: properties.get("RSSI").and_then(|value| i16::try_from(value).ok()),
                battery: interfaces
                    .get(BATTERY)
                    .and_then(|battery| battery.get("Percentage"))
                    .and_then(|value| u8::try_from(value).ok()),
            });
        }
    }
    state
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    const HCI: &str = "/org/bluez/hci0";
    const PHONES: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
    const MOUSE: &str = "/org/bluez/hci0/dev_66_77_88_99_AA_BB";

    fn world(powered: bool, connected: &[&str]) -> State {
        let device = |path: &str, name: &str| Device {
            path: path.into(),
            adapter: HCI.into(),
            name: name.into(),
            paired: true,
            connected: connected.contains(&path),
            ..Device::default()
        };
        State {
            available: true,
            adapters: vec![Adapter { path: HCI.into(), name: "laptop".into(), powered }],
            devices: vec![device(PHONES, "Headphones"), device(MOUSE, "Mouse")],
        }
    }

    fn targets(memory: &Memory, state: &State) -> Vec<String> {
        memory.targets(state).into_iter().map(|(_, name)| name).collect()
    }

    #[test]
    fn brings_back_what_sleep_took() {
        let mut memory = Memory::default();
        let start = Instant::now();
        let both = world(true, &[PHONES, MOUSE]);
        memory.follow(&State::default(), &both, start, false);
        // Sleep, then the links drop while asleep.
        memory.hold(start);
        let none = world(true, &[]);
        memory.follow(&both, &none, start, true);
        // The mouse came back by itself; only the headphones are left.
        let mouse = world(true, &[MOUSE]);
        memory.follow(&none, &mouse, start, true);
        assert_eq!(targets(&memory, &mouse), ["Headphones"]);
    }

    #[test]
    fn leaves_switched_off_devices_alone() {
        let mut memory = Memory::default();
        let start = Instant::now();
        let both = world(true, &[PHONES, MOUSE]);
        memory.follow(&State::default(), &both, start, false);
        // The headphones are switched off well before the computer sleeps.
        let mouse = world(true, &[MOUSE]);
        memory.follow(&both, &mouse, start, false);
        memory.hold(start + GRACE * 2);
        let none = world(true, &[]);
        memory.follow(&mouse, &none, start + GRACE * 2, true);
        assert_eq!(targets(&memory, &none), ["Mouse"]);
        // Connecting them by hand later remembers them again.
        let phones = world(true, &[PHONES]);
        memory.follow(&none, &phones, start + GRACE * 3, false);
        memory.follow(&phones, &none, start + GRACE * 3, true);
        assert_eq!(targets(&memory, &none), ["Headphones", "Mouse"]);
    }

    #[test]
    fn adapter_off_takes_devices_down_with_it() {
        let mut memory = Memory::default();
        let start = Instant::now();
        let both = world(true, &[PHONES, MOUSE]);
        memory.follow(&State::default(), &both, start, false);
        // Older BlueZ drops the links before saying the adapter is off.
        let dropped = world(true, &[]);
        memory.follow(&both, &dropped, start, false);
        let off = world(false, &[]);
        memory.follow(&dropped, &off, start, false);
        memory.hold(start + Duration::from_millis(50));
        assert!(targets(&memory, &off).is_empty(), "nothing while the adapter is off");
        let back_on = world(true, &[]);
        memory.follow(&off, &back_on, start + Duration::from_secs(60), false);
        assert_eq!(targets(&memory, &back_on), ["Headphones", "Mouse"]);
    }

    #[test]
    fn forgets_unpaired_devices_but_not_while_bluez_restarts() {
        let mut memory = Memory::default();
        let now = Instant::now();
        let both = world(true, &[PHONES]);
        memory.follow(&State::default(), &both, now, false);
        let gone = State::default();
        memory.follow(&both, &gone, now, true);
        let back = world(true, &[]);
        memory.follow(&gone, &back, now, true);
        assert_eq!(targets(&memory, &back), ["Headphones"]);
        let mut unpaired = back.clone();
        unpaired.devices[0].paired = false;
        memory.follow(&back, &unpaired, now, false);
        assert!(targets(&memory, &back).is_empty());
    }

    #[test]
    fn explains_failures() {
        let error = |name: &str, detail: &str| {
            zbus::Error::MethodError(
                name.try_into().unwrap(),
                Some(detail.into()),
                zbus::Message::method_call("/", "x").unwrap().build(&()).unwrap(),
            )
        };
        assert_eq!(reason(&error("org.bluez.Error.AlreadyConnected", "")), None);
        assert_eq!(
            reason(&error("org.bluez.Error.Failed", "br-connection-page-timeout")).unwrap(),
            "It did not answer. Is it switched on and nearby?"
        );
        assert!(
            reason(&error("org.bluez.Error.Failed", "br-connection-profile-unavailable")).unwrap().contains("PipeWire")
        );
        assert_eq!(reason(&error("org.bluez.Error.Failed", "odd")).unwrap(), "BlueZ said: odd");
    }

    #[test]
    fn says_what_is_wrong() {
        let powered = world(true, &[]);
        assert_eq!(Problem::find(&powered, Rfkill::Clear), None);
        assert_eq!(Problem::find(&State::default(), Rfkill::Clear), Some(Problem::NotRunning));
        let off = world(false, &[]);
        assert_eq!(Problem::find(&off, Rfkill::Clear), Some(Problem::Off));
        assert_eq!(Problem::find(&off, Rfkill::Soft), Some(Problem::SoftBlocked));
        assert_eq!(Problem::find(&off, Rfkill::Hard), Some(Problem::HardBlocked));
        let bare = State { available: true, ..State::default() };
        assert_eq!(Problem::find(&bare, Rfkill::Clear), Some(Problem::NoAdapter));
        assert!(Problem::Off.fixable() && !Problem::HardBlocked.fixable());
    }

    #[test]
    fn finds_devices_by_name_or_address() {
        let state = world(true, &[PHONES]);
        assert_eq!(state.find("headphones").unwrap().path, PHONES);
        assert_eq!(state.find("head").unwrap().path, PHONES);
        assert_eq!(state.find(MOUSE).unwrap().name, "Mouse");
        assert!(state.find("keyboard").is_err());
        assert_eq!(state.paired()[0].name, "Headphones", "connected first");
    }

    #[test]
    fn chips_show_what_is_connected() {
        assert!(chips(&world(true, &[])).chips.is_empty());
        let mut one = world(true, &[PHONES]);
        one.devices[0].battery = Some(12);
        let chip = &chips(&one).chips[0];
        assert_eq!((chip.label.as_str(), chip.tint), ("Headphones 12%", Tint::Danger));
        assert_eq!(chips(&world(true, &[PHONES, MOUSE])).chips[0].label, "2 devices");
    }

    #[test]
    fn rows_switch_connections() {
        let state = world(true, &[PHONES]);
        let phones = row(state.device(PHONES).unwrap(), false);
        assert!(phones.switch.as_ref().unwrap().checked);
        assert_eq!(phones.detail, "Connected");
        let mouse = row(state.device(MOUSE).unwrap(), true);
        let switch = mouse.switch.unwrap();
        assert!(!switch.checked && switch.pending);
        assert_eq!(mouse.detail, "Connecting\u{2026}");
        assert_eq!(switch.action, Some(tool_action("toggle", json!({ "device": MOUSE }))));
    }

    /// A stand-in for BlueZ and a notification service, on a private bus.
    mod fake {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use zbus::{
            interface,
            message::Header,
            object_server::{ObjectServer, SignalEmitter},
            zvariant::{OwnedObjectPath, OwnedValue},
        };

        /// Where a phone shows up while searching.
        pub const PHONE: &str = "/org/bluez/hci0/dev_12_34_56_78_9A_BC";

        #[derive(Debug, zbus::DBusError)]
        #[zbus(prefix = "org.bluez.Error")]
        pub enum BluezError {
            #[zbus(error)]
            ZBus(zbus::Error),
            Failed(String),
            AuthenticationRejected(String),
        }

        /// The agent registered last: its bus name and path.
        pub type Agents = Arc<Mutex<Option<(String, String)>>>;

        pub struct AgentManager(pub Agents);

        #[interface(name = "org.bluez.AgentManager1")]
        impl AgentManager {
            fn register_agent(&self, #[zbus(header)] header: Header<'_>, agent: OwnedObjectPath, _capability: String) {
                let sender = header.sender().map(ToString::to_string).unwrap_or_default();
                *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((sender, agent.to_string()));
            }

            fn request_default_agent(&self, _agent: OwnedObjectPath) {}
        }

        pub struct Adapter {
            pub powered: bool,
            pub agents: Agents,
        }

        #[interface(name = "org.bluez.Adapter1")]
        impl Adapter {
            /// A phone in pairing mode is found.
            async fn start_discovery(&self, #[zbus(object_server)] server: &ObjectServer) -> zbus::fdo::Result<()> {
                let phone = Device {
                    alias: "Phone",
                    address: "12:34:56:78:9A:BC",
                    connected: false,
                    answers: true,
                    paired: false,
                    trusted: false,
                    path: PHONE,
                    agents: self.agents.clone(),
                };
                server.at(PHONE, phone).await?;
                Ok(())
            }

            fn stop_discovery(&self) {}

            async fn remove_device(
                &self,
                #[zbus(object_server)] server: &ObjectServer,
                device: OwnedObjectPath,
            ) -> zbus::fdo::Result<()> {
                server.remove::<Device, _>(device.as_str()).await?;
                Ok(())
            }

            #[zbus(property)]
            fn alias(&self) -> String {
                "test-adapter".into()
            }

            #[zbus(property)]
            fn powered(&self) -> bool {
                self.powered
            }

            #[zbus(property)]
            fn set_powered(&mut self, powered: bool) {
                self.powered = powered;
            }
        }

        pub struct Device {
            pub alias: &'static str,
            pub address: &'static str,
            pub connected: bool,
            /// Answers Connect; otherwise it times out, as when switched off.
            pub answers: bool,
            pub paired: bool,
            pub trusted: bool,
            pub path: &'static str,
            pub agents: Agents,
        }

        #[interface(name = "org.bluez.Device1")]
        impl Device {
            /// Asks the agent to confirm 123456, as a phone does.
            async fn pair(
                &mut self,
                #[zbus(connection)] connection: &zbus::Connection,
                #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
            ) -> Result<(), BluezError> {
                let agent = self.agents.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                let (name, path) = agent.ok_or_else(|| BluezError::Failed("no agent".into()))?;
                let device = zbus::zvariant::ObjectPath::from_static_str_unchecked(self.path);
                let asked = connection
                    .call_method(
                        Some(name.as_str()),
                        path.as_str(),
                        Some("org.bluez.Agent1"),
                        "RequestConfirmation",
                        &(device, 123_456u32),
                    )
                    .await;
                if asked.is_err() {
                    return Err(BluezError::AuthenticationRejected("Authentication Rejected".into()));
                }
                self.paired = true;
                self.paired_changed(&emitter).await?;
                Ok(())
            }

            async fn connect(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> Result<(), BluezError> {
                if !self.answers {
                    return Err(BluezError::Failed("br-connection-page-timeout".into()));
                }
                self.connected = true;
                self.connected_changed(&emitter).await?;
                Ok(())
            }

            async fn disconnect(
                &mut self,
                #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
            ) -> Result<(), BluezError> {
                self.connected = false;
                self.connected_changed(&emitter).await?;
                Ok(())
            }

            #[zbus(property)]
            fn alias(&self) -> String {
                self.alias.into()
            }

            #[zbus(property)]
            fn address(&self) -> String {
                self.address.into()
            }

            #[zbus(property)]
            fn name(&self) -> String {
                self.alias.into()
            }

            #[zbus(property, name = "RSSI")]
            fn rssi(&self) -> i16 {
                -50
            }

            #[zbus(property)]
            fn paired(&self) -> bool {
                self.paired
            }

            #[zbus(property)]
            fn trusted(&self) -> bool {
                self.trusted
            }

            #[zbus(property)]
            fn set_trusted(&mut self, trusted: bool) {
                self.trusted = trusted;
            }

            #[zbus(property)]
            fn connected(&self) -> bool {
                self.connected
            }
        }

        /// BlueZ with one powered adapter and these devices, on its own connection.
        pub fn bluez(address: &str, devices: Vec<Device>) -> zbus::blocking::Connection {
            let agents = Agents::default();
            let mut builder = zbus::blocking::connection::Builder::address(address)
                .unwrap()
                .name("org.bluez")
                .unwrap()
                .serve_at("/", zbus::fdo::ObjectManager)
                .unwrap()
                .serve_at("/org/bluez", AgentManager(agents.clone()))
                .unwrap()
                .serve_at("/org/bluez/hci0", Adapter { powered: true, agents: agents.clone() })
                .unwrap();
            for mut device in devices {
                device.agents = agents.clone();
                builder = builder.serve_at(device.path, device).unwrap();
            }
            builder.build().unwrap()
        }

        /// Keeps "summary: body" of each notification.
        pub struct Notes(pub Arc<Mutex<Vec<String>>>);

        #[interface(name = "org.freedesktop.Notifications")]
        impl Notes {
            #[allow(clippy::too_many_arguments, reason = "the D-Bus method's signature")]
            fn notify(
                &self,
                _app: String,
                _replaces: u32,
                _icon: String,
                summary: String,
                body: String,
                _actions: Vec<String>,
                _hints: HashMap<String, OwnedValue>,
                _timeout: i32,
            ) -> u32 {
                let mut notes = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                notes.push(format!("{summary}: {body}"));
                notes.len() as u32
            }
        }
    }

    struct Bus(std::process::Child, String);

    impl Bus {
        fn new() -> Self {
            use std::io::BufRead;
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--print-address", "--nofork"])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("tests require dbus-daemon");
            let mut address = String::new();
            std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut address).unwrap();
            Self(child, address.trim().to_string())
        }

        fn connect(&self) -> Connection {
            zbus::blocking::connection::Builder::address(self.1.as_str()).unwrap().build().unwrap()
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A notification service keeping "summary: body" of each.
    fn notifications(bus: &Bus) -> (Arc<Mutex<Vec<String>>>, Connection) {
        let notes = Arc::new(Mutex::new(Vec::new()));
        let connection = zbus::blocking::connection::Builder::address(bus.1.as_str())
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at("/org/freedesktop/Notifications", fake::Notes(notes.clone()))
            .unwrap()
            .build()
            .unwrap();
        (notes, connection)
    }

    #[test]
    fn asks_before_pairing_started_elsewhere() {
        let bus = Bus::new();
        let stranger = fake::Device {
            alias: "Phone",
            address: "12:34:56:78:9A:BC",
            connected: false,
            answers: true,
            paired: false,
            trusted: false,
            path: fake::PHONE,
            agents: fake::Agents::default(),
        };
        let bluez = fake::bluez(&bus.1, vec![stranger]);
        let (notes, notifications) = notifications(&bus);
        let noted = |text: &str| notes.lock().unwrap().iter().any(|note| note.contains(text));
        let mut plugin = BluetoothPlugin::new(&[]);
        let (sender, _changes) = mpsc::channel();
        let client = bus.connect();
        plugin.run(client.clone(), Some(client), false, Notifier { sender });
        let agents = bluez.object_server().interface::<_, fake::AgentManager>("/org/bluez").unwrap().get().0.clone();
        wait_for("the agent", || agents.lock().unwrap().is_some());
        wait_for("the phone", || plugin.shared().state.device(fake::PHONE).is_some());
        let (name, path) = agents.lock().unwrap().clone().unwrap();
        // BlueZ asking the agent, as when the phone starts pairing.
        // The answer arrives on the returned channel; waiting for it has a limit.
        let ask = |method: &'static str, code: Option<u32>| {
            let (bluez, name, path) = (bluez.clone(), name.clone(), path.clone());
            let (sender, answered) = mpsc::channel();
            std::thread::spawn(move || {
                let device = zbus::zvariant::ObjectPath::try_from(fake::PHONE).unwrap();
                let agent = Some("org.bluez.Agent1");
                let result = match code {
                    Some(code) => bluez.call_method(Some(name.as_str()), path.as_str(), agent, method, &(device, code)),
                    None => bluez.call_method(Some(name.as_str()), path.as_str(), agent, method, &(device,)),
                };
                let _ = sender.send(result.map(drop));
            });
            answered
        };
        let answer_of = |answered: &mpsc::Receiver<zbus::Result<()>>| {
            answered.recv_timeout(Duration::from_secs(10)).expect("the agent answers in time")
        };
        let code = |plugin: &BluetoothPlugin| plugin.shared().prompt.as_ref().map(|prompt| prompt.code.clone());

        // A code to confirm shows in the block and in a notification, whose button answers.
        let confirming = ask("RequestConfirmation", Some(654_321));
        wait_for("the question", || noted("Pair Phone?: Phone wants to pair. Does it show 654321?"));
        assert_eq!(code(&plugin).as_deref(), Some("654321"));
        // The plugin knows which notification asks once Notify answered it.
        wait_for("the question's id", || plugin.shared().ask_id.is_some());
        let question = notes.lock().unwrap().len() as u32;
        notifications
            .emit_signal(
                None::<&str>,
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "ActionInvoked",
                &(question, "pair"),
            )
            .unwrap();
        assert!(answer_of(&confirming).is_ok(), "accepted with the notification's button");
        wait_for("the code gone", || code(&plugin).is_none());

        // Pairing without a code is asked about too, and declined in the block.
        let authorizing = ask("RequestAuthorization", None);
        wait_for("the question", || code(&plugin).as_deref() == Some(""));
        {
            let shared = plugin.shared();
            let card = prompt_card(shared.prompt.as_ref().unwrap(), &shared.state);
            assert_eq!(card.detail.as_deref(), Some("It wants to pair with this computer"));
        }
        plugin.call("answer", &json!({ "accept": false })).unwrap();
        assert!(answer_of(&authorizing).is_err(), "declined");

        // A device that is not paired gets no services.
        let device = zbus::zvariant::ObjectPath::try_from(fake::PHONE).unwrap();
        let service = (device, "0000110b-0000-1000-8000-00805f9b34fb");
        let authorized = bluez.call_method(
            Some(name.as_str()),
            path.as_str(),
            Some("org.bluez.Agent1"),
            "AuthorizeService",
            &service,
        );
        assert!(authorized.is_err());
    }

    #[test]
    fn pairs_with_a_code_and_forgets() {
        let bus = Bus::new();
        let bluez = fake::bluez(&bus.1, Vec::new());
        let (notes, _notifications) = notifications(&bus);
        let noted = |text: &str| notes.lock().unwrap().iter().any(|note| note.contains(text));
        let mut plugin = BluetoothPlugin::new(&[]);
        let (sender, _changes) = mpsc::channel();
        let client = bus.connect();
        plugin.run(client.clone(), Some(client), false, Notifier { sender });
        let status = |plugin: &mut BluetoothPlugin| {
            plugin.call("command", &json!({ "text": "" })).unwrap()["text"].as_str().unwrap().to_string()
        };
        let code = |plugin: &BluetoothPlugin| {
            plugin.block().views.first().and_then(|view| match view.items.first() {
                Some(Item::Card(card)) => Some(card.value.clone()),
                _ => None,
            })
        };
        wait_for("the notice", || {
            plugin.block().notice.is_some_and(|notice| notice.text.starts_with("No paired devices"))
        });

        // Search, and the phone shows up nearby.
        plugin.call("search", &json!({ "on": true })).unwrap();
        wait_for("the phone nearby", || status(&mut plugin).contains("nearby:\n  Phone [12:34:56:78:9A:BC] close by"));
        let block = plugin.block();
        let Some(Item::Group(nearby)) = block.views[0].items.last() else { panic!("a nearby group") };
        let Item::Row(phone) = &nearby.items[0] else { panic!("a row") };
        assert_eq!((nearby.title.as_str(), phone.title.as_str()), ("Nearby", "Phone"));
        assert!(plugin.call("answer", &json!({ "accept": true })).is_err(), "no code to answer yet");

        // Declining the code fails the pairing, and says so.
        plugin.call("pair", &json!({ "device": "phone" })).unwrap();
        wait_for("the code", || code(&plugin).as_deref() == Some("123456"));
        plugin.call("answer", &json!({ "accept": false })).unwrap();
        wait_for("a notification", || noted("Could not pair Phone: Pairing was cancelled."));
        assert_eq!(code(&plugin), None);

        // Confirming it pairs, trusts and connects the phone.
        plugin.call("command", &json!({ "text": "pair Phone" })).unwrap();
        wait_for("the code", || code(&plugin).is_some());
        assert!(status(&mut plugin).contains("pairing code 123456: confirm or reject"));
        plugin.call("command", &json!({ "text": "confirm" })).unwrap();
        wait_for("the phone connected", || status(&mut plugin).contains("  Phone [12:34:56:78:9A:BC] connected"));
        let device = bluez.object_server().interface::<_, fake::Device>(fake::PHONE).unwrap();
        assert!(device.get().trusted, "trusted, so it is reconnected after sleep");
        drop(device);

        // Forgetting removes it.
        let rows = plugin.call("launcher_search", &json!({ "prefix": "bt", "query": "forget ph" })).unwrap();
        assert_eq!(rows["rows"][0]["title"], "Forget Phone");
        plugin.call("forget", &json!({ "device": "Phone" })).unwrap();
        wait_for("the phone gone", || !status(&mut plugin).contains("Phone"));
        plugin.call("search", &json!({ "on": false })).unwrap();
        wait_for("searching stopped", || !plugin.shared().searching);
    }

    #[test]
    fn follows_and_reconnects_through_bluez() {
        const SPEAKER: &str = MOUSE;
        let bus = Bus::new();
        let device = |path, alias, address, answers| fake::Device {
            alias,
            address,
            connected: true,
            answers,
            paired: true,
            trusted: true,
            path,
            agents: fake::Agents::default(),
        };
        let bluez = fake::bluez(
            &bus.1,
            vec![
                device(PHONES, "Headphones", "00:11:22:33:44:55", true),
                device(SPEAKER, "Speaker", "66:77:88:99:AA:BB", false),
            ],
        );
        let (notes, _notifications) = notifications(&bus);
        let noted = |text: &str| notes.lock().unwrap().iter().any(|note| note.contains(text));
        let set_connected = |path: &str, connected: bool| {
            let device = bluez.object_server().interface::<_, fake::Device>(path).unwrap();
            device.get_mut().connected = connected;
            zbus::block_on(device.get().connected_changed(device.signal_emitter())).unwrap();
        };
        let set_powered = |powered: bool| {
            let adapter = bluez.object_server().interface::<_, fake::Adapter>(HCI).unwrap();
            adapter.get_mut().powered = powered;
            zbus::block_on(adapter.get().powered_changed(adapter.signal_emitter())).unwrap();
        };

        let mut plugin = BluetoothPlugin::new(&[]);
        let (sender, _changes) = mpsc::channel();
        let client = bus.connect();
        plugin.run(client.clone(), Some(client), false, Notifier { sender });
        let status =
            |plugin: &mut BluetoothPlugin| plugin.call("command", &json!({ "text": "" })).unwrap()["text"].to_string();
        wait_for("both connected", || {
            let text = status(&mut plugin);
            text.contains("Headphones [00:11:22:33:44:55] connected")
                && text.contains("Speaker [66:77:88:99:AA:BB] connected")
        });

        // The adapter goes off, the links dropping first as with older BlueZ.
        set_connected(PHONES, false);
        set_connected(SPEAKER, false);
        set_powered(false);
        wait_for("the adapter off", || plugin.block().summary == "Off" || plugin.block().summary == "Blocked");
        assert!(plugin.call("toggle", &json!({ "device": "Headphones" })).is_err(), "nothing connects while off");

        // Switching it on brings the headphones back; the speaker does not answer.
        plugin.call("power", &json!({ "on": true })).unwrap();
        wait_for("headphones reconnected", || status(&mut plugin).contains("Headphones [00:11:22:33:44:55] connected"));
        wait_for("a notification", || noted("Could not reconnect Speaker: It did not answer"));

        // By hand: a failure says so, and a device disconnected by hand stays so.
        plugin.call("toggle", &json!({ "device": "speaker" })).unwrap();
        wait_for("a notification", || noted("Could not connect Speaker"));
        plugin.call("disconnect", &json!({ "device": "headphones" })).unwrap();
        wait_for("headphones disconnected", || {
            status(&mut plugin).contains("Headphones [00:11:22:33:44:55] not connected")
        });
        let reconnecting = plugin.call("command", &json!({ "text": "reconnect" })).unwrap();
        assert_eq!(reconnecting["text"], "reconnecting Speaker\n");

        let block = plugin.block();
        assert_eq!(block.summary, "Nothing connected");
        let Item::Row(first) = &block.views[0].items[0] else { panic!("a row") };
        assert_eq!(first.title, "Headphones");
        let rows = plugin.call("launcher_search", &json!({ "prefix": "bt", "query": "head" })).unwrap();
        assert_eq!(rows["rows"][0]["title"], "Connect Headphones");
    }

    #[test]
    fn reads_bluez_objects() {
        let value = |value: zbus::zvariant::Value<'static>| OwnedValue::try_from(value).unwrap();
        let mut objects = Objects::new();
        objects.insert(
            HCI.into(),
            HashMap::from([(
                ADAPTER.into(),
                HashMap::from([
                    ("Alias".into(), value("laptop".into())),
                    ("Powered".into(), value(true.into())),
                    ("PowerState".into(), value("off-disabling".into())),
                ]),
            )]),
        );
        objects.insert(
            PHONES.into(),
            HashMap::from([
                (
                    DEVICE.into(),
                    HashMap::from([
                        ("Address".into(), value("00:11:22:33:44:55".into())),
                        ("Alias".into(), value("Headphones".into())),
                        ("Paired".into(), value(true.into())),
                        ("Connected".into(), value(true.into())),
                    ]),
                ),
                (BATTERY.into(), HashMap::from([("Percentage".into(), value(80u8.into()))])),
            ]),
        );
        let state = state(&objects, true);
        assert!(!state.adapters[0].powered, "on its way off");
        let phones = &state.devices[0];
        assert_eq!((phones.adapter.as_str(), phones.name.as_str(), phones.battery), (HCI, "Headphones", Some(80)));
        assert!(phones.connected && phones.paired);
        assert_eq!(detail(phones, false), "Connected, battery 80%");
    }
}
