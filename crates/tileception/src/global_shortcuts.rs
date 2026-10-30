//! Global shortcuts for applications. Tileception is a GlobalShortcuts
//! backend for xdg-desktop-portal, which is how KeePassXC's global auto-type
//! shortcut works on Wayland. The application names its shortcuts and the user
//! picks the keys: when a shortcut has none yet, or the application's
//! Configure button asks, a notification waits for a key combination. Keys are
//! kept per program in `$XDG_STATE_HOME/tileception/shortcuts.toml`, so later
//! sessions bind them without asking. Pressing one sends Activated, letting go
//! Deactivated; a shortcut does nothing while the screen is locked.
//!
//! As with typing (`remote_desktop`), the D-Bus side only keeps track of
//! sessions and leaves every decision to the compositor.

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};
use smithay::reexports::calloop::channel;
use zbus::{
    ObjectServer,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

use crate::{Tileception, config::Mods, remote_desktop::Program};

const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.impl.portal.GlobalShortcuts";
/// Portal responses.
const SUCCESS: u32 = 0;
const CANCELLED: u32 = 1;
const ENDED: u32 = 2;

type Options = HashMap<String, OwnedValue>;
type Results = HashMap<String, OwnedValue>;

/// A key combination: modifiers and the unmodified keysym.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    #[serde(default)]
    logo: bool,
    #[serde(default)]
    ctrl: bool,
    #[serde(default)]
    alt: bool,
    #[serde(default)]
    shift: bool,
    key: u32,
}

impl Trigger {
    pub fn new(mods: Mods, key: u32) -> Trigger {
        Trigger { logo: mods.logo, ctrl: mods.ctrl, alt: mods.alt, shift: mods.shift, key }
    }

    fn mods(self) -> Mods {
        Mods { logo: self.logo, ctrl: self.ctrl, alt: self.alt, shift: self.shift }
    }

    /// "Ctrl+Alt+A".
    fn label(self) -> String {
        crate::launcher::combo_label(self.mods(), self.key)
    }
}

/// One of an application's shortcuts.
#[derive(Debug, Clone, PartialEq)]
pub struct Shortcut {
    pub shortcut_id: String,
    pub description: String,
    pub trigger: Option<Trigger>,
}

/// What the D-Bus side asks of the compositor.
pub enum Message {
    /// Bind a session's shortcuts, asking for the keys of those that have
    /// none. The reply lists the bound ones.
    Bind {
        session: String,
        program: Program,
        shortcuts: Vec<(String, String)>,
        reply: async_channel::Sender<Vec<Shortcut>>,
    },
    /// The bound shortcuts of a session, or those kept for its program.
    List {
        session: String,
        program: Program,
        reply: async_channel::Sender<Vec<Shortcut>>,
    },
    /// Ask for new keys for all of a session's shortcuts.
    Configure(String),
    /// The application no longer waits for a Bind.
    Withdraw(String),
    Closed(String),
}

#[derive(Clone)]
struct Shared {
    programs: Arc<Mutex<HashMap<String, Program>>>,
    compositor: channel::Sender<Message>,
}

impl Shared {
    fn programs(&self) -> MutexGuard<'_, HashMap<String, Program>> {
        // Plain data, so a panic elsewhere cannot leave it half written.
        self.programs.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn program(&self, session: &str) -> Option<Program> {
        self.programs().get(session).cloned()
    }

    /// Sends a message that is answered, and waits for the answer.
    async fn ask(&self, message: Message, answer: async_channel::Receiver<Vec<Shortcut>>) -> Option<Vec<Shortcut>> {
        self.compositor.send(message).ok()?;
        answer.recv().await.ok()
    }
}

