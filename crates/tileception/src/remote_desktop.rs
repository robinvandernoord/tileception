//! Typing for applications. Tileception is a RemoteDesktop backend for
//! xdg-desktop-portal, keyboard only, which is how KeePassXC's auto-type
//! sends key presses on Wayland. Starting a session asks in a notification
//! first; a program that asked to be remembered comes back with restore data
//! and types without asking again. Nothing is typed while the screen is locked.
//!
//! The D-Bus side runs off the compositor thread and only keeps track of
//! sessions; questions and key presses go to the compositor as [`Message`]s.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use smithay::{
    backend::input::KeyState,
    input::keyboard::{Keycode, xkb},
    reexports::calloop::channel,
};
use zbus::{
    ObjectServer,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use crate::{Tileception, inject::Keys};

/// The only device type offered.
const KEYBOARD: u32 = 1;
/// Portal responses.
const SUCCESS: u32 = 0;
const CANCELLED: u32 = 1;
const ENDED: u32 = 2;
/// Restore data names its maker and format; the data is the program's path.
const VENDOR: &str = "tileception";
const RESTORE_VERSION: u32 = 1;
/// Evdev key codes are this much lower than xkb's.
const EVDEV_OFFSET: u32 = 8;

type Options = HashMap<String, OwnedValue>;
type Results = HashMap<String, OwnedValue>;
/// The model, layout, variant and options a keymap is compiled from.
type KeymapNames = (String, String, String, Option<String>);

/// The program behind a session.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// Empty for programs that are not sandboxed and did not register one.
    pub app_id: String,
    /// The executable, while the caller's process can be found.
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Answer {
    Deny,
    Allow,
    /// Allow, and let the portal keep the permission as long as the program asked.
    Remember,
}

/// One key the program presses: a keysym, or an evdev key code.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    Sym(u32),
    Code(u32),
}

/// What the D-Bus side asks of the compositor.
pub enum Message {
    /// Ask whether `program` may type. `persist` is how long it asked to be
    /// remembered, 0 when it cannot be. Closing `reply` without an answer
    /// withdraws the request.
    Ask {
        request: String,
        program: Program,
        persist: u32,
        reply: async_channel::Sender<Answer>,
    },
    /// The program no longer waits for an answer.
    Withdraw(String),
    Key {
        session: String,
        key: Key,
        pressed: bool,
    },
    /// The session ended; whatever it holds down comes up.
    Closed(String),
}

struct Session {
    program: Program,
    /// Device types asked for.
    types: u32,
    /// 0 forgets the permission with the session, 1 keeps it while the
    /// program runs, 2 until revoked.
    persist: u32,
    /// Came back with restore data for the same program.
    restored: bool,
    /// Started and allowed.
    typing: bool,
}

#[derive(Clone)]
struct Shared {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    compositor: channel::Sender<Message>,
}

