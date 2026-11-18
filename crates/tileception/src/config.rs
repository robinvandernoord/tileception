//! `~/.config/tileception/config.toml`: settings and key bindings.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct ConfigFile {
    pub general: General,
    /// Where plugins were configured before `[plugins]`.
    pub launcher: crate::plugins::LauncherConfig,
    /// Plugins by ID, overriding their manifests.
    pub plugins: BTreeMap<String, crate::plugins::PluginConfig>,
    pub clipboard: crate::clipboard::ClipboardConfig,
    pub theme: crate::theme::ThemeFile,
    pub keyboard: Keyboard,
    pub touchpad: crate::gestures::TouchpadConfig,
    /// Exact touchscreen device names mapped to output connector names.
    pub touchscreens: BTreeMap<String, crate::touch::TouchscreenConfig>,
    pub idle: crate::idle::IdleConfig,
    pub media: crate::media::MediaConfig,
    pub battery: crate::battery::BatteryConfig,
    pub updates: crate::updates::UpdatesConfig,
    /// Events in the bar from .ics files and URLs.
    pub calendar: crate::calendar::CalendarConfig,
    pub bluetooth: crate::bundled::bluetooth::BluetoothConfig,
    pub shell: crate::shell::ShellConfig,
    pub duck: crate::duck::DuckConfig,
    /// Formats and sizes for area screenshots.
    pub capture: crate::shot::CaptureConfig,
    /// `[wizard]`, once called `[doctor]`, which still works.
    #[serde(rename = "wizard", alias = "doctor")]
    pub doctor: crate::doctor::DoctorConfig,
    /// What this session shows on the login screen.
    pub greeter: crate::greeter::ShareConfig,
    pub rules: Vec<crate::rules::WindowRule>,
    /// Per screen, by connector name ("*" for any).
    pub outputs: BTreeMap<String, crate::scale::OutputConfig>,
    /// Sharing the mouse, keyboard and clipboard with other machines.
    pub handoff: crate::handoff::HandoffConfig,
    /// Tuya smart home devices in the notification panel.
    pub home: crate::home::HomeConfig,
    /// Extra arguments for programs the launcher starts, by desktop file name.
    pub launch_args: BTreeMap<String, Vec<String>>,
    pub binds: Option<BTreeMap<String, BindValue>>,
}

/// One command, or several run in order.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BindValue {
    One(String),
    Many(Vec<String>),
}

