//! The `[shell]` section: each built-in tool can be kept, switched off, or
//! replaced by an outside program, as a lasting choice.

use serde::{Deserialize, Serialize};

use crate::Tileception;

/// How one part of the shell is provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// Tileception's own.
    Builtin,
    /// Switched off.
    Off,
    /// This shell command line runs instead.
    Command(String),
}

impl Part {
    pub fn parse(value: &str) -> Part {
        match value.trim() {
            "" | "builtin" | "built-in" => Part::Builtin,
            "none" | "off" => Part::Off,
            command => Part::Command(command.to_string()),
        }
    }

    pub fn is_builtin(&self) -> bool {
        *self == Part::Builtin
    }
}

/// The edge used by the built-in bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BarPosition {
    #[default]
    Top,
    Bottom,
}

/// The `[shell]` section as written.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShellConfig {
    pub launcher: String,
    pub notifications: String,
    pub power_menu: String,
    pub lock: String,
    pub media: String,
    /// The doctor panel: "builtin" or "none".
    pub doctor: String,
    /// The built-in bar or its replacement.
    pub bar: String,
    pub bar_position: BarPosition,
    /// Maximum bar slots, including the overflow button. At least one.
    pub tray_max_icons: usize,
}

impl Default for ShellConfig {
    fn default() -> Self {
        let builtin = || "builtin".to_string();
        Self {
            launcher: builtin(),
            notifications: builtin(),
            power_menu: builtin(),
            lock: builtin(),
            media: builtin(),
            doctor: builtin(),
            bar: builtin(),
            bar_position: BarPosition::Top,
            tray_max_icons: 3,
        }
    }
}

impl ShellConfig {
    pub fn launcher(&self) -> Part {
        Part::parse(&self.launcher)
    }
    pub fn notifications(&self) -> Part {
        Part::parse(&self.notifications)
    }
    pub fn power_menu(&self) -> Part {
        Part::parse(&self.power_menu)
    }
    pub fn lock(&self) -> Part {
        Part::parse(&self.lock)
    }
    /// Volume, media keys and brightness have no outside replacement here:
    /// bind the keys to your own commands instead.
    pub fn media(&self) -> bool {
        Part::parse(&self.media).is_builtin()
    }
    /// The doctor panel has no outside replacement: it is on or off.
    pub fn doctor(&self) -> bool {
        Part::parse(&self.doctor).is_builtin()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.tray_max_icons == 0 {
            return Err("[shell] tray_max_icons must be at least 1".into());
        }
        for (name, value) in [("media", &self.media), ("doctor", &self.doctor)] {
            if let Part::Command(other) = Part::parse(value) {
                return Err(format!("[shell] {name} is builtin or none, not {other:?}"));
            }
        }
        Ok(())
    }
}

impl Tileception {
    pub fn shell(&self) -> &ShellConfig {
        &self.config.file.shell
    }

    /// The `launcher` command.
    pub fn shell_launcher(&mut self) {
        match self.shell().launcher() {
            Part::Builtin => self.toggle_launcher(),
            Part::Off => tracing::info!("the launcher is switched off in [shell]"),
            Part::Command(line) => self.spawn("sh", &["-c".into(), line]),
        }
    }

    /// The `power-menu` command.
    pub fn shell_power_menu(&mut self) {
        match self.shell().power_menu() {
            Part::Builtin => self.open_power_menu(),
            Part::Off => tracing::info!("the power menu is switched off in [shell]"),
            Part::Command(line) => self.spawn("sh", &["-c".into(), line]),
        }
    }

    /// The `notifications` command: the built-in panel, or the replacement's.
    /// With notifications switched off the panel still opens for the sound
    /// and playback controls.
    pub fn shell_notifications(&mut self) {
        match self.shell().notifications() {
            Part::Command(line) => self.spawn("sh", &["-c".into(), line]),
            Part::Builtin | Part::Off => self.toggle_notifications(),
        }
    }

    /// Starts and stops the services a changed `[shell]` section asks for, so
    /// a replacement can take over without a new login.
    pub fn apply_shell(&mut self, old: &ShellConfig) {
        let new = self.shell().clone();
        if old.notifications().is_builtin() != new.notifications().is_builtin() {
            if new.notifications().is_builtin() {
                if self.session_services || self.notification_bus.is_some() {
                    self.start_notifications(self.notification_bus.clone());
                }
            } else {
                // Gives up org.freedesktop.Notifications for the replacement.
                self.stop_notifications();
            }
        }
        if old.media() != new.media() {
            if new.media() {
                if self.session_services || self.media_bus.is_some() {
                    self.start_media(self.media_bus.clone().filter(|bus| bus != "session"));
                }
            } else {
                self.stop_media();
            }
        }
        if !new.launcher().is_builtin() && self.launcher.open {
            self.close_launcher();
        }
        if !new.power_menu().is_builtin() {
            self.power_menu = None;
        }
        if !new.doctor() && self.doctor.open {
            self.close_doctor();
        }
        self.apply_bar_command();
        self.apply_tray();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_parse() {
        assert_eq!(Part::parse("builtin"), Part::Builtin);
        assert_eq!(Part::parse(" "), Part::Builtin);
        assert_eq!(Part::parse("none"), Part::Off);
        assert_eq!(Part::parse("fuzzel --dmenu"), Part::Command("fuzzel --dmenu".into()));
        let shell = ShellConfig { media: "pamixer".into(), ..ShellConfig::default() };
        assert!(shell.validate().is_err());
        let shell = ShellConfig { doctor: "claude".into(), ..ShellConfig::default() };
        assert!(shell.validate().is_err());
        assert!(ShellConfig::default().validate().is_ok());
        assert!(ShellConfig::default().media());
        assert!(ShellConfig::default().doctor());
    }
}
