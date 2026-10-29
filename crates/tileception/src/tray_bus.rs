//! StatusNotifierItem discovery and actions, kept off the compositor thread.
//! KDE's interface names are the ones used by Qt and AppIndicator in practice.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use smithay::reexports::calloop::channel;
use zbus::blocking::{Connection, Proxy, connection::Builder};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
type Properties = HashMap<String, OwnedValue>;
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;
type Layout = (i32, Properties, Vec<OwnedValue>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub key: String,
    pub title: String,
    pub description: String,
    pub icon: String,
    pub theme_path: String,
    pub pixmaps: Pixmaps,
    pub attention: bool,
    pub passive: bool,
    pub menu: Option<String>,
    pub is_menu: bool,
}

#[derive(Clone, Debug)]
pub struct MenuRow {
    pub row_id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    pub submenu: bool,
    pub checked: Option<bool>,
}

pub enum Event {
    Items(Vec<Item>),
    Menu { key: String, parent: i32, rows: Vec<MenuRow> },
    Stopped,
}

#[allow(clippy::min_ident_chars, reason = "preserve existing enum fields")]
pub enum Request {
    Activate { key: String, method: &'static str, x: i32, y: i32 },
    Scroll { key: String, delta: i32 },
    Menu { key: String, path: String, parent: i32 },
    Click { key: String, path: String, id: i32 },
}

#[derive(Default)]
struct Registry {
    items: BTreeMap<String, (String, String)>,
}

struct Watcher(Arc<Mutex<Registry>>);

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &self,
        service: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header.sender().ok_or_else(|| zbus::fdo::Error::Failed("missing sender".into()))?;
        let (bus, path) = if service.starts_with('/') {
            (sender.to_string(), service.to_string())
        } else {
            let proxy = zbus::fdo::DBusProxy::new(connection).await?;
            let name = zbus::names::BusName::try_from(service)
                .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
            (proxy.get_name_owner(name).await?.to_string(), "/StatusNotifierItem".into())
        };
        OwnedObjectPath::try_from(path.as_str()).map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        let key = format!("{bus}{path}");
        let added = self
            .0
            .lock()
            .expect("tray registry mutex is not poisoned")
            .items
            .insert(key.clone(), (bus, path))
            .is_none();
        if added {
            Self::status_notifier_item_registered(&emitter, &key).await?;
        }
        Ok(())
    }

    async fn register_status_notifier_host(
        &self,
        _service: &str,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        Self::status_notifier_host_registered(&emitter).await?;
        Ok(())
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.0.lock().expect("tray registry mutex is not poisoned").items.keys().cloned().collect()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

fn string(properties: &Properties, name: &str) -> String {
    properties.get(name).and_then(|value| <&str>::try_from(value).ok()).unwrap_or_default().to_string()
}

fn boolean(properties: &Properties, name: &str, default: bool) -> bool {
    properties.get(name).and_then(|value| bool::try_from(value).ok()).unwrap_or(default)
}

fn pixmaps(properties: &Properties, name: &str) -> Pixmaps {
    properties
        .get(name)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Pixmaps::try_from(value).ok())
        .unwrap_or_default()
}

fn read_item(connection: &Connection, key: &str, bus: &str, path: &str) -> zbus::Result<Item> {
    let proxy = Proxy::new(connection, bus, path, "org.freedesktop.DBus.Properties")?;
    let properties: Properties = proxy.call("GetAll", &(ITEM,))?;
    let status = string(&properties, "Status");
    let attention = status == "NeedsAttention";
    let mut icon = if attention { string(&properties, "AttentionIconName") } else { String::new() };
    if icon.is_empty() {
        icon = string(&properties, "IconName");
    }
    let theme_path = string(&properties, "IconThemePath");
    if !theme_path.is_empty() {
        let lookup = tileception_apps::IconLookup::new("hicolor", &[theme_path.clone().into()]);
        if let Some(path) = lookup.find(&icon, 16, 1) {
            icon = path.to_string_lossy().into_owned();
        }
    }
    let mut images = if attention { pixmaps(&properties, "AttentionIconPixmap") } else { Vec::new() };
    if images.is_empty() {
        images = pixmaps(&properties, "IconPixmap");
    }
    let title = string(&properties, "Title");
    let title = if title.is_empty() { string(&properties, "Id") } else { title };
    let tooltip = properties
        .get("ToolTip")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| <(String, Pixmaps, String, String)>::try_from(value).ok());
    let description =
        tooltip.map(|(_, _, title, body)| format!("{title}\n{body}").trim().to_string()).unwrap_or_default();
    let menu = properties
        .get("Menu")
        .and_then(|value| <&zbus::zvariant::ObjectPath<'_>>::try_from(value).ok())
        .map(ToString::to_string)
        .filter(|path| path != "/");
    Ok(Item {
        key: key.into(),
        title,
        description,
        icon,
        theme_path,
        pixmaps: images,
        attention,
        passive: status == "Passive",
        menu,
        is_menu: boolean(&properties, "ItemIsMenu", false),
    })
}

fn split_key(key: &str) -> Option<(&str, &str)> {
    let slash = key.find('/')?;
    Some((&key[..slash], &key[slash..]))
}

