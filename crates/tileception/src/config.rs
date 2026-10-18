//! `~/.config/tileception/config.toml`: settings and key bindings.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::Deserialize;
use smithay::{
    input::keyboard::{ModifiersState, xkb},
    reexports::calloop::{
        LoopHandle,
        timer::{TimeoutAction, Timer},
    },
};

use crate::{
    Tileception,
    commands::{self, Command},
};

pub const DEFAULT: &str = include_str!("default-config.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct ConfigFile {
    pub general: General,
    pub theme: crate::theme::ThemeFile,
    pub keyboard: Keyboard,
    pub touchpad: crate::gestures::TouchpadConfig,
    pub idle: crate::idle::IdleConfig,
    pub media: crate::media::MediaConfig,
    pub battery: crate::battery::BatteryConfig,
    pub shell: crate::shell::ShellConfig,
    pub duck: crate::duck::DuckConfig,
    pub rules: Vec<crate::rules::WindowRule>,
    /// Per screen, by connector name ("*" for any).
    pub outputs: BTreeMap<String, crate::scale::OutputConfig>,
    /// Extra arguments for programs the launcher starts, by desktop file name.
    pub launch_args: BTreeMap<String, Vec<String>>,
    pub binds: Option<BTreeMap<String, BindValue>>,
}

/// One command, or several run in order.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BindValue {
    One(String),
    Many(Vec<String>),
}

