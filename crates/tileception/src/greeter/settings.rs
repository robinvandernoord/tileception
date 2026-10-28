//! `/etc/tileception/greeter.toml`, which the administrator writes, and the
//! greeter's own memory of who logged in last, with which session.

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{config::Keyboard, theme::ThemeFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsersAs {
    /// A tab per person on the login tile.
    Tabs,
    /// A tile per person (not drawn yet: shown as tabs).
    Tiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperFallback {
    /// A backdrop made from the person's accent.
    Generated,
    /// The `[theme]` wallpaper.
    System,
    /// Plain graphite.
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    Login,
    Clock,
    Session,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Panels {
    /// Columns of panels, each column top to bottom.
    pub columns: Vec<Vec<Panel>>,
    /// How wide the columns are, relative to each other.
    pub ratios: Vec<f64>,
}

impl Default for Panels {
    fn default() -> Self {
        Panels { columns: vec![vec![Panel::Login], vec![Panel::Clock, Panel::Session]], ratios: vec![1.75, 1.0] }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PerUser {
    /// Each person's own accent colours.
    pub accent: bool,
    /// Each person's own wallpaper between the tiles.
    pub wallpaper: bool,
    pub wallpaper_fallback: WallpaperFallback,
}

impl Default for PerUser {
    fn default() -> Self {
        PerUser { accent: true, wallpaper: true, wallpaper_fallback: WallpaperFallback::Generated }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GreeterFile {
    /// The session preselected for someone who has not logged in here before,
    /// by entry id (file name without `.desktop`).
    pub default_session: String,
    pub hidden_sessions: Vec<String>,
    pub hidden_users: Vec<String>,
    pub users_as: UsersAs,
    /// A strftime format, such as `%H:%M` or `%I:%M %p`.
    pub clock_format: String,
    /// What starts an X server around an X session's command.
    pub xsession_wrapper: String,
    /// Where the greeter remembers the last user and their sessions.
    pub state_file: Option<PathBuf>,
    pub panels: Panels,
    pub per_user: PerUser,
    pub theme: ThemeFile,
    /// The same keys as the session's `[keyboard]`; without a layout, the
    /// system's keymap from `/etc/default/keyboard`.
    pub keyboard: Keyboard,
}

impl Default for GreeterFile {
    fn default() -> Self {
        GreeterFile {
            default_session: "tileception".into(),
            hidden_sessions: vec!["tileception-nested".into()],
            hidden_users: Vec::new(),
            users_as: UsersAs::Tabs,
            clock_format: "%H:%M".into(),
            xsession_wrapper: "startx /usr/bin/env".into(),
            state_file: None,
            panels: Panels::default(),
            per_user: PerUser::default(),
            theme: ThemeFile::default(),
            keyboard: Keyboard::default(),
        }
    }
}

impl GreeterFile {
    pub fn parse(text: &str) -> Result<GreeterFile, String> {
        let file: GreeterFile = toml::from_str(text).map_err(|e| e.to_string())?;
        crate::theme::Theme::from_file(&file.theme)?;
        if file.panels.columns.iter().all(|c| !c.contains(&Panel::Login)) {
            return Err("[panels] columns must include \"login\"".into());
        }
        Ok(file)
    }

    pub fn path() -> PathBuf {
        std::env::var_os("TILECEPTION_GREETER_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/etc/tileception/greeter.toml".into())
    }

    /// The file, or the defaults when it is missing or wrong (with the reason
    /// in the log). Never written: it belongs to the administrator.
    pub fn load() -> GreeterFile {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => GreeterFile::parse(&text).unwrap_or_else(|e| {
                tracing::error!("greeter: {}: {e}; using defaults", path.display());
                GreeterFile::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => GreeterFile::default(),
            Err(e) => {
                tracing::error!("greeter: {}: {e}; using defaults", path.display());
                GreeterFile::default()
            }
        }
    }

    /// The keymap to type with: this file's, else the system's.
    pub fn keymap(&self) -> Keyboard {
        if self.keyboard.layout.is_some() {
            return self.keyboard.clone();
        }
        let system = std::fs::read_to_string("/etc/default/keyboard").map(|t| system_keyboard(&t)).unwrap_or_default();
        Keyboard { repeat_delay: self.keyboard.repeat_delay, repeat_rate: self.keyboard.repeat_rate, ..system }
    }

    pub fn state_path(&self) -> PathBuf {
        std::env::var_os("TILECEPTION_GREETER_STATE")
            .map(PathBuf::from)
            .or_else(|| self.state_file.clone())
            .unwrap_or_else(|| "/var/lib/tileception-greeter/state.toml".into())
    }
}

/// The keymap in Debian's `/etc/default/keyboard` (shell variables).
fn system_keyboard(text: &str) -> Keyboard {
    let value = |key: &str| {
        text.lines().find_map(|line| {
            let (name, value) = line.trim().split_once('=')?;
            let value = value.trim().trim_matches('"').trim_matches('\'').trim();
            (name.trim() == key && !value.is_empty()).then(|| value.to_string())
        })
    };
    Keyboard {
        layout: value("XKBLAYOUT"),
        variant: value("XKBVARIANT"),
        options: value("XKBOPTIONS"),
        model: value("XKBMODEL"),
        ..Default::default()
    }
}

/// What the greeter remembers between boots.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub last_user: Option<String>,
    /// Each person's last session, by login.
    pub sessions: BTreeMap<String, String>,
}

impl Memory {
    pub fn load(path: &std::path::Path) -> Memory {
        std::fs::read_to_string(path).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
    }

    /// Written to a temporary file first, so a crash never leaves half of it.
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        let temporary = path.with_extension("toml.new");
        std::fs::write(&temporary, text)
            .and_then(|()| std::fs::rename(&temporary, path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_example_parses() {
        let text = r##"
default_session = "tileception"
hidden_sessions = ["tileception-nested"]
hidden_users = ["backup"]
users_as = "tabs"
clock_format = "%H:%M"

[panels]
columns = [["login"], ["clock", "session"]]
ratios = [1.75, 1]

[per_user]
accent = true
wallpaper = true
wallpaper_fallback = "generated"

[theme]
accent = "#2bd4a7"
accent_end = "#5c8ff2"
wallpaper = "/usr/share/backgrounds/tide.jpg"
"##;
        let file = GreeterFile::parse(text).expect("parses");
        assert_eq!(file.hidden_users, ["backup"]);
        assert_eq!(file.panels.columns[1], [Panel::Clock, Panel::Session]);
        assert_eq!(file.per_user.wallpaper_fallback, WallpaperFallback::Generated);
        assert_eq!(file.theme.wallpaper.as_deref(), Some("/usr/share/backgrounds/tide.jpg"));
    }

    #[test]
    fn the_shipped_example_is_the_defaults() {
        let file = GreeterFile::parse(include_str!("../../../../resources/greeter.toml")).expect("parses");
        let defaults = GreeterFile::default();
        assert_eq!(file.default_session, defaults.default_session);
        assert_eq!(file.hidden_sessions, defaults.hidden_sessions);
        assert_eq!(file.clock_format, defaults.clock_format);
        assert_eq!(file.xsession_wrapper, defaults.xsession_wrapper);
        assert_eq!(file.panels.columns, defaults.panels.columns);
        assert_eq!(file.panels.ratios, defaults.panels.ratios);
        assert_eq!(file.per_user.wallpaper_fallback, defaults.per_user.wallpaper_fallback);
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(GreeterFile::parse("users_as = \"cards\"").is_err());
        assert!(GreeterFile::parse("[panels]\ncolumns = [[\"clock\"]]").is_err());
        assert!(GreeterFile::parse("[theme]\naccent = \"green\"").is_err());
        assert!(GreeterFile::parse("unknown = 1").is_err());
    }

    #[test]
    fn reads_the_system_keymap() {
        let text = "# KEYBOARD CONFIGURATION FILE\nXKBMODEL=\"pc105\"\nXKBLAYOUT=\"nl\"\nXKBVARIANT=\"\"\nXKBOPTIONS=\"compose:ralt\"\nBACKSPACE=\"guess\"\n";
        let keyboard = system_keyboard(text);
        assert_eq!(keyboard.layout.as_deref(), Some("nl"));
        assert_eq!(keyboard.variant, None);
        assert_eq!(keyboard.options.as_deref(), Some("compose:ralt"));
        assert_eq!(keyboard.model.as_deref(), Some("pc105"));
    }

    #[test]
    fn memory_round_trips() {
        let path = std::env::temp_dir().join(format!("tileception-greeter-state-{}.toml", std::process::id()));
        let mut memory = Memory { last_user: Some("sam".into()), ..Default::default() };
        memory.sessions.insert("sam".into(), "gnome".into());
        memory.save(&path).expect("saves");
        assert_eq!(Memory::load(&path), memory);
        let _ = std::fs::remove_file(&path);
        assert_eq!(Memory::load(&path), Memory::default());
    }
}
