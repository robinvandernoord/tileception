//! Screen sharing for applications: Tileception is the ScreenCast backend
//! for xdg-desktop-portal, passing every call on to xdg-desktop-portal-wlr,
//! which makes the PipeWire stream, and adding restore data of its own.
//!
//! xdg-desktop-portal-wlr hands out restore data only for screens. Chromium-
//! based browsers show what was chosen in a preview session, then share it
//! in a second session started with the restore token from the first;
//! without one, choosing a window asked twice. Restore data here names the
//! screen or window chosen in Tileception's launcher (see
//! [`crate::share_pick`]), so the second session gets it without asking.
//!
//! The D-Bus side runs off the compositor thread. The chooser runs inside
//! xdg-desktop-portal-wlr's SelectSources, while that call is passed on, so
//! what it picked is known when the call returns. The chooser cannot say
//! which request it is for, so one SelectSources is passed on at a time:
//! restore data and the choice then belong to that request alone.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use smithay::reexports::calloop::channel;
use zbus::{
    ObjectServer,
    object_server::SignalEmitter,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use crate::{Tileception, share_pick::Target};

/// xdg-desktop-portal-wlr, which this passes calls on to.
const WLR: &str = "org.freedesktop.impl.portal.desktop.wlr";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SCREEN_CAST: &str = "org.freedesktop.impl.portal.ScreenCast";
const SESSION: &str = "org.freedesktop.impl.portal.Session";
const REQUEST: &str = "org.freedesktop.impl.portal.Request";
/// Restore data names its maker and format.
const VENDOR: &str = "tileception";
const RESTORE_VERSION: u32 = 1;
/// Portal responses.
const SUCCESS: u32 = 0;
const CANCELLED: u32 = 1;
const ENDED: u32 = 2;
/// Source types and cursor modes, for when xdg-desktop-portal-wlr cannot be asked.
const MONITOR: u32 = 1;
const HIDDEN_AND_EMBEDDED: u32 = 1 | 2;
/// Persist mode: remembered while the program runs.
const TRANSIENT: u32 = 1;
/// Version 4 brings restore data and persistence.
const PERSIST_VERSION: u32 = 4;

type Options = HashMap<String, OwnedValue>;
type Reply = (u32, Options);

/// What the D-Bus side asks of the compositor.
pub enum Message {
    /// The next chooser gives this without asking.
    Restore(Target),
    /// The request that restore data was for is over: no later chooser
    /// gets it.
    Forget,
}

/// A session passed on to xdg-desktop-portal-wlr.
#[derive(Default)]
struct Cast {
    /// How long the program asked the choice to be remembered.
    persist: u32,
    /// What its chooser gave.
    target: Option<Target>,
}

/// How long a program that dropped a restored session before starting it
/// may ask again for the same choice: Chromium-based browsers sometimes start
/// the capture twice and drop the first, which has used up the token.
const RETRY: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone)]
struct Shared {
    sessions: Arc<Mutex<HashMap<String, Cast>>>,
    /// Restored choices not started yet, by the program that asked:
    /// what was restored, and when.
    unstarted: Arc<Mutex<HashMap<String, (Target, std::time::Instant)>>>,
    /// What the last chooser gave, written by the compositor.
    chosen: Arc<Mutex<Option<Target>>>,
    /// Held while a SelectSources is passed on, so that a chooser's restore
    /// data and answer cannot cross to another program's request.
    choosing: Arc<async_lock::Mutex<()>>,
    compositor: channel::Sender<Message>,
    /// Whether xdg-desktop-portal-wlr's Closed signals are passed on yet.
    watching: Arc<AtomicBool>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Plain data: a panic elsewhere cannot leave it half written.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Restore data for what was chosen: `(suv)` with `{kind: id}` inside.
fn restore_data(target: &Target) -> Option<OwnedValue> {
    let (kind, name) = match target {
        Target::Screen(name) => ("screen", name.as_str()),
        Target::Window(identifier) => ("window", identifier.as_str()),
    };
    let inner: HashMap<&str, Value<'_>> = HashMap::from([(kind, Value::from(name))]);
    // The structure's third field is a Value, so it goes as a variant: `(suv)`.
    OwnedValue::try_from(Value::from((VENDOR, RESTORE_VERSION, Value::from(inner)))).ok()
}

/// What restore data of ours names; None for anyone else's.
fn restored(data: &Value<'_>) -> Option<Target> {
    let Value::Structure(structure) = data else { return None };
    let [Value::Str(vendor), Value::U32(version), Value::Value(stored)] = structure.fields() else { return None };
    if vendor.as_str() != VENDOR || *version != RESTORE_VERSION {
        return None;
    }
    let Value::Dict(dict) = &**stored else { return None };
    let text = |key: &str| match dict.get::<&str, Value<'_>>(&key).ok().flatten() {
        Some(Value::Value(value)) => match *value {
            Value::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Some(Value::Str(text)) => Some(text.to_string()),
        _ => None,
    };
    text("window").map(Target::Window).or_else(|| text("screen").map(Target::Screen))
}

fn number(options: &Options, key: &str) -> Option<u32> {
    options.get(key).and_then(|value| u32::try_from(value).ok())
}

/// Calls a method of xdg-desktop-portal-wlr's at `path`.
async fn call<B>(
    connection: &zbus::Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    connection.call_method(Some(WLR), path, Some(interface), method, body).await
}

/// A request passed on: the portal may call it off, also while it waits
/// for its turn before being passed on.
struct CastRequest {
    path: String,
    closed: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl CastRequest {
    async fn close(&self, #[zbus(connection)] connection: &zbus::Connection) {
        self.closed.store(true, Ordering::Relaxed);
        let _ = call(connection, &self.path, REQUEST, "Close", &()).await;
    }
}

/// A session passed on: closing it closes xdg-desktop-portal-wlr's.
struct CastSession {
    path: String,
    shared: Shared,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl CastSession {
    async fn close(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) {
        let _ = call(connection, &self.path, SESSION, "Close", &()).await;
        lock(&self.shared.sessions).remove(&self.path);
        let _ = server.remove::<CastSession, _>(self.path.as_str()).await;
    }

    /// The backend's Closed signal has no arguments; the portal adds the
    /// details of its own.
    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

pub struct ScreenCast {
    shared: Shared,
}

impl ScreenCast {
    /// Lets the portal call off the request at `handle` from now on;
    /// the flag says whether it did.
    async fn accept_close(server: &ObjectServer, handle: &OwnedObjectPath) -> Arc<AtomicBool> {
        let closed = Arc::new(AtomicBool::new(false));
        let request = CastRequest { path: handle.to_string(), closed: closed.clone() };
        let _ = server.at(handle.as_str(), request).await;
        closed
    }

    /// Passes a call on to xdg-desktop-portal-wlr, letting the portal call
    /// it off through `handle` meanwhile.
    async fn pass_on<B>(
        &self,
        connection: &zbus::Connection,
        server: &ObjectServer,
        handle: &OwnedObjectPath,
        method: &str,
        body: &B,
    ) -> Reply
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let request = handle.as_str();
        // SelectSources registers its request before waiting for its turn.
        let _ = server.at(request, CastRequest { path: request.to_string(), closed: Default::default() }).await;
        let reply = call(connection, PATH, SCREEN_CAST, method, body)
            .await
            .and_then(|reply| reply.body().deserialize::<Reply>());
        let _ = server.remove::<CastRequest, _>(request).await;
        reply.unwrap_or_else(|err| {
            tracing::warn!("screen cast: {method} failed in xdg-desktop-portal-wlr: {err}");
            (ENDED, Options::new())
        })
    }

    /// Passes xdg-desktop-portal-wlr's Closed signals on, from the first session.
    fn watch_closed(&self, connection: &zbus::Connection) {
        if self.shared.watching.swap(true, Ordering::Relaxed) {
            return;
        }
        let watched = connection.clone();
        let shared = self.shared.clone();
        let watch = async move {
            if let Err(err) = pass_on_closed(&watched, &shared).await {
                tracing::warn!("screen cast: cannot follow sessions closing: {err}");
                shared.watching.store(false, Ordering::Relaxed);
            }
        };
        connection.executor().spawn(watch, "screen cast closed").detach();
    }

    async fn property(connection: &zbus::Connection, name: &str) -> Option<u32> {
        let reply = connection
            .call_method(Some(WLR), PATH, Some("org.freedesktop.DBus.Properties"), "Get", &(SCREEN_CAST, name))
            .await
            .ok()?;
        let value: OwnedValue = reply.body().deserialize().ok()?;
        u32::try_from(value).ok()
    }
}

async fn pass_on_closed(connection: &zbus::Connection, shared: &Shared) -> zbus::Result<()> {
    use zbus::export::futures_core::Stream;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(WLR)?
        .interface(SESSION)?
        .member("Closed")?
        .build();
    let mut stream = zbus::MessageStream::for_match_rule(rule, connection, None).await?;
    while let Some(message) = std::future::poll_fn(|context| std::pin::Pin::new(&mut stream).poll_next(context)).await {
        let Ok(message) = message else { continue };
        let Some(path) = message.header().path().map(|path| path.to_string()) else { continue };
        lock(&shared.sessions).remove(&path);
        let server = connection.object_server();
        if let Ok(session) = server.interface::<_, CastSession>(path.as_str()).await {
            let _ = CastSession::closed(session.signal_emitter()).await;
            let _ = server.remove::<CastSession, _>(path.as_str()).await;
        }
    }
    Ok(())
}

#[zbus::interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCast {
    async fn create_session(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: Options,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Reply {
        self.watch_closed(connection);
        let body = (&handle, &session_handle, &app_id, &options);
        let reply = self.pass_on(connection, server, &handle, "CreateSession", &body).await;
        if reply.0 == SUCCESS {
            let path = session_handle.to_string();
            lock(&self.shared.sessions).insert(path.clone(), Cast::default());
            let session = CastSession { path: path.clone(), shared: self.shared.clone() };
            if !server.at(path.as_str(), session).await.unwrap_or(false) {
                lock(&self.shared.sessions).remove(&path);
                let _ = call(connection, &path, SESSION, "Close", &()).await;
                return (ENDED, Options::new());
            }
        }
        reply
    }

    async fn select_sources(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        mut options: Options,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Reply {
        let closed = Self::accept_close(server, &handle).await;
        let _choosing = self.shared.choosing.lock().await;
        if closed.load(Ordering::Relaxed) {
            let _ = server.remove::<CastRequest, _>(handle.as_str()).await;
            return (CANCELLED, Options::new());
        }
        // Choices are remembered only while the program runs (transient):
        // enough for a browser's preview and share, without a choice made
        // once being given again after a restart without asking.
        let persist = number(&options, "persist_mode").unwrap_or(0).min(TRANSIENT);
        // xdg-desktop-portal-wlr restoring a screen itself, without the
        // chooser, grants what was asked: ask it for no more either.
        if options.contains_key("persist_mode") {
            options.insert("persist_mode".into(), OwnedValue::from(persist));
        }
        // Our restore data is for the chooser; xdg-desktop-portal-wlr keeps
        // its own for screens.
        let program = crate::remote_desktop::caller(session_handle.as_str());
        let ours = options.get("restore_data").and_then(|data| restored(data));
        if ours.is_some() {
            options.remove("restore_data");
        }
        // A program asking again right after dropping a restored session
        // that never started gets that choice again: its token went with it.
        let retried = || {
            let mut unstarted = lock(&self.shared.unstarted);
            let (target, dropped) = unstarted.remove(program.as_deref()?)?;
            (dropped.elapsed() < RETRY).then_some(target)
        };
        // A retry is answered once: it does not make room for another.
        let restore = match ours {
            Some(target) => {
                if let Some(program) = &program {
                    lock(&self.shared.unstarted).insert(program.clone(), (target.clone(), std::time::Instant::now()));
                }
                Some(target)
            }
            None => retried(),
        };
        if let Some(target) = restore {
            let _ = self.shared.compositor.send(Message::Restore(target));
        }
        *lock(&self.shared.chosen) = None;
        let body = (&handle, &session_handle, &app_id, &options);
        let reply = self.pass_on(connection, server, &handle, "SelectSources", &body).await;
        let _ = self.shared.compositor.send(Message::Forget);
        let target = lock(&self.shared.chosen).take();
        if let Some(cast) = lock(&self.shared.sessions).get_mut(session_handle.as_str()) {
            cast.persist = persist;
            cast.target = target;
        }
        reply
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the portal's Start takes five arguments, and passing it on needs the connection and object server"
    )]
    async fn start(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: Options,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Reply {
        let body = (&handle, &session_handle, &app_id, &parent_window, &options);
        let (code, mut results) = self.pass_on(connection, server, &handle, "Start", &body).await;
        if let Some(program) = crate::remote_desktop::caller(session_handle.as_str()) {
            lock(&self.shared.unstarted).remove(&program);
        }
        let remembered = lock(&self.shared.sessions)
            .get(session_handle.as_str())
            .filter(|cast| cast.persist > 0)
            .and_then(|cast| Some((cast.persist, restore_data(cast.target.as_ref()?)?)));
        if code == SUCCESS
            && let Some((persist, data)) = remembered
        {
            results.insert("restore_data".into(), data);
            results.insert("persist_mode".into(), OwnedValue::from(persist));
        }
        // Whatever gave the restore data, it lasts no longer than the program.
        if number(&results, "persist_mode").is_some_and(|granted| granted > TRANSIENT) {
            results.insert("persist_mode".into(), OwnedValue::from(TRANSIENT));
        }
        (code, results)
    }

    #[zbus(property)]
    async fn available_source_types(&self, #[zbus(connection)] connection: &zbus::Connection) -> u32 {
        Self::property(connection, "AvailableSourceTypes").await.unwrap_or(MONITOR)
    }

    #[zbus(property)]
    async fn available_cursor_modes(&self, #[zbus(connection)] connection: &zbus::Connection) -> u32 {
        Self::property(connection, "AvailableCursorModes").await.unwrap_or(HIDDEN_AND_EMBEDDED)
    }

    /// At least version 4, which brings restore data, whatever
    /// xdg-desktop-portal-wlr's own version: the restore data is kept here.
    #[zbus(property, name = "version")]
    async fn version(&self, #[zbus(connection)] connection: &zbus::Connection) -> u32 {
        Self::property(connection, "version").await.unwrap_or(0).max(PERSIST_VERSION)
    }
}

impl Tileception {
    pub fn serve_screen_cast<'a>(
        &mut self,
        bus: zbus::blocking::connection::Builder<'a>,
    ) -> zbus::Result<zbus::blocking::connection::Builder<'a>> {
        let (compositor, messages) = channel::channel();
        self.loop_handle
            .insert_source(messages, |event, _, state| match event {
                channel::Event::Msg(Message::Restore(target)) => {
                    state.sharing.restore = Some((target, std::time::Instant::now()));
                }
                channel::Event::Msg(Message::Forget) => state.sharing.restore = None,
                channel::Event::Closed => {}
            })
            .map_err(|err| zbus::Error::Failure(err.to_string()))?;
        let shared = Shared {
            sessions: Default::default(),
            unstarted: Default::default(),
            chosen: self.sharing.chosen.clone(),
            choosing: Default::default(),
            compositor,
            watching: Default::default(),
        };
        bus.serve_at(PATH, ScreenCast { shared })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn restore_data_names_what_was_chosen() {
        for target in [Target::Window("a1b2".into()), Target::Screen("HDMI-A-1".into())] {
            let data = restore_data(&target).unwrap();
            assert_eq!(data.value_signature().to_string(), "(suv)");
            assert_eq!(restored(&data), Some(target));
        }
        // xdg-desktop-portal-wlr's own, for screens, is left to it.
        let theirs: HashMap<&str, Value<'_>> = HashMap::from([("output_name", Value::from("eDP-1"))]);
        let theirs = Value::from(("wlroots", 1u32, Value::from(theirs)));
        assert_eq!(restored(&theirs), None);
    }
}
