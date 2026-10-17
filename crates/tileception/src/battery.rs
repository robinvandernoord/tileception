//! The laptop battery: its charge in the bar and on the focus border, alerts
//! as it runs low, hibernating before it runs out, and power profiles that
//! follow the charger. Without a battery (a desktop) none of this happens.
//!
//! A worker thread reads `/sys/class/power_supply` (some firmware answers
//! slowly) and does the D-Bus calls; what to do about a reading is decided
//! here, on the compositor thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Deserialize;
use smithay::reexports::calloop::{RegistrationToken, channel};
use zbus::zvariant::Value;

use crate::{Tileception, theme::Colour, theme::Theme};

const ROOT: &str = "/sys/class/power_supply";
const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
/// Plugging the charger in shows within this.
const POLL: Duration = Duration::from_secs(5);
/// Alerts while discharging, highest first: the level, how long the
/// notification stays (0 until dismissed), its urgency and text.
const ALERTS: [(u32, i32, u8, &str, &str); 3] = [
    (20, 6000, 0, "Battery low", "Consider connecting your charger."),
    (15, 8000, 1, "Battery very low", "Please connect your charger soon."),
    (10, 0, 2, "Battery critical", "Plug in now."),
];

/// The `[battery]` section.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BatteryConfig {
    /// Colour the focus border by charge.
    pub border: bool,
    /// The border while charging.
    pub charging: String,
    /// The border at or below each level, discharging.
    pub levels: Vec<BorderLevel>,
    /// Notify at 20, 15 and 10 percent.
    pub alerts: bool,
    /// Hibernate at this percentage; 0 never does.
    pub hibernate_at: u32,
    /// Performance on the charger, balanced on battery, power saver when low.
    pub power_profiles: bool,
    pub power_saver_at: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BorderLevel {
    pub at: u32,
    pub colour: String,
}

impl Default for BatteryConfig {
    fn default() -> Self {
        let level = |at, colour: &str| BorderLevel { at, colour: colour.into() };
        Self {
            border: true,
            charging: "#8fd65a".into(),
            levels: vec![level(35, "#ecd75a"), level(25, "warning"), level(15, "danger")],
            alerts: true,
            hibernate_at: 5,
            power_profiles: true,
            power_saver_at: 20,
        }
    }
}

/// The border colours, parsed against the theme.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BorderColours {
    charging: Option<Colour>,
    /// Lowest level first, so the first match is the tightest.
    levels: Vec<(u32, Colour)>,
}

impl BatteryConfig {
    pub fn colours(&self, theme: &Theme) -> Result<BorderColours, String> {
        if !self.border {
            return Ok(BorderColours::default());
        }
        let colour = |name: &str| match name.trim() {
            "accent" => Ok(theme.accent),
            "warning" => Ok(theme.warning),
            "danger" => Ok(theme.danger),
            hex => Colour::parse(hex).map_err(|e| format!("[battery] {e}")),
        };
        let mut levels =
            self.levels.iter().map(|l| Ok((l.at.min(100), colour(&l.colour)?))).collect::<Result<Vec<_>, String>>()?;
        levels.sort_by_key(|(at, _)| *at);
        Ok(BorderColours { charging: Some(colour(&self.charging)?), levels })
    }
}