/// `a(sa{sv})`: each bound shortcut with what it does and its keys.
fn shortcut_list(shortcuts: &[Shortcut]) -> Vec<(String, HashMap<String, Value<'static>>)> {
    shortcuts
        .iter()
        .filter_map(|shortcut| {
            let trigger = shortcut.trigger?;
            let properties = HashMap::from([
                ("description".to_string(), Value::from(shortcut.description.clone())),
                ("trigger_description".to_string(), Value::from(trigger.label())),
            ]);
            Some((shortcut.shortcut_id.clone(), properties))
        })
        .collect()
}

fn results(shortcuts: &[Shortcut]) -> Results {
    OwnedValue::try_from(Value::from(shortcut_list(shortcuts)))
        .map(|list| Results::from([("shortcuts".to_string(), list)]))
        .unwrap_or_default()
}

/// Lets the portal call off a Bind it is waiting on.
struct Request {
    session: String,
    compositor: channel::Sender<Message>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    fn close(&self) {
        let _ = self.compositor.send(Message::Withdraw(self.session.clone()));
    }
}

struct ShortcutSession {
    path: String,
    shared: Shared,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl ShortcutSession {
    async fn close(&self, #[zbus(object_server)] server: &ObjectServer) {
        if self.shared.programs().remove(&self.path).is_some() {
            let _ = self.shared.compositor.send(Message::Closed(self.path.clone()));
        }
        let _ = server.remove::<ShortcutSession, _>(self.path.as_str()).await;
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

pub struct GlobalShortcuts {
    shared: Shared,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.GlobalShortcuts")]
impl GlobalShortcuts {
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
        let program = Program { app_id, path: crate::remote_desktop::program_path(connection, &path).await };
        self.shared.programs().insert(path.clone(), program);
        let served = ShortcutSession { path: path.clone(), shared: self.shared.clone() };
        if !server.at(path.as_str(), served).await.unwrap_or(false) {
            self.shared.programs().remove(&path);
            return (ENDED, Results::new());
        }
        (SUCCESS, Results::new())
    }

    async fn bind_shortcuts(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        shortcuts: Vec<(String, Options)>,
        _parent_window: String,
        _options: Options,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, Results) {
        let session = session_handle.to_string();
        let Some(program) = self.shared.program(&session) else {
            return (ENDED, Results::new());
        };
        let shortcuts = shortcuts
            .into_iter()
            .map(|(shortcut_id, properties)| {
                let description = properties.get("description").and_then(|value| String::try_from(value.clone()).ok());
                let description = description.unwrap_or_else(|| shortcut_id.clone());
                (shortcut_id, description)
            })
            .collect();
        let closer = Request { session: session.clone(), compositor: self.shared.compositor.clone() };
        let _ = server.at(handle.as_str(), closer).await;
        let (reply, answer) = async_channel::bounded(1);
        let bound = self.shared.ask(Message::Bind { session, program, shortcuts, reply }, answer).await;
        let _ = server.remove::<Request, _>(handle.as_str()).await;
        match bound {
            Some(bound) => (SUCCESS, results(&bound)),
            None => (CANCELLED, Results::new()),
        }
    }

    async fn list_shortcuts(&self, _handle: OwnedObjectPath, session_handle: OwnedObjectPath) -> (u32, Results) {
        let session = session_handle.to_string();
        let Some(program) = self.shared.program(&session) else {
            return (ENDED, Results::new());
        };
        let (reply, answer) = async_channel::bounded(1);
        match self.shared.ask(Message::List { session, program, reply }, answer).await {
            Some(shortcuts) => (SUCCESS, results(&shortcuts)),
            None => (ENDED, Results::new()),
        }
    }

    /// Asks for new keys; ShortcutsChanged follows.
    fn configure_shortcuts(&self, session_handle: OwnedObjectPath, _parent_window: String, _options: Options) {
        if self.shared.programs().contains_key(session_handle.as_str()) {
            let _ = self.shared.compositor.send(Message::Configure(session_handle.to_string()));
        }
    }

    /// Version 2 brings ConfigureShortcuts.
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

/// Keys kept for one program's shortcut.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Kept {
    program: String,
    #[serde(rename = "id")]
    shortcut_id: String,
    #[serde(flatten)]
    trigger: Trigger,
}

#[derive(Default, Serialize, Deserialize)]
struct KeptFile {
    #[serde(default)]
    shortcut: Vec<Kept>,
}

fn kept_path() -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    root.join("tileception/shortcuts.toml")
}

/// What keys are kept under: the app ID, or else the executable. None when
/// neither is known, and the keys only last as long as the session.
fn program_key(program: &Program) -> Option<String> {
    if !program.app_id.is_empty() {
        return Some(program.app_id.clone());
    }
    program.path.as_ref().and_then(|path| path.to_str()).map(str::to_owned)
}

struct Session {
    program: Option<String>,
    /// What to call the program, and its desktop entry for an icon.
    name: String,
    desktop_entry: Option<String>,
    shortcuts: Vec<Shortcut>,
}

/// Asking for the keys of some of a session's shortcuts, one at a time.
struct Job {
    session: String,
    /// Indexes into the session's shortcuts, still to ask for.
    missing: VecDeque<usize>,
    /// Bind waits for this; Configure announces ShortcutsChanged instead.
    reply: Option<async_channel::Sender<Vec<Shortcut>>>,
}

/// The compositor's side.
#[derive(Default)]
pub struct Shortcuts {
    sessions: HashMap<String, Session>,
    kept: Vec<Kept>,
    loaded: bool,
    jobs: VecDeque<Job>,
    /// The notification waiting for a key combination.
    asking: Option<u32>,
    /// Key codes held down that set off a shortcut, with its session and shortcut_id.
    pressed: HashMap<u32, (String, String)>,
}

impl Shortcuts {
    fn kept(&mut self) -> &mut Vec<Kept> {
        if !self.loaded {
            self.loaded = true;
            self.kept = std::fs::read_to_string(kept_path())
                .ok()
                .and_then(|text| toml::from_str::<KeptFile>(&text).ok())
                .map(|file| file.shortcut)
                .unwrap_or_default();
        }
        &mut self.kept
    }

    fn kept_trigger(&mut self, program: Option<&str>, shortcut_id: &str) -> Option<Trigger> {
        let program = program?;
        self.kept()
            .iter()
            .find(|kept| kept.program == program && kept.shortcut_id == shortcut_id)
            .map(|kept| kept.trigger)
    }

    fn keep(&mut self, program: Option<&str>, shortcut_id: &str, trigger: Trigger) {
        let Some(program) = program else {
            return;
        };
        let kept = self.kept();
        kept.retain(|kept| !(kept.program == program && kept.shortcut_id == shortcut_id));
        kept.push(Kept { program: program.to_owned(), shortcut_id: shortcut_id.to_owned(), trigger });
        let file = KeptFile { shortcut: kept.clone() };
        let path = kept_path();
        let written = toml::to_string(&file).map_err(|err| err.to_string()).and_then(|text| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
            }
            std::fs::write(&path, text).map_err(|err| err.to_string())
        });
        if let Err(err) = written {
            tracing::warn!("could not keep shortcut keys in {}: {err}", path.display());
        }
    }