fn handle(connection: &Connection, request: Request, events: &channel::Sender<Event>) -> zbus::Result<()> {
    match request {
        Request::Activate { key, method, x: event_x, y: event_y } => {
            if let Some((bus, path)) = split_key(&key) {
                Proxy::new(connection, bus, path, ITEM)?.call::<_, _, ()>(method, &(event_x, event_y))?;
            }
        }
        Request::Scroll { key, delta } => {
            if let Some((bus, path)) = split_key(&key) {
                Proxy::new(connection, bus, path, ITEM)?.call::<_, _, ()>("Scroll", &(delta, "vertical"))?;
            }
        }
        Request::Menu { key, path, parent } => {
            if let Some((bus, _)) = split_key(&key) {
                let result = (|| -> zbus::Result<Vec<MenuRow>> {
                    let proxy = Proxy::new(connection, bus, path.as_str(), MENU)?;
                    let _: Result<bool, _> = proxy.call("AboutToShow", &(parent,));
                    let (_, (_, _, children)): (u32, Layout) =
                        proxy.call("GetLayout", &(parent, 1i32, Vec::<&str>::new()))?;
                    let rows = children
                        .into_iter()
                        .filter_map(|child| {
                            let (row_id, props, _): Layout = child.try_into().ok()?;
                            if !boolean(&props, "visible", true) {
                                return None;
                            }
                            let toggle = string(&props, "toggle-type");
                            let checked = (!toggle.is_empty()).then(|| {
                                props.get("toggle-state").and_then(|value| i32::try_from(value).ok()) == Some(1)
                            });
                            Some(MenuRow {
                                row_id,
                                label: string(&props, "label").replace('_', ""),
                                enabled: boolean(&props, "enabled", true),
                                separator: string(&props, "type") == "separator",
                                submenu: string(&props, "children-display") == "submenu",
                                checked,
                            })
                        })
                        .collect();
                    drop(proxy);
                    Ok(rows)
                })();
                let rows = result.unwrap_or_else(|error| {
                    tracing::warn!("tray menu: {error}");
                    vec![MenuRow {
                        row_id: 0,
                        label: "Unable to load menu".into(),
                        enabled: false,
                        separator: false,
                        submenu: false,
                        checked: None,
                    }]
                });
                let _ = events.send(Event::Menu { key, parent, rows });
            }
        }
        Request::Click { key, path, id: row_id } => {
            if let Some((bus, _)) = split_key(&key) {
                Proxy::new(connection, bus, path.as_str(), MENU)?
                    .call::<_, _, ()>("Event", &(row_id, "clicked", Value::from(0i32), 0u32))?;
            }
        }
    }
    Ok(())
}

pub fn run(address: Option<String>, events: channel::Sender<Event>, incoming: mpsc::Receiver<Request>) {
    if let Err(error) = serve(address.as_deref(), &events, &incoming) {
        tracing::warn!("system tray: {error}");
    }
    let _ = events.send(Event::Stopped);
}

fn serve(
    address: Option<&str>,
    events: &channel::Sender<Event>,
    incoming: &mpsc::Receiver<Request>,
) -> zbus::Result<()> {
    let registry = Arc::new(Mutex::new(Registry::default()));
    let builder = match address {
        Some(address) => Builder::address(address)?,
        None => Builder::session()?,
    };
    let host = format!("org.kde.StatusNotifierHost.tileception{}", std::process::id());
    let connection = builder
        .method_timeout(Duration::from_secs(2))
        .serve_at(PATH, Watcher(registry.clone()))?
        .name(host.as_str())?
        .build()?;
    // An existing watcher (for example Waybar) can also supply our item list.
    let owned = connection.request_name(WATCHER).is_ok();
    if !owned {
        Proxy::new(&connection, WATCHER, PATH, WATCHER)?
            .call::<_, _, ()>("RegisterStatusNotifierHost", &(host.as_str(),))?;
    }
    let dbus = Proxy::new(&connection, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus")?;
    let mut previous = Vec::new();
    loop {
        let entries: Vec<(String, String, String)> = if owned {
            let snapshot = registry.lock().expect("tray registry mutex is not poisoned").items.clone();
            let mut entries = Vec::new();
            for (key, (bus, path)) in snapshot {
                if dbus.call::<_, _, bool>("NameHasOwner", &(bus.as_str(),)).unwrap_or(false) {
                    entries.push((key, bus, path));
                } else {
                    registry.lock().expect("tray registry mutex is not poisoned").items.remove(&key);
                    let _ =
                        connection.emit_signal(None::<&str>, PATH, WATCHER, "StatusNotifierItemUnregistered", &(key,));
                }
            }
            entries
        } else {
            let proxy = Proxy::new(&connection, WATCHER, PATH, WATCHER)?;
            let keys: Vec<String> = proxy.get_property("RegisteredStatusNotifierItems")?;
            keys.into_iter()
                .map(|key| {
                    let (bus, path) = split_key(&key).unwrap_or((&key, "/StatusNotifierItem"));
                    (format!("{bus}{path}"), bus.to_string(), path.to_string())
                })
                .collect()
        };
        let items: Vec<Item> =
            entries.iter().filter_map(|(key, bus, path)| read_item(&connection, key, bus, path).ok()).collect();
        if items != previous {
            previous = items.clone();
            if events.send(Event::Items(items)).is_err() {
                break;
            }
        }
        // Polling also catches older clients that do not emit PropertiesChanged.
        // All bus traffic stays on this worker, including menus and activation.
        match incoming.recv_timeout(Duration::from_millis(750)) {
            Ok(request) => {
                if let Err(error) = handle(&connection, request, events) {
                    tracing::debug!("tray action: {error}");
                }
                for request in incoming.try_iter().take(32) {
                    if let Err(error) = handle(&connection, request, events) {
                        tracing::debug!("tray action: {error}");
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}
