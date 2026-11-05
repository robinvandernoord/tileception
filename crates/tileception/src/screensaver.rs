//! `org.freedesktop.ScreenSaver` on the session bus, for applications that
//! hold idle off there instead of through the Wayland idle-inhibit protocol:
//! X11 programs such as VLC 3, and browsers. While any inhibitor is held the
//! screen neither locks nor turns off. An inhibitor ends with UnInhibit or
//! when the application leaves the bus.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use zbus::blocking::{Connection, connection::Builder};

const NAME: &str = "org.freedesktop.ScreenSaver";
/// Where applications look: freedesktop's path, and KDE's that VLC uses.
const PATHS: [&str; 2] = ["/org/freedesktop/ScreenSaver", "/ScreenSaver"];

#[derive(Default)]
struct Held {
    /// By cookie: the holder's unique bus name and application.
    inhibitors: HashMap<u32, (String, String)>,
    next_cookie: u32,
    /// SimulateUserActivity was called since the last look.
    poked: bool,
}

/// What the bus service shares with the idle timer.
#[derive(Clone, Default)]
pub struct Inhibitors(Arc<Mutex<Held>>);

impl Inhibitors {
    fn lock(&self) -> MutexGuard<'_, Held> {
        // Plain data, so a panic elsewhere cannot leave it half written.
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Some application holds idle off.
    pub fn any(&self) -> bool {
        !self.lock().inhibitors.is_empty()
    }

    /// Whether an application asked to count as activity since the last call.
    pub fn take_poke(&self) -> bool {
        std::mem::take(&mut self.lock().poked)
    }

    fn inhibit(&self, sender: String, application: &str) -> u32 {
        let mut held = self.lock();
        held.next_cookie = held.next_cookie.wrapping_add(1).max(1);
        while held.inhibitors.contains_key(&held.next_cookie) {
            held.next_cookie = held.next_cookie.wrapping_add(1).max(1);
        }
        let cookie = held.next_cookie;
        held.inhibitors.insert(cookie, (sender, application.to_owned()));
        cookie
    }

    fn uninhibit(&self, sender: &str, cookie: u32) -> Option<String> {
        let mut held = self.lock();
        let owned = held.inhibitors.get(&cookie).is_some_and(|(holder, _)| holder == sender);
        owned.then(|| held.inhibitors.remove(&cookie)).flatten().map(|(_, application)| application)
    }

    /// Drops what a client that left the bus held.
    fn forget(&self, sender: &str) -> Vec<String> {
        let mut gone = Vec::new();
        self.lock().inhibitors.retain(|_, (holder, application)| {
            let keep = holder != sender;
            if !keep {
                gone.push(std::mem::take(application));
            }
            keep
        });
        gone
    }
}

struct ScreenSaver(Inhibitors);

fn sender(header: &zbus::message::Header<'_>) -> zbus::fdo::Result<String> {
    header.sender().map(ToString::to_string).ok_or_else(|| zbus::fdo::Error::Failed("missing sender".into()))
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaver {
    fn inhibit(
        &self,
        application_name: &str,
        reason_for_inhibit: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<u32> {
        let cookie = self.0.inhibit(sender(&header)?, application_name);
        tracing::info!("{application_name} holds idle off: {reason_for_inhibit}");
        Ok(cookie)
    }

    fn un_inhibit(&self, cookie: u32, #[zbus(header)] header: zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        if let Some(application) = self.0.uninhibit(&sender(&header)?, cookie) {
            tracing::info!("{application} lets idle go");
        }
        Ok(())
    }

    fn simulate_user_activity(&self) {
        self.0.lock().poked = true;
    }

    fn get_active(&self) -> bool {
        false
    }
}

/// The running service; the connection lives as long as this does.
pub struct ScreenSaverService {
    _connection: Connection,
}

impl ScreenSaverService {
    fn start(bus: Builder<'_>, inhibitors: &Inhibitors) -> zbus::Result<ScreenSaverService> {
        let bus = PATHS.iter().try_fold(bus, |bus, path| bus.serve_at(*path, ScreenSaver(inhibitors.clone())))?;
        let connection = bus.build()?;
        // Listening before taking the name, so no holder can leave unseen.
        let departures = zbus::blocking::fdo::DBusProxy::new(&connection)?.receive_name_owner_changed()?;
        let watched = inhibitors.clone();
        std::thread::Builder::new().name("tileception-screensaver".into()).spawn(move || {
            for change in departures {
                let Ok(args) = change.args() else { continue };
                if args.new_owner().is_none() && args.name().starts_with(':') {
                    for application in watched.forget(args.name()) {
                        tracing::info!("{application} left; it no longer holds idle off");
                    }
                }
            }
        })?;
        connection.request_name(NAME)?;
        Ok(ScreenSaverService { _connection: connection })
    }
}

impl crate::Tileception {
    /// Serves `org.freedesktop.ScreenSaver`, unless something else already does.
    pub fn start_screensaver(&mut self) {
        let inhibitors = self.idle.bus_inhibitors.clone();
        match Builder::session().and_then(|bus| ScreenSaverService::start(bus, &inhibitors)) {
            Ok(service) => self.screensaver = Some(service),
            Err(err) => tracing::warn!("could not serve {NAME}; X11 players cannot keep the screen on: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_holder_releases_and_leaving_releases_all() {
        let inhibitors = Inhibitors::default();
        let first = inhibitors.inhibit(":1.5".into(), "vlc");
        let second = inhibitors.inhibit(":1.5".into(), "vlc");
        let other = inhibitors.inhibit(":1.9".into(), "firefox");
        assert!(first != second && second != other);
        assert_eq!(inhibitors.uninhibit(":1.9", first), None);
        assert_eq!(inhibitors.uninhibit(":1.5", first).as_deref(), Some("vlc"));
        assert_eq!(inhibitors.forget(":1.5"), vec!["vlc".to_owned()]);
        assert!(inhibitors.any());
        assert_eq!(inhibitors.uninhibit(":1.9", other).as_deref(), Some("firefox"));
        assert!(!inhibitors.any());
    }

    #[test]
    fn a_poke_is_taken_once() {
        let inhibitors = Inhibitors::default();
        inhibitors.lock().poked = true;
        assert!(inhibitors.take_poke());
        assert!(!inhibitors.take_poke());
    }
}
