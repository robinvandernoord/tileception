//! Bluetooth through BlueZ. A worker follows adapters and devices on the
//! system bus and sends each new state to the compositor thread, which
//! reconnects devices that were connected before the computer slept or the
//! adapter was switched off, and says why when that fails.
//!
//! BlueZ reconnects few devices itself: keyboards and mice usually come back
//! on their own, headphones and speakers usually do not.

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::{RegistrationToken, channel, timer::TimeoutAction, timer::Timer};
use tileception_notify::Urgency;
use zbus::blocking::{Connection, MessageIterator, fdo::DBusProxy};
use zbus::message::Type;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::Tileception;

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const BATTERY: &str = "org.bluez.Battery1";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// After waking or switching the adapter on, devices that come back by
/// themselves get this long first.
const SETTLE: Duration = Duration::from_secs(3);
/// A device that disconnects this shortly before sleep or the adapter going
/// off was taken down with it, not switched off by its owner.
const GRACE: Duration = Duration::from_secs(5);

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
pub struct Adapter {
    pub path: String,
    pub name: String,
    /// On and not on its way off.
    pub powered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Device {
    pub path: String,
    pub adapter: String,
    pub name: String,
    pub address: String,
    pub paired: bool,
    pub connected: bool,
    /// Percent, for devices that report it.
    pub battery: Option<u8>,
}

/// What BlueZ has, as one value so changes compare easily.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// BlueZ is running.
    pub available: bool,
    pub adapters: Vec<Adapter>,
    pub devices: Vec<Device>,
}

impl State {
    fn powered(&self, adapter: &str) -> bool {
        self.adapters.iter().any(|candidate| candidate.path == adapter && candidate.powered)
    }

    fn device(&self, path: &str) -> Option<&Device> {
        self.devices.iter().find(|device| device.path == path)
    }

