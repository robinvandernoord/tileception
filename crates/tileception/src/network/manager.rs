//! NetworkManager on the system bus, on threads of their own: a watcher
//! that follows its signals, and a worker that reads its objects into a
//! [`Snapshot`] and makes the calls, which can take seconds each.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, MessageIterator, fdo::DBusProxy};
use zbus::message::Type;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zeroize::Zeroizing;

pub const NETWORK_MANAGER: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
/// Where NetworkManager's ObjectManager is.
const ROOT: &str = "/org/freedesktop";
const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const IP4: &str = "org.freedesktop.NetworkManager.IP4Config";
const IP6: &str = "org.freedesktop.NetworkManager.IP6Config";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// A stand-in for NetworkManager on another bus, for tests: its address.
pub const BUS_VARIABLE: &str = "TILECEPTION_NETWORK_BUS";

const DEVICE_ETHERNET: u32 = 1;
const DEVICE_WIFI: u32 = 2;
const STATE_UNAVAILABLE: u32 = 20;
const STATE_ACTIVATED: u32 = 100;
const STATE_FAILED: u32 = 120;
const ACTIVE_ACTIVATED: u32 = 2;
/// Reads again this often without signals, in case one was missed.
const POLL: Duration = Duration::from_secs(30);
/// Changes this close together are read once.
const SETTLE: Duration = Duration::from_millis(120);
/// Connecting gives up after this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);

/// How a Wi-Fi network is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Open,
    /// WPA or WPA3 with a password; `sae` for WPA3 only.
    Password {
        sae: bool,
    },
    /// 802.1X, which needs more than a password.
    Enterprise,
    /// WEP, which is not offered here.
    Wep,
}

impl Security {
    pub fn secured(self) -> bool {
        self != Security::Open
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Link {
    #[default]
    Idle,
    Connecting,
    Connected,
}

/// A Wi-Fi network: the access points that share one name, as the
/// strongest of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wifi {
    pub ssid: Vec<u8>,
    pub access_point: String,
    /// Percent.
    pub strength: u8,
    pub security: Security,
    /// The saved connection for it.
    pub saved: Option<String>,
    pub link: Link,
}

impl Wifi {
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.ssid).into_owned()
    }
}

/// A saved Wi-Fi connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub path: String,
    /// The connection's name, usually the network's.
    pub name: String,
    pub ssid: Vec<u8>,
    /// Joins by itself when in range.
    pub autoconnect: bool,
}

/// A connection that is up: where it is and the addresses it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    pub wifi: bool,
    /// The connection's name.
    pub name: String,
    pub interface: String,
    pub hardware: String,
    /// With their prefix length, IPv4 first.
    pub addresses: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

/// What NetworkManager has, as one value so changes compare easily.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// NetworkManager is running.
    pub running: bool,
    /// The first Wi-Fi device.
    pub wifi_device: Option<String>,
    pub wifi_enabled: bool,
    /// False while a hardware switch or airplane mode keeps Wi-Fi off.
    pub wifi_hardware: bool,
    /// A cable that is plugged in: how far its connection is.
    pub wired: Option<Link>,
    /// Connected and connecting first, then strongest first.
    pub networks: Vec<Wifi>,
    /// Saved Wi-Fi connections, by name.
    pub saved: Vec<Saved>,
    /// The connections that are up, wired first.
    pub details: Vec<Details>,
}

impl Snapshot {
    /// The network Wi-Fi is on or getting on.
    pub fn current(&self) -> Option<&Wifi> {
        self.networks.iter().find(|network| network.link != Link::Idle)
    }
}

/// What the compositor asks for.
pub enum Request {
    Wifi(bool),
    Scan,
    Connect {
        ssid: Vec<u8>,
        access_point: String,
        saved: Option<String>,
        security: Security,
        password: Option<Zeroizing<String>>,
    },
    Disconnect,
    /// Removes a saved connection.
    Forget(String),
    /// Whether a saved connection joins by itself.
    Autoconnect(String, bool),
    /// Joins a network that does not say its name; without a password it
    /// is open.
    Hidden {
        ssid: Vec<u8>,
        password: Option<Zeroizing<String>>,
    },
}

/// What the compositor hears.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Snapshot(Snapshot),
    /// Connecting to a network did not work: why, and whether a password
    /// would help.
    Failed {
        ssid: Vec<u8>,
        reason: String,
        password: bool,
    },
}

/// What the worker waits for.
pub enum Input {
    Request(Request),
    /// Something changed; `settings` when a saved connection did.
    Changed {
        settings: bool,
    },
    /// A device failed to connect, with NetworkManager's reason.
    DeviceFailed {
        device: String,
        reason: u32,
    },
    Stop,
}

type Properties = HashMap<String, OwnedValue>;
type Objects = HashMap<String, HashMap<String, Properties>>;

/// A connection being made, until it is up or has failed.
struct Pending {
    ssid: Vec<u8>,
    device: String,
    active: String,
    /// The connection made for it, removed again when it fails for want of
    /// the right password.
    created: Option<String>,
    since: Instant,
    /// Seen activating at least once.
    seen: bool,
}