impl BindValue {
    fn lines(&self) -> Vec<&str> {
        match self {
            BindValue::One(s) => vec![s.as_str()],
            BindValue::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub terminal: Option<String>,
    pub gap: f64,
    pub animation_ms: f64,
    pub drag_modifier: String,
    pub share_removed_space: bool,
    /// No longer used: floating windows hang above the layout. Still
    /// accepted so older config files keep loading.
    pub sole_float_takes_desktop: bool,
    pub tab_height: f64,
    /// Shell command lines run once at startup.
    pub autostart: Vec<String>,
    /// Command for the `lock` action; `{wallpaper}` is replaced by the
    /// wallpaper's path. Unset: the built-in password lock screen.
    pub lock_command: Option<String>,
    /// Experimental app-matched layout restoration; opt in explicitly.
    pub restore_layout: bool,
    /// Enable suspend/resume through the power menu; enabled by default.
    pub suspend_enabled: bool,
    /// Start from the built-in bindings, with `[binds]` adding to them and
    /// overriding them. Off: `[binds]` is the complete list.
    pub default_binds: bool,
    /// Show the launcher, with a clock, on an empty desktop.
    pub welcome: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            terminal: None,
            gap: 12.0,
            animation_ms: 280.0,
            drag_modifier: "Alt".into(),
            share_removed_space: true,
            sole_float_takes_desktop: true,
            tab_height: 28.0,
            autostart: Vec::new(),
            lock_command: None,
            restore_layout: false,
            suspend_enabled: true,
            default_binds: true,
            welcome: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keyboard {
    pub layout: Option<String>,
    pub variant: Option<String>,
    pub options: Option<String>,
    pub model: Option<String>,
    pub repeat_delay: Option<i32>,
    pub repeat_rate: Option<i32>,
}

impl Keyboard {
    /// The xkb model, layout, variant and options. Without a layout it is US
    /// international: AltGr types accents and dead keys, ' and " stay plain.
    pub fn names(&self) -> (String, String, String, Option<String>) {
        let (layout, variant) = match &self.layout {
            Some(layout) => (layout.clone(), self.variant.clone().unwrap_or_default()),
            None => ("us".into(), self.variant.clone().unwrap_or_else(|| "altgr-intl".into())),
        };
        (self.model.clone().unwrap_or_default(), layout, variant, self.options.clone())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub logo: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

impl Mods {
    pub fn from_state(state: &ModifiersState) -> Mods {
        Mods { logo: state.logo, alt: state.alt, ctrl: state.ctrl, shift: state.shift }
    }

    /// Whether `self` (what is held) includes the single modifier `m`.
    pub fn has(&self, m: Mods) -> bool {
        (!m.logo || self.logo) && (!m.alt || self.alt) && (!m.ctrl || self.ctrl) && (!m.shift || self.shift)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bind {
    pub mods: Mods,
    /// Lower-case keysym, compared against the unmodified key.
    pub key: u32,
    pub commands: Vec<Command>,
}

/// The parsed, ready-to-use configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub file: ConfigFile,
    pub binds: Vec<Bind>,
    pub drag_mod: Mods,
    pub theme: crate::theme::Theme,
    pub battery: crate::battery::BorderColours,
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let file: ConfigFile = toml::from_str(text).map_err(|e| e.to_string())?;
        let defaults: ConfigFile = toml::from_str(DEFAULT).expect("built-in config parses");
        let mut binds: Vec<Bind> = Vec::new();
        let mut errors = Vec::new();
        // The built-in bindings, then yours on top: a key you list replaces
        // its default, and "none" removes it.
        let mut tables = Vec::new();
        if file.general.default_binds || file.binds.is_none() {
            tables.push(defaults.binds.unwrap_or_default());
        }
        tables.extend(file.binds.clone());
        for (combo, value) in tables.iter().flatten() {
            let combo_keys = match parse_combo(combo) {
                Ok(keys) => keys,
                Err(e) => {
                    errors.push(format!("{combo:?}: {e}"));
                    continue;
                }
            };
            binds.retain(|b| (b.mods, b.key) != combo_keys);
            if matches!(value, BindValue::One(s) if s.trim() == "none") {
                continue;
            }
            let parsed: Result<Vec<Command>, String> = value
                .lines()
                .into_iter()
                .map(|line| {
                    let line = match line.trim() {
                        "spawn fuzzel" => "launcher",
                        "spawn tileception-power" => "power-menu",
                        "spawn swaync-client -t" => "notifications",
                        other => other,
                    };
                    commands::parse(line)
                })
                .collect();
            match parsed {
                Ok(commands) => binds.push(Bind { mods: combo_keys.0, key: combo_keys.1, commands }),
                Err(e) => errors.push(format!("{combo:?}: {e}")),
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        let drag_mod = parse_modifier(&file.general.drag_modifier)
            .ok_or_else(|| format!("unknown drag_modifier {:?}", file.general.drag_modifier))?;
        let theme = crate::theme::Theme::from_file(&file.theme).map_err(|e| format!("[theme] {e}"))?;
        file.shell.validate()?;
        crate::rules::validate(&file.rules)?;
        crate::scale::validate(&file.outputs)?;
        file.touchpad.validate()?;
        let battery = file.battery.colours(&theme)?;
        Ok(Config { file, binds, drag_mod, theme, battery })
    }

    pub fn default_config() -> Config {
        Config::parse(DEFAULT).expect("built-in config parses")
    }

    /// The command bound to a key press, if any. `key` is the unmodified
    /// keysym; modifiers must match exactly.
    pub fn lookup(&self, mods: Mods, key: u32) -> Option<&[Command]> {
        let key = lower(key);
        self.binds.iter().find(|b| b.mods == mods && b.key == key).map(|b| b.commands.as_slice())
    }

    /// Extra launch arguments for a desktop entry id (`spotify.desktop`), from
    /// `[launch_args]`; the key may leave off `.desktop`.
    pub fn launch_args(&self, id: &str) -> Vec<String> {
        let name = id.strip_suffix(".desktop").unwrap_or(id);
        self.file
            .launch_args
            .iter()
            .find(|(key, _)| key.strip_suffix(".desktop").unwrap_or(key) == name)
            .map(|(_, args)| args.clone())
            .unwrap_or_default()
    }

    pub fn world_settings(&self) -> tileception_core::Settings {
        let g = &self.file.general;
        tileception_core::Settings {
            gap: g.gap.max(0.0),
            animation: (g.animation_ms / 1000.0).max(0.0),
            share_removed_space: g.share_removed_space,
            tab_height: g.tab_height.max(0.0),
        }
    }
}

fn parse_modifier(name: &str) -> Option<Mods> {
    let mut m = Mods::default();
    match name.to_ascii_lowercase().as_str() {
        "super" | "logo" | "mod4" | "win" | "meta" => m.logo = true,
        "alt" | "mod1" => m.alt = true,
        "ctrl" | "control" => m.ctrl = true,
        "shift" => m.shift = true,
        _ => return None,
    }
    Some(m)
}

/// Parses `Super+Shift+T` into modifiers and a lower-case keysym.
pub fn parse_combo(combo: &str) -> Result<(Mods, u32), String> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    if let [only] = parts[..] {
        // A modifier on its own: pressed and let go with nothing else.
        if let Some(m) = parse_modifier(only) {
            return Ok((m, TAP));
        }
    }
    let (key, mods) = parts.split_last().ok_or("empty key binding")?;
    let mut all = Mods::default();
    for m in mods {
        let one = parse_modifier(m).ok_or_else(|| format!("unknown modifier {m:?}"))?;
        all.logo |= one.logo;
        all.alt |= one.alt;
        all.ctrl |= one.ctrl;
        all.shift |= one.shift;
    }
    if let Some(wheel) = wheel_key(key) {
        return Ok((all, wheel));
    }
    if let Some(gesture) = crate::gestures::parse_key(key) {
        return gesture.map(|g| (all, g));
    }
    let mut sym = xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS);
    if sym.raw() == 0 {
        sym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
    }
    if sym.raw() == 0 {
        return Err(format!("unknown key {key:?}"));
    }
    Ok((all, lower(sym.raw())))
}

/// Pseudo keysyms for the mouse wheel, outside the range xkb uses.
pub const WHEEL_UP: u32 = 0x7f00_0001;
pub const WHEEL_DOWN: u32 = 0x7f00_0002;
pub const WHEEL_LEFT: u32 = 0x7f00_0003;
pub const WHEEL_RIGHT: u32 = 0x7f00_0004;
/// Pseudo keysym for tapping a modifier on its own.
pub const TAP: u32 = 0x7f00_0010;

/// The modifier a key is, if it is one.
pub fn modifier_of(sym: u32) -> Option<Mods> {
    let mut m = Mods::default();
    match sym {
        0xffeb | 0xffec => m.logo = true,  // Super_L, Super_R
        0xffe9 | 0xffea => m.alt = true,   // Alt_L, Alt_R
        0xffe3 | 0xffe4 => m.ctrl = true,  // Control_L, Control_R
        0xffe1 | 0xffe2 => m.shift = true, // Shift_L, Shift_R
        _ => return None,
    }
    Some(m)
}

fn wheel_key(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "wheelup" | "mouse_up" => Some(WHEEL_UP),
        "wheeldown" | "mouse_down" => Some(WHEEL_DOWN),
        "wheelleft" | "mouse_left" => Some(WHEEL_LEFT),
        "wheelright" | "mouse_right" => Some(WHEEL_RIGHT),
        _ => None,
    }
}

/// Lower-cases Latin letter keysyms, so `Super+T` matches the `t` key.
fn lower(sym: u32) -> u32 {
    match sym {
        0x41..=0x5a => sym + 0x20,
        0xc0..=0xde if sym != 0xd7 => sym + 0x20,
        _ => sym,
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("tileception").join("config.toml")
}

/// Reads the config file, writing the defaults there first if it is missing.
pub fn load(path: &Path) -> Config {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(path, DEFAULT) {
            Ok(()) => tracing::info!("wrote default config to {}", path.display()),
            Err(err) => tracing::warn!("could not write {}: {err}", path.display()),
        }
    }
    match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| Config::parse(&t)) {
        Ok(config) => config,
        Err(err) => {
            tracing::error!("config {}: {err}; using defaults", path.display());
            Config::default_config()
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Reloads the config whenever the file changes.
pub fn watch(handle: &LoopHandle<'static, Tileception>, path: PathBuf) {
    let mut last = modified(&path);
    let _ = handle.insert_source(Timer::from_duration(Duration::from_secs(1)), move |_, _, state| {
        let now = modified(&path);
        if now != last {
            last = now;
            if let Err(err) = state.reload_config() {
                tracing::error!("config: {err}");
            }
        }
        TimeoutAction::ToDuration(Duration::from_secs(1))
    });
}

impl Tileception {
    pub fn reload_config(&mut self) -> Result<(), String> {
        let path = path();
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let config = Config::parse(&text)?;
        self.apply_config(config);
        tracing::info!("config reloaded");
        Ok(())
    }

    pub fn apply_config(&mut self, config: Config) {
        self.world.settings = config.world_settings();
        if let Some(keyboard) = self.seat.get_keyboard() {
            let k = &config.file.keyboard;
            keyboard.change_repeat_info(k.repeat_rate.unwrap_or(30), k.repeat_delay.unwrap_or(400));
        }
        if let Some(t) = &config.file.general.terminal {
            self.terminal = t.clone();
        }
        if self.theme != config.theme {
            self.theme = config.theme.clone();
            self.update_appearance();
            self.text.borrow_mut().clear();
            self.wallpaper = None;
            self.wallpaper_failed = None;
        }
        let old_shell = std::mem::replace(&mut self.config, config).file.shell;
        self.apply_shell(&old_shell);
        self.configure_touchpads();
        // Also lays everything out again.
        self.apply_output_scale();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;
    use crate::input::Action;

    #[test]
    fn keyboard_defaults_to_us_international() {
        let names = |k: Keyboard| {
            let (_, layout, variant, _) = k.names();
            (layout, variant)
        };
        assert_eq!(names(Keyboard::default()), ("us".into(), "altgr-intl".into()));
        let (model, layout, variant, options) = Keyboard::default().names();
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap =
            xkb::Keymap::new_from_names(&context, "", &model, &layout, &variant, options, xkb::KEYMAP_COMPILE_NO_FLAGS)
                .expect("US international compiles");
        // AltGr+' is a dead acute.
        let quote = keymap.key_by_name("AC11").expect("apostrophe key");
        let dead_acute = xkb::keysym_from_name("dead_acute", xkb::KEYSYM_NO_FLAGS);
        assert_eq!(keymap.key_get_syms_by_level(quote, 0, 2), [dead_acute]);
        let nl = Keyboard { layout: Some("nl".into()), ..Default::default() };
        assert_eq!(names(nl), ("nl".into(), String::new()));
    }

    #[test]
    fn default_config_parses() {
        let c = Config::default_config();
        assert!(c.binds.len() > 30);
        let t = xkb::keysym_from_name("t", xkb::KEYSYM_NO_FLAGS).raw();
        let logo = Mods { logo: true, ..Default::default() };
        assert_eq!(c.lookup(logo, t), Some(&[Command::Action(Action::Spawn)][..]));
        let one = xkb::keysym_from_name("1", xkb::KEYSYM_NO_FLAGS).raw();
        let logo_shift = Mods { logo: true, shift: true, ..Default::default() };
        assert_eq!(c.lookup(logo_shift, one), Some(&[Command::Action(Action::SaveView(1))][..]));
        assert_eq!(c.lookup(logo, WHEEL_UP), Some(&[Command::Action(Action::StepInPointer)][..]));
        assert_eq!(c.lookup(Mods::default(), t), None);
    }

    #[test]
    fn combos_and_errors() {
        let (m, k) = parse_combo("Ctrl+Alt+Page_Up").unwrap();
        assert!(m.ctrl && m.alt && !m.logo);
        assert_eq!(k, xkb::keysym_from_name("Page_Up", xkb::KEYSYM_NO_FLAGS).raw());
        assert!(parse_combo("Hyper+T").is_err());
        assert!(parse_combo("Super+NoSuchKey").is_err());
        assert!(Config::parse("[binds]\n\"Super+T\" = \"fly\"").is_err());
        assert!(Config::parse("[general]\nbogus = 1").is_err());
    }

    #[test]
    fn modifier_taps() {
        let c = Config::parse("[binds]\n\"Super\" = \"spawn fuzzel\"").unwrap();
        let logo = Mods { logo: true, ..Default::default() };
        assert_eq!(c.lookup(logo, TAP), Some(&[Command::Action(crate::input::Action::Launcher)][..]));
        assert_eq!(modifier_of(0xffeb), Some(logo));
    }

    #[test]
    fn several_commands_per_key() {
        let c = Config::parse("[binds]\n\"Super+Escape\" = [\"reload\", \"spawn notify-send hi\"]").unwrap();
        let esc = xkb::keysym_from_name("Escape", xkb::KEYSYM_NO_FLAGS).raw();
        let logo = Mods { logo: true, ..Default::default() };
        assert_eq!(c.lookup(logo, esc).map(|c| c.len()), Some(2));
    }

    #[test]
    fn user_binds_add_to_and_override_the_defaults() {
        let defaults = Config::default_config().binds.len();
        let logo = Mods { logo: true, ..Default::default() };
        let key = |name| xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS).raw();
        let c = Config::parse("[binds]\n\"Super+Return\" = \"spawn foot\"").unwrap();
        assert_eq!(c.binds.len(), defaults + 1, "a new key joins the defaults");
        assert_eq!(c.lookup(logo, key("h")), Some(&[Command::Action(crate::input::Action::Minimize)][..]));
        // The same key written another way still replaces its default.
        let c = Config::parse("[binds]\n\"Shift+Super+h\" = \"close\"").unwrap();
        let logo_shift = Mods { logo: true, shift: true, ..Default::default() };
        assert_eq!(c.binds.len(), defaults);
        assert_eq!(c.lookup(logo_shift, key("h")), Some(&[Command::Action(crate::input::Action::Close)][..]));
        let c = Config::parse("[binds]\n\"Super+H\" = \"none\"").unwrap();
        assert_eq!(c.binds.len(), defaults - 1);
        assert_eq!(c.lookup(logo, key("h")), None);
        let c = Config::parse("[general]\ndefault_binds = false\n[binds]\n\"Super+Return\" = \"spawn foot\"").unwrap();
        assert_eq!(c.binds.len(), 1, "without defaults the table is the whole list");
        let c = Config::parse("[general]\ngap = 4").unwrap();
        assert_eq!(c.binds.len(), defaults);
        assert_eq!(c.world_settings().gap, 4.0);
    }

    #[test]
    fn launch_args_come_from_the_config_only() {
        let c = Config::default_config();
        assert!(c.launch_args("spotify.desktop").is_empty(), "nothing is built in");
        let c = Config::parse(
            "[launch_args]\nspotify = [\"--ozone-platform=wayland\"]\n\"com.spotify.Client.desktop\" = [\"--ozone-platform=x11\"]\nfirefox = [\"-P\", \"work\"]",
        )
        .unwrap();
        assert_eq!(c.launch_args("spotify.desktop"), ["--ozone-platform=wayland"]);
        assert_eq!(c.launch_args("com.spotify.Client.desktop"), ["--ozone-platform=x11"]);
        assert!(c.launch_args("spotify_spotify.desktop").is_empty());
        assert_eq!(c.launch_args("firefox"), ["-P", "work"]);
        assert!(Config::parse("[launch_args]\nspotify = \"--ozone-platform=x11\"").is_err());
    }
}
