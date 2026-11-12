//! ext-session-lock: screen lockers such as swaylock.
//!
//! While locked, only the locker's surface is drawn and gets input; key
//! bindings are off except switching consoles. If the locker dies, the screen
//! stays locked (black) until a new locker unlocks it, as the protocol asks.
//!
//! `[lock]` says what Tileception's own lock screen shows of notifications.

use smithay::{
    backend::renderer::{element::Kind, element::surface::render_elements_from_surface_tree, gles::GlesRenderer},
    delegate_session_lock,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Point, Size},
    wayland::session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
};

use serde::{Deserialize, Serialize};

use crate::{Tileception, render::OutputElement};

/// How much of an app's notifications Tileception's own lock screen shows,
/// least first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Shown {
    /// Nothing, not even in the count.
    None,
    /// Only counted.
    #[default]
    Count,
    /// The app's name and icon, with the content hidden.
    Apps,
    /// The summary, the body and any picture, but never the buttons.
    Full,
}

/// `[lock]`: what Tileception's own lock screen shows. External lockers draw
/// only themselves.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LockConfig {
    /// What every app's notifications show.
    pub notifications: Shown,
    /// Exceptions, by desktop entry (`.desktop` may be left off) or app name.
    pub notification_apps: std::collections::BTreeMap<String, Shown>,
}

impl LockConfig {
    /// The exception that names one of `apps` (an app's desktop entry, then
    /// its name), with the key it is written under.
    fn exception(&self, apps: &[&str]) -> Option<(&str, Shown)> {
        let bare = |name: &str| name.strip_suffix(".desktop").unwrap_or(name).to_string();
        apps.iter().filter(|app| !app.is_empty()).find_map(|app| {
            self.notification_apps
                .iter()
                .find(|(key, _)| bare(key) == bare(app))
                .map(|(key, shown)| (key.as_str(), *shown))
        })
    }

    /// What an app's notifications show by this config alone: its exception,
    /// else the default.
    pub fn shown(&self, apps: &[&str]) -> Shown {
        self.exception(apps).map_or(self.notifications, |(_, shown)| shown)
    }

    /// What the lock screen shows of an app's notifications: nothing when
    /// the administrator's greeter.toml hides the count, and only the count
    /// under Do not disturb.
    pub fn on_lock_screen(&self, apps: &[&str], allowed: bool, do_not_disturb: bool) -> Shown {
        match self.shown(apps) {
            _ if !allowed => Shown::None,
            shown if do_not_disturb => shown.min(Shown::Count),
            shown => shown,
        }
    }

    /// Whether the notification panel's "Show on lock screen" is on for an app.
    pub fn shows_in_full(&self, apps: &[&str]) -> bool {
        self.shown(apps) == Shown::Full
    }

    /// The exception that flips "Show on lock screen" for an app: removed
    /// (`None`) where the default already says it, else written.
    pub fn toggled(&self, apps: &[&str]) -> Option<Shown> {
        let wanted = if self.shows_in_full(apps) { Shown::Count } else { Shown::Full };
        match (self.notifications, wanted) {
            (Shown::Full, Shown::Full) => None,
            (Shown::Full, _) => Some(wanted),
            (_, Shown::Full) => Some(Shown::Full),
            _ => None,
        }
    }

    /// The keys an app's exceptions may be under, the one to write first.
    pub fn app_keys<'lock>(&'lock self, apps: &[&'lock str]) -> Vec<&'lock str> {
        let mut keys: Vec<&str> = apps.iter().copied().filter(|app| !app.is_empty()).collect();
        if let Some((key, _)) = self.exception(apps)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
        keys
    }
}

#[derive(Debug, Default)]
pub enum LockState {
    #[default]
    Unlocked,
    BuiltIn,
    /// `--greeter`: the login screen, which nothing unlocks.
    Greeter,
    Locked {
        surfaces: std::collections::BTreeMap<String, LockSurface>,
    },
}

impl SessionLockHandler for Tileception {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if self.is_locked() {
            return;
        }
        tracing::info!("session locked");
        self.lock = LockState::Locked { surfaces: Default::default() };
        self.launcher.open = false;
        self.settings_release();
        self.power_menu = None;
        self.selection.anchor = None;
        self.pending_press = None;
        self.tap_armed = None;
        self.tap_fired.clear();
        self.move_drag = None;
        self.resize_drag = None;
        self.gap_drag = None;
        // Nothing but black is drawn from the next frame on.
        confirmation.lock();
        self.force_keyboard_focus_sync();
        self.handoff_lock_changed();
        self.sleep_check(false);
    }

    fn unlock(&mut self) {
        if self.draws_lock_screen() {
            return;
        }
        tracing::info!("session unlocked");
        self.lock = LockState::Unlocked;
        self.force_keyboard_focus_sync();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let Some(output) = smithay::output::Output::from_resource(&wl_output) else { return };
        let Some(rect) = self.with_output(&output, |state| state.output_rect()) else { return };
        surface.with_pending_state(|state| state.size = Some((rect.w as u32, rect.h as u32).into()));
        surface.send_configure();
        if let LockState::Locked { surfaces } = &mut self.lock {
            surfaces.insert(output.name(), surface);
        }
        self.force_keyboard_focus_sync();
    }
}

delegate_session_lock!(Tileception);

impl Tileception {
    /// Unlocks without the locker's consent; see `Command::Unlock`.
    pub fn force_unlock(&mut self) {
        if self.is_locked() && !matches!(self.lock, LockState::Greeter) {
            tracing::warn!("session unlocked by command");
            self.lock = LockState::Unlocked;
            self.end_greeter_keymap();
            self.greeter = None;
            self.force_keyboard_focus_sync();
        }
    }