/// Watches NetworkManager's signals and feeds the worker until the worker
/// is gone.
pub fn watch(bus: &Connection, inputs: &mpsc::Sender<Input>) -> zbus::Result<()> {
    let messages = MessageIterator::from(bus);
    let dbus = DBusProxy::new(bus)?;
    let rule = |interface: &'static str, member: &'static str| -> zbus::Result<zbus::MatchRule<'static>> {
        Ok(zbus::MatchRule::builder().msg_type(Type::Signal).interface(interface)?.member(member)?.build())
    };
    dbus.add_match_rule(
        zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg(0, NETWORK_MANAGER)?
            .build(),
    )?;
    dbus.add_match_rule(rule(OBJECT_MANAGER, "InterfacesAdded")?)?;
    dbus.add_match_rule(rule(OBJECT_MANAGER, "InterfacesRemoved")?)?;
    dbus.add_match_rule(rule(DEVICE, "StateChanged")?)?;
    dbus.add_match_rule(rule(CONNECTION, "Updated")?)?;
    dbus.add_match_rule(
        zbus::MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(PROPERTIES)?
            .member("PropertiesChanged")?
            .path_namespace(NM_PATH)?
            .build(),
    )?;
    if inputs.send(Input::Changed { settings: true }).is_err() {
        return Ok(());
    }
    for message in messages {
        let Ok(message) = message else { continue };
        let header = message.header();
        if header.message_type() != Type::Signal {
            continue;
        }
        let interface = header.interface().map(|name| name.as_str().to_string()).unwrap_or_default();
        let member = header.member().map(|name| name.as_str().to_string()).unwrap_or_default();
        let path = header.path().map(|path| path.as_str().to_string()).unwrap_or_default();
        let input = match (interface.as_str(), member.as_str()) {
            ("org.freedesktop.DBus", "NameOwnerChanged") => {
                match message.body().deserialize::<(String, String, String)>() {
                    Ok((name, _, _)) if name == NETWORK_MANAGER => Input::Changed { settings: true },
                    _ => continue,
                }
            }
            (DEVICE, "StateChanged") => match message.body().deserialize::<(u32, u32, u32)>() {
                Ok((STATE_FAILED, _, reason)) => Input::DeviceFailed { device: path, reason },
                Ok(_) => Input::Changed { settings: false },
                Err(_) => continue,
            },
            (CONNECTION, "Updated") => Input::Changed { settings: true },
            (OBJECT_MANAGER, _) => Input::Changed { settings: path.is_empty() || path.starts_with(SETTINGS_PATH) },
            (PROPERTIES, _) if path.starts_with(NM_PATH) => Input::Changed { settings: false },
            _ => continue,
        };
        if inputs.send(input).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

/// Reads NetworkManager and does what is asked until told to stop.
pub fn run(bus: &Connection, inputs: &mpsc::Receiver<Input>, events: &dyn Fn(Event) -> bool) {
    let mut worker = Worker { bus, saved: HashMap::new(), last: None, pending: None, events };
    worker.refresh();
    loop {
        let wait = if worker.pending.is_some() { Duration::from_secs(1) } else { POLL };
        let mut changed = match inputs.recv_timeout(wait) {
            Ok(input) => match worker.take(input) {
                Some(changed) => changed,
                None => return,
            },
            Err(mpsc::RecvTimeoutError::Timeout) => true,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        // Signals come in bursts: read once they settle.
        let settle = Instant::now() + SETTLE;
        while let Some(left) = settle.checked_duration_since(Instant::now()) {
            match inputs.recv_timeout(left) {
                Ok(input) => match worker.take(input) {
                    Some(more) => changed |= more,
                    None => return,
                },
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        if changed {
            worker.refresh();
        }
        worker.check_pending();
    }
}

struct Worker<'a> {
    bus: &'a Connection,
    /// Saved Wi-Fi connections by path; `None` for other kinds.
    saved: HashMap<String, Option<Saved>>,
    last: Option<Snapshot>,
    pending: Option<Pending>,
    events: &'a dyn Fn(Event) -> bool,
}

impl Worker<'_> {
    /// Handles one input: whether to read again, or `None` to stop.
    fn take(&mut self, input: Input) -> Option<bool> {
        match input {
            Input::Stop => None,
            Input::Changed { settings } => {
                if settings {
                    self.saved.clear();
                }
                Some(true)
            }
            Input::DeviceFailed { device, reason } => {
                if self.pending.as_ref().is_some_and(|pending| pending.device == device) {
                    let (text, password) = failure(reason);
                    self.fail(text.to_string(), password);
                }
                Some(true)
            }
            Input::Request(request) => {
                self.handle(request);
                Some(true)
            }
        }
    }

    fn send(&self, event: Event) -> bool {
        (self.events)(event)
    }

    fn call<B>(&self, path: &str, interface: &str, method: &str, body: &B) -> zbus::Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.bus.call_method(Some(NETWORK_MANAGER), path, Some(interface), method, body)
    }

    fn device(&self) -> Option<String> {
        self.last.as_ref().and_then(|snapshot| snapshot.wifi_device.clone())
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Wifi(enabled) => {
                if let Err(error) =
                    self.call(NM_PATH, PROPERTIES, "Set", &(NETWORK_MANAGER, "WirelessEnabled", Value::from(enabled)))
                {
                    tracing::warn!("network: could not switch Wi-Fi: {error}");
                }
            }
            Request::Scan => {
                if let Some(device) = self.device() {
                    let options: HashMap<&str, Value<'_>> = HashMap::new();
                    // NetworkManager refuses scans close together; that is fine.
                    let _ = self.call(&device, WIRELESS, "RequestScan", &(options,));
                }
            }
            Request::Disconnect => {
                if let Some(device) = self.device()
                    && let Err(error) = self.call(&device, DEVICE, "Disconnect", &())
                {
                    tracing::warn!("network: could not disconnect: {error}");
                }
            }
            Request::Forget(saved) => {
                if let Err(error) = self.call(&saved, CONNECTION, "Delete", &()) {
                    tracing::warn!("network: could not forget a network: {error}");
                }
                self.saved.remove(&saved);
            }
            Request::Autoconnect(saved, autoconnect) => {
                if let Err(error) = self.set_autoconnect(&saved, autoconnect) {
                    tracing::warn!("network: could not change auto-join: {error}");
                }
                self.saved.remove(&saved);
            }
            Request::Hidden { ssid, password } => {
                let Some(device) = self.device() else { return };
                self.pending = None;
                let security = if password.is_some() { Security::Password { sae: false } } else { Security::Open };
                match self.connect_hidden(&device, &ssid, security, password) {
                    Ok((active, created)) => {
                        self.pending = Some(Pending {
                            ssid,
                            device,
                            active,
                            created: Some(created),
                            since: Instant::now(),
                            seen: false,
                        });
                    }
                    Err(error) => {
                        self.send(Event::Failed { ssid, reason: reason(&error), password: false });
                    }
                }
            }
            Request::Connect { ssid, access_point, saved, security, password } => {
                let Some(device) = self.device() else { return };
                self.pending = None;
                match self.connect(&device, &ssid, &access_point, saved.as_deref(), security, password) {
                    Ok((active, created)) => {
                        self.pending =
                            Some(Pending { ssid, device, active, created, since: Instant::now(), seen: false });
                    }
                    Err(error) => {
                        self.send(Event::Failed { ssid, reason: reason(&error), password: false });
                    }
                }
            }
        }
    }

    /// Starts connecting: the active connection's path, and the connection
    /// made for it when it was new.
    fn connect(
        &self,
        device: &str,
        ssid: &[u8],
        access_point: &str,
        saved: Option<&str>,
        security: Security,
        password: Option<Zeroizing<String>>,
    ) -> zbus::Result<(String, Option<String>)> {
        let device = ObjectPath::try_from(device)?;
        let specific =
            ObjectPath::try_from(access_point).unwrap_or_else(|_| ObjectPath::from_static_str_unchecked("/"));
        if let Some(saved) = saved {
            if let Some(password) = &password {
                self.update_password(saved, security, password)?;
            }
            let connection = ObjectPath::try_from(saved)?;
            let reply = self.call(NM_PATH, NETWORK_MANAGER, "ActivateConnection", &(connection, device, specific))?;
            let active: OwnedObjectPath = reply.body().deserialize()?;
            return Ok((active.to_string(), None));
        }
        let settings = new_connection(ssid, security, password.as_ref().map(|password| password.as_str()));
        let reply = self.call(NM_PATH, NETWORK_MANAGER, "AddAndActivateConnection", &(settings, device, specific))?;
        let (created, active): (OwnedObjectPath, OwnedObjectPath) = reply.body().deserialize()?;
        Ok((active.to_string(), Some(created.to_string())))
    }

    fn connect_hidden(
        &self,
        device: &str,
        ssid: &[u8],
        security: Security,
        password: Option<Zeroizing<String>>,
    ) -> zbus::Result<(String, String)> {
        let device = ObjectPath::try_from(device)?;
        let mut settings = new_connection(ssid, security, password.as_ref().map(|password| password.as_str()));
        if let Some(wireless) = settings.get_mut("802-11-wireless") {
            wireless.insert("hidden", Value::from(true));
        }
        let specific = ObjectPath::from_static_str_unchecked("/");
        let reply = self.call(NM_PATH, NETWORK_MANAGER, "AddAndActivateConnection", &(settings, device, specific))?;
        let (created, active): (OwnedObjectPath, OwnedObjectPath) = reply.body().deserialize()?;
        Ok((active.to_string(), created.to_string()))
    }

    /// Changes whether a saved connection joins by itself. NetworkManager
    /// keeps the secrets an update leaves out.
    fn set_autoconnect(&self, saved: &str, autoconnect: bool) -> zbus::Result<()> {
        let reply = self.call(saved, CONNECTION, "GetSettings", &())?;
        let mut settings: HashMap<String, HashMap<String, OwnedValue>> = reply.body().deserialize()?;
        let value = OwnedValue::try_from(Value::from(autoconnect)).map_err(zbus::Error::from)?;
        settings.entry("connection".into()).or_default().insert("autoconnect".into(), value);
        self.call(saved, CONNECTION, "Update", &(settings,))?;
        Ok(())
    }

    /// Stores a new password in a saved connection, kept by NetworkManager
    /// itself rather than by a secret agent that may not be running.
    fn update_password(&self, saved: &str, security: Security, password: &str) -> zbus::Result<()> {
        let reply = self.call(saved, CONNECTION, "GetSettings", &())?;
        let mut settings: HashMap<String, HashMap<String, OwnedValue>> = reply.body().deserialize()?;
        let owned = |value: Value<'_>| OwnedValue::try_from(value).map_err(zbus::Error::from);
        let section = settings.entry("802-11-wireless-security".into()).or_default();
        if !section.contains_key("key-mgmt") {
            section.insert("key-mgmt".into(), owned(Value::from(key_management(security)))?);
        }
        section.insert("psk".into(), owned(Value::from(password))?);
        section.insert("psk-flags".into(), owned(Value::from(0u32))?);
        self.call(saved, CONNECTION, "Update", &(settings,))?;
        Ok(())
    }

    fn fail(&mut self, reason: String, password: bool) {
        let Some(pending) = self.pending.take() else { return };
        if password && let Some(created) = &pending.created {
            // A connection with a wrong password would only fail again.
            let _ = self.call(created, CONNECTION, "Delete", &());
        }
        self.send(Event::Failed { ssid: pending.ssid, reason, password });
    }

    /// Follows a connection being made, until it is up or has failed.
    fn check_pending(&mut self) {
        let Some(pending) = &self.pending else { return };
        let state = self.bus.call_method(
            Some(NETWORK_MANAGER),
            pending.active.as_str(),
            Some(PROPERTIES),
            "Get",
            &(ACTIVE, "State"),
        );
        let state = state.ok().and_then(|reply| reply.body().deserialize::<OwnedValue>().ok());
        let elapsed = pending.since.elapsed();
        // Right after asking, the connection may not be there yet.
        let early = !pending.seen && elapsed < Duration::from_secs(5);
        match state.and_then(|value| u32::try_from(&value).ok()) {
            Some(ACTIVE_ACTIVATED) => self.pending = None,
            Some(1) if elapsed < CONNECT_TIMEOUT => {
                if let Some(pending) = &mut self.pending {
                    pending.seen = true;
                }
            }
            Some(1) => self.fail("The network did not answer".into(), false),
            Some(0) | None if early => {}
            _ => self.fail("Could not connect".into(), false),
        }
    }

    /// Reads everything again and tells the compositor when it changed.
    fn refresh(&mut self) {
        let snapshot = match self.read() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if self.last.as_ref().is_none_or(|last| last.running) {
                    tracing::info!("network: NetworkManager is not available: {error}");
                }
                Snapshot::default()
            }
        };
        if self.last.as_ref() != Some(&snapshot) {
            self.last = Some(snapshot.clone());
            self.send(Event::Snapshot(snapshot));
        }
    }

    fn read(&mut self) -> zbus::Result<Snapshot> {
        let reply =
            self.bus.call_method(Some(NETWORK_MANAGER), ROOT, Some(OBJECT_MANAGER), "GetManagedObjects", &())?;
        let objects: HashMap<OwnedObjectPath, HashMap<String, Properties>> = reply.body().deserialize()?;
        let objects: Objects = objects.into_iter().map(|(path, interfaces)| (path.to_string(), interfaces)).collect();
        let manager = match objects.get(NM_PATH).and_then(|interfaces| interfaces.get(NETWORK_MANAGER)) {
            Some(manager) => manager.clone(),
            None => {
                let reply = self.call(NM_PATH, PROPERTIES, "GetAll", &(NETWORK_MANAGER,))?;
                reply.body().deserialize()?
            }
        };
        let listed: Vec<OwnedObjectPath> =
            self.call(SETTINGS_PATH, SETTINGS, "ListConnections", &())?.body().deserialize()?;
        let listed: Vec<String> = listed.into_iter().map(|path| path.to_string()).collect();
        self.saved.retain(|path, _| listed.contains(path));
        for path in listed {
            if self.saved.contains_key(&path) {
                continue;
            }
            let saved = self
                .call(&path, CONNECTION, "GetSettings", &())
                .and_then(|reply| reply.body().deserialize::<HashMap<String, Properties>>())
                .ok()
                .and_then(|settings| saved_wifi(&path, &settings));
            self.saved.insert(path, saved);
        }
        Ok(snapshot(&manager, &objects, &self.saved))
    }
}

