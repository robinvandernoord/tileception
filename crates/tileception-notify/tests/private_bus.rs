use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use tileception_notify::{CloseReason, Config, Event, Image, Notification, NotifyService, Request, Urgency};
use zbus::blocking::{connection::Builder, Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn new() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--print-address", "--nofork"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("tests require dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut address).unwrap();
        assert!(!address.trim().is_empty());
        Self { child, address: address.trim().to_owned() }
    }

    fn connect(&self) -> Connection {
        Builder::address(self.address.as_str()).unwrap().method_timeout(Duration::from_secs(3)).build().unwrap()
    }

    fn start(&self, config: Config) -> (NotifyService, mpsc::Receiver<Event>) {
        NotifyService::start(config, Some(self.address.clone()), || {}).unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn proxy(connection: &Connection) -> Proxy<'_> {
    Proxy::new(connection, NAME, PATH, NAME).unwrap()
}

fn persistent() -> Config {
    Config { default_timeout: None, ..Config::default() }
}

fn notify(
    proxy: &Proxy<'_>,
    replaces: u32,
    body: &str,
    actions: &[&str],
    hints: HashMap<&str, OwnedValue>,
    timeout: i32,
) -> u32 {
    proxy.call("Notify", &("test-app", replaces, "test-icon", "summary", body, actions, hints, timeout)).unwrap()
}

fn shown(events: &mpsc::Receiver<Event>) -> Notification {
    match events.recv_timeout(Duration::from_secs(3)).unwrap() {
        Event::Shown(item) | Event::Replaced(item) => item,
        event => panic!("expected notification, got {event:?}"),
    }
}

fn closed(events: &mpsc::Receiver<Event>, id: u32, reason: CloseReason) {
    match events.recv_timeout(Duration::from_secs(3)).unwrap() {
        Event::Closed { id: actual_id, reason: actual_reason } => {
            assert_eq!(actual_id, id);
            assert_eq!(actual_reason, reason);
        }
        event => panic!("expected close, got {event:?}"),
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < deadline, "condition timed out");
        thread::sleep(Duration::from_millis(5));
    }
}

fn signals(bus: &Bus) -> mpsc::Receiver<zbus::Message> {
    let connection = bus.connect();
    let (sender, receiver) = mpsc::channel();
    let (ready, started) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let proxy = proxy(&connection);
        let signals = proxy.receive_all_signals().unwrap();
        ready.send(()).unwrap();
        for signal in signals {
            if sender.send(signal).is_err() {
                break;
            }
        }
    });
    started.recv_timeout(Duration::from_secs(3)).unwrap();
    receiver
}

fn signal(receiver: &mpsc::Receiver<zbus::Message>, name: &str) -> zbus::Message {
    let message = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(message.header().member().unwrap().as_str(), name);
    message
}

