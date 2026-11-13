//! The device file the cloud import writes: one entry per device with its
//! local key, room and data point mapping. It holds secrets, so it is
//! written readable by its owner only.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// The file name, under Tileception's config directory.
pub const FILE_NAME: &str = "home-devices.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceFile {
    /// Where the devices came from, for messages: "tuya" or "simulated".
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub devices: Vec<DeviceEntry>,
    /// Credentials retained at import for startup inventory refresh,
    /// cloud-only sensor readings, or both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<CloudAccess>,
}

/// The Tuya project's credentials and the independent uses allowed at import.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CloudAccess {
    pub client_id: String,
    pub region: String,
    pub secret: String,
    /// Older files retained credentials for sensor polling.
    #[serde(default = "enabled")]
    pub poll_sensors: bool,
    #[serde(default = "enabled")]
    pub startup_refresh: bool,
}

fn enabled() -> bool {
    true
}

impl std::fmt::Debug for CloudAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloudAccess")
            .field("client_id", &self.client_id)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceEntry {
    #[serde(rename = "id")]
    pub device_id: String,
    pub name: String,
    /// The Tuya home (a place, such as a house or an office) it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// Tuya's category code: `dj` for lights, `kt` for air conditioners...
    #[serde(default)]
    pub category: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub product: String,
    /// The local key: 16 characters.
    #[serde(default)]
    pub key: String,
    /// The last known address; announcements on the LAN update it.
    #[serde(rename = "ip", default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Only for simulated devices: Tuya devices listen on 6668.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// "3.3", "3.4" or "3.5"; announcements fill it in when missing.
    /// 3.4 negotiates a session key for AES-128-ECB and HMAC-SHA256 framing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A device behind a gateway: the gateway's id and this device's node id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Data points by number.
    #[serde(default)]
    pub mapping: BTreeMap<String, DataPoint>,
    /// Which readings its history shows, chosen in Settings; unset shows
    /// every reading it reports. Kept across imports and refreshes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readings: Option<Readings>,
}

/// The readings chosen for a device's history, for one that reports both
/// temperature and humidity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Readings {
    Temperature,
    Humidity,
    Both,
}

impl Readings {
    pub const ALL: [Readings; 3] = [Readings::Temperature, Readings::Humidity, Readings::Both];

    pub fn label(self) -> &'static str {
        match self {
            Readings::Temperature => "Temperature",
            Readings::Humidity => "Humidity",
            Readings::Both => "Both",
        }
    }
}

impl DeviceFile {
    /// Carries what is kept locally over from an earlier file to a freshly
    /// fetched one: the readings chosen for each device's history.
    pub fn keep_choices(&mut self, earlier: &DeviceFile) {
        for device in &mut self.devices {
            if let Some(old) = earlier.devices.iter().find(|old| old.device_id == device.device_id) {
                device.readings = old.readings;
            }
        }
    }

    /// The file without the readings chosen in Settings: whether two files
    /// differ in more than those choices.
    pub fn without_choices(&self) -> DeviceFile {
        let mut file = self.clone();
        for device in &mut file.devices {
            device.readings = None;
        }
        file
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DataPoint {
    /// Tuya's standard code, such as `switch_led` or `temp_set`.
    pub code: String,
    /// `Boolean`, `Integer`, `Enum`, `String` or `Json`.
    #[serde(rename = "type", default)]
    pub kind: String,
    /// For integers `min`, `max`, `scale`, `step` and `unit`; for enums `range`.
    #[serde(default)]
    pub values: serde_json::Value,
}

/// Where the device file lives: `$XDG_CONFIG_HOME/tileception` or
/// `~/.config/tileception`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("tileception").join(FILE_NAME))
}

pub fn load(path: &Path) -> Result<DeviceFile, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
}

/// Writes the file readable by its owner only, replacing it in one step.
pub fn save(path: &Path, file: &DeviceFile) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(file).map_err(|err| err.to_string())?;
    let partial = path.with_extension("json.partial");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)
        .map_err(|err| format!("{}: {err}", partial.display()))?;
    // An older partial file may have looser permissions.
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("{}: {err}", partial.display()))?;
    out.write_all(text.as_bytes())
        .and_then(|()| out.sync_all())
        .map_err(|err| format!("{}: {err}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_files_are_private_and_load_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tileception-home-store-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        let file = DeviceFile {
            source: "simulated".into(),
            devices: vec![DeviceEntry {
                device_id: "abc".into(),
                name: "Desk lamp".into(),
                room: Some("Office".into()),
                category: "dj".into(),
                key: "0123456789abcdef".into(),
                ..Default::default()
            }],
            cloud: None,
        };
        save(&path, &file).expect("saved");
        let mode = std::fs::metadata(&path).expect("exists").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(load(&path).expect("loads"), file);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn chosen_readings_are_kept_across_imports() {
        let entry = |device_id: &str, readings| DeviceEntry {
            device_id: device_id.into(),
            name: device_id.into(),
            category: "wsdcg".into(),
            readings,
            ..Default::default()
        };
        let earlier = DeviceFile {
            devices: vec![entry("a", Some(Readings::Humidity)), entry("gone", Some(Readings::Temperature))],
            ..Default::default()
        };
        // An entry without a choice does not write one.
        let text = serde_json::to_string(&earlier).expect("serialized");
        assert!(text.contains(r#""readings":"humidity""#), "{text}");
        assert_eq!(serde_json::to_value(entry("b", None)).expect("serialized").get("readings"), None);

        let mut fetched = DeviceFile { devices: vec![entry("a", None), entry("b", None)], ..Default::default() };
        fetched.keep_choices(&earlier);
        assert_eq!(fetched.devices[0].readings, Some(Readings::Humidity));
        assert_eq!(fetched.devices[1].readings, None);
        // Files that differ only in the choices are the same inventory.
        assert_eq!(
            fetched.without_choices(),
            DeviceFile { devices: vec![entry("a", None), entry("b", None)], ..Default::default() }
        );
        assert_ne!(fetched, fetched.without_choices());
    }
}