/// The settings for a new Wi-Fi connection; NetworkManager fills in the rest
/// from the access point.
fn new_connection<'a>(
    ssid: &'a [u8],
    security: Security,
    password: Option<&'a str>,
) -> HashMap<&'static str, HashMap<&'static str, Value<'a>>> {
    let mut settings = HashMap::new();
    settings.insert(
        "connection",
        HashMap::from([
            ("id", Value::from(String::from_utf8_lossy(ssid).into_owned())),
            ("type", Value::from("802-11-wireless")),
        ]),
    );
    settings.insert(
        "802-11-wireless",
        HashMap::from([("ssid", Value::from(ssid.to_vec())), ("mode", Value::from("infrastructure"))]),
    );
    if let (Security::Password { .. }, Some(password)) = (security, password) {
        settings.insert(
            "802-11-wireless-security",
            HashMap::from([("key-mgmt", Value::from(key_management(security))), ("psk", Value::from(password))]),
        );
    }
    settings
}

fn key_management(security: Security) -> &'static str {
    match security {
        Security::Password { sae: true } => "sae",
        _ => "wpa-psk",
    }
}

/// A saved connection, for Wi-Fi connections.
fn saved_wifi(path: &str, settings: &HashMap<String, Properties>) -> Option<Saved> {
    let connection = settings.get("connection")?;
    if <&str>::try_from(connection.get("type")?).ok()? != "802-11-wireless" {
        return None;
    }
    let ssid = bytes(settings.get("802-11-wireless")?.get("ssid")?)?;
    let name = connection
        .get("id")
        .and_then(|name| <&str>::try_from(name).ok())
        .map_or_else(|| String::from_utf8_lossy(&ssid).into_owned(), str::to_string);
    let autoconnect = connection.get("autoconnect").is_none_or(|value| bool::try_from(value).unwrap_or(true));
    Some(Saved { path: path.to_string(), name, ssid, autoconnect })
}

