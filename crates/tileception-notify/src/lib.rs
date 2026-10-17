use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};
use zbus::zvariant::OwnedValue;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

#[derive(Clone, Debug)]
pub struct Config {
    pub default_timeout: Option<Duration>,
    pub critical_timeout: Option<Duration>,
    pub history_limit: usize,
    pub critical_popups_in_dnd: bool,
    pub replace: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_timeout: Some(Duration::from_secs(5)),
            critical_timeout: None,
            history_limit: 100,
            critical_popups_in_dnd: true,
            replace: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Image {
    Rgba { width: u32, height: u32, data: Vec<u8> },
    Path(String),
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub app_icon: Option<String>,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub default_action: bool,
    pub urgency: Urgency,
    pub image: Option<Image>,
    pub desktop_entry: Option<String>,
    pub category: Option<String>,
    pub resident: bool,
    pub transient: bool,
    pub suppress_sound: bool,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub sound_file: Option<String>,
    pub created: SystemTime,
    pub expires: Option<Instant>,
    pub show_popup: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    ClosedByCall = 3,
    Undefined = 4,
}

#[derive(Clone, Debug)]
pub enum Event {
    Shown(Notification),
    Replaced(Notification),
    Closed { id: u32, reason: CloseReason },
}

#[derive(Clone, Debug)]
pub enum Request {
    Dismiss(u32),
    DismissAll,
    Invoke { id: u32, action: String, token: Option<String> },
    SetDoNotDisturb(bool),
    ClearHistory,
    RemoveFromHistory(u32),
}

#[derive(Debug)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

struct State {
    config: Config,
    active: HashMap<u32, Notification>,
    history: VecDeque<Notification>,
    next_id: u32,
    dnd: bool,
    events: mpsc::Sender<Event>,
    wake: Box<dyn Fn() + Send>,
}

impl State {
    fn publish(&self, event: Event) {
        if self.events.send(event).is_ok() {
            (self.wake)();
        }
    }

    fn close(&mut self, id: u32, reason: CloseReason) -> bool {
        if self.active.remove(&id).is_none() {
            return false;
        }
        self.publish(Event::Closed { id, reason });
        true
    }

    fn record(&mut self, notification: &Notification) {
        self.history.retain(|item| item.id != notification.id);
        if !notification.transient && self.config.history_limit > 0 {
            self.history.push_back(notification.clone());
            while self.history.len() > self.config.history_limit {
                self.history.pop_front();
            }
        }
    }
}

type Shared = Arc<Mutex<State>>;

/// Owns a dedicated service thread. Dropping it joins the thread and releases the bus name.
pub struct NotifyService {
    requests: mpsc::Sender<Option<Request>>,
    state: Shared,
    thread: Option<JoinHandle<()>>,
}

impl NotifyService {
    pub fn start(
        config: Config,
        bus_address: Option<String>,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(Self, mpsc::Receiver<Event>), Error> {
        let (events, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(State {
            config: config.clone(),
            active: HashMap::new(),
            history: VecDeque::new(),
            next_id: 1,
            dnd: false,
            events,
            wake: Box::new(wake),
        }));
        let worker_state = state.clone();
        let (requests, incoming) = mpsc::channel();
        let (ready, started) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("tileception-notify".into())
            .spawn(move || {
                let connect = || -> zbus::Result<zbus::blocking::Connection> {
                    let builder = match bus_address.as_deref() {
                        Some(address) => zbus::blocking::connection::Builder::address(address)?,
                        None => zbus::blocking::connection::Builder::session()?,
                    };
                    builder
                        .serve_at(PATH, Interface(worker_state.clone()))?
                        .name(NAME)?
                        .replace_existing_names(config.replace)
                        .allow_name_replacements(true)
                        .build()
                };
                match connect() {
                    Ok(connection) => {
                        if ready.send(Ok(())).is_ok() {
                            run(connection, worker_state, incoming);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(Error(format!(
                            "could not own {NAME} (another notification daemon may own it): {error}"
                        ))));
                    }
                }
            })
            .map_err(|error| Error(error.to_string()))?;
        match started.recv() {
            Ok(Ok(())) => Ok((Self { requests, state, thread: Some(thread) }, receiver)),
            result => {
                let _ = thread.join();
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => Error("notification thread stopped during startup".into()),
                })
            }
        }
    }

    pub fn send(&self, request: Request) {
        let _ = self.requests.send(Some(request));
    }

    /// Returns history in oldest-to-newest order, including active notifications.
    pub fn history(&self) -> Vec<Notification> {
        self.state.lock().expect("notification state mutex must not be poisoned").history.iter().cloned().collect()
    }

    /// How many notifications the history holds, without copying them.
    pub fn history_len(&self) -> usize {
        self.state.lock().expect("notification state mutex must not be poisoned").history.len()
    }