    pub fn is_locked(&self) -> bool {
        !matches!(self.lock, LockState::Unlocked)
    }

    /// Tileception draws the lock screen itself: the built-in locker or the greeter.
    pub fn draws_lock_screen(&self) -> bool {
        matches!(self.lock, LockState::BuiltIn | LockState::Greeter)
    }

    pub fn lock_surface(&self) -> Option<&WlSurface> {
        match &self.lock {
            LockState::Locked { surfaces } => {
                self.output.as_ref().and_then(|output| surfaces.get(&output.name())).map(|surface| surface.wl_surface())
            }
            _ => None,
        }
    }

    /// The lock screen, topmost first: its panels, then the backdrop they
    /// stand on (which frost blurs). A locker program's surface is all panel.
    pub fn lock_elements(&self, renderer: &mut GlesRenderer) -> (Vec<OutputElement>, Vec<OutputElement>) {
        match self.lock {
            LockState::BuiltIn | LockState::Greeter if self.is_active_output() => {
                return self.greeter_elements(renderer);
            }
            _ => {}
        }
        let Some(surface) = self.lock_surface() else { return Default::default() };
        let surfaces = render_elements_from_surface_tree(renderer, surface, (0, 0), 1.0, 1.0, Kind::Unspecified);
        (crate::render::enlarged(surfaces, (0, 0).into(), self.scale).collect(), Vec::new())
    }

    pub fn lock_surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let surface = self.lock_surface()?;
        smithay::desktop::utils::under_from_surface_tree(surface, pos, (0, 0), smithay::desktop::WindowSurfaceType::ALL)
            .map(|(surface, location)| (surface, location.to_f64()))
    }

    /// Keeps the lock surface the size of the output.
    pub fn resize_lock_surface(&self) {
        if let LockState::Locked { surfaces } = &self.lock
            && let Some(surface) = self.output.as_ref().and_then(|output| surfaces.get(&output.name()))
        {
            let rect = self.output_rect();
            let size: Size<u32, Logical> = (rect.w as u32, rect.h as u32).into();
            surface.with_pending_state(|state| state.size = Some(size));
            surface.send_configure();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> LockConfig {
        toml::from_str(text).expect("parses")
    }

    const CALENDAR: &[&str] = &["org.gnome.Calendar", "Calendar"];
    const CHAT: &[&str] = &["chat", "Chat"];

    #[test]
    fn the_default_is_a_count_for_everyone() {
        let lock = LockConfig::default();
        assert_eq!(lock.shown(CALENDAR), Shown::Count);
        assert_eq!(lock.on_lock_screen(CALENDAR, true, false), Shown::Count);
        assert!(toml::from_str::<LockConfig>("notifications = \"loud\"").is_err());
        assert!(toml::from_str::<LockConfig>("sounds = true").is_err());
    }

    #[test]
    fn exceptions_override_the_default_by_entry_or_name() {
        let opt_in = config("notifications = \"count\"\n[notification_apps]\n\"org.gnome.Calendar\" = \"full\"\n");
        assert_eq!(opt_in.shown(CALENDAR), Shown::Full);
        assert_eq!(opt_in.shown(CHAT), Shown::Count);
        let opt_out =
            config("notifications = \"full\"\n[notification_apps]\nChat = \"none\"\n\"x.desktop\" = \"apps\"\n");
        assert_eq!(opt_out.shown(CALENDAR), Shown::Full);
        assert_eq!(opt_out.shown(CHAT), Shown::None, "by app name when the entry has none");
        assert_eq!(opt_out.shown(&["x", "X"]), Shown::Apps, ".desktop may be left off");
        assert_eq!(opt_out.shown(&["", ""]), Shown::Full);
    }

    #[test]
    fn the_administrator_and_do_not_disturb_limit_it() {
        let lock = config("notifications = \"full\"\n[notification_apps]\nChat = \"none\"\n");
        assert_eq!(lock.on_lock_screen(CALENDAR, false, false), Shown::None);
        assert_eq!(lock.on_lock_screen(CALENDAR, true, true), Shown::Count);
        assert_eq!(lock.on_lock_screen(CHAT, true, true), Shown::None);
        assert_eq!(lock.on_lock_screen(CALENDAR, true, false), Shown::Full);
    }

    #[test]
    fn the_panel_toggle_writes_the_smallest_exception() {
        // A low default: on writes "full", off removes it again.
        let low = LockConfig::default();
        assert!(!low.shows_in_full(CALENDAR));
        assert_eq!(low.toggled(CALENDAR), Some(Shown::Full));
        let opted_in = config("[notification_apps]\nCalendar = \"full\"\n");
        assert!(opted_in.shows_in_full(CALENDAR));
        assert_eq!(opted_in.toggled(CALENDAR), None);
        assert_eq!(opted_in.app_keys(CALENDAR), ["org.gnome.Calendar", "Calendar"]);
        // "full" for everyone: off writes "count", on removes it.
        let high = config("notifications = \"full\"\n");
        assert_eq!(high.toggled(CALENDAR), Some(Shown::Count));
        let off = config("notifications = \"full\"\n[notification_apps]\n\"org.gnome.Calendar.desktop\" = \"apps\"\n");
        assert_eq!(off.toggled(CALENDAR), None);
        assert_eq!(off.app_keys(CALENDAR), ["org.gnome.Calendar", "Calendar", "org.gnome.Calendar.desktop"]);
        // An "apps" exception under a low default goes up to "full".
        let apps = config("[notification_apps]\nCalendar = \"apps\"\n");
        assert_eq!(apps.toggled(CALENDAR), Some(Shown::Full));
    }
}