#[test]
fn protocol_replacement_close_and_shutdown() {
    let bus = Bus::new();
    let wakes = Arc::new(AtomicUsize::new(0));
    let callback = wakes.clone();
    let (service, events) = NotifyService::start(persistent(), Some(bus.address.clone()), move || {
        callback.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    let connection = bus.connect();
    let proxy = proxy(&connection);
    let signals = signals(&bus);
    let caps: Vec<String> = proxy.call("GetCapabilities", &()).unwrap();
    assert_eq!(caps, ["actions", "body", "body-markup", "icon-static", "persistence"]);
    let info: (String, String, String, String) = proxy.call("GetServerInformation", &()).unwrap();
    assert_eq!(info, ("Tileception".into(), "Tileception".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into()));
    let introspection = Proxy::new(&connection, NAME, PATH, "org.freedesktop.DBus.Introspectable").unwrap();
    let xml: String = introspection.call("Introspect", &()).unwrap();
    for name in ["NotificationClosed", "ActionInvoked", "ActivationToken"] {
        assert!(xml.contains(&format!("signal name=\"{name}\"")));
    }
    let id = notify(&proxy, 0, "first", &[], HashMap::new(), 0);
    assert_ne!(id, 0);
    let first = shown(&events);
    assert_eq!(first.app_name, "test-app");
    assert_eq!(first.app_icon.as_deref(), Some("test-icon"));
    assert_eq!(notify(&proxy, id, "replacement", &[], HashMap::new(), 0), id);
    assert!(
        matches!(events.recv_timeout(Duration::from_secs(3)).unwrap(), Event::Replaced(item) if item.body == "replacement")
    );
    assert_eq!(service.history().len(), 1);
    assert_eq!(service.history()[0].body, "replacement");
    proxy.call::<_, _, ()>("CloseNotification", &(id,)).unwrap();
    closed(&events, id, CloseReason::ClosedByCall);
    assert_eq!(signal(&signals, "NotificationClosed").body().deserialize::<(u32, u32)>().unwrap(), (id, 3));
    assert!(proxy.call::<_, _, ()>("CloseNotification", &(id,)).is_err());
    let next = notify(&proxy, 0, "next", &[], HashMap::new(), 0);
    assert!(next > id);
    shown(&events);
    service.send(Request::Dismiss(next));
    closed(&events, next, CloseReason::Dismissed);
    assert_eq!(signal(&signals, "NotificationClosed").body().deserialize::<(u32, u32)>().unwrap(), (next, 2));
    wait_until(|| wakes.load(Ordering::SeqCst) == 5);
    assert_eq!(service.history().len(), 2);
    drop(service);
    let (second, _) = bus.start(persistent());
    drop(second);
}

#[test]
fn actions_tokens_resident_and_default_action() {
    let bus = Bus::new();
    let (service, events) = bus.start(persistent());
    let connection = bus.connect();
    let proxy = proxy(&connection);
    let signals = signals(&bus);
    let id = notify(&proxy, 0, "actions", &["default", "Open", "reply", "Reply", "dangling"], HashMap::new(), 0);
    let item = shown(&events);
    assert!(item.default_action);
    assert_eq!(item.actions, [("reply".into(), "Reply".into())]);
    service.send(Request::Invoke { id, action: "reply".into(), token: Some("activation-token".into()) });
    assert_eq!(
        signal(&signals, "ActivationToken").body().deserialize::<(u32, String)>().unwrap(),
        (id, "activation-token".into())
    );
    assert_eq!(signal(&signals, "ActionInvoked").body().deserialize::<(u32, String)>().unwrap(), (id, "reply".into()));
    assert_eq!(signal(&signals, "NotificationClosed").body().deserialize::<(u32, u32)>().unwrap(), (id, 2));
    closed(&events, id, CloseReason::Dismissed);
    let id = notify(&proxy, 0, "resident", &["default", "Open"], HashMap::from([("resident", true.into())]), 0);
    assert!(shown(&events).resident);
    service.send(Request::Invoke { id, action: "default".into(), token: None });
    assert_eq!(
        signal(&signals, "ActionInvoked").body().deserialize::<(u32, String)>().unwrap(),
        (id, "default".into())
    );
    assert!(events.recv_timeout(Duration::from_millis(80)).is_err());
    service.send(Request::DismissAll);
    closed(&events, id, CloseReason::Dismissed);
}

#[test]
fn timeouts_defaults_critical_and_replacement_reset() {
    let bus = Bus::new();
    let (service, events) = bus.start(Config { default_timeout: Some(Duration::from_millis(35)), ..Config::default() });
    let connection = bus.connect();
    let proxy = proxy(&connection);
    let signals = signals(&bus);
    let id = notify(&proxy, 0, "default timeout", &[], HashMap::new(), -1);
    assert!(shown(&events).expires.is_some());
    closed(&events, id, CloseReason::Expired);
    assert_eq!(signal(&signals, "NotificationClosed").body().deserialize::<(u32, u32)>().unwrap(), (id, 1));
    let critical = notify(&proxy, 0, "critical", &[], HashMap::from([("urgency", 2_u8.into())]), -1);
    let item = shown(&events);
    assert_eq!(item.urgency, Urgency::Critical);
    assert!(item.expires.is_none());
    let id = notify(&proxy, 0, "explicit", &[], HashMap::new(), 50);
    shown(&events);
    notify(&proxy, id, "reset to forever", &[], HashMap::new(), 0);
    assert!(shown(&events).expires.is_none());
    assert!(events.recv_timeout(Duration::from_millis(150)).is_err());
    service.send(Request::Dismiss(critical));
    closed(&events, critical, CloseReason::Dismissed);
    service.send(Request::Dismiss(id));
    closed(&events, id, CloseReason::Dismissed);
}

#[test]
fn dnd_history_limits_transient_and_mutation() {
    let bus = Bus::new();
    let (service, events) = bus.start(Config { history_limit: 2, ..persistent() });
    let connection = bus.connect();
    let proxy = proxy(&connection);
    service.send(Request::SetDoNotDisturb(true));
    wait_until(|| service.do_not_disturb());
    let first = notify(&proxy, 0, "hidden", &[], HashMap::new(), 0);
    assert!(!shown(&events).show_popup);
    let second = notify(&proxy, 0, "critical", &[], HashMap::from([("urgency", 2_u8.into())]), 0);
    assert!(shown(&events).show_popup);
    let third = notify(&proxy, 0, "third", &[], HashMap::new(), 0);
    shown(&events);
    assert_eq!(service.history().iter().map(|item| item.id).collect::<Vec<_>>(), [second, third]);
    let transient = notify(&proxy, 0, "transient", &[], HashMap::from([("transient", true.into())]), 0);
    assert!(shown(&events).transient);
    assert_eq!(service.history().len(), 2);
    notify(&proxy, third, "now transient", &[], HashMap::from([("transient", true.into())]), 0);
    shown(&events);
    assert_eq!(service.history().len(), 1);
    service.send(Request::RemoveFromHistory(second));
    wait_until(|| service.history().is_empty());
    notify(&proxy, 0, "history again", &[], HashMap::new(), 0);
    shown(&events);
    service.send(Request::ClearHistory);
    wait_until(|| service.history().is_empty());
    service.send(Request::SetDoNotDisturb(false));
    wait_until(|| !service.do_not_disturb());
    notify(&proxy, 0, "visible", &[], HashMap::new(), 0);
    assert!(shown(&events).show_popup);
    for id in [first, second, third, transient] {
        service.send(Request::Dismiss(id));
    }
}

#[test]
fn configurable_critical_policy_and_zero_history() {
    let bus = Bus::new();
    let (service, events) = bus.start(Config {
        critical_popups_in_dnd: false,
        critical_timeout: Some(Duration::from_millis(25)),
        history_limit: 0,
        ..persistent()
    });
    let connection = bus.connect();
    let proxy = proxy(&connection);
    service.send(Request::SetDoNotDisturb(true));
    wait_until(|| service.do_not_disturb());
    let id = notify(&proxy, 0, "critical policy", &[], HashMap::from([("urgency", 2_u8.into())]), -1);
    assert!(!shown(&events).show_popup);
    assert!(service.history().is_empty());
    closed(&events, id, CloseReason::Expired);
}

fn image_value(raw: (i32, i32, i32, bool, i32, i32, Vec<u8>)) -> OwnedValue {
    Value::from(raw).try_into().unwrap()
}

#[test]
fn image_data_aliases_stride_validation_and_hints() {
    let bus = Bus::new();
    let (_service, events) = bus.start(persistent());
    let connection = bus.connect();
    let proxy = proxy(&connection);
    for key in ["image-data", "image_data", "icon_data"] {
        let hints = HashMap::from([
            (key, image_value((1, 2, 4, false, 8, 3, vec![1, 2, 3, 99, 4, 5, 6]))),
            ("urgency", 0_u8.into()),
            ("desktop-entry", Value::from("org.test.App").try_into().unwrap()),
            ("category", Value::from("email.arrived").try_into().unwrap()),
            ("suppress-sound", true.into()),
            ("x", 12_i32.into()),
            ("y", (-5_i32).into()),
            ("sound-file", Value::from("/tmp/sound.ogg").try_into().unwrap()),
        ]);
        notify(&proxy, 0, "image", &[], hints, 0);
        let item = shown(&events);
        assert_eq!(item.image, Some(Image::Rgba { width: 1, height: 2, data: vec![1, 2, 3, 255, 4, 5, 6, 255] }));
        assert_eq!(item.urgency, Urgency::Low);
        assert_eq!(item.desktop_entry.as_deref(), Some("org.test.App"));
        assert_eq!(item.category.as_deref(), Some("email.arrived"));
        assert!(item.suppress_sound);
        assert_eq!((item.x, item.y), (Some(12), Some(-5)));
        assert_eq!(item.sound_file.as_deref(), Some("/tmp/sound.ogg"));
    }
    notify(
        &proxy,
        0,
        "alpha",
        &[],
        HashMap::from([("image-data", image_value((1, 1, 4, true, 8, 4, vec![1, 2, 3, 4])))]),
        0,
    );
    assert_eq!(shown(&events).image, Some(Image::Rgba { width: 1, height: 1, data: vec![1, 2, 3, 4] }));
    for invalid in [
        (1, 1, 2, false, 8, 3, vec![0; 3]),
        (-1, 1, 4, true, 8, 4, vec![0; 4]),
        (1, 1, 4, true, 16, 4, vec![0; 4]),
        (1, 2, 4, true, 8, 4, vec![0; 4]),
    ] {
        notify(
            &proxy,
            0,
            "invalid image",
            &[],
            HashMap::from([
                ("image-data", image_value(invalid)),
                ("image-path", Value::from("file:///tmp/fallback.png").try_into().unwrap()),
            ]),
            0,
        );
        assert_eq!(shown(&events).image, Some(Image::Path("file:///tmp/fallback.png".into())));
    }
    notify(
        &proxy,
        0,
        "path alias",
        &[],
        HashMap::from([("image_path", Value::from("test-icon").try_into().unwrap())]),
        0,
    );
    assert_eq!(shown(&events).image, Some(Image::Path("test-icon".into())));
    notify(
        &proxy,
        0,
        "path precedes legacy data",
        &[],
        HashMap::from([
            ("icon_data", image_value((1, 1, 4, true, 8, 4, vec![1, 2, 3, 4]))),
            ("image-path", Value::from("preferred-icon").try_into().unwrap()),
        ]),
        0,
    );
    assert_eq!(shown(&events).image, Some(Image::Path("preferred-icon".into())));
}

#[test]
fn markup_is_restricted_and_safe() {
    let bus = Bus::new();
    let (_service, events) = bus.start(persistent());
    let connection = bus.connect();
    let proxy = proxy(&connection);
    notify(&proxy, 0, "<b class='bad'>bold</b><i>italic</i><u>underline</u><a href='https://example.org' onclick='bad()'>link</a><img src='file:///tmp/icon.png' alt='icon' onerror='bad()'><script>alert(1)</script><span>plain</span><a href='javascript:bad()'>unsafe</a>", &[], HashMap::new(), 0);
    let body = shown(&events).body;
    assert!(body.contains("<b>bold</b><i>italic</i><u>underline</u>"));
    assert!(body.contains("href=\"https://example.org\""));
    assert!(body.contains("src=\"file:///tmp/icon.png\""));
    assert!(body.contains("plain"));
    for forbidden in ["script", "alert", "span", "onclick", "onerror", "javascript", "class="] {
        assert!(!body.contains(forbidden), "{body}");
    }
    notify(
        &proxy,
        0,
        "<img src='https://remote.example/a.png'><img src='//remote.example/a.png'><img src='/tmp/local.png'>",
        &[],
        HashMap::new(),
        0,
    );
    let body = shown(&events).body;
    assert!(!body.contains("remote.example"));
    assert!(body.contains("src=\"/tmp/local.png\""));
}

#[test]
fn name_conflict_and_explicit_replacement() {
    let bus = Bus::new();
    let (first, _) = bus.start(persistent());
    let error = match NotifyService::start(persistent(), Some(bus.address.clone()), || {}) {
        Ok(_) => panic!("must not steal an owned name"),
        Err(error) => error,
    };
    assert!(error.to_string().contains(NAME));
    let (replacement, events) = bus.start(Config { replace: true, ..persistent() });
    let connection = bus.connect();
    notify(&proxy(&connection), 0, "replacement daemon", &[], HashMap::new(), 0);
    assert_eq!(shown(&events).body, "replacement daemon");
    drop(first);
    drop(replacement);
}
