//! Light or dark for applications. Tileception is a Settings backend for
//! xdg-desktop-portal and serves the `org.freedesktop.appearance` namespace:
//! the theme's colour scheme and accent colour. `tileception-portals.conf`
//! puts it ahead of the GTK backend, which answers everything else.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use zbus::zvariant::{OwnedValue, Value};

use crate::{
    theme::{ColourScheme, Theme},
    Tileception,
};

const NAME: &str = "org.freedesktop.impl.portal.desktop.tileception";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.impl.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";

/// What applications are told, taken from the theme.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Appearance {
    /// 0 no preference, 1 dark, 2 light.
    colour_scheme: u32,
    /// sRGB, from 0 to 1.
    accent: (f64, f64, f64),
}

impl Appearance {
    fn of(theme: &Theme) -> Appearance {
        let colour_scheme = match theme.colour_scheme {
            ColourScheme::Default => 0,
            ColourScheme::Dark => 1,
            ColourScheme::Light => 2,
        };
        let [r, g, b, _] = theme.accent.straight_rgba8().map(|c| f64::from(c) / 255.0);
        Appearance { colour_scheme, accent: (r, g, b) }
    }

    /// The namespace's keys and values.
    fn values(self) -> Vec<(&'static str, OwnedValue)> {
        let mut values = vec![("color-scheme", OwnedValue::from(self.colour_scheme))];
        // A structure of plain numbers always converts.
        if let Ok(accent) = OwnedValue::try_from(Value::from(self.accent)) {
            values.push(("accent-color", accent));
        }
        values
    }
}

/// The running service.
pub struct AppearanceService {
    connection: zbus::blocking::Connection,
    current: Arc<Mutex<Appearance>>,
}

fn lock(current: &Mutex<Appearance>) -> MutexGuard<'_, Appearance> {
    // The value is plain data, so a panic elsewhere cannot leave it half written.
    current.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a ReadAll pattern (a namespace, possibly ending in `*`, or empty
/// for all) selects `namespace`.
fn selects(pattern: &str, namespace: &str) -> bool {
    pattern.is_empty()
        || pattern == namespace
        || pattern.strip_suffix('*').is_some_and(|prefix| namespace.starts_with(prefix))
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.portal.Error")]
enum Error {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// The portal asks the next backend for settings this one lacks.
    NotFound(String),
}

struct Settings(Arc<Mutex<Appearance>>);

#[zbus::interface(name = "org.freedesktop.impl.portal.Settings")]
impl Settings {
    fn read_all(&self, namespaces: Vec<String>) -> HashMap<String, HashMap<String, OwnedValue>> {
        let mut all = HashMap::new();
        if namespaces.is_empty() || namespaces.iter().any(|p| selects(p, NAMESPACE)) {
            let values = lock(&self.0).values().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
            all.insert(NAMESPACE.to_string(), values);
        }
        all
    }

    fn read(&self, namespace: &str, key: &str) -> Result<OwnedValue, Error> {
        let found =
            (namespace == NAMESPACE).then(|| lock(&self.0).values().into_iter().find(|(k, _)| *k == key)).flatten();
        found.map(|(_, value)| value).ok_or_else(|| Error::NotFound(format!("{namespace} {key}")))
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

impl AppearanceService {
    fn start(bus: zbus::blocking::connection::Builder<'_>, theme: &Theme) -> zbus::Result<AppearanceService> {
        let current = Arc::new(Mutex::new(Appearance::of(theme)));
        let connection = bus.name(NAME)?.serve_at(PATH, Settings(current.clone()))?.build()?;
        Ok(AppearanceService { connection, current })
    }

    /// Announces the values that differ from what was served before.
    fn update(&self, theme: &Theme) {
        let now = Appearance::of(theme);
        let before = std::mem::replace(&mut *lock(&self.current), now);
        for ((key, value), (_, old)) in now.values().into_iter().zip(before.values()) {
            if value != old {
                let body = (NAMESPACE, key, &value);
                if let Err(err) = self.connection.emit_signal(None::<&str>, PATH, INTERFACE, "SettingChanged", &body) {
                    tracing::warn!("could not announce {key}: {err}");
                }
            }
        }
    }
}

impl Tileception {
    /// Starts serving the theme's appearance on the session bus. Done before
    /// the session environment is exported, so the portal finds it when it
    /// starts.
    pub fn start_appearance(&mut self) {
        match zbus::blocking::connection::Builder::session().and_then(|bus| AppearanceService::start(bus, &self.theme))
        {
            Ok(service) => self.appearance = Some(service),
            Err(err) => tracing::error!("could not serve the appearance settings: {err}"),
        }
    }

    /// Tells applications what changed with the theme.
    pub fn update_appearance(&self) {
        if let Some(service) = &self.appearance {
            service.update(&self.theme);
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

    use zbus::blocking::{connection::Builder, Connection, MessageIterator, Proxy};

    use super::*;

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

    fn settings(connection: &Connection) -> Proxy<'_> {
        Proxy::new(connection, NAME, PATH, INTERFACE).unwrap()
    }

    #[test]
    fn serves_the_portal_interface() {
        let bus = Bus::new();
        let mut theme = Theme::default();
        let service = AppearanceService::start(bus.builder(), &theme).unwrap();
        let client = bus.builder().build().unwrap();
        let proxy = settings(&client);

        assert_eq!(proxy.get_property::<u32>("version").unwrap(), 2);
        let scheme: OwnedValue = proxy.call("Read", &(NAMESPACE, "color-scheme")).unwrap();
        assert_eq!(scheme, OwnedValue::from(1u32));
        let all: HashMap<String, HashMap<String, OwnedValue>> =
            proxy.call("ReadAll", &(vec!["org.freedesktop.*"],)).unwrap();
        assert_eq!(all[NAMESPACE].len(), 2);
        let none: HashMap<String, HashMap<String, OwnedValue>> =
            proxy.call("ReadAll", &(vec!["org.gnome.*"],)).unwrap();
        assert!(none.is_empty());

        // Unknown settings fall through to the next backend.
        let err = proxy.call::<_, _, OwnedValue>("Read", &("org.gnome.desktop.interface", "gtk-theme")).unwrap_err();
        assert!(matches!(err, zbus::Error::MethodError(name, ..) if name == "org.freedesktop.portal.Error.NotFound"));

        // A theme change is announced, once per changed key.
        let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).member("SettingChanged").unwrap();
        let mut signals = MessageIterator::for_match_rule(rule.build(), &client, None).unwrap();
        theme.colour_scheme = ColourScheme::Light;
        service.update(&theme);
        let message = signals.next().unwrap().unwrap();
        let (namespace, key, value): (String, String, OwnedValue) = message.body().deserialize().unwrap();
        assert_eq!((namespace.as_str(), key.as_str(), value), (NAMESPACE, "color-scheme", OwnedValue::from(2u32)));
    }

    #[test]
    fn dark_by_default() {
        let values = Appearance::of(&Theme::default()).values();
        assert_eq!(values[0], ("color-scheme", OwnedValue::from(1u32)));
        assert_eq!(values[1].0, "accent-color");
    }

    #[test]
    fn namespace_patterns() {
        assert!(selects("", NAMESPACE));
        assert!(selects("org.freedesktop.appearance", NAMESPACE));
        assert!(selects("org.freedesktop.*", NAMESPACE));
        assert!(!selects("org.gnome.*", NAMESPACE));
    }
}