    /// `tileception msg bluetooth`.
    pub fn describe(&self) -> String {
        if !self.available {
            return "bluetooth: unavailable (is bluetooth.service running?)\n".into();
        }
        if self.adapters.is_empty() {
            return "bluetooth: no adapter\n".into();
        }
        let mut text = String::new();
        for adapter in &self.adapters {
            let power = if adapter.powered { "on" } else { "off" };
            text += &format!("{} ({}): {power}\n", adapter.name, adapter.path);
            for device in self.devices.iter().filter(|device| device.adapter == adapter.path && device.paired) {
                let status = if device.connected { "connected" } else { "disconnected" };
                text += &format!("  {} [{}] {status}", device.name, device.address);
                if let Some(percent) = device.battery {
                    text += &format!(", {percent}%");
                }
                text += "\n";
            }
        }
        text
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

enum Event {
    State(State),
    /// A reconnect finished: the names that came back and those that did not, with why.
    Reconnected {
        connected: Vec<String>,
        failed: Vec<(String, String)>,
    },
}

enum Request {
    Connect(Vec<(String, String)>),
}

#[derive(Default)]
pub struct Bluetooth {
    pub state: State,
    memory: Memory,
    requests: Option<mpsc::Sender<Request>>,
    /// Between sleep and the reconnect after it.
    asleep: bool,
    /// The reconnect waiting for devices to settle.
    pending: Option<RegistrationToken>,
}

impl Tileception {
    /// Follows BlueZ on the system bus, or on `address` for tests.
    pub fn start_bluetooth(&mut self, address: Option<String>) {
        let (sender, receiver) = channel::channel::<Event>();
        let token = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.bluetooth_event(event);
            }
        });
        if let Err(error) = token {
            return tracing::error!("could not watch Bluetooth: {error}");
        }
        let bus = match connect(address.as_deref()) {
            Ok(bus) => bus,
            Err(error) => return tracing::warn!("bluetooth: no system bus: {error}"),
        };
        let (requests, incoming) = mpsc::channel();
        let watcher = {
            let (bus, sender) = (bus.clone(), sender.clone());
            thread::Builder::new().name("tileception-bluetooth".into()).spawn(move || {
                if let Err(error) = watch(&bus, &sender) {
                    tracing::warn!("bluetooth: stopped following BlueZ: {error}");
                }
            })
        };
        let connector = thread::Builder::new()
            .name("tileception-bluetooth-connect".into())
            .spawn(move || serve(&bus, &incoming, &sender));
        if let Err(error) = watcher.and(connector) {
            return tracing::error!("could not start the Bluetooth worker: {error}");
        }
        self.bluetooth.requests = Some(requests);
    }

    fn bluetooth_event(&mut self, event: Event) {
        match event {
            Event::State(new) => {
                let old = std::mem::replace(&mut self.bluetooth.state, new);
                let new = &self.bluetooth.state;
                let now = Instant::now();
                let holding = self.bluetooth.asleep || self.bluetooth.pending.is_some();
                self.bluetooth.memory.follow(&old, new, now, holding);
                let went_off = old.adapters.iter().any(|adapter| adapter.powered && !new.powered(&adapter.path));
                let came_on = new.adapters.iter().any(|adapter| {
                    adapter.powered && old.adapters.iter().any(|before| before.path == adapter.path && !before.powered)
                });
                if old.available != new.available {
                    tracing::info!("bluetooth: {}", if new.available { "BlueZ is running" } else { "BlueZ stopped" });
                }
                if went_off {
                    self.bluetooth.memory.hold(now);
                }
                if came_on {
                    self.schedule_bluetooth_reconnect();
                }
            }
            Event::Reconnected { connected, failed } => {
                for name in &connected {
                    tracing::info!("bluetooth: reconnected {name}");
                }
                for (name, reason) in &failed {
                    tracing::warn!("bluetooth: could not reconnect {name}: {reason}");
                }
                if !failed.is_empty() {
                    let summary = match failed.as_slice() {
                        [(name, _)] => format!("Could not reconnect {name}"),
                        _ => "Could not reconnect Bluetooth devices".into(),
                    };
                    let body = match failed.as_slice() {
                        [(_, reason)] => reason.clone(),
                        _ => failed
                            .iter()
                            .map(|(name, reason)| format!("{name}: {reason}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    self.notify_locally(&summary, &body, Urgency::Low);
                }
            }
        }
    }

    /// The computer is about to sleep.
    pub fn bluetooth_sleeping(&mut self) {
        self.bluetooth.asleep = true;
        self.bluetooth.memory.hold(Instant::now());
    }

    /// The computer woke: reconnect once devices had a moment to come back.
    pub fn bluetooth_woke(&mut self) {
        self.schedule_bluetooth_reconnect();
    }

    fn schedule_bluetooth_reconnect(&mut self) {
        if self.bluetooth.requests.is_none() || !self.config.file.bluetooth.reconnect {
            self.bluetooth.asleep = false;
            return;
        }
        if let Some(token) = self.bluetooth.pending.take() {
            self.loop_handle.remove(token);
        }
        let token = self.loop_handle.insert_source(Timer::from_duration(SETTLE), |_, _, state| {
            state.bluetooth.pending = None;
            state.reconnect_bluetooth();
            TimeoutAction::Drop
        });
        match token {
            Ok(token) => self.bluetooth.pending = Some(token),
            Err(error) => tracing::warn!("bluetooth: could not time the reconnect: {error}"),
        }
    }

    /// Connects the remembered devices that are not connected; returns their names.
    pub fn reconnect_bluetooth(&mut self) -> Vec<String> {
        self.bluetooth.asleep = false;
        let targets = self.bluetooth.memory.targets(&self.bluetooth.state);
        let names = targets.iter().map(|(_, name)| name.clone()).collect();
        if let (Some(requests), false) = (&self.bluetooth.requests, targets.is_empty()) {
            let _ = requests.send(Request::Connect(targets));
        }
        names
    }

    /// `tileception msg bluetooth [reconnect]`.
    pub fn bluetooth_command(&mut self, reconnect: bool) -> String {
        if self.bluetooth.requests.is_none() {
            return "bluetooth: not started\n".into();
        }
        if !reconnect {
            return self.bluetooth.state.describe();
        }
        match self.reconnect_bluetooth().as_slice() {
            [] => "nothing to reconnect\n".into(),
            names => format!("reconnecting {}\n", names.join(", ")),
        }
    }
}

/// `bluetooth` and `bluetooth reconnect`; true for the latter.
pub fn parse(args: &[&str]) -> Result<bool, String> {
    match args {
        [] => Ok(false),
        ["reconnect"] => Ok(true),
        _ => Err("bluetooth takes no arguments or reconnect".into()),
    }
}

fn connect(address: Option<&str>) -> zbus::Result<Connection> {
    match address {
        Some(address) => zbus::blocking::connection::Builder::address(address)?.build(),
        None => Connection::system(),
    }
}

/// Interfaces by object path, as BlueZ's object manager reports them.
type Objects = BTreeMap<String, HashMap<String, HashMap<String, OwnedValue>>>;

fn watch(bus: &Connection, events: &channel::Sender<Event>) -> zbus::Result<()> {
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
        match managed_objects(bus) {
            Ok(fetched) => *objects = fetched,
            Err(error) => tracing::warn!("bluetooth: could not list BlueZ's objects: {error}"),
        }
        Some(owner)
    };
    let mut objects = Objects::new();
    let mut owner = fetch(&mut objects);
    let mut last = state(&objects, owner.is_some());
    if !last.available {
        tracing::info!("bluetooth: BlueZ is not running");
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
            state.adapters.push(Adapter {
                path: path.clone(),
                name: Some(text(properties, "Alias"))
                    .filter(|alias| !alias.is_empty())
                    .unwrap_or_else(|| text(properties, "Name")),
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
                battery: interfaces
                    .get(BATTERY)
                    .and_then(|battery| battery.get("Percentage"))
                    .and_then(|value| u8::try_from(value).ok()),
            });
        }
    }
    state
}

/// Connects devices one at a time, as asked.
fn serve(bus: &Connection, incoming: &mpsc::Receiver<Request>, events: &channel::Sender<Event>) {
    while let Ok(Request::Connect(devices)) = incoming.recv() {
        let (mut connected, mut failed) = (Vec::new(), Vec::new());
        for (path, name) in devices {
            match bus.call_method(Some(BLUEZ), path.as_str(), Some(DEVICE), "Connect", &()) {
                Ok(_) => connected.push(name),
                Err(error) => match reason(&error) {
                    Some(reason) => failed.push((name, reason)),
                    // Already connected, or someone else is connecting it.
                    None => connected.push(name),
                },
            }
        }
        if events.send(Event::Reconnected { connected, failed }).is_err() {
            return;
        }
    }
}

/// Why connecting failed, for people; `None` when it did not really fail.
fn reason(error: &zbus::Error) -> Option<String> {
    let zbus::Error::MethodError(name, detail, _) = error else { return Some(error.to_string()) };
    let detail = detail.as_deref().unwrap_or_default();
    let text = match name.as_str() {
        "org.bluez.Error.AlreadyConnected" | "org.bluez.Error.InProgress" => return None,
        "org.bluez.Error.NotReady" => "Bluetooth is off.".into(),
        "org.bluez.Error.DoesNotExist" => "It is no longer paired.".into(),
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
        assert!(state.describe().contains("Headphones [00:11:22:33:44:55] connected, 80%"));
    }
}