impl Shared {
    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Session>> {
        // Sessions are plain data, so a panic elsewhere cannot leave them half written.
        self.sessions.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn forget(&self, session: &str) {
        if self.sessions().remove(session).is_some() {
            let _ = self.compositor.send(Message::Closed(session.to_string()));
        }
    }
}

/// The unique bus name of whoever made a portal session, from its path:
/// `/org/freedesktop/portal/desktop/session/1_42/token` belongs to `:1.42`.
fn caller(session: &str) -> Option<String> {
    let mut parts = session.strip_prefix("/org/freedesktop/portal/desktop/session/")?.split('/');
    let sender = parts.next().filter(|sender| !sender.is_empty())?;
    parts.next()?;
    Some(format!(":{}", sender.replace('_', ".")))
}

/// The executable of whoever made the session.
async fn program_path(connection: &zbus::Connection, session: &str) -> Option<PathBuf> {
    let name = zbus::names::BusName::try_from(caller(session)?).ok()?;
    let pid = zbus::fdo::DBusProxy::new(connection).await.ok()?.get_connection_unix_process_id(name).await.ok()?;
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

fn restore_data(path: &str) -> Option<OwnedValue> {
    OwnedValue::try_from(Value::from((VENDOR, RESTORE_VERSION, Value::new(path)))).ok()
}

/// Whether restore data, `(suv)`, is ours and for `path`.
fn restores(data: &Value<'_>, path: &str) -> bool {
    let Value::Structure(structure) = data else {
        return false;
    };
    match structure.fields() {
        [Value::Str(vendor), Value::U32(version), Value::Value(stored)] => {
            vendor.as_str() == VENDOR
                && *version == RESTORE_VERSION
                && matches!(&**stored, Value::Str(stored) if stored.as_str() == path)
        }
        _ => false,
    }
}

fn number(options: &Options, key: &str) -> Option<u32> {
    options.get(key).and_then(|value| u32::try_from(value).ok())
}

/// Lets the portal call off a question it is waiting on.
struct Request {
    path: String,
    compositor: channel::Sender<Message>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    fn close(&self) {
        let _ = self.compositor.send(Message::Withdraw(self.path.clone()));
    }
}

struct PortalSession {
    path: String,
    shared: Shared,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl PortalSession {
    async fn close(&self, #[zbus(object_server)] server: &ObjectServer) {
        self.shared.forget(&self.path);
        let _ = server.remove::<PortalSession, _>(self.path.as_str()).await;
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

pub struct RemoteDesktop {
    shared: Shared,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
impl RemoteDesktop {
    async fn create_session(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Options,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, Results) {
        let path = session_handle.to_string();
        let program = Program { app_id, path: program_path(connection, &path).await };
        let session = Session { program, types: KEYBOARD, persist: 0, restored: false, typing: false };
        self.shared.sessions().insert(path.clone(), session);
        let served = PortalSession { path: path.clone(), shared: self.shared.clone() };
        if !server.at(path.as_str(), served).await.unwrap_or(false) {
            self.shared.forget(&path);
            return (ENDED, Results::new());
        }
        (SUCCESS, Results::new())
    }

    fn select_devices(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        options: Options,
    ) -> (u32, Results) {
        let mut sessions = self.shared.sessions();
        let Some(session) = sessions.get_mut(session_handle.as_str()) else {
            return (ENDED, Results::new());
        };
        session.types = number(&options, "types").unwrap_or(KEYBOARD);
        session.persist = number(&options, "persist_mode").unwrap_or(0).min(2);
        let path = session.program.path.as_ref().and_then(|path| path.to_str());
        session.restored = path.zip(options.get("restore_data")).is_some_and(|(path, data)| restores(data, path));
        (SUCCESS, Results::new())
    }

    async fn start(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        _options: Options,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, Results) {
        let found = self.shared.sessions().get(session_handle.as_str()).map(|session| {
            let path = session.program.path.as_ref().and_then(|path| path.to_str()).map(str::to_owned);
            (session.program.clone(), path, session.types & KEYBOARD != 0, session.persist, session.restored)
        });
        let Some((program, path, keyboard, persist, restored)) = found else {
            return (ENDED, Results::new());
        };
        if !keyboard {
            return (ENDED, Results::new());
        }
        // Without its path the program cannot be recognised next time.
        let persist = if path.is_some() { persist } else { 0 };
        let answer = if restored { Some(Answer::Remember) } else { self.ask(&handle, program, persist, server).await };
        match answer {
            Some(Answer::Allow | Answer::Remember) => {}
            Some(Answer::Deny) => return (CANCELLED, Results::new()),
            None => return (ENDED, Results::new()),
        }
        match self.shared.sessions().get_mut(session_handle.as_str()) {
            Some(session) => session.typing = true,
            None => return (ENDED, Results::new()),
        }
        let mut results =
            Results::from([("devices".into(), OwnedValue::from(KEYBOARD)), ("clipboard_enabled".into(), false.into())]);
        if answer == Some(Answer::Remember)
            && persist > 0
            && let Some(data) = path.as_deref().and_then(restore_data)
        {
            results.insert("restore_data".into(), data);
            results.insert("persist_mode".into(), OwnedValue::from(persist));
        }
        (SUCCESS, results)
    }

    fn notify_keyboard_keysym(&self, session_handle: OwnedObjectPath, _options: Options, keysym: i32, state: u32) {
        if let Ok(sym) = u32::try_from(keysym) {
            self.send_key(session_handle.as_str(), Key::Sym(sym), state);
        }
    }

    fn notify_keyboard_keycode(&self, session_handle: OwnedObjectPath, _options: Options, keycode: i32, state: u32) {
        if let Ok(code) = u32::try_from(keycode) {
            self.send_key(session_handle.as_str(), Key::Code(code), state);
        }
    }

    /// Key presses arrive one call each; there is no EIS server.
    fn connect_to_eis(
        &self,
        _session_handle: OwnedObjectPath,
        _app_id: String,
        _options: Options,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        Err(zbus::fdo::Error::NotSupported("Tileception takes key presses through NotifyKeyboard calls".into()))
    }

    #[zbus(property)]
    fn available_device_types(&self) -> u32 {
        KEYBOARD
    }

    /// Version 2 brings restore data and persistence.
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

impl RemoteDesktop {
    /// Asks the compositor, and waits for the answer. None when the request
    /// was called off or the compositor is gone.
    async fn ask(
        &self,
        handle: &OwnedObjectPath,
        program: Program,
        persist: u32,
        server: &ObjectServer,
    ) -> Option<Answer> {
        let request = handle.to_string();
        let (reply, answer) = async_channel::bounded(1);
        let closer = Request { path: request.clone(), compositor: self.shared.compositor.clone() };
        let _ = server.at(request.as_str(), closer).await;
        let asked = self.shared.compositor.send(Message::Ask { request: request.clone(), program, persist, reply });
        let answer = if asked.is_ok() { answer.recv().await.ok() } else { None };
        let _ = server.remove::<Request, _>(request.as_str()).await;
        answer
    }

    fn send_key(&self, session: &str, key: Key, state: u32) {
        let typing = self.shared.sessions().get(session).is_some_and(|session| session.typing);
        if typing {
            let _ =
                self.shared.compositor.send(Message::Key { session: session.to_string(), key, pressed: state != 0 });
        }
    }
}

/// The compositor's side: questions waiting for an answer and the keys each
/// session holds down.
#[derive(Default)]
pub struct Typing {
    /// By notification id.
    questions: HashMap<u32, Question>,
    /// What each session pressed, with the keys pressed for it, in order.
    held: HashMap<String, Vec<(Key, Vec<Keycode>)>>,
    /// The keymap keysyms are looked up in, and the names it was compiled from.
    keys: Option<(KeymapNames, Keys)>,
}

struct Question {
    request: String,
    reply: async_channel::Sender<Answer>,
}

/// The keys to hold for a keysym found on `level`: Shift for 1, AltGr for 2,
/// both for 3, then the key itself.
fn chord(keys: &Keys, code: Keycode, level: u32, layout: u32) -> Option<Vec<Keycode>> {
    let mut chord = Vec::new();
    for (bit, modifier) in [(1, xkb::Keysym::Shift_L), (2, xkb::Keysym::ISO_Level3_Shift)] {
        if level & bit != 0 {
            chord.push(keys.locate(modifier, layout, 1)?.0);
        }
    }
    chord.push(code);
    Some(chord)
}

impl Tileception {
    /// Hands the D-Bus side of the portal its connection to the compositor and
    /// serves it on `bus`.
    pub fn serve_remote_desktop<'a>(
        &mut self,
        bus: zbus::blocking::connection::Builder<'a>,
    ) -> zbus::Result<zbus::blocking::connection::Builder<'a>> {
        let (compositor, messages) = channel::channel();
        self.loop_handle
            .insert_source(messages, |event, _, state| {
                if let channel::Event::Msg(message) = event {
                    state.remote_desktop_message(message);
                }
            })
            .map_err(|err| zbus::Error::Failure(err.to_string()))?;
        let shared = Shared { sessions: Default::default(), compositor };
        bus.serve_at("/org/freedesktop/portal/desktop", RemoteDesktop { shared })
    }

    fn remote_desktop_message(&mut self, message: Message) {
        match message {
            Message::Ask { request, program, persist, reply } => self.ask_to_type(request, &program, persist, reply),
            Message::Withdraw(request) => {
                let found = self.typing.questions.iter().find(|(_, question)| question.request == request);
                if let Some(&notification_id) = found.map(|(notification_id, _)| notification_id) {
                    // Dropping the reply tells the waiting call.
                    self.typing.questions.remove(&notification_id);
                    self.withdraw_question(notification_id);
                }
            }
            Message::Key { session, key, pressed } => self.type_key(session, key, pressed),
            Message::Closed(session) => {
                for (_, codes) in self.typing.held.remove(&session).unwrap_or_default().into_iter().rev() {
                    self.release_keys(&codes);
                }
            }
        }
    }

    fn ask_to_type(&mut self, request: String, program: &Program, persist: u32, reply: async_channel::Sender<Answer>) {
        if self.is_locked() {
            let _ = reply.try_send(Answer::Deny);
            return;
        }
        let entry = (!program.app_id.is_empty())
            .then(|| self.launcher.app_for_id(&program.app_id))
            .flatten()
            .or_else(|| program.path.as_deref().and_then(|path| self.launcher.app_for_program(path)));
        let name = match &entry {
            Some((name, _)) => name.clone(),
            None if !program.app_id.is_empty() => program.app_id.clone(),
            None => program
                .path
                .as_deref()
                .and_then(|path| path.file_name())
                .map_or_else(|| "An application".into(), |name| name.to_string_lossy().into_owned()),
        };
        let mut actions = vec![("deny".to_string(), "Deny".to_string()), ("allow".into(), "Allow once".into())];
        match persist {
            1 => actions.push(("remember".into(), "Until it quits".into())),
            2 => actions.push(("remember".into(), "Always".into())),
            _ => {}
        }
        let notification_id = self.ask_locally(
            &format!("{name} wants to type for you"),
            "It can send key presses to the window you are using, such as a password it fills in.",
            entry.map(|(_, desktop_entry)| desktop_entry),
            actions,
        );
        self.typing.questions.insert(notification_id, Question { request, reply });
    }

    /// The user answered with a button, or closed the question (None).
    pub fn answer_question(&mut self, notification_id: u32, action: Option<&str>) {
        if let Some(question) = self.typing.questions.remove(&notification_id) {
            let answer = match action {
                Some("allow") => Answer::Allow,
                Some("remember") => Answer::Remember,
                _ => Answer::Deny,
            };
            let _ = question.reply.try_send(answer);
        }
    }

    /// Presses or lets go of a session's key. Presses are dropped while the
    /// screen is locked; letting go always works, so nothing stays stuck.
    fn type_key(&mut self, session: String, key: Key, pressed: bool) {
        let time = (self.now() * 1000.0) as u32;
        if !pressed {
            let held = self.typing.held.get_mut(&session);
            if let Some(held) = held
                && let Some(index) = held.iter().rposition(|(down, _)| *down == key)
            {
                let (_, codes) = held.remove(index);
                self.release_keys(&codes);
            }
            return;
        }
        if self.is_locked() {
            return;
        }
        let chord = match key {
            Key::Code(code) => Some(vec![Keycode::new(code + EVDEV_OFFSET)]),
            Key::Sym(sym) => self.chord_for(xkb::Keysym::new(sym)),
        };
        let Some(chord) = chord else {
            tracing::warn!("no key types keysym {key:?} in the current layout");
            return;
        };
        let held = self.typing.held.entry(session).or_default();
        if held.iter().any(|(down, _)| *down == key) {
            return;
        }
        // Modifiers the program already holds stay as they are.
        let fresh: Vec<Keycode> =
            chord.into_iter().filter(|code| !held.iter().any(|(_, codes)| codes.contains(code))).collect();
        held.push((key, fresh.clone()));
        for code in fresh {
            self.handle_key(code, KeyState::Pressed, time);
        }
    }

    fn release_keys(&mut self, codes: &[Keycode]) {
        let time = (self.now() * 1000.0) as u32;
        for code in codes.iter().rev() {
            self.handle_key(*code, KeyState::Released, time);
        }
    }

    /// The keys that type `sym` in the active layout.
    fn chord_for(&mut self, sym: xkb::Keysym) -> Option<Vec<Keycode>> {
        let keyboard = self.seat.get_keyboard()?;
        let layout =
            keyboard.with_xkb_state(self, |context| context.xkb().lock().map(|xkb| xkb.active_layout().0).unwrap_or(0));
        let names = self.config.file.keyboard.names();
        if self.typing.keys.as_ref().is_none_or(|(compiled, _)| *compiled != names) {
            self.typing.keys = Keys::new(&self.config.file.keyboard).map(|keys| (names, keys));
        }
        let (_, keys) = self.typing.keys.as_ref()?;
        let (code, level) = keys.locate(sym, layout, 4)?;
        chord(keys, code, level, layout)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Command, Stdio},
        time::Duration,
    };

    use smithay::reexports::calloop;
    use zbus::blocking::{Connection, Proxy, connection::Builder};

    use super::*;

    const NAME: &str = "org.freedesktop.impl.portal.desktop.tileception";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const INTERFACE: &str = "org.freedesktop.impl.portal.RemoteDesktop";

    #[test]
    fn callers_from_session_paths() {
        assert_eq!(caller("/org/freedesktop/portal/desktop/session/1_42/token7").as_deref(), Some(":1.42"));
        assert_eq!(caller("/org/freedesktop/portal/desktop/session/1_42"), None);
        assert_eq!(caller("/org/freedesktop/portal/desktop/request/1_42/token7"), None);
    }

    #[test]
    fn restore_data_round_trips() {
        let data = restore_data("/usr/bin/keepassxc").unwrap();
        assert_eq!(data.value_signature().to_string(), "(suv)");
        assert!(restores(&data, "/usr/bin/keepassxc"));
        assert!(!restores(&data, "/usr/bin/other"));
        let foreign = OwnedValue::try_from(Value::from(("GNOME", RESTORE_VERSION, Value::new("/usr/bin/keepassxc"))));
        assert!(!restores(&foreign.unwrap(), "/usr/bin/keepassxc"));
        assert!(!restores(&Value::from("/usr/bin/keepassxc"), "/usr/bin/keepassxc"));
    }

    #[test]
    fn chords_hold_shift_and_altgr() {
        let keyboard = crate::config::Keyboard { layout: Some("de".into()), ..Default::default() };
        let keys = Keys::new(&keyboard).expect("de keymap");
        let typed = |character: char| {
            let (code, level) = keys.locate(xkb::utf32_to_keysym(character as u32), 0, 4).unwrap();
            chord(&keys, code, level, 0).unwrap().len()
        };
        assert_eq!(typed('a'), 1);
        assert_eq!(typed('A'), 2);
        // AltGr+Q on a German keyboard.
        assert_eq!(typed('@'), 2);
        assert!(keys.locate(xkb::Keysym::Tab, 0, 4).is_some_and(|(_, level)| level == 0));
    }

    /// A private session bus.
    struct Bus(Child, String);

    impl Bus {
        fn new() -> Bus {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--print-address", "--nofork"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("tests require dbus-daemon");
            let mut address = String::new();
            BufReader::new(child.stdout.take().unwrap()).read_line(&mut address).unwrap();
            Bus(child, address.trim().to_owned())
        }

        fn builder(&self) -> Builder<'_> {
            Builder::address(self.1.as_str()).unwrap().method_timeout(Duration::from_secs(3))
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Serves the portal, answering every question with `answer`, and hands
    /// back what reached the compositor.
    fn serve(bus: &Bus, answer: Answer) -> (Connection, std::sync::mpsc::Receiver<Message>) {
        let (compositor, messages) = channel::channel();
        let shared = Shared { sessions: Default::default(), compositor };
        let service = bus.builder().name(NAME).unwrap().serve_at(PATH, RemoteDesktop { shared }).unwrap().build();
        let (seen, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut event_loop = calloop::EventLoop::<()>::try_new().unwrap();
            event_loop
                .handle()
                .insert_source(messages, move |event, _, _| {
                    if let channel::Event::Msg(message) = event {
                        if let Message::Ask { reply, .. } = &message {
                            let _ = reply.try_send(answer);
                        }
                        let _ = seen.send(message);
                    }
                })
                .unwrap();
            event_loop.run(None, &mut (), |_| {}).unwrap();
        });
        (service.unwrap(), heard)
    }

    fn session_path(client: &Connection) -> String {
        let unique = client.unique_name().unwrap().trim_start_matches(':').replace('.', "_");
        format!("/org/freedesktop/portal/desktop/session/{unique}/s1")
    }

    fn request(client: &Connection, number: u32) -> OwnedObjectPath {
        let unique = client.unique_name().unwrap().trim_start_matches(':').replace('.', "_");
        OwnedObjectPath::try_from(format!("/org/freedesktop/portal/desktop/request/{unique}/r{number}")).unwrap()
    }

    /// Creates a session, selects the keyboard and starts it, as the portal does.
    fn start(client: &Connection, options: Options) -> (String, u32, Results) {
        let proxy = Proxy::new(client, NAME, PATH, INTERFACE).unwrap();
        let session = OwnedObjectPath::try_from(session_path(client)).unwrap();
        let (created, _): (u32, Results) =
            proxy.call("CreateSession", &(request(client, 1), &session, "", Options::new())).unwrap();
        assert_eq!(created, SUCCESS);
        let (selected, _): (u32, Results) =
            proxy.call("SelectDevices", &(request(client, 2), &session, "", options)).unwrap();
        assert_eq!(selected, SUCCESS);
        let (response, results): (u32, Results) =
            proxy.call("Start", &(request(client, 3), &session, "", "", Options::new())).unwrap();
        (session.to_string(), response, results)
    }

    fn keyboard_options(persist: u32) -> Options {
        Options::from([
            ("types".into(), OwnedValue::from(KEYBOARD)),
            ("persist_mode".into(), OwnedValue::from(persist)),
        ])
    }

    #[test]
    fn asks_then_types() {
        let bus = Bus::new();
        let (_service, heard) = serve(&bus, Answer::Remember);
        let client = bus.builder().build().unwrap();
        let proxy = Proxy::new(&client, NAME, PATH, INTERFACE).unwrap();
        assert_eq!(proxy.get_property::<u32>("AvailableDeviceTypes").unwrap(), KEYBOARD);
        assert_eq!(proxy.get_property::<u32>("version").unwrap(), 2);

        let (session, response, results) = start(&client, keyboard_options(2));
        assert_eq!(response, SUCCESS);
        assert_eq!(results.get("devices"), Some(&OwnedValue::from(KEYBOARD)));
        let Message::Ask { program, persist, .. } = heard.recv_timeout(Duration::from_secs(3)).unwrap() else {
            panic!("the user is asked first");
        };
        // The test itself made the session.
        assert_eq!(program.path, std::fs::read_link("/proc/self/exe").ok());
        assert_eq!(persist, 2);
        assert_eq!(results.get("persist_mode"), Some(&OwnedValue::from(2u32)));
        assert!(results.contains_key("restore_data"));

        let path = OwnedObjectPath::try_from(session.as_str()).unwrap();
        let () = proxy.call("NotifyKeyboardKeysym", &(&path, Options::new(), 0x61i32, 1u32)).unwrap();
        let () = proxy.call("NotifyKeyboardKeycode", &(&path, Options::new(), 30i32, 0u32)).unwrap();
        let keys: Vec<(Key, bool)> = (0..2)
            .map(|_| match heard.recv_timeout(Duration::from_secs(3)).unwrap() {
                Message::Key { session: typed_in, key, pressed } if typed_in == session => (key, pressed),
                _ => panic!("expected a key press"),
            })
            .collect();
        assert_eq!(keys, [(Key::Sym(0x61), true), (Key::Code(30), false)]);

        let closer = Proxy::new(&client, NAME, session.as_str(), "org.freedesktop.impl.portal.Session").unwrap();
        let () = closer.call("Close", &()).unwrap();
        assert!(
            matches!(heard.recv_timeout(Duration::from_secs(3)).unwrap(), Message::Closed(closed) if closed == session)
        );
        // A closed session types nothing more.
        let () = proxy.call("NotifyKeyboardKeysym", &(&path, Options::new(), 0x61i32, 1u32)).unwrap();
        assert!(heard.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn restored_sessions_skip_the_question() {
        let bus = Bus::new();
        let (_service, heard) = serve(&bus, Answer::Deny);
        let client = bus.builder().build().unwrap();
        let exe = std::fs::read_link("/proc/self/exe").unwrap();
        let mut options = keyboard_options(1);
        options.insert("restore_data".into(), restore_data(exe.to_str().unwrap()).unwrap());
        let (_, response, results) = start(&client, options);
        assert_eq!(response, SUCCESS);
        assert_eq!(results.get("persist_mode"), Some(&OwnedValue::from(1u32)));
        assert!(heard.try_recv().is_err(), "nobody was asked");
    }

    #[test]
    fn denied_sessions_cannot_type() {
        let bus = Bus::new();
        let (_service, heard) = serve(&bus, Answer::Deny);
        let client = bus.builder().build().unwrap();
        let (session, response, results) = start(&client, keyboard_options(0));
        assert_eq!(response, CANCELLED);
        assert!(results.is_empty());
        assert!(matches!(heard.recv_timeout(Duration::from_secs(3)).unwrap(), Message::Ask { persist: 0, .. }));
        let proxy = Proxy::new(&client, NAME, PATH, INTERFACE).unwrap();
        let path = OwnedObjectPath::try_from(session.as_str()).unwrap();
        let () = proxy.call("NotifyKeyboardKeysym", &(&path, Options::new(), 0x61i32, 1u32)).unwrap();
        assert!(heard.recv_timeout(Duration::from_millis(200)).is_err());
    }
}