impl BindValue {
    fn lines(&self) -> Vec<&str> {
        match self {
            BindValue::One(line) => vec![line.as_str()],
            BindValue::Many(lines) => lines.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub terminal: Option<String>,
    pub gap: f64,
    pub animation_ms: f64,
    /// Smallest uniform window scale that still accepts pointer input.
    pub min_live_scale: f64,
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
    /// Map the full edge proportionally when crossing to an adjacent screen.
    pub stretch_screen_edges: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            terminal: None,
            gap: 12.0,
            animation_ms: 280.0,
            min_live_scale: 0.5,
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
            stretch_screen_edges: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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

    /// Each layout in the keymap with its variant, in xkb's order, such as
    /// ("us", "") and ("nl", "") for `layout = "us,nl"`.
    pub fn layouts(&self) -> Vec<(String, String)> {
        let (_, layout, variant, _) = self.names();
        let mut variants = variant.split(',');
        layout
            .split(',')
            .map(|layout| (layout.trim().to_string(), variants.next().unwrap_or_default().trim().to_string()))
            .collect()
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

    /// Whether `self` (what is held) includes the single `modifier`.
    pub fn has(&self, modifier: Mods) -> bool {
        (!modifier.logo || self.logo)
            && (!modifier.alt || self.alt)
            && (!modifier.ctrl || self.ctrl)
            && (!modifier.shift || self.shift)
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
    /// The whole file, or an error for the first thing wrong in it. Use this
    /// to check a draft before saving it; loading uses `parse_lenient`.
    pub fn parse(text: &str) -> Result<Config, String> {
        let file: ConfigFile = toml::from_str(text).map_err(|error| error.to_string())?;
        Config::from_file(file)
    }

    /// Everything in the file that is right, leaving out each setting, key
    /// binding or list entry that is wrong, so one mistake costs only itself.
    /// Only text that is not TOML at all is an error.
    pub fn parse_lenient(text: &str) -> Result<(Config, Vec<Problem>), String> {
        let strict = match Config::parse(text) {
            Ok(config) => return Ok((config, Vec::new())),
            Err(error) => error,
        };
        let table: toml::Table = toml::from_str(text).map_err(|error| error.to_string())?;
        let mut kept = toml::Table::new();
        let mut left_out = Vec::new();
        for (key, value) in table {
            recover(&mut kept, vec![Step::Key(key)], value, &mut left_out);
        }
        let problems: Vec<Problem> =
            left_out.into_iter().map(|(steps, _, message)| Problem { path: describe_steps(&steps), message }).collect();
        match Config::from_table(kept) {
            Ok(config) => Ok((config, problems)),
            // Not expected: every part kept was checked on the way in.
            Err(error) => Ok((
                Config::default_config(),
                vec![Problem { path: String::new(), message: format!("{strict}; {error}") }],
            )),
        }
    }

    fn from_table(table: toml::Table) -> Result<Config, String> {
        let file: ConfigFile =
            toml::Value::Table(table).try_into().map_err(|error: toml::de::Error| error.message().to_string())?;
        Config::from_file(file)
    }

    fn from_file(file: ConfigFile) -> Result<Config, String> {
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
                Err(error) => {
                    errors.push(format!("{combo:?}: {error}"));
                    continue;
                }
            };
            binds.retain(|bind| (bind.mods, bind.key) != combo_keys);
            if matches!(value, BindValue::One(line) if line.trim() == "none") {
                continue;
            }
            let parsed: Result<Vec<Command>, String> = value
                .lines()
                .into_iter()
                .map(|line| {
                    // Old and renamed Tileception commands only, so older
                    // configs keep working. Never list an external program
                    // here: someone may really want to run it.
                    let line = match line.trim() {
                        "spawn tileception-power" => "power-menu",
                        "spawn tileception-screenshot region --ask" | "spawn tileception-screenshot region" => "snip",
                        other => other,
                    };
                    commands::parse(line)
                })
                .collect();
            match parsed {
                Ok(commands) => binds.push(Bind { mods: combo_keys.0, key: combo_keys.1, commands }),
                Err(error) => errors.push(format!("{combo:?}: {error}")),
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        let drag_mod = parse_modifier(&file.general.drag_modifier)
            .ok_or_else(|| format!("unknown drag_modifier {:?}", file.general.drag_modifier))?;
        let theme = crate::theme::Theme::from_file(&file.theme).map_err(|error| format!("[theme] {error}"))?;
        file.shell.validate()?;
        file.doctor.validate()?;
        if !file.general.min_live_scale.is_finite() || !(0.1..=1.0).contains(&file.general.min_live_scale) {
            return Err("general.min_live_scale must be finite and between 0.1 and 1.0".into());
        }
        crate::rules::validate(&file.rules)?;
        crate::scale::validate(&file.outputs)?;
        file.touchpad.validate()?;
        crate::touch::validate(&file.touchscreens)?;
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
        self.binds.iter().find(|bind| bind.mods == mods && bind.key == key).map(|bind| bind.commands.as_slice())
    }

    /// Extra launch arguments for a desktop entry id (`spotify.desktop`), from
    /// `[launch_args]`; the key may leave off `.desktop`.
    pub fn launch_args(&self, desktop_id: &str) -> Vec<String> {
        let name = desktop_id.strip_suffix(".desktop").unwrap_or(desktop_id);
        self.file
            .launch_args
            .iter()
            .find(|(key, _)| key.strip_suffix(".desktop").unwrap_or(key) == name)
            .map(|(_, args)| args.clone())
            .unwrap_or_default()
    }

    pub fn world_settings(&self) -> tileception_core::Settings {
        let general = &self.file.general;
        tileception_core::Settings {
            gap: general.gap.max(0.0),
            animation: (general.animation_ms / 1000.0).max(0.0),
            share_removed_space: general.share_removed_space,
            tab_height: general.tab_height.max(0.0),
        }
    }
}

fn parse_modifier(name: &str) -> Option<Mods> {
    let mut modifier = Mods::default();
    match name.to_ascii_lowercase().as_str() {
        "super" | "logo" | "mod4" | "win" | "meta" => modifier.logo = true,
        "alt" | "mod1" => modifier.alt = true,
        "ctrl" | "control" => modifier.ctrl = true,
        "shift" => modifier.shift = true,
        _ => return None,
    }
    Some(modifier)
}

/// Parses `Super+Shift+T` into modifiers and a lower-case keysym.
pub fn parse_combo(combo: &str) -> Result<(Mods, u32), String> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    if let [only] = parts[..] {
        // A modifier on its own: pressed and let go with nothing else.
        if let Some(modifier) = parse_modifier(only) {
            return Ok((modifier, TAP));
        }
    }
    let (key, mods) = parts.split_last().ok_or("empty key binding")?;
    let mut all = Mods::default();
    for modifier in mods {
        let one = parse_modifier(modifier).ok_or_else(|| format!("unknown modifier {modifier:?}"))?;
        all.logo |= one.logo;
        all.alt |= one.alt;
        all.ctrl |= one.ctrl;
        all.shift |= one.shift;
    }
    if let Some(wheel) = wheel_key(key) {
        return Ok((all, wheel));
    }
    if let Some(gesture) = crate::gestures::parse_key(key) {
        return gesture.map(|gesture| (all, gesture));
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
    let mut modifier = Mods::default();
    match sym {
        0xffeb | 0xffec => modifier.logo = true,  // Super_L, Super_R
        0xffe9 | 0xffea => modifier.alt = true,   // Alt_L, Alt_R
        0xffe3 | 0xffe4 => modifier.ctrl = true,  // Control_L, Control_R
        0xffe1 | 0xffe2 => modifier.shift = true, // Shift_L, Shift_R
        _ => return None,
    }
    Some(modifier)
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

/// Something in the config file that was left out so the rest could load.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    /// Where it is, such as `theme.accent` or `rules #2`; empty for the file.
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(formatter, "{}", self.message)
        } else {
            write!(formatter, "{}: {}", self.path, self.message)
        }
    }
}

/// One step down into the file: a key in a table, or an entry of a list of
/// tables (counted in the file, from 0). An entry is always the last step.
#[derive(Debug, Clone)]
enum Step {
    Key(String),
    Index(usize),
}

type LeftOut = (Vec<Step>, toml::Value, String);

fn check(table: &toml::Table) -> Result<(), String> {
    Config::from_table(table.clone()).map(|_| ())
}

/// Puts `value` at `steps` in `table`, appending it when the last step is a
/// list entry. Every table and list on the way must already be there.
fn insert(table: &mut toml::Table, steps: &[Step], value: toml::Value) -> bool {
    match steps {
        [Step::Key(key)] => {
            table.insert(key.clone(), value);
            true
        }
        [Step::Key(key), Step::Index(_)] => match table.get_mut(key) {
            Some(toml::Value::Array(items)) => {
                items.push(value);
                true
            }
            _ => false,
        },
        [Step::Key(key), rest @ ..] => match table.get_mut(key) {
            Some(toml::Value::Table(inner)) => insert(inner, rest, value),
            _ => false,
        },
        _ => false,
    }
}

/// Adds `value` to `kept` if the result still loads. Otherwise a table is
/// added key by key and a list of tables entry by entry, so only the parts
/// that are wrong are left out. A list entry is kept or left out whole: half
/// a window rule would match windows it was not written for.
fn recover(kept: &mut toml::Table, steps: Vec<Step>, value: toml::Value, left_out: &mut Vec<LeftOut>) {
    let mut trial = kept.clone();
    if !insert(&mut trial, &steps, value.clone()) {
        return;
    }
    let error = match check(&trial) {
        Ok(()) => {
            *kept = trial;
            return;
        }
        Err(error) => error,
    };
    let in_table = matches!(steps.last(), Some(Step::Key(_)));
    let parts: Option<(toml::Value, Vec<(Step, toml::Value)>)> = match &value {
        toml::Value::Table(entries) if in_table && !entries.is_empty() => Some((
            toml::Value::Table(toml::Table::new()),
            entries.iter().map(|(key, inner)| (Step::Key(key.clone()), inner.clone())).collect(),
        )),
        toml::Value::Array(items) if in_table && !items.is_empty() && items.iter().all(toml::Value::is_table) => {
            Some((
                toml::Value::Array(Vec::new()),
                items.iter().cloned().enumerate().map(|(index, item)| (Step::Index(index), item)).collect(),
            ))
        }
        _ => None,
    };
    if let Some((empty, parts)) = parts {
        let mut trial = kept.clone();
        if insert(&mut trial, &steps, empty) && check(&trial).is_ok() {
            *kept = trial;
            for (step, inner) in parts {
                let mut path = steps.clone();
                path.push(step);
                recover(kept, path, inner, left_out);
            }
            return;
        }
    }
    // A binding's error starts with its key, which the path already says.
    let error = match steps.last() {
        Some(Step::Key(key)) => error.strip_prefix(&format!("{key:?}: ")).map(str::to_string).unwrap_or(error),
        _ => error,
    };
    left_out.push((steps, value, error));
}

/// `theme.accent`, `binds."Super+T"` or `rules #2`, as the file would say it.
fn describe_steps(steps: &[Step]) -> String {
    let mut text = String::new();
    for step in steps {
        match step {
            Step::Key(key) => {
                if !text.is_empty() {
                    text.push('.');
                }
                if !key.is_empty()
                    && key.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
                {
                    text.push_str(key);
                } else {
                    text.push_str(&format!("{key:?}"));
                }
            }
            Step::Index(index) => text.push_str(&format!(" #{}", index + 1)),
        }
    }
    text
}

/// One line for all problems, for a log or a reply.
pub fn describe(problems: &[Problem]) -> String {
    problems.iter().map(Problem::to_string).collect::<Vec<_>>().join("; ")
}

/// A copy of the last config file that loaded without problems, used at
/// startup when the file is not TOML at all.
fn last_good_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("tileception/config.last-good.toml")
}

fn remember_good(text: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = last_good_path();
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // The config can hold passphrases and tokens.
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(text.as_bytes()));
    if let Err(error) = written {
        tracing::warn!("could not keep a copy of the config in {}: {error}", path.display());
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
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
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            tracing::error!("config {}: {error}; using defaults", path.display());
            return Config::default_config();
        }
    };
    match Config::parse_lenient(&text) {
        Ok((config, problems)) => {
            if problems.is_empty() {
                remember_good(&text);
            }
            for problem in &problems {
                tracing::warn!("config {}: left out {problem}", path.display());
            }
            config
        }
        Err(error) => {
            tracing::error!("config {}: {error}", path.display());
            let last_good = last_good_path();
            match std::fs::read_to_string(&last_good)
                .map_err(|error| error.to_string())
                .and_then(|text| Config::parse_lenient(&text))
            {
                Ok((config, _)) => {
                    tracing::warn!("config: using the last one that loaded, kept in {}", last_good.display());
                    config
                }
                Err(_) => {
                    tracing::warn!("config: using defaults");
                    Config::default_config()
                }
            }
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok()
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
        let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        // Not TOML at all: the config that is running stays.
        let (config, problems) = Config::parse_lenient(&text)?;
        let result = self.apply_config(config);
        // An open Settings follows the file, keeping what was edited there.
        self.settings_config_changed(&text);
        result?;
        if !problems.is_empty() {
            return Err(format!("reloaded, leaving out {}", describe(&problems)));
        }
        remember_good(&text);
        tracing::info!("config reloaded");
        Ok(())
    }

    pub fn apply_config(&mut self, config: Config) -> Result<(), String> {
        self.cancel_output_preview();
        if self.config.file.touchscreens != config.file.touchscreens {
            self.touch_cancel((self.now() * 1000.0) as u32);
        }
        let republish = self.theme != config.theme || self.config.file.greeter != config.file.greeter;
        self.world.settings = config.world_settings();
        if let Some(keyboard) = self.seat.get_keyboard() {
            let keyboard_config = &config.file.keyboard;
            keyboard.change_repeat_info(
                keyboard_config.repeat_rate.unwrap_or(30),
                keyboard_config.repeat_delay.unwrap_or(400),
            );
        }
        // As at startup, so removing `terminal` returns to the detected one.
        self.terminal = std::env::var("TILECEPTION_TERMINAL")
            .ok()
            .or_else(|| config.file.general.terminal.clone())
            .unwrap_or_else(|| crate::state::default_terminal(self.launcher.apps()));
        if self.theme != config.theme {
            let frost = self.theme.frost;
            self.theme = config.theme.clone();
            if frost != self.theme.frost {
                self.announce_blur();
            }
            self.update_appearance();
            self.text.borrow_mut().clear();
            self.wallpaper = None;
            self.wallpaper_failed = None;
            self.rotate_wallpaper(false);
        }
        let old = std::mem::replace(&mut self.config, config).file;
        self.apply_shell(&old.shell);
        self.apply_clipboard();
        self.apply_handoff_config(&old.handoff);
        // The smart home is a bundled plugin that runs while [home] is enabled.
        if old.plugins != self.config.file.plugins
            || old.launcher != self.config.file.launcher
            || old.home != self.config.file.home
        {
            self.apply_plugins();
        }
        if old.calendar != self.config.file.calendar {
            self.apply_calendar();
        }
        if republish {
            self.publish_greeter_card();
        }
        self.configure_touchpads();
        // Also lays everything out again.
        if let Err(error) = self.configure_outputs() {
            self.config.file.outputs = old.outputs;
            self.configure_output_layouts();
            return Err(format!("reloaded, but outputs: {error}"));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;
    use crate::input::Action;

    #[test]
    fn keyboard_defaults_to_us_international() {
        let names = |keyboard: Keyboard| {
            let (_, layout, variant, _) = keyboard.names();
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
        let dutch = Keyboard { layout: Some("nl".into()), ..Default::default() };
        assert_eq!(names(dutch), ("nl".into(), String::new()));
    }

    #[test]
    fn layouts_pair_with_variants() {
        let pair = |layout: &str, variant: &str| (layout.to_string(), variant.to_string());
        assert_eq!(Keyboard::default().layouts(), [pair("us", "altgr-intl")]);
        let two = Keyboard { layout: Some("us, nl".into()), variant: Some("intl,".into()), ..Default::default() };
        assert_eq!(two.layouts(), [pair("us", "intl"), pair("nl", "")]);
    }

    #[test]
    fn default_config_parses() {
        let config = Config::default_config();
        assert!(config.binds.len() > 30);
        let t_key = xkb::keysym_from_name("t", xkb::KEYSYM_NO_FLAGS).raw();
        let logo = Mods { logo: true, ..Default::default() };
        assert_eq!(config.lookup(logo, t_key), Some(&[Command::Action(Action::Spawn)][..]));
        let one = xkb::keysym_from_name("1", xkb::KEYSYM_NO_FLAGS).raw();
        let logo_shift = Mods { logo: true, shift: true, ..Default::default() };
        assert_eq!(config.lookup(logo_shift, one), Some(&[Command::Action(Action::SaveView(1))][..]));
        assert_eq!(config.lookup(logo, WHEEL_UP), Some(&[Command::Action(Action::StepInPointer)][..]));
        assert_eq!(config.lookup(Mods::default(), t_key), None);
    }

    #[test]
    fn combos_and_errors() {
        let (mods, keysym) = parse_combo("Ctrl+Alt+Page_Up").unwrap();
        assert!(mods.ctrl && mods.alt && !mods.logo);
        assert_eq!(keysym, xkb::keysym_from_name("Page_Up", xkb::KEYSYM_NO_FLAGS).raw());
        assert!(parse_combo("Hyper+T").is_err());
        assert!(parse_combo("Super+NoSuchKey").is_err());
        assert!(Config::parse("[binds]\n\"Super+T\" = \"fly\"").is_err());
        assert!(Config::parse("[general]\nbogus = 1").is_err());
    }

    #[test]
    fn modifier_taps() {
        let config = Config::parse("[binds]\n\"Super\" = \"launcher\"").unwrap();
        let logo = Mods { logo: true, ..Default::default() };
        assert_eq!(config.lookup(logo, TAP), Some(&[Command::Action(crate::input::Action::Launcher)][..]));
        assert_eq!(modifier_of(0xffeb), Some(logo));
    }

    #[test]
    fn old_area_screenshot_binds_snip() {
        let print = xkb::keysym_from_name("Print", xkb::KEYSYM_NO_FLAGS).raw();
        let shift = Mods { shift: true, ..Default::default() };
        for line in ["spawn tileception-screenshot region --ask", "spawn tileception-screenshot region"] {
            let config = Config::parse(&format!("[binds]\n\"Shift+Print\" = \"{line}\"")).unwrap();
            assert_eq!(config.lookup(shift, print), Some(&[Command::Capture][..]));
        }
        // A deliberate choice of the old flow without the prompt is kept.
        let config =
            Config::parse("[binds]\n\"Shift+Print\" = \"spawn tileception-screenshot region --no-ask\"").unwrap();
        assert!(matches!(config.lookup(shift, print), Some([Command::Spawn(_)])));
    }

    #[test]
    fn several_commands_per_key() {
        let config = Config::parse("[binds]\n\"Super+Escape\" = [\"reload\", \"spawn notify-send hi\"]").unwrap();
        let esc = xkb::keysym_from_name("Escape", xkb::KEYSYM_NO_FLAGS).raw();
        let logo = Mods { logo: true, ..Default::default() };
        assert_eq!(config.lookup(logo, esc).map(|commands| commands.len()), Some(2));
    }

    #[test]
    fn user_binds_add_to_and_override_the_defaults() {
        let defaults = Config::default_config().binds.len();
        let logo = Mods { logo: true, ..Default::default() };
        let key = |name| xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS).raw();
        let config = Config::parse("[binds]\n\"Super+Y\" = \"spawn foot\"").unwrap();
        assert_eq!(config.binds.len(), defaults + 1, "a new key joins the defaults");
        assert_eq!(config.lookup(logo, key("h")), Some(&[Command::Action(crate::input::Action::Minimize)][..]));
        // The same key written another way still replaces its default.
        let config = Config::parse("[binds]\n\"Shift+Super+h\" = \"close\"").unwrap();
        let logo_shift = Mods { logo: true, shift: true, ..Default::default() };
        assert_eq!(config.binds.len(), defaults);
        assert_eq!(config.lookup(logo_shift, key("h")), Some(&[Command::Action(crate::input::Action::Close)][..]));
        let config = Config::parse("[binds]\n\"Super+H\" = \"none\"").unwrap();
        assert_eq!(config.binds.len(), defaults - 1);
        assert_eq!(config.lookup(logo, key("h")), None);
        let config =
            Config::parse("[general]\ndefault_binds = false\n[binds]\n\"Super+Return\" = \"spawn foot\"").unwrap();
        assert_eq!(config.binds.len(), 1, "without defaults the table is the whole list");
        let config = Config::parse("[general]\ngap = 4").unwrap();
        assert_eq!(config.binds.len(), defaults);
        assert_eq!(config.world_settings().gap, 4.0);
    }

    #[test]
    fn a_mistake_leaves_out_only_itself() {
        let paths = |problems: &[Problem]| problems.iter().map(|problem| problem.path.clone()).collect::<Vec<_>>();
        let text =
            "[general]\ngap = 4\nbogus = 1\n[theme]\nwallpaper = \"/pictures/sea.png\"\nwallpaper_fit = \"tile\"\n";
        let (config, problems) = Config::parse_lenient(text).unwrap();
        assert_eq!(config.world_settings().gap, 4.0);
        assert_eq!(config.theme.wallpaper.as_deref(), Some(Path::new("/pictures/sea.png")));
        assert_eq!(paths(&problems), ["general.bogus", "theme.wallpaper_fit"]);
        assert!(problems[1].message.contains("tile"), "{}", problems[1].message);

        let defaults = Config::default_config().binds.len();
        let text = "[binds]\n\"Super+T\" = \"fly\"\n\"Super+Y\" = \"spawn foot\"\n\"Hyper+U\" = \"close\"\n";
        let (config, problems) = Config::parse_lenient(text).unwrap();
        assert_eq!(paths(&problems), ["binds.\"Hyper+U\"", "binds.\"Super+T\""]);
        assert_eq!(problems[1].to_string(), "binds.\"Super+T\": unknown command \"fly\"");
        assert_eq!(config.binds.len(), defaults + 1, "Super+Y joins, Super+T keeps its default");
        let t_key = xkb::keysym_from_name("t", xkb::KEYSYM_NO_FLAGS).raw();
        let logo = Mods { logo: true, ..Default::default() };
        assert_eq!(config.lookup(logo, t_key), Some(&[Command::Action(Action::Spawn)][..]));

        let text = "[[rules]]\napp_id = \"one\"\n[[rules]]\nfloat = true\n[[rules]]\napp_id = \"three\"\nsize = \"big\"\n[[rules]]\ntitle = \"four\"\n";
        let (config, problems) = Config::parse_lenient(text).unwrap();
        assert_eq!(paths(&problems), ["rules #2", "rules #3"]);
        let kept: Vec<_> =
            config.file.rules.iter().map(|rule| rule.app_id.clone().or(rule.title.clone()).unwrap()).collect();
        assert_eq!(kept, ["one", "four"]);

        let (config, problems) = Config::parse_lenient("theme = 5\n[general]\ngap = 2").unwrap();
        assert_eq!(paths(&problems), ["theme"]);
        assert_eq!(config.world_settings().gap, 2.0);
    }

    #[test]
    fn text_that_is_not_toml_is_an_error() {
        assert!(Config::parse_lenient("[general\ngap = 4").is_err());
        let (config, problems) = Config::parse_lenient(DEFAULT).unwrap();
        assert!(problems.is_empty());
        assert_eq!(config.binds.len(), Config::default_config().binds.len());
    }

    #[test]
    fn launch_args_come_from_the_config_only() {
        let config = Config::default_config();
        assert!(config.launch_args("spotify.desktop").is_empty(), "nothing is built in");
        let config = Config::parse(
            "[launch_args]\nspotify = [\"--ozone-platform=wayland\"]\n\"com.spotify.Client.desktop\" = [\"--ozone-platform=x11\"]\nfirefox = [\"-P\", \"work\"]",
        )
        .unwrap();
        assert_eq!(config.launch_args("spotify.desktop"), ["--ozone-platform=wayland"]);
        assert_eq!(config.launch_args("com.spotify.Client.desktop"), ["--ozone-platform=x11"]);
        assert!(config.launch_args("spotify_spotify.desktop").is_empty());
        assert_eq!(config.launch_args("firefox"), ["-P", "work"]);
        assert!(Config::parse("[launch_args]\nspotify = \"--ozone-platform=x11\"").is_err());
    }
}