fn text(properties: &Properties, name: &str) -> String {
    properties.get(name).and_then(|value| <&str>::try_from(value).ok()).unwrap_or_default().to_string()
}

/// An entry of an `a{sv}` value.
fn entry<'v>(dict: &'v zbus::zvariant::Dict<'_, '_>, name: &str) -> Option<&'v Value<'v>> {
    let (_, value) = dict.iter().find(|(key, _)| matches!(key, Value::Str(key) if key.as_str() == name))?;
    Some(match value {
        Value::Value(inner) => inner,
        other => other,
    })
}

/// The "address" (and "prefix") of each entry of an `aa{sv}`, as text.
fn address_data(value: Option<&OwnedValue>, prefix: bool) -> Vec<String> {
    let Some(Value::Array(array)) = value.map(|value| &**value) else { return Vec::new() };
    array
        .iter()
        .filter_map(|item| {
            let Value::Dict(dict) = item else { return None };
            let Some(Value::Str(address)) = entry(dict, "address") else { return None };
            let length = entry(dict, "prefix").and_then(|length| u32::try_from(length).ok());
            Some(match length {
                Some(length) if prefix => format!("{address}/{length}"),
                _ => address.to_string(),
            })
        })
        .collect()
}

/// Where a connection that is up is, and the addresses it got.
fn details(objects: &Objects, device: &Properties, wifi: bool, extra: Option<&Properties>) -> Details {
    let config = |property: &str, interface: &str| {
        object(device.get(property)).and_then(|path| objects.get(&path)?.get(interface).cloned())
    };
    let ip4 = config("Ip4Config", IP4);
    let ip6 = config("Ip6Config", IP6);
    let mut addresses = Vec::new();
    let mut gateway = None;
    let mut dns = Vec::new();
    for (config, version) in [(&ip4, 4), (&ip6, 6)] {
        let Some(config) = config else { continue };
        addresses.extend(
            address_data(config.get("AddressData"), true)
                .into_iter()
                .filter(|address| version == 4 || !address.starts_with("fe80")),
        );
        if gateway.is_none() {
            gateway = Some(text(config, "Gateway")).filter(|gateway| !gateway.is_empty());
        }
        dns.extend(address_data(config.get("NameserverData"), false));
        if version == 6
            && let Some(Value::Array(servers)) = config.get("Nameservers").map(|value| &**value)
        {
            // IPv6 servers are byte arrays.
            for server in servers.iter() {
                let Value::Array(octets) = server else { continue };
                let octets: Option<Vec<u8>> = octets.iter().map(|byte| u8::try_from(byte).ok()).collect();
                if let Some(Ok(octets)) = octets.map(<[u8; 16]>::try_from) {
                    dns.push(std::net::Ipv6Addr::from(octets).to_string());
                }
            }
        }
    }
    let name = object(device.get("ActiveConnection"))
        .and_then(|active| objects.get(&active)?.get(ACTIVE).map(|active| text(active, "Id")))
        .unwrap_or_default();
    let hardware = Some(text(device, "HwAddress"))
        .filter(|address| !address.is_empty())
        .or_else(|| extra.map(|extra| text(extra, "HwAddress")))
        .unwrap_or_default();
    Details { wifi, name, interface: text(device, "Interface"), hardware, addresses, gateway, dns }
}

/// What a device's failure reason means to someone connecting, and whether
/// asking for the password again could help.
pub fn failure(reason: u32) -> (&'static str, bool) {
    match reason {
        // No secrets, the supplicant disconnected or failed: usually the password.
        7 | 8 | 10 => ("Wrong password?", true),
        9 => ("The network's settings do not work", true),
        11 => ("The network did not answer", false),
        5 | 6 => ("No address from the network", false),
        53 => ("The network is out of range", false),
        _ => ("Could not connect", false),
    }
}

/// A call's error as a short sentence.
fn reason(error: &zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(_, Some(detail), _) => detail.clone(),
        zbus::Error::MethodError(name, None, _) => name.to_string(),
        other => other.to_string(),
    }
}

pub fn link(state: u32) -> Link {
    match state {
        STATE_ACTIVATED => Link::Connected,
        40..=90 => Link::Connecting,
        _ => Link::Idle,
    }
}

pub fn security(flags: u32, wpa: u32, rsn: u32) -> Security {
    const PRIVACY: u32 = 0x1;
    const PSK: u32 = 0x100;
    const EAP: u32 = 0x200;
    const SAE: u32 = 0x400;
    let both = wpa | rsn;
    if both & EAP != 0 {
        Security::Enterprise
    } else if both & (PSK | SAE) != 0 {
        Security::Password { sae: both & PSK == 0 }
    } else if flags & PRIVACY != 0 && both == 0 {
        Security::Wep
    } else {
        // Open, or OWE, which needs nothing from the user either.
        Security::Open
    }
}

fn bytes(value: &OwnedValue) -> Option<Vec<u8>> {
    match &**value {
        Value::Array(array) => array.iter().map(|byte| u8::try_from(byte).ok()).collect(),
        _ => None,
    }
}

fn object(value: Option<&OwnedValue>) -> Option<String> {
    match value.map(|value| &**value) {
        Some(Value::ObjectPath(path)) if path.as_str() != "/" => Some(path.to_string()),
        _ => None,
    }
}