impl BorderColours {
    /// The border for this reading; `None` keeps the accent.
    pub fn pick(&self, reading: &Reading) -> Option<Colour> {
        match reading.charge {
            Charge::Charging => self.charging,
            Charge::Plugged => None,
            Charge::Discharging => self.levels.iter().find(|(at, _)| reading.percent <= *at).map(|(_, c)| *c),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Discharging,
    Charging,
    /// On the charger but not charging: full, or held below full to spare it.
    Plugged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub percent: u32,
    pub charge: Charge,
    /// Until empty when discharging, until full when charging.
    pub remaining: Option<Duration>,
}

impl Reading {
    /// "82%, charging, full in 1 h 5 min"
    pub fn describe(&self) -> String {
        let state = match self.charge {
            Charge::Discharging => "on battery",
            Charge::Charging => "charging",
            Charge::Plugged => "plugged in",
        };
        let mut text = format!("{}%, {state}", self.percent);
        if let Some(remaining) = self.remaining.filter(|_| self.charge != Charge::Plugged) {
            let minutes = remaining.as_secs() / 60;
            let time = match (minutes / 60, minutes % 60) {
                (0, m) => format!("{m} min"),
                (h, 0) => format!("{h} h"),
                (h, m) => format!("{h} h {m} min"),
            };
            text +=
                &if self.charge == Charge::Charging { format!(", full in {time}") } else { format!(", {time} left") };
        }
        text
    }
}

/// One supply's files that matter.
#[derive(Default)]
struct Supply {
    kind: String,
    scope: String,
    present: bool,
    online: bool,
    status: String,
    capacity: Option<u64>,
    /// µWh or µAh, with the matching rate in µW or µA.
    now: Option<u64>,
    full: Option<u64>,
    rate: Option<u64>,
}

fn read_supply(dir: &Path) -> Supply {
    let text = |name: &str| std::fs::read_to_string(dir.join(name)).map(|s| s.trim().to_string()).unwrap_or_default();
    let number = |name: &str| text(name).parse::<i64>().ok().map(i64::unsigned_abs);
    let first = |names: [&str; 2]| names.into_iter().find_map(number);
    Supply {
        kind: text("type"),
        scope: text("scope"),
        present: text("present") != "0",
        online: text("online") == "1",
        status: text("status"),
        capacity: number("capacity"),
        now: first(["energy_now", "charge_now"]),
        full: first(["energy_full", "charge_full"]),
        rate: first(["power_now", "current_now"]),
    }
}

/// The system's batteries taken together. Peripherals (a mouse, a headset)
/// report `scope = Device` and are left out, as UPower does.
pub fn read(root: &Path) -> Option<Reading> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    let supplies: Vec<Supply> = entries.iter().map(|dir| read_supply(dir)).filter(|s| s.scope != "Device").collect();
    let batteries: Vec<&Supply> = supplies.iter().filter(|s| s.kind == "Battery" && s.present).collect();
    if batteries.is_empty() {
        return None;
    }
    let mains = supplies.iter().any(|s| s.kind != "Battery" && s.online);

    // Energy adds up across two batteries; percentages would not.
    let energy: Option<(u64, u64)> = batteries
        .iter()
        .map(|b| Some((b.now?, b.full.filter(|f| *f > 0)?)))
        .try_fold((0, 0), |(now, full), b| b.map(|(n, f)| (now + n, full + f)));
    let percent = match energy {
        Some((now, full)) => (now * 100 + full / 2) / full,
        None => {
            let known: Vec<u64> = batteries.iter().filter_map(|b| b.capacity).collect();
            if known.is_empty() {
                return None;
            }
            known.iter().sum::<u64>() / known.len() as u64
        }
    }
    .min(100) as u32;

    let has = |status: &str| batteries.iter().any(|b| b.status == status);
    let charge = if has("Charging") {
        Charge::Charging
    } else if has("Discharging") {
        Charge::Discharging
    } else if mains || has("Full") || has("Not charging") {
        Charge::Plugged
    } else {
        Charge::Discharging
    };

    let rate: u64 = batteries.iter().filter_map(|b| b.rate).sum();
    let remaining = energy.filter(|_| rate > 0).and_then(|(now, full)| {
        let left = match charge {
            Charge::Discharging => now,
            Charge::Charging => full.saturating_sub(now),
            Charge::Plugged => return None,
        };
        Some(Duration::from_secs(left * 3600 / rate))
    });
    Some(Reading { percent, charge, remaining })
}

/// Work for the worker thread.
enum Request {
    /// Read the batteries now, after waking from sleep.
    Read,
    Notify {
        summary: String,
        body: String,
        timeout: i32,
        urgency: u8,
    },
    /// Closes the last alert, once the charger is in.
    Withdraw,
    Hibernate,
    Profile(&'static str),
}

/// What the compositor knows about the battery and has done about it.
#[derive(Default)]
pub struct Battery {
    pub reading: Option<Reading>,
    requests: Option<mpsc::Sender<Option<Request>>>,
    thread: Option<JoinHandle<()>>,
    source: Option<RegistrationToken>,
    /// The lowest alert shown this discharge.
    alerted: Option<u32>,
    hibernating: bool,
    profile: Option<&'static str>,
    /// Alerts, hibernation and profiles only in a real session.
    act: bool,
}

impl Drop for Battery {
    fn drop(&mut self) {
        if let Some(requests) = self.requests.take() {
            let _ = requests.send(None);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Tileception {
    /// Starts reading the battery. `root` overrides `/sys/class/power_supply`
    /// for tests; `act` lets it notify, hibernate and change power profiles.
    pub fn start_battery(&mut self, root: Option<PathBuf>, act: bool) {
        let root = root.unwrap_or_else(|| PathBuf::from(ROOT));
        let (sender, receiver) = channel::channel::<Option<Reading>>();
        let token = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(reading) = event {
                state.battery_reading(reading);
            }
        });
        let token = match token {
            Ok(token) => token,
            Err(error) => return tracing::error!("could not watch the battery: {error}"),
        };
        let (requests, incoming) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("tileception-battery".into())
            .spawn(move || run(&root, &sender, &incoming))
            .map_err(|e| tracing::error!("could not start the battery worker: {e}"))
            .ok();
        self.battery.source = Some(token);
        self.battery.requests = Some(requests);
        self.battery.thread = thread;
        self.battery.act = act;
    }

    fn battery_request(&self, request: Request) {
        if let Some(requests) = &self.battery.requests {
            let _ = requests.send(Some(request));
        }
    }

    /// Reads the battery again now, as after waking.
    pub fn refresh_battery(&self) {
        self.battery_request(Request::Read);
    }

    /// The focus border's colour for the charge, when it is not the accent.
    pub fn battery_border(&self) -> Option<Colour> {
        self.config.battery.pick(self.battery.reading.as_ref()?)
    }

    fn battery_reading(&mut self, reading: Option<Reading>) {
        if self.battery.reading != reading {
            if self.battery.reading.is_none() && reading.is_some() {
                tracing::info!("battery: {}", reading.as_ref().map(Reading::describe).unwrap_or_default());
            }
            self.battery.reading = reading;
            self.schedule_render();
        }
        let Some(reading) = reading else { return };
        if self.battery.act {
            self.battery_alerts(&reading);
            self.battery_hibernate(&reading);
            self.battery_profile(&reading);
        }
    }

    fn battery_alerts(&mut self, reading: &Reading) {
        if reading.charge != Charge::Discharging {
            if self.battery.alerted.take().is_some() {
                self.battery_request(Request::Withdraw);
            }
            return;
        }
        let alert = ALERTS.iter().rev().find(|(at, ..)| reading.percent <= *at);
        let Some(&(at, timeout, urgency, summary, body)) = alert else {
            self.battery.alerted = None;
            return;
        };
        if self.config.file.battery.alerts && self.battery.alerted.is_none_or(|shown| at < shown) {
            self.battery.alerted = Some(at);
            let summary = format!("{summary} \u{b7} {}%", reading.percent);
            self.battery_request(Request::Notify { summary, body: body.into(), timeout, urgency });
        }
    }

    fn battery_hibernate(&mut self, reading: &Reading) {
        let at = self.config.file.battery.hibernate_at.min(50);
        let low = at > 0 && reading.charge == Charge::Discharging && reading.percent <= at;
        if !low {
            self.battery.hibernating = false;
        } else if !self.battery.hibernating {
            // Once per discharge: waking still on battery does not hibernate again.
            self.battery.hibernating = true;
            tracing::warn!("battery at {}%: hibernating", reading.percent);
            self.battery_request(Request::Hibernate);
        }
    }

    fn battery_profile(&mut self, reading: &Reading) {
        let config = &self.config.file.battery;
        if !config.power_profiles {
            self.battery.profile = None;
            return;
        }
        let profile = match reading.charge {
            Charge::Charging | Charge::Plugged => "performance",
            Charge::Discharging if reading.percent <= config.power_saver_at => "power-saver",
            Charge::Discharging => "balanced",
        };
        // Only on a change, so a profile picked by hand stays until the next.
        if self.battery.profile != Some(profile) {
            self.battery.profile = Some(profile);
            self.battery_request(Request::Profile(profile));
        }
    }

    /// `tileception msg battery`.
    pub fn battery_status(&self) -> String {
        let Some(reading) = &self.battery.reading else { return "no battery\n".into() };
        let border = match self.battery_border() {
            Some(colour) => {
                let [r, g, b, _] = colour.straight_rgba8();
                format!("#{r:02x}{g:02x}{b:02x}")
            }
            None => "accent".into(),
        };
        format!("{}\nborder: {border}\n", reading.describe())
    }
}

fn run(root: &Path, readings: &channel::Sender<Option<Reading>>, incoming: &mpsc::Receiver<Option<Request>>) {
    let mut bus = Bus::default();
    let mut last = None;
    let mut first = true;
    loop {
        let reading = read(root);
        if first || reading != last {
            if first && reading.is_none() {
                tracing::info!("no battery");
            }
            first = false;
            last = reading;
            if readings.send(reading).is_err() {
                return;
            }
        }
        match incoming.recv_timeout(POLL) {
            Ok(Some(Request::Read)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Some(request)) => bus.handle(request),
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// D-Bus connections, made when first needed and again after an error.
#[derive(Default)]
struct Bus {
    session: Option<zbus::blocking::Connection>,
    system: Option<zbus::blocking::Connection>,
    /// The alert on screen, replaced by the next.
    alert: u32,
}

impl Bus {
    fn handle(&mut self, request: Request) {
        let result = match request {
            Request::Read => Ok(()),
            Request::Notify { summary, body, timeout, urgency } => self.notify(&summary, &body, timeout, urgency),
            Request::Withdraw => self.withdraw(),
            Request::Hibernate => self.hibernate(),
            Request::Profile(profile) => self.profile(profile),
        };
        if let Err(error) = result {
            tracing::warn!("battery: {error}");
        }
    }

    fn session(&mut self) -> zbus::Result<&zbus::blocking::Connection> {
        if self.session.is_none() {
            self.session = Some(zbus::blocking::Connection::session()?);
        }
        Ok(self.session.as_ref().expect("connected just now"))
    }

    fn system(&mut self) -> zbus::Result<&zbus::blocking::Connection> {
        if self.system.is_none() {
            self.system = Some(zbus::blocking::Connection::system()?);
        }
        Ok(self.system.as_ref().expect("connected just now"))
    }

    fn notify(&mut self, summary: &str, body: &str, timeout: i32, urgency: u8) -> Result<(), String> {
        let replaces = self.alert;
        let hints: HashMap<&str, Value<'_>> = HashMap::from([("urgency", Value::U8(urgency))]);
        let reply = self.session().and_then(|bus| {
            bus.call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &("Battery", replaces, "battery-caution", summary, body, Vec::<&str>::new(), hints, timeout),
            )
        });
        match reply.and_then(|reply| reply.body().deserialize::<u32>()) {
            Ok(id) => {
                self.alert = id;
                Ok(())
            }
            Err(error) => {
                self.session = None;
                Err(format!("could not show an alert: {error}"))
            }
        }
    }

    fn withdraw(&mut self) -> Result<(), String> {
        let id = std::mem::take(&mut self.alert);
        if id == 0 {
            return Ok(());
        }
        self.session()
            .and_then(|bus| {
                bus.call_method(
                    Some("org.freedesktop.Notifications"),
                    "/org/freedesktop/Notifications",
                    Some("org.freedesktop.Notifications"),
                    "CloseNotification",
                    &(id,),
                )
            })
            .map(drop)
            .map_err(|error| {
                self.session = None;
                format!("could not close the alert: {error}")
            })
    }

    fn hibernate(&mut self) -> Result<(), String> {
        let result = self.system().and_then(|bus| {
            let manager = |method: &str, interactive: Option<bool>| match interactive {
                Some(interactive) => bus.call_method(Some(LOGIND), LOGIND_PATH, Some(MANAGER), method, &(interactive,)),
                None => bus.call_method(Some(LOGIND), LOGIND_PATH, Some(MANAGER), method, &()),
            };
            let can: String = manager("CanHibernate", None)?.body().deserialize()?;
            if can != "yes" {
                return Ok(Some(can));
            }
            manager("Hibernate", Some(false))?;
            Ok(None)
        });
        let problem = match result {
            Ok(None) => return Ok(()),
            Ok(Some(can)) => format!("hibernation is not available ({can})"),
            Err(error) => {
                self.system = None;
                format!("could not hibernate: {error}")
            }
        };
        tracing::error!("battery: {problem}");
        self.notify("Could not hibernate", "Plug in immediately: the battery is nearly empty.", 0, 2)?;
        Err(problem)
    }

    fn profile(&mut self, wanted: &'static str) -> Result<(), String> {
        use zbus::zvariant::OwnedValue;
        // power-profiles-daemon, under its current name and its old one.
        const DAEMONS: [(&str, &str); 2] = [
            ("org.freedesktop.UPower.PowerProfiles", "/org/freedesktop/UPower/PowerProfiles"),
            ("net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"),
        ];
        let bus = self.system().map_err(|e| e.to_string())?.clone();
        let get = |name: &str, path: &str, property: &str| -> zbus::Result<OwnedValue> {
            let reply =
                bus.call_method(Some(name), path, Some("org.freedesktop.DBus.Properties"), "Get", &(name, property))?;
            reply.body().deserialize::<OwnedValue>()
        };
        let Some((name, path, profiles)) =
            DAEMONS.iter().find_map(|(name, path)| Some((*name, *path, get(name, path, "Profiles").ok()?)))
        else {
            tracing::debug!("battery: no power-profiles-daemon");
            return Ok(());
        };
        let offered: Vec<HashMap<String, OwnedValue>> = profiles.try_into().unwrap_or_default();
        let has = |profile: &str| {
            offered.iter().any(|p| p.get("Profile").and_then(|v| <&str>::try_from(v).ok()) == Some(profile))
        };
        // Not every machine has a performance profile.
        let profile = if has(wanted) { wanted } else { "balanced" };
        let active: String = get(name, path, "ActiveProfile").and_then(|v| Ok(v.try_into()?)).unwrap_or_default();
        if active == profile {
            return Ok(());
        }
        bus.call_method(
            Some(name),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &(name, "ActiveProfile", Value::from(profile)),
        )
        .map_err(|e| format!("could not switch to the {profile} power profile: {e}"))?;
        tracing::info!("power profile: {profile}");
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, value) in files {
            std::fs::write(dir.join(file), format!("{value}\n")).unwrap();
        }
    }

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("tileception-battery-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn desktops_have_no_battery() {
        let root = root("desktop");
        assert_eq!(read(&root), None);
        // A wireless mouse is not the computer's battery.
        supply(&root, "hidpp_battery_0", &[("type", "Battery"), ("scope", "Device"), ("capacity", "40")]);
        assert_eq!(read(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_charge_and_time() {
        let root = root("laptop");
        supply(&root, "ADP1", &[("type", "Mains"), ("online", "0")]);
        supply(
            &root,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("capacity", "41"),
                ("energy_now", "20000000"),
                ("energy_full", "50000000"),
                ("power_now", "10000000"),
            ],
        );
        let reading = read(&root).unwrap();
        assert_eq!(
            reading,
            Reading { percent: 40, charge: Charge::Discharging, remaining: Some(Duration::from_secs(7200)) }
        );
        assert_eq!(reading.describe(), "40%, on battery, 2 h left");
        supply(&root, "BAT0", &[("status", "Not charging")]);
        supply(&root, "ADP1", &[("online", "1")]);
        assert_eq!(read(&root).unwrap().charge, Charge::Plugged);
        supply(&root, "BAT0", &[("status", "Charging"), ("power_now", "15000000")]);
        assert_eq!(read(&root).unwrap().describe(), "40%, charging, full in 2 h");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_batteries_add_up() {
        let root = root("two");
        let battery =
            |status, now, full| [("type", "Battery"), ("status", status), ("energy_now", now), ("energy_full", full)];
        supply(&root, "BAT0", &battery("Unknown", "10000000", "20000000"));
        supply(&root, "BAT1", &battery("Discharging", "50000000", "60000000"));
        let reading = read(&root).unwrap();
        assert_eq!((reading.percent, reading.charge), (75, Charge::Discharging));
        assert_eq!(reading.remaining, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn border_follows_charge() {
        let theme = Theme::default();
        let colours = BatteryConfig::default().colours(&theme).unwrap();
        let at = |percent, charge| colours.pick(&Reading { percent, charge, remaining: None });
        assert_eq!(at(80, Charge::Discharging), None);
        assert_eq!(at(36, Charge::Discharging), None);
        assert_eq!(at(35, Charge::Discharging), Some(Colour::parse("#ecd75a").unwrap()));
        assert_eq!(at(25, Charge::Discharging), Some(theme.warning));
        assert_eq!(at(3, Charge::Discharging), Some(theme.danger));
        assert_eq!(at(3, Charge::Charging), Some(Colour::parse("#8fd65a").unwrap()));
        assert_eq!(at(100, Charge::Plugged), None);
        let off = BatteryConfig { border: false, ..Default::default() }.colours(&theme).unwrap();
        assert_eq!(off.pick(&Reading { percent: 3, charge: Charge::Discharging, remaining: None }), None);
        let bad = BatteryConfig { charging: "green".into(), ..Default::default() };
        assert!(bad.colours(&theme).is_err());
        // As the config file writes it.
        let text = "[battery]\ncharging = \"accent\"\nlevels = [{ at = 50, colour = \"#ff0000\" }]\n";
        let config = crate::config::Config::parse(text).unwrap();
        let low = Reading { percent: 50, charge: Charge::Discharging, remaining: None };
        assert_eq!(config.battery.pick(&low), Some(Colour::parse("#ff0000").unwrap()));
        assert!(crate::config::Config::parse("[battery]\nlevels = [{ at = 50, colour = \"red\" }]").is_err());
    }
}