    /// The session and shortcut a key combination sets off.
    fn matching(&self, trigger: Trigger) -> Option<(String, String)> {
        self.sessions.iter().find_map(|(path, session)| {
            let shortcut = session.shortcuts.iter().find(|shortcut| shortcut.trigger == Some(trigger))?;
            Some((path.clone(), shortcut.shortcut_id.clone()))
        })
    }

    /// The shortcut being asked for: its job and index.
    fn current(&self) -> Option<(&Session, &Shortcut)> {
        let job = self.jobs.front()?;
        let session = self.sessions.get(&job.session)?;
        Some((session, session.shortcuts.get(*job.missing.front()?)?))
    }
}

impl Tileception {
    /// Hands the D-Bus side of the portal its connection to the compositor and
    /// serves it on `bus`.
    pub fn serve_global_shortcuts<'a>(
        &mut self,
        bus: zbus::blocking::connection::Builder<'a>,
    ) -> zbus::Result<zbus::blocking::connection::Builder<'a>> {
        let (compositor, messages) = channel::channel();
        self.loop_handle
            .insert_source(messages, |event, _, state| {
                if let channel::Event::Msg(message) = event {
                    state.global_shortcuts_message(message);
                }
            })
            .map_err(|err| zbus::Error::Failure(err.to_string()))?;
        let shared = Shared { programs: Default::default(), compositor };
        bus.serve_at(PATH, GlobalShortcuts { shared })
    }

    fn global_shortcuts_message(&mut self, message: Message) {
        match message {
            Message::Bind { session, program, shortcuts, reply } => {
                let key = program_key(&program);
                let (name, desktop_entry) = self.program_name(&program);
                let shortcuts: Vec<Shortcut> = shortcuts
                    .into_iter()
                    .map(|(shortcut_id, description)| {
                        let trigger = self.shortcuts.kept_trigger(key.as_deref(), &shortcut_id);
                        Shortcut { shortcut_id, description, trigger }
                    })
                    .collect();
                let missing = shortcuts.iter().enumerate().filter(|(_, shortcut)| shortcut.trigger.is_none());
                let missing = missing.map(|(index, _)| index).collect();
                let session_state = Session { program: key, name, desktop_entry, shortcuts };
                self.shortcuts.sessions.insert(session.clone(), session_state);
                self.shortcuts.jobs.push_back(Job { session, missing, reply: Some(reply) });
                self.next_shortcut_question();
            }
            Message::List { session, program, reply } => {
                let listed = match self.shortcuts.sessions.get(&session) {
                    Some(bound) => bound.shortcuts.clone(),
                    None => {
                        let key = program_key(&program);
                        let kept = self.shortcuts.kept().iter().filter(|kept| Some(&kept.program) == key.as_ref());
                        kept.map(|kept| Shortcut {
                            shortcut_id: kept.shortcut_id.clone(),
                            description: kept.shortcut_id.clone(),
                            trigger: Some(kept.trigger),
                        })
                        .collect()
                    }
                };
                let _ = reply.try_send(listed);
            }
            Message::Configure(session) => {
                let Some(bound) = self.shortcuts.sessions.get(&session) else {
                    return;
                };
                let missing = (0..bound.shortcuts.len()).collect();
                self.shortcuts.jobs.push_back(Job { session, missing, reply: None });
                self.next_shortcut_question();
            }
            Message::Withdraw(session) => self.drop_shortcut_jobs(&session),
            Message::Closed(session) => {
                self.drop_shortcut_jobs(&session);
                self.shortcuts.sessions.remove(&session);
                self.shortcuts.pressed.retain(|_, (pressed_in, _)| *pressed_in != session);
            }
        }
    }

    fn drop_shortcut_jobs(&mut self, session: &str) {
        let asking_here = self.shortcuts.jobs.front().is_some_and(|job| job.session == session);
        if asking_here && let Some(notification_id) = self.shortcuts.asking.take() {
            self.withdraw_question(notification_id);
        }
        // Dropping a Bind's reply tells the waiting call.
        self.shortcuts.jobs.retain(|job| job.session != session);
        self.next_shortcut_question();
    }

    /// Asks for the next shortcut's keys, finishing the jobs that need nothing more.
    fn next_shortcut_question(&mut self) {
        if self.shortcuts.asking.is_some() {
            return;
        }
        while let Some(job) = self.shortcuts.jobs.front() {
            if !job.missing.is_empty() {
                break;
            }
            if let Some(job) = self.shortcuts.jobs.pop_front() {
                self.finish_shortcut_job(job);
            }
        }
        let Some((session, shortcut)) = self.shortcuts.current() else {
            return;
        };
        let summary = format!("{}: {}", session.name, shortcut.description);
        let body = match shortcut.trigger {
            Some(trigger) => format!("Press new keys for this shortcut, now {}. Escape keeps them.", trigger.label()),
            None => "Press the keys for this shortcut, such as Ctrl+Alt+A. Escape leaves it unset.".into(),
        };
        let desktop_entry = session.desktop_entry.clone();
        let actions = vec![("skip".to_string(), "Skip".to_string())];
        self.shortcuts.asking = Some(self.ask_locally(&summary, &body, desktop_entry, actions));
    }

    fn finish_shortcut_job(&mut self, job: Job) {
        let Some(session) = self.shortcuts.sessions.get(&job.session) else {
            return;
        };
        match job.reply {
            Some(reply) => {
                let _ = reply.try_send(session.shortcuts.clone());
            }
            None => {
                let changed = shortcut_list(&session.shortcuts);
                let Ok(path) = ObjectPath::try_from(job.session.as_str()) else {
                    return;
                };
                self.emit_shortcut_signal("ShortcutsChanged", &(path, changed));
            }
        }
    }

    /// Whether a question for shortcut keys is up; it takes every key press.
    pub fn asking_for_shortcut(&self) -> bool {
        self.shortcuts.asking.is_some()
    }

    /// A key combination pressed while asking. `taken` when Tileception
    /// already uses it. Escape on its own skips the shortcut.
    pub fn shortcut_keys_pressed(&mut self, trigger: Trigger, typing: bool, taken: bool) {
        let escape = trigger == Trigger::new(Mods::default(), smithay::input::keyboard::keysyms::KEY_Escape);
        if escape {
            self.skip_shortcut();
            return;
        }
        // A letter on its own or with Shift would fire while typing.
        let shift = Mods { shift: true, ..Default::default() };
        if (trigger.mods() == Mods::default() || trigger.mods() == shift) && typing {
            return;
        }
        let Some(job) = self.shortcuts.jobs.front() else {
            return;
        };
        let asked_id = self.shortcuts.current().map(|(_, shortcut)| shortcut.shortcut_id.clone()).unwrap_or_default();
        let asked = (job.session.clone(), asked_id);
        // Keys another shortcut has, and not the one being changed.
        let clash = self.shortcuts.matching(trigger).is_some_and(|found| found != asked);
        if taken || clash {
            let label = trigger.label();
            self.notify_locally(
                &format!("{label} is taken"),
                "Press other keys for this shortcut.",
                Default::default(),
            );
            return;
        }
        let Some(job) = self.shortcuts.jobs.front_mut() else {
            return;
        };
        let Some(index) = job.missing.pop_front() else {
            return;
        };
        let session = job.session.clone();
        let Some(bound) = self.shortcuts.sessions.get_mut(&session) else {
            return;
        };
        let program = bound.program.clone();
        let Some(shortcut) = bound.shortcuts.get_mut(index) else {
            return;
        };
        shortcut.trigger = Some(trigger);
        let shortcut_id = shortcut.shortcut_id.clone();
        self.shortcuts.keep(program.as_deref(), &shortcut_id, trigger);
        self.close_shortcut_question();
    }

    /// The question's Skip or close button.
    pub fn answer_shortcut(&mut self, notification_id: u32) {
        if self.shortcuts.asking == Some(notification_id) {
            self.shortcuts.asking = None;
            if let Some(job) = self.shortcuts.jobs.front_mut() {
                job.missing.pop_front();
            }
            self.next_shortcut_question();
        }
    }

    fn skip_shortcut(&mut self) {
        if let Some(job) = self.shortcuts.jobs.front_mut() {
            job.missing.pop_front();
        }
        self.close_shortcut_question();
    }

    fn close_shortcut_question(&mut self) {
        if let Some(notification_id) = self.shortcuts.asking.take() {
            self.withdraw_question(notification_id);
        }
        self.next_shortcut_question();
    }

    /// A key press or release outside questions: true when it set off an
    /// application's shortcut, which then keeps the key.
    pub fn portal_shortcut_key(&mut self, keycode: u32, trigger: Trigger, pressed: bool) -> bool {
        let timestamp = (self.now() * 1000.0) as u64;
        if !pressed {
            if let Some((session, shortcut_id)) = self.shortcuts.pressed.remove(&keycode) {
                self.emit_shortcut_activation("Deactivated", &session, &shortcut_id, timestamp);
            }
            return false;
        }
        let Some((session, shortcut_id)) = self.shortcuts.matching(trigger) else {
            return false;
        };
        self.emit_shortcut_activation("Activated", &session, &shortcut_id, timestamp);
        self.shortcuts.pressed.insert(keycode, (session, shortcut_id));
        true
    }

    fn emit_shortcut_activation(&self, signal: &str, session: &str, shortcut_id: &str, timestamp: u64) {
        if let Ok(path) = ObjectPath::try_from(session) {
            self.emit_shortcut_signal(signal, &(path, shortcut_id, timestamp, HashMap::<&str, Value<'_>>::new()));
        }
    }

    fn emit_shortcut_signal<B: serde::Serialize + zbus::zvariant::DynamicType>(&self, signal: &str, body: &B) {
        let Some(service) = &self.appearance else {
            return;
        };
        if let Err(err) = service.connection().emit_signal(None::<&str>, PATH, INTERFACE, signal, body) {
            tracing::warn!("could not send {signal}: {err}");
        }
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

    #[test]
    fn triggers_are_described_and_kept() {
        let trigger = Trigger::new(Mods { ctrl: true, alt: true, ..Default::default() }, 'a' as u32);
        assert_eq!(trigger.label(), "Ctrl+Alt+A");
        let file = KeptFile {
            shortcut: vec![Kept { program: "/usr/bin/keepassxc".into(), shortcut_id: "autotype".into(), trigger }],
        };
        let text = toml::to_string(&file).unwrap();
        let read: KeptFile = toml::from_str(&text).unwrap();
        assert_eq!(read.shortcut, file.shortcut);
        assert_eq!(program_key(&Program { app_id: String::new(), path: None }), None);
        let keepassxc = Program { app_id: String::new(), path: Some("/usr/bin/keepassxc".into()) };
        assert_eq!(program_key(&keepassxc).as_deref(), Some("/usr/bin/keepassxc"));
    }

    #[test]
    fn shortcut_lists_hold_bound_shortcuts() {
        let trigger = Trigger::new(Mods { ctrl: true, alt: true, ..Default::default() }, 'a' as u32);
        let shortcuts = [
            Shortcut {
                shortcut_id: "autotype".into(),
                description: "Trigger global Auto-Type".into(),
                trigger: Some(trigger),
            },
            Shortcut { shortcut_id: "unset".into(), description: "Not bound".into(), trigger: None },
        ];
        let listed = results(&shortcuts);
        assert_eq!(listed["shortcuts"].value_signature().to_string(), "a(sa{sv})");
        let list = shortcut_list(&shortcuts);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1["trigger_description"], Value::from("Ctrl+Alt+A"));
    }

    #[test]
    fn matching_finds_the_session() {
        let trigger = Trigger::new(Mods { ctrl: true, alt: true, ..Default::default() }, 'a' as u32);
        let mut shortcuts = Shortcuts::default();
        let shortcut = Shortcut { shortcut_id: "autotype".into(), description: String::new(), trigger: Some(trigger) };
        let session = Session { program: None, name: String::new(), desktop_entry: None, shortcuts: vec![shortcut] };
        shortcuts.sessions.insert("/s".into(), session);
        assert_eq!(shortcuts.matching(trigger), Some(("/s".into(), "autotype".into())));
        assert_eq!(shortcuts.matching(Trigger::new(Mods::default(), 'a' as u32)), None);
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

    fn path(client: &Connection, kind: &str, token: &str) -> OwnedObjectPath {
        let unique = client.unique_name().unwrap().trim_start_matches(':').replace('.', "_");
        OwnedObjectPath::try_from(format!("/org/freedesktop/portal/desktop/{kind}/{unique}/{token}")).unwrap()
    }

    #[test]
    fn binds_through_the_compositor() {
        let bus = Bus::new();
        let (compositor, messages) = channel::channel();
        let shared = Shared { programs: Default::default(), compositor };
        let _service = bus.builder().name(NAME).unwrap().serve_at(PATH, GlobalShortcuts { shared }).unwrap().build();
        // A compositor that binds every shortcut to Ctrl+Alt+A.
        let (seen, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut event_loop = calloop::EventLoop::<()>::try_new().unwrap();
            let trigger = Trigger::new(Mods { ctrl: true, alt: true, ..Default::default() }, 'a' as u32);
            event_loop
                .handle()
                .insert_source(messages, move |event, _, _| match event {
                    channel::Event::Msg(Message::Bind { shortcuts, reply, program, .. }) => {
                        let bound = shortcuts
                            .into_iter()
                            .map(|(shortcut_id, description)| Shortcut {
                                shortcut_id,
                                description,
                                trigger: Some(trigger),
                            })
                            .collect();
                        let _ = reply.try_send(bound);
                        let _ = seen.send(format!("bind {:?}", program.path.is_some()));
                    }
                    channel::Event::Msg(Message::List { reply, .. }) => {
                        let _ = reply.try_send(Vec::new());
                    }
                    channel::Event::Msg(Message::Closed(_)) => {
                        let _ = seen.send("closed".into());
                    }
                    _ => {}
                })
                .unwrap();
            event_loop.run(None, &mut (), |_| {}).unwrap();
        });
        let client = bus.builder().build().unwrap();
        let proxy = Proxy::new(&client, NAME, PATH, INTERFACE).unwrap();
        assert_eq!(proxy.get_property::<u32>("version").unwrap(), 2);
        let session = path(&client, "session", "s1");
        let (created, _): (u32, Results) =
            proxy.call("CreateSession", &(path(&client, "request", "r1"), &session, "", Options::new())).unwrap();
        assert_eq!(created, SUCCESS);
        let wanted = vec![("autotype", HashMap::from([("description", Value::from("Trigger global Auto-Type"))]))];
        let (bound, results): (u32, Results) = proxy
            .call("BindShortcuts", &(path(&client, "request", "r2"), &session, wanted, "", Options::new()))
            .unwrap();
        assert_eq!(bound, SUCCESS);
        assert_eq!(heard.recv_timeout(Duration::from_secs(3)).unwrap(), "bind true");
        let list: Vec<(String, HashMap<String, OwnedValue>)> =
            results["shortcuts"].try_clone().unwrap().try_into().unwrap();
        assert_eq!(list[0].0, "autotype");
        assert_eq!(list[0].1["trigger_description"], OwnedValue::from(zbus::zvariant::Str::from("Ctrl+Alt+A")));

        let (listed, _): (u32, Results) =
            proxy.call("ListShortcuts", &(path(&client, "request", "r3"), &session)).unwrap();
        assert_eq!(listed, SUCCESS);
        let closer = Proxy::new(&client, NAME, session.as_str(), "org.freedesktop.impl.portal.Session").unwrap();
        let () = closer.call("Close", &()).unwrap();
        assert_eq!(heard.recv_timeout(Duration::from_secs(3)).unwrap(), "closed");
        let (ended, _): (u32, Results) =
            proxy.call("ListShortcuts", &(path(&client, "request", "r4"), &session)).unwrap();
        assert_eq!(ended, ENDED);
    }
}