fn objects_in(value: Option<&OwnedValue>) -> Vec<String> {
    match value.map(|value| &**value) {
        Some(Value::Array(array)) => array
            .iter()
            .filter_map(|item| match item {
                Value::ObjectPath(path) => Some(path.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn number(properties: &Properties, name: &str) -> u32 {
    properties.get(name).and_then(|value| u32::try_from(value).ok()).unwrap_or(0)
}

fn flag(properties: &Properties, name: &str) -> bool {
    properties.get(name).and_then(|value| bool::try_from(value).ok()).unwrap_or(false)
}

/// NetworkManager's objects as a snapshot. `saved` holds the saved Wi-Fi
/// connections' network names by path.
fn snapshot(manager: &Properties, objects: &Objects, saved: &HashMap<String, Option<Saved>>) -> Snapshot {
    let mut snapshot = Snapshot {
        running: true,
        wifi_enabled: flag(manager, "WirelessEnabled"),
        wifi_hardware: manager.get("WirelessHardwareEnabled").is_none_or(|value| bool::try_from(value).unwrap_or(true)),
        ..Snapshot::default()
    };
    let mut devices: Vec<(&String, &HashMap<String, Properties>)> =
        objects.iter().filter(|(_, interfaces)| interfaces.contains_key(DEVICE)).collect();
    devices.sort_by(|left, right| left.0.cmp(right.0));
    let mut wifi = None;
    for (path, interfaces) in devices {
        let device = &interfaces[DEVICE];
        let state = number(device, "State");
        let kind = number(device, "DeviceType");
        if state == STATE_ACTIVATED && matches!(kind, DEVICE_ETHERNET | DEVICE_WIFI) {
            let extra = interfaces.get(if kind == DEVICE_WIFI {
                WIRELESS
            } else {
                "org.freedesktop.NetworkManager.Device.Wired"
            });
            snapshot.details.push(details(objects, device, kind == DEVICE_WIFI, extra));
        }
        match kind {
            DEVICE_ETHERNET if snapshot.wired.is_none() && state > STATE_UNAVAILABLE => {
                snapshot.wired = Some(link(state));
            }
            // Unmanaged devices (state 10) are left out.
            DEVICE_WIFI if wifi.is_none() && state >= STATE_UNAVAILABLE => {
                wifi = Some((path.clone(), interfaces));
            }
            _ => {}
        }
    }
    snapshot.details.sort_by_key(|details| details.wifi);
    snapshot.saved = saved.values().flatten().cloned().collect();
    snapshot.saved.sort_by(|left, right| {
        left.name.to_lowercase().cmp(&right.name.to_lowercase()).then_with(|| left.path.cmp(&right.path))
    });
    let Some((device_path, interfaces)) = wifi else { return snapshot };
    snapshot.wifi_device = Some(device_path);
    let device = &interfaces[DEVICE];
    let state = link(number(device, "State"));
    let wireless = interfaces.get(WIRELESS);
    // The network the device is on: from its connection, else its access point.
    let active_ssid = object(device.get("ActiveConnection"))
        .and_then(|active| objects.get(&active)?.get(ACTIVE)?.get("Connection").map(|value| object(Some(value))))
        .flatten()
        .and_then(|connection| saved.get(&connection).cloned().flatten().map(|saved| saved.ssid))
        .or_else(|| {
            let point = object(wireless.and_then(|wireless| wireless.get("ActiveAccessPoint")))?;
            bytes(objects.get(&point)?.get(ACCESS_POINT)?.get("Ssid")?)
        });
    let mut networks: Vec<Wifi> = Vec::new();
    for point in objects_in(wireless.and_then(|wireless| wireless.get("AccessPoints"))) {
        let Some(properties) = objects.get(&point).and_then(|interfaces| interfaces.get(ACCESS_POINT)) else {
            continue;
        };
        let Some(ssid) = properties.get("Ssid").and_then(bytes).filter(|ssid| ssid.iter().any(|byte| *byte != 0))
        else {
            continue;
        };
        let strength = properties.get("Strength").and_then(|value| u8::try_from(value).ok()).unwrap_or(0).min(100);
        let secured =
            security(number(properties, "Flags"), number(properties, "WpaFlags"), number(properties, "RsnFlags"));
        match networks.iter_mut().find(|network| network.ssid == ssid) {
            Some(network) if network.strength >= strength => {}
            Some(network) => {
                network.access_point = point;
                network.strength = strength;
                network.security = secured;
            }
            None => networks.push(Wifi {
                saved: saved
                    .iter()
                    .filter(|(_, saved)| saved.as_ref().is_some_and(|saved| saved.ssid == ssid))
                    .map(|(path, _)| path.clone())
                    .min(),
                link: if active_ssid.as_deref() == Some(&ssid[..]) { state } else { Link::Idle },
                ssid,
                access_point: point,
                strength,
                security: secured,
            }),
        }
    }
    networks.sort_by(|left, right| {
        (left.link == Link::Idle)
            .cmp(&(right.link == Link::Idle))
            .then(right.strength.cmp(&left.strength))
            .then_with(|| left.name().to_lowercase().cmp(&right.name().to_lowercase()))
    });
    snapshot.networks = networks;
    snapshot
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn value(value: Value<'static>) -> OwnedValue {
        OwnedValue::try_from(value).unwrap()
    }

    fn path(text: &str) -> Value<'static> {
        Value::from(OwnedObjectPath::try_from(text.to_string()).unwrap())
    }

    fn props(entries: Vec<(&str, Value<'static>)>) -> Properties {
        entries.into_iter().map(|(name, item)| (name.to_string(), value(item))).collect()
    }

    fn point(ssid: &str, strength: u8, rsn: u32) -> HashMap<String, Properties> {
        HashMap::from([(
            ACCESS_POINT.to_string(),
            props(vec![
                ("Ssid", Value::from(ssid.as_bytes().to_vec())),
                ("Strength", Value::from(strength)),
                ("Flags", Value::from(u32::from(rsn != 0))),
                ("WpaFlags", Value::from(0u32)),
                ("RsnFlags", Value::from(rsn)),
            ]),
        )])
    }

    #[test]
    fn reads_networks_strongest_first_with_the_current_one_on_top() {
        let manager = props(vec![("WirelessEnabled", Value::from(true))]);
        let device = "/org/freedesktop/NetworkManager/Devices/2";
        let points = ["/ap/1", "/ap/2", "/ap/3", "/ap/4", "/ap/5"];
        let mut objects: Objects = HashMap::new();
        objects.insert(
            device.into(),
            HashMap::from([
                (
                    DEVICE.to_string(),
                    props(vec![
                        ("DeviceType", Value::from(DEVICE_WIFI)),
                        ("State", Value::from(STATE_ACTIVATED)),
                        ("ActiveConnection", path("/active/1")),
                    ]),
                ),
                (
                    WIRELESS.to_string(),
                    props(vec![
                        (
                            "AccessPoints",
                            Value::from(
                                points
                                    .iter()
                                    .map(|point| OwnedObjectPath::try_from(point.to_string()).unwrap())
                                    .collect::<Vec<_>>(),
                            ),
                        ),
                        ("ActiveAccessPoint", path("/ap/1")),
                    ]),
                ),
            ]),
        );
        objects.insert(
            "/org/freedesktop/NetworkManager/Devices/1".into(),
            HashMap::from([(
                DEVICE.to_string(),
                props(vec![("DeviceType", Value::from(DEVICE_ETHERNET)), ("State", Value::from(STATE_UNAVAILABLE))]),
            )]),
        );
        objects.insert(
            "/active/1".into(),
            HashMap::from([(ACTIVE.to_string(), props(vec![("Connection", path("/settings/home"))]))]),
        );
        objects.insert("/ap/1".into(), point("Home", 40, 0x108));
        objects.insert("/ap/2".into(), point("Cafe", 70, 0));
        objects.insert("/ap/3".into(), point("Office", 55, 0x288));
        objects.insert("/ap/4".into(), point("Home", 60, 0x108));
        objects.insert("/ap/5".into(), point("", 90, 0));
        let saved = HashMap::from([
            (
                "/settings/home".to_string(),
                Some(Saved {
                    path: "/settings/home".into(),
                    name: "Home".into(),
                    ssid: b"Home".to_vec(),
                    autoconnect: true,
                }),
            ),
            ("/settings/vpn".to_string(), None),
        ]);
        let snapshot = snapshot(&manager, &objects, &saved);
        assert!(snapshot.running && snapshot.wifi_enabled && snapshot.wifi_hardware);
        assert_eq!(snapshot.wifi_device.as_deref(), Some(device));
        assert_eq!(snapshot.wired, None, "an unplugged cable is left out");
        let names: Vec<String> = snapshot.networks.iter().map(Wifi::name).collect();
        assert_eq!(names, ["Home", "Cafe", "Office"], "hidden networks are left out");
        let home = &snapshot.networks[0];
        assert_eq!((home.link, home.strength, home.access_point.as_str()), (Link::Connected, 60, "/ap/4"));
        assert_eq!(home.saved.as_deref(), Some("/settings/home"));
        assert_eq!(home.security, Security::Password { sae: false });
        assert_eq!(snapshot.networks[1].security, Security::Open);
        assert_eq!(snapshot.networks[2].security, Security::Enterprise);
        assert_eq!(snapshot.current().map(Wifi::name).as_deref(), Some("Home"));
    }

    #[test]
    fn tells_security_and_failures_apart() {
        assert_eq!(security(1, 0, 0x400), Security::Password { sae: true });
        assert_eq!(security(1, 0x100, 0x500), Security::Password { sae: false });
        assert_eq!(security(1, 0, 0), Security::Wep);
        assert_eq!(security(0, 0, 0x800), Security::Open);
        assert_eq!(failure(7), ("Wrong password?", true));
        assert!(!failure(53).1);
        assert_eq!(link(70), Link::Connecting);
        let settings = new_connection(b"Cafe", Security::Password { sae: true }, Some("secret12"));
        assert_eq!(settings["802-11-wireless-security"]["key-mgmt"], Value::from("sae"));
        assert!(!new_connection(b"Cafe", Security::Open, None).contains_key("802-11-wireless-security"));
    }

    /// A stand-in for NetworkManager: one Wi-Fi device that sees "Home"
    /// (saved) and "Cafe", each with its own password.
    mod fake {
        use std::collections::{BTreeMap, HashMap};
        use std::sync::{Arc, Mutex, MutexGuard};

        use zbus::{
            interface,
            object_server::{ObjectServer, SignalEmitter},
            zvariant::{OwnedObjectPath, OwnedValue, Value},
        };

        pub const DEVICE_PATH: &str = "/org/freedesktop/NetworkManager/Devices/1";
        const ACTIVE_PATH: &str = "/org/freedesktop/NetworkManager/ActiveConnection/1";
        const IP4_PATH: &str = "/org/freedesktop/NetworkManager/IP4Config/1";
        /// What a failed activation returns: it never shows up.
        const GONE_PATH: &str = "/org/freedesktop/NetworkManager/ActiveConnection/99";
        const POINTS: [(&str, &str, u8); 2] = [
            ("/org/freedesktop/NetworkManager/AccessPoint/1", "Home", 70),
            ("/org/freedesktop/NetworkManager/AccessPoint/2", "Cafe", 50),
        ];

        #[derive(Default)]
        pub struct World {
            pub wifi_enabled: bool,
            /// Saved connections by path: the network's name and password.
            pub connections: BTreeMap<String, (Vec<u8>, Option<String>)>,
            /// The connection Wi-Fi is on.
            pub active: Option<String>,
            pub scans: usize,
            /// Saved connections that do not join by themselves.
            pub manual: Vec<String>,
            /// Networks joined as hidden ones.
            pub hidden: Vec<Vec<u8>>,
            next: u32,
        }

        pub type Shared = Arc<Mutex<World>>;

        fn lock(world: &Shared) -> MutexGuard<'_, World> {
            world.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn path(text: &str) -> OwnedObjectPath {
            OwnedObjectPath::try_from(text.to_string()).unwrap()
        }

        fn owned(value: Value<'_>) -> OwnedValue {
            OwnedValue::try_from(value).unwrap()
        }

        /// The password a network takes; none for an open one.
        fn right_password(ssid: &[u8]) -> Option<&'static str> {
            match ssid {
                b"Home" => Some("home-password"),
                b"Attic" => None,
                _ => Some("cafe-password"),
            }
        }

        /// Turns the device on to `connection` when its password is right;
        /// else fails as a wrong password does.
        async fn activate(
            world: &Shared,
            connection: &zbus::Connection,
            server: &ObjectServer,
            saved: &str,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let right = lock(world)
                .connections
                .get(saved)
                .is_some_and(|(ssid, password)| password.as_deref() == right_password(ssid));
            let emitter = SignalEmitter::new(connection, DEVICE_PATH)?;
            if !right {
                Device::device_state_changed(&emitter, super::STATE_FAILED, 50, 7).await?;
                return Ok(path(GONE_PATH));
            }
            let name = {
                let mut world = lock(world);
                world.active = Some(saved.to_string());
                world.connections.get(saved).map(|(ssid, _)| String::from_utf8_lossy(ssid).into_owned())
            };
            // There is none the first time.
            let _ = server.remove::<Active, _>(ACTIVE_PATH).await;
            let active = Active { connection: saved.to_string(), name: name.unwrap_or_default() };
            server.at(ACTIVE_PATH, active).await?;
            Device::device_state_changed(&emitter, super::STATE_ACTIVATED, 50, 0).await?;
            Ok(path(ACTIVE_PATH))
        }

        pub struct Manager(pub Shared);

        #[interface(name = "org.freedesktop.NetworkManager")]
        impl Manager {
            async fn activate_connection(
                &self,
                #[zbus(connection)] connection: &zbus::Connection,
                #[zbus(object_server)] server: &ObjectServer,
                saved: OwnedObjectPath,
                _device: OwnedObjectPath,
                _specific: OwnedObjectPath,
            ) -> zbus::fdo::Result<OwnedObjectPath> {
                activate(&self.0, connection, server, saved.as_str()).await
            }

            async fn add_and_activate_connection(
                &self,
                #[zbus(connection)] connection: &zbus::Connection,
                #[zbus(object_server)] server: &ObjectServer,
                settings: HashMap<String, HashMap<String, OwnedValue>>,
                _device: OwnedObjectPath,
                _specific: OwnedObjectPath,
            ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
                let ssid = settings
                    .get("802-11-wireless")
                    .and_then(|wireless| wireless.get("ssid"))
                    .and_then(super::bytes)
                    .ok_or_else(|| zbus::fdo::Error::InvalidArgs("no ssid".into()))?;
                let password = settings
                    .get("802-11-wireless-security")
                    .and_then(|security| security.get("psk"))
                    .and_then(|psk| <&str>::try_from(psk).ok())
                    .map(str::to_string);
                let hidden = settings
                    .get("802-11-wireless")
                    .and_then(|wireless| wireless.get("hidden"))
                    .is_some_and(|hidden| bool::try_from(hidden).unwrap_or(false));
                let saved = {
                    let mut world = lock(&self.0);
                    if hidden {
                        world.hidden.push(ssid.clone());
                    }
                    world.next += 1;
                    let saved = format!("/org/freedesktop/NetworkManager/Settings/{}", world.next + 10);
                    world.connections.insert(saved.clone(), (ssid, password));
                    saved
                };
                server.at(saved.as_str(), Saved { path: saved.clone(), world: self.0.clone() }).await?;
                let active = activate(&self.0, connection, server, &saved).await?;
                Ok((path(&saved), active))
            }

            #[zbus(property)]
            fn wireless_enabled(&self) -> bool {
                lock(&self.0).wifi_enabled
            }

            #[zbus(property)]
            fn set_wireless_enabled(&mut self, enabled: bool) {
                let mut world = lock(&self.0);
                world.wifi_enabled = enabled;
                if !enabled {
                    world.active = None;
                }
            }

            #[zbus(property)]
            fn wireless_hardware_enabled(&self) -> bool {
                true
            }
        }

        pub struct Settings(pub Shared);

        #[interface(name = "org.freedesktop.NetworkManager.Settings")]
        impl Settings {
            fn list_connections(&self) -> Vec<OwnedObjectPath> {
                lock(&self.0).connections.keys().map(|saved| path(saved)).collect()
            }
        }

        pub struct Saved {
            pub path: String,
            pub world: Shared,
        }

        #[interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
        impl Saved {
            fn get_settings(&self) -> HashMap<String, HashMap<String, OwnedValue>> {
                let world = lock(&self.world);
                let ssid = world.connections.get(&self.path).map(|(ssid, _)| ssid.clone()).unwrap_or_default();
                let autoconnect = !world.manual.contains(&self.path);
                HashMap::from([
                    (
                        "connection".to_string(),
                        HashMap::from([
                            ("type".to_string(), owned(Value::from("802-11-wireless"))),
                            ("id".to_string(), owned(Value::from(String::from_utf8_lossy(&ssid).into_owned()))),
                            ("autoconnect".to_string(), owned(Value::from(autoconnect))),
                        ]),
                    ),
                    ("802-11-wireless".to_string(), HashMap::from([("ssid".to_string(), owned(Value::from(ssid)))])),
                ])
            }

            fn update(&self, settings: HashMap<String, HashMap<String, OwnedValue>>) {
                let autoconnect = settings
                    .get("connection")
                    .and_then(|connection| connection.get("autoconnect"))
                    .is_none_or(|value| bool::try_from(value).unwrap_or(true));
                {
                    let mut world = lock(&self.world);
                    world.manual.retain(|saved| *saved != self.path);
                    if !autoconnect {
                        world.manual.push(self.path.clone());
                    }
                }
                let password = settings
                    .get("802-11-wireless-security")
                    .and_then(|security| security.get("psk"))
                    .and_then(|psk| <&str>::try_from(psk).ok())
                    .map(str::to_string);
                // As NetworkManager does, an update without secrets keeps them.
                if let Some((_, saved)) = lock(&self.world).connections.get_mut(&self.path)
                    && password.is_some()
                {
                    *saved = password;
                }
            }

            async fn delete(&self, #[zbus(object_server)] server: &ObjectServer) -> zbus::fdo::Result<()> {
                lock(&self.world).connections.remove(&self.path);
                server.remove::<Saved, _>(self.path.as_str()).await?;
                Ok(())
            }
        }

        pub struct Device(pub Shared);

        #[interface(name = "org.freedesktop.NetworkManager.Device")]
        impl Device {
            fn disconnect(&self) {
                lock(&self.0).active = None;
            }

            #[zbus(signal, name = "StateChanged")]
            async fn device_state_changed(
                emitter: &SignalEmitter<'_>,
                new: u32,
                old: u32,
                reason: u32,
            ) -> zbus::Result<()>;

            #[zbus(property)]
            fn device_type(&self) -> u32 {
                super::DEVICE_WIFI
            }

            #[zbus(property)]
            fn state(&self) -> u32 {
                if lock(&self.0).active.is_some() { super::STATE_ACTIVATED } else { 30 }
            }

            #[zbus(property)]
            fn active_connection(&self) -> OwnedObjectPath {
                path(if lock(&self.0).active.is_some() { ACTIVE_PATH } else { "/" })
            }

            #[zbus(property)]
            fn ip4_config(&self) -> OwnedObjectPath {
                path(if lock(&self.0).active.is_some() { IP4_PATH } else { "/" })
            }

            #[zbus(property)]
            fn interface(&self) -> String {
                "wlp2s0".into()
            }

            #[zbus(property)]
            fn hw_address(&self) -> String {
                "AA:BB:CC:DD:EE:FF".into()
            }
        }

        pub struct Ip4;

        #[interface(name = "org.freedesktop.NetworkManager.IP4Config")]
        impl Ip4 {
            #[zbus(property)]
            fn address_data(&self) -> Vec<HashMap<String, OwnedValue>> {
                vec![HashMap::from([
                    ("address".to_string(), owned(Value::from("192.168.1.23"))),
                    ("prefix".to_string(), owned(Value::from(24u32))),
                ])]
            }

            #[zbus(property)]
            fn gateway(&self) -> String {
                "192.168.1.1".into()
            }

            #[zbus(property)]
            fn nameserver_data(&self) -> Vec<HashMap<String, OwnedValue>> {
                vec![HashMap::from([("address".to_string(), owned(Value::from("1.1.1.1")))])]
            }
        }

        pub struct Wireless(pub Shared);

        #[interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
        impl Wireless {
            fn request_scan(&self, _options: HashMap<String, OwnedValue>) {
                lock(&self.0).scans += 1;
            }

            #[zbus(property)]
            fn access_points(&self) -> Vec<OwnedObjectPath> {
                POINTS.iter().map(|(point, ..)| path(point)).collect()
            }

            #[zbus(property)]
            fn active_access_point(&self) -> OwnedObjectPath {
                path("/")
            }
        }

        pub struct Point {
            ssid: &'static str,
            strength: u8,
        }

        #[interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
        impl Point {
            #[zbus(property)]
            fn ssid(&self) -> Vec<u8> {
                self.ssid.as_bytes().to_vec()
            }

            #[zbus(property)]
            fn strength(&self) -> u8 {
                self.strength
            }

            #[zbus(property)]
            fn flags(&self) -> u32 {
                1
            }

            #[zbus(property)]
            fn wpa_flags(&self) -> u32 {
                0
            }

            #[zbus(property)]
            fn rsn_flags(&self) -> u32 {
                0x188
            }
        }

        pub struct Active {
            connection: String,
            name: String,
        }

        #[interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
        impl Active {
            #[zbus(property)]
            fn connection(&self) -> OwnedObjectPath {
                path(&self.connection)
            }

            #[zbus(property)]
            fn state(&self) -> u32 {
                super::ACTIVE_ACTIVATED
            }

            #[zbus(property, name = "Id")]
            fn name(&self) -> String {
                self.name.clone()
            }
        }

        /// NetworkManager with Wi-Fi on and "Home" saved, on its own connection.
        pub fn network_manager(address: &str) -> (zbus::blocking::Connection, Shared) {
            let home = "/org/freedesktop/NetworkManager/Settings/1".to_string();
            let world = Shared::new(Mutex::new(World {
                wifi_enabled: true,
                connections: BTreeMap::from([(home.clone(), (b"Home".to_vec(), Some("home-password".into())))]),
                ..World::default()
            }));
            let mut builder = zbus::blocking::connection::Builder::address(address)
                .unwrap()
                .name(super::NETWORK_MANAGER)
                .unwrap()
                .serve_at(super::ROOT, zbus::fdo::ObjectManager)
                .unwrap()
                .serve_at(super::NM_PATH, Manager(world.clone()))
                .unwrap()
                .serve_at(super::SETTINGS_PATH, Settings(world.clone()))
                .unwrap()
                .serve_at(home.as_str(), Saved { path: home.clone(), world: world.clone() })
                .unwrap()
                .serve_at(DEVICE_PATH, Device(world.clone()))
                .unwrap()
                .serve_at(DEVICE_PATH, Wireless(world.clone()))
                .unwrap()
                .serve_at(IP4_PATH, Ip4)
                .unwrap();
            for (point, ssid, strength) in POINTS {
                builder = builder.serve_at(point, Point { ssid, strength }).unwrap();
            }
            (builder.build().unwrap(), world)
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

    /// Events until one matches, failing after ten seconds.
    fn until(events: &mpsc::Receiver<Event>, what: &str, wanted: impl Fn(&Event) -> bool) -> Event {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let left =
                end.checked_duration_since(Instant::now()).unwrap_or_else(|| panic!("timed out waiting for {what}"));
            let event = events.recv_timeout(left).unwrap_or_else(|_| panic!("timed out waiting for {what}"));
            if wanted(&event) {
                return event;
            }
        }
    }

    fn network<'a>(event: &'a Event, name: &str) -> Option<&'a Wifi> {
        match event {
            Event::Snapshot(snapshot) => snapshot.networks.iter().find(|network| network.name() == name),
            Event::Failed { .. } => None,
        }
    }

    #[test]
    fn joins_networks_through_network_manager() {
        let bus = Bus::new();
        let (_service, world) = fake::network_manager(&bus.1);
        let (inputs, incoming) = mpsc::channel();
        let (sender, events) = mpsc::channel();
        let watched = bus.connect();
        let watcher_inputs = inputs.clone();
        std::thread::spawn(move || watch(&watched, &watcher_inputs));
        let worker_bus = bus.connect();
        let worker = std::thread::spawn(move || run(&worker_bus, &incoming, &|event| sender.send(event).is_ok()));
        let first = until(&events, "the networks", |event| network(event, "Cafe").is_some());
        let Event::Snapshot(snapshot) = &first else { unreachable!() };
        assert!(snapshot.running && snapshot.wifi_enabled);
        assert_eq!(snapshot.wifi_device.as_deref(), Some(fake::DEVICE_PATH));
        let home = network(&first, "Home").unwrap();
        assert_eq!(home.saved.as_deref(), Some("/org/freedesktop/NetworkManager/Settings/1"));
        assert_eq!(home.link, Link::Idle);
        let cafe = network(&first, "Cafe").unwrap().clone();
        assert_eq!((cafe.security, cafe.saved.clone()), (Security::Password { sae: false }, None));

        inputs.send(Input::Request(Request::Scan)).unwrap();
        let connect = |password: &str| Request::Connect {
            ssid: cafe.ssid.clone(),
            access_point: cafe.access_point.clone(),
            saved: None,
            security: cafe.security,
            password: Some(Zeroizing::new(password.into())),
        };
        inputs.send(Input::Request(connect("not-the-password"))).unwrap();
        let failed = until(&events, "the wrong password", |event| matches!(event, Event::Failed { .. }));
        assert_eq!(failed, Event::Failed { ssid: b"Cafe".to_vec(), reason: "Wrong password?".into(), password: true });
        let saved: Vec<Vec<u8>> = world.lock().unwrap().connections.values().map(|(ssid, _)| ssid.clone()).collect();
        assert_eq!(saved, [b"Home".to_vec()], "the connection with the wrong password is gone");
        assert_eq!(world.lock().unwrap().scans, 1);

        inputs.send(Input::Request(connect("cafe-password"))).unwrap();
        let joined = until(&events, "joining Cafe", |event| {
            network(event, "Cafe").is_some_and(|network| network.link == Link::Connected)
        });
        assert!(network(&joined, "Cafe").unwrap().saved.is_some());
        let Event::Snapshot(snapshot) = &joined else { unreachable!() };
        assert_eq!(snapshot.current().map(Wifi::name).as_deref(), Some("Cafe"));
        assert_eq!(
            snapshot.details,
            [Details {
                wifi: true,
                name: "Cafe".into(),
                interface: "wlp2s0".into(),
                hardware: "AA:BB:CC:DD:EE:FF".into(),
                addresses: vec!["192.168.1.23/24".into()],
                gateway: Some("192.168.1.1".into()),
                dns: vec!["1.1.1.1".into()],
            }]
        );
        let names: Vec<&str> = snapshot.saved.iter().map(|saved| saved.name.as_str()).collect();
        assert_eq!(names, ["Cafe", "Home"]);
        let cafe_saved = snapshot.saved[0].clone();
        assert!(cafe_saved.autoconnect);

        // Settings: Cafe stops joining by itself, keeping its password.
        inputs.send(Input::Request(Request::Autoconnect(cafe_saved.path.clone(), false))).unwrap();
        until(
            &events,
            "Cafe joining only when chosen",
            |event| matches!(event, Event::Snapshot(snapshot) if snapshot.saved.iter().any(|saved| saved.name == "Cafe" && !saved.autoconnect)),
        );
        let password =
            world.lock().unwrap().connections.get(&cafe_saved.path).and_then(|(_, password)| password.clone());
        assert_eq!(password.as_deref(), Some("cafe-password"));

        inputs.send(Input::Request(Request::Disconnect)).unwrap();
        until(
            &events,
            "leaving Cafe",
            |event| matches!(event, Event::Snapshot(snapshot) if snapshot.current().is_none()),
        );

        // A saved network whose password changed asks again, then keeps the new one.
        world.lock().unwrap().connections.values_mut().for_each(|(ssid, password)| {
            if ssid == b"Home" {
                *password = Some("old-password".into());
            }
        });
        let home = Request::Connect {
            ssid: b"Home".to_vec(),
            access_point: home.access_point.clone(),
            saved: home.saved.clone(),
            security: home.security,
            password: None,
        };
        inputs.send(Input::Request(home)).unwrap();
        let failed = until(&events, "Home's old password", |event| matches!(event, Event::Failed { .. }));
        assert!(matches!(failed, Event::Failed { password: true, .. }));
        assert_eq!(world.lock().unwrap().connections.len(), 2, "a saved connection stays when it fails");
        let home = network(&first, "Home").unwrap();
        inputs
            .send(Input::Request(Request::Connect {
                ssid: home.ssid.clone(),
                access_point: home.access_point.clone(),
                saved: home.saved.clone(),
                security: home.security,
                password: Some(Zeroizing::new("home-password".into())),
            }))
            .unwrap();
        until(&events, "joining Home", |event| {
            network(event, "Home").is_some_and(|network| network.link == Link::Connected)
        });

        // Settings: forgetting Cafe, and joining a hidden open network.
        inputs.send(Input::Request(Request::Forget(cafe_saved.path.clone()))).unwrap();
        until(
            &events,
            "Cafe forgotten",
            |event| matches!(event, Event::Snapshot(snapshot) if snapshot.saved.iter().all(|saved| saved.name != "Cafe")),
        );
        inputs.send(Input::Request(Request::Hidden { ssid: b"Attic".to_vec(), password: None })).unwrap();
        until(
            &events,
            "joining the hidden Attic",
            |event| matches!(event, Event::Snapshot(snapshot) if snapshot.details.iter().any(|details| details.name == "Attic")),
        );
        assert_eq!(world.lock().unwrap().hidden, [b"Attic".to_vec()]);

        inputs.send(Input::Request(Request::Wifi(false))).unwrap();
        until(&events, "Wi-Fi off", |event| matches!(event, Event::Snapshot(snapshot) if !snapshot.wifi_enabled));
        inputs.send(Input::Stop).unwrap();
        worker.join().unwrap();
    }
}