    pub fn do_not_disturb(&self) -> bool {
        self.state.lock().expect("notification state mutex must not be poisoned").dnd
    }
}

impl Drop for NotifyService {
    fn drop(&mut self) {
        let _ = self.requests.send(None);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn emit(
    connection: &zbus::blocking::Connection,
    signal: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) {
    let _ = connection.emit_signal(None::<&str>, PATH, NAME, signal, body);
}

fn close(connection: &zbus::blocking::Connection, state: &Shared, id: u32, reason: CloseReason) {
    if state.lock().expect("notification state mutex must not be poisoned").close(id, reason) {
        emit(connection, "NotificationClosed", &(id, reason as u32));
    }
}

fn run(connection: zbus::blocking::Connection, state: Shared, incoming: mpsc::Receiver<Option<Request>>) {
    loop {
        let now = Instant::now();
        let (expired, wait) = {
            let mut state = state.lock().expect("notification state mutex must not be poisoned");
            let deadlines: Vec<_> =
                state.active.values().filter_map(|item| item.expires.map(|time| (item.id, time))).collect();
            let expired = deadlines.iter().filter(|(_, time)| *time <= now).map(|(id, _)| *id).collect::<Vec<_>>();
            let wait = deadlines
                .iter()
                .map(|(_, time)| time.saturating_duration_since(now))
                .min()
                .unwrap_or(Duration::from_millis(50))
                .min(Duration::from_millis(50));
            for id in &expired {
                state.close(*id, CloseReason::Expired);
            }
            (expired, wait)
        };
        for id in expired {
            emit(&connection, "NotificationClosed", &(id, CloseReason::Expired as u32));
        }
        match incoming.recv_timeout(wait) {
            Ok(Some(Request::Dismiss(id))) => close(&connection, &state, id, CloseReason::Dismissed),
            Ok(Some(Request::DismissAll)) => {
                let ids: Vec<_> = state
                    .lock()
                    .expect("notification state mutex must not be poisoned")
                    .active
                    .keys()
                    .copied()
                    .collect();
                for id in ids {
                    close(&connection, &state, id, CloseReason::Dismissed);
                }
            }
            Ok(Some(Request::Invoke { id, action, token })) => {
                let resident =
                    state.lock().expect("notification state mutex must not be poisoned").active.get(&id).and_then(
                        |item| {
                            let valid = (action == "default" && item.default_action)
                                || item.actions.iter().any(|(key, _)| *key == action);
                            valid.then_some(item.resident)
                        },
                    );
                if let Some(resident) = resident {
                    if let Some(token) = token {
                        emit(&connection, "ActivationToken", &(id, token));
                    }
                    emit(&connection, "ActionInvoked", &(id, action));
                    if !resident {
                        close(&connection, &state, id, CloseReason::Dismissed);
                    }
                }
            }
            Ok(Some(Request::SetDoNotDisturb(value))) => {
                state.lock().expect("notification state mutex must not be poisoned").dnd = value
            }
            Ok(Some(Request::ClearHistory)) => {
                state.lock().expect("notification state mutex must not be poisoned").history.clear()
            }
            Ok(Some(Request::RemoveFromHistory(id))) => state
                .lock()
                .expect("notification state mutex must not be poisoned")
                .history
                .retain(|item| item.id != id),
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

struct Interface(Shared);

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Interface {
    #[allow(clippy::too_many_arguments, reason = "the D-Bus Notify specification fixes this method's arguments")]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let mut state = self.0.lock().expect("notification state mutex must not be poisoned");
        let replaced = replaces_id != 0 && state.active.contains_key(&replaces_id);
        let id = if replaced {
            replaces_id
        } else {
            while state.active.contains_key(&state.next_id) || state.history.iter().any(|item| item.id == state.next_id)
            {
                state.next_id = state.next_id.wrapping_add(1).max(1);
            }
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1).max(1);
            id
        };
        let urgency = match hints.get("urgency").and_then(|value| u8::try_from(value).ok()) {
            Some(0) => Urgency::Low,
            Some(2) => Urgency::Critical,
            _ => Urgency::Normal,
        };
        let timeout = match expire_timeout {
            0 => None,
            value if value > 0 => Some(Duration::from_millis(value as u64)),
            _ if urgency == Urgency::Critical => state.config.critical_timeout,
            _ => state.config.default_timeout,
        };
        let action_pairs: Vec<_> =
            actions.as_chunks::<2>().0.iter().map(|pair| (pair[0].clone(), pair[1].clone())).collect();
        let notification = Notification {
            id,
            app_name,
            app_icon: (!app_icon.is_empty()).then_some(app_icon),
            summary,
            body: sanitize_body(&body),
            default_action: action_pairs.iter().any(|(key, _)| key == "default"),
            actions: action_pairs.into_iter().filter(|(key, _)| key != "default").collect(),
            urgency,
            image: parse_image(&hints),
            desktop_entry: string_hint(&hints, "desktop-entry"),
            category: string_hint(&hints, "category"),
            resident: bool_hint(&hints, "resident"),
            transient: bool_hint(&hints, "transient"),
            suppress_sound: bool_hint(&hints, "suppress-sound"),
            x: hints.get("x").and_then(|value| i32::try_from(value).ok()),
            y: hints.get("y").and_then(|value| i32::try_from(value).ok()),
            sound_file: string_hint(&hints, "sound-file"),
            created: SystemTime::now(),
            expires: timeout.and_then(|timeout| Instant::now().checked_add(timeout)),
            show_popup: !state.dnd || (urgency == Urgency::Critical && state.config.critical_popups_in_dnd),
        };
        state.record(&notification);
        state.active.insert(id, notification.clone());
        state.publish(if replaced { Event::Replaced(notification) } else { Event::Shown(notification) });
        id
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let closed =
            self.0.lock().expect("notification state mutex must not be poisoned").close(id, CloseReason::ClosedByCall);
        if closed {
            connection
                .emit_signal(None::<&str>, PATH, NAME, "NotificationClosed", &(id, CloseReason::ClosedByCall as u32))
                .await
                .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        } else {
            return Err(zbus::fdo::Error::InvalidArgs(String::new()));
        }
        Ok(())
    }

    fn get_capabilities(&self) -> Vec<&str> {
        vec!["actions", "body", "body-markup", "icon-static", "persistence"]
    }

    fn get_server_information(&self) -> (&str, &str, &str, &str) {
        ("Tileception", "Tileception", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn activation_token(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: u32,
        activation_token: &str,
    ) -> zbus::Result<()>;
}

fn string_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    hints.get(key).and_then(|value| <&str>::try_from(value).ok()).map(str::to_owned)
}

fn bool_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    hints.get(key).and_then(|value| bool::try_from(value).ok()).unwrap_or(false)
}

/// Keeps the specification's formatting tags and removes unsafe attributes and URLs.
pub fn sanitize_body(body: &str) -> String {
    ammonia::Builder::default()
        .tags(["b", "i", "u", "a", "img"].into_iter().collect())
        .generic_attributes(Default::default())
        .tag_attributes(
            [("a", ["href"].into_iter().collect()), ("img", ["src", "alt"].into_iter().collect())]
                .into_iter()
                .collect(),
        )
        .url_schemes(["http", "https", "mailto", "file"].into_iter().collect())
        .attribute_filter(|element, attribute, value| {
            if element == "img"
                && attribute == "src"
                && !(value.starts_with("file:///") || (value.starts_with('/') && !value.starts_with("//")))
            {
                None
            } else {
                Some(std::borrow::Cow::Borrowed(value))
            }
        })
        .link_rel(None)
        .clean(body)
        .to_string()
}

fn parse_image(hints: &HashMap<String, OwnedValue>) -> Option<Image> {
    for key in ["image-data", "image_data"] {
        if let Some(image) = hints.get(key).and_then(decode_image) {
            return Some(image);
        }
    }
    ["image-path", "image_path"]
        .into_iter()
        .find_map(|key| string_hint(hints, key).map(Image::Path))
        .or_else(|| hints.get("icon_data").and_then(decode_image))
}

fn decode_image(value: &OwnedValue) -> Option<Image> {
    type RawImage = (i32, i32, i32, bool, i32, i32, Vec<u8>);
    let (width, height, stride, alpha, bits, channels): (usize, usize, usize, bool, i32, usize);
    let raw: RawImage = value.try_clone().ok()?.try_into().ok()?;
    (width, height, stride, alpha, bits, channels) =
        (raw.0.try_into().ok()?, raw.1.try_into().ok()?, raw.2.try_into().ok()?, raw.3, raw.4, raw.5.try_into().ok()?);
    if width == 0 || height == 0 || bits != 8 || channels != if alpha { 4 } else { 3 } {
        return None;
    }
    let row = width.checked_mul(channels)?;
    let required = stride.checked_mul(height.checked_sub(1)?)?.checked_add(row)?;
    let size = width.checked_mul(height)?.checked_mul(4)?;
    if stride < row || required > raw.6.len() || size > 64 * 1024 * 1024 {
        return None;
    }
    let mut data = Vec::with_capacity(size);
    for y in 0..height {
        for pixel in raw.6[y * stride..y * stride + row].chunks_exact(channels) {
            data.extend_from_slice(&pixel[..3]);
            data.push(if alpha { pixel[3] } else { 255 });
        }
    }
    Some(Image::Rgba { width: width as u32, height: height as u32, data })
}
