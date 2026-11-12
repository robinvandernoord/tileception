//! The optional smart home: its `[home]` settings, `tileception msg home`
//! commands, what Settings shows of it, and the `tileception home` command
//! line that imports the devices.
//!
//! The devices themselves belong to the bundled `home` plugin
//! (`tileception plugin home`, see [`crate::bundled`]), which runs while
//! `[home]` is enabled: it offers the panel block, `home` in the launcher,
//! and answers the commands that reach devices.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tileception_home::{Change, HomeService, Kind, Options, Reach, store};

use crate::{Tileception, bundled::home::DEVICES};

/// The bundled plugin's id.
const PLUGIN: &str = "home";

/// The `[home]` section.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HomeConfig {
    /// Show the smart home block in the notification panel.
    pub enabled: bool,
    /// The device file; `~/.config/tileception/home-devices.json` by default.
    pub devices: Option<String>,
}

impl HomeConfig {
    pub(crate) fn path(&self) -> Option<PathBuf> {
        match &self.devices {
            Some(path) => Some(crate::wallpaper::expand_home(path)),
            None => store::default_path(),
        }
    }
}

/// A device as Settings lists it.
pub(crate) struct Live {
    pub name: String,
    pub place: String,
    pub status: String,
    pub answering: bool,
}

impl Tileception {
    /// The home plugin's device list, as last read.
    fn home_resource(&self) -> Option<&Value> {
        self.plugins
            .as_ref()
            .filter(|plugins| plugins.running(PLUGIN))
            .and_then(|plugins| plugins.resource(PLUGIN, DEVICES))
    }

    /// Puts the addresses the running connection heard into a device file.
    pub(crate) fn settings_home_update_addresses(&self, file: &mut store::DeviceFile) {
        let Some(discovered) = self.home_resource().and_then(|resource| resource["discovered"].as_array()) else {
            return;
        };
        for heard in discovered {
            let Some(device) = file.devices.iter_mut().find(|device| heard["device_id"] == device.device_id.as_str())
            else {
                continue;
            };
            if let Some(host) = heard["host"].as_str() {
                device.host = Some(host.into());
            }
            if let Some(version) = heard["version"].as_str() {
                device.version = Some(version.into());
            }
        }
    }

    pub(crate) fn settings_home_status(&self) -> String {
        let Some(resource) = self.home_resource() else { return "Disconnected".into() };
        if let Some(problem) = resource["problem"].as_str() {
            problem.into()
        } else if resource["connected"] == true {
            format!("Connected: {} devices", resource["devices"].as_array().map_or(0, Vec::len))
        } else {
            "Disconnected".into()
        }
    }

    pub(crate) fn settings_home_cloud_label(&self, sensors: bool) -> String {
        let enabled = self
            .config
            .file
            .home
            .path()
            .and_then(|path| store::load(&path).ok())
            .and_then(|file| file.cloud)
            .is_some_and(|cloud| if sensors { cloud.poll_sensors } else { cloud.startup_refresh });
        format!("{}: {}", if sensors { "Cloud sensors" } else { "Startup refresh" }, if enabled { "on" } else { "off" })
    }

    /// Switches polling cloud sensors or the startup refresh; the plugin
    /// sees the file change and connects again.
    pub(crate) fn settings_home_cloud(&mut self, sensors: bool) -> Result<(), String> {
        let path = self.config.file.home.path().ok_or("No config directory")?;
        switch_cloud_use(&path, sensors).map(|_| ())
    }

    pub(crate) fn settings_home_connected(&self) -> bool {
        self.home_resource().is_some_and(|resource| resource["connected"] == true)
    }

    /// The devices as the running connection sees them.
    pub(crate) fn settings_home_live(&self) -> Vec<Live> {
        let Some(devices) = self.home_resource().and_then(|resource| resource["devices"].as_array()) else {
            return Vec::new();
        };
        devices
            .iter()
            .map(|device| Live {
                name: device["name"].as_str().unwrap_or_default().into(),
                place: device["place"].as_str().unwrap_or_default().into(),
                status: device["status"].as_str().unwrap_or_default().into(),
                answering: device["answering"] == true,
            })
            .collect()
    }

    /// `tileception msg home ...`: folding, the view and the selection are
    /// the panel's; what reaches devices goes to the plugin, which answers
    /// later.
    pub fn home_command(&mut self, command: HomeCommand, text: &str) -> Result<String, String> {
        if let HomeCommand::Enable(wanted) = command {
            let value = if wanted { "true" } else { "false" };
            let path = crate::config::path();
            crate::config_edit::set_value(&path, "home.enabled", value)
                .map_err(|err| format!("could not set home.enabled in {}: {err}", path.display()))?;
            // The config watcher reloads the file and applies the change.
            return Ok(format!("home.enabled = {value} in {}", path.display()));
        }
        if !self.config.file.home.enabled {
            return Err("the smart home is switched off: set enabled = true under [home], \
                 or run `tileception msg home enable`"
                .into());
        }
        match command {
            HomeCommand::Fold(folded) => self.panel_fold(PLUGIN, folded).map(|()| String::new()),
            HomeCommand::View(all) => {
                self.panel_view(PLUGIN, if all { "All" } else { "Rooms" }).map(|()| String::new())
            }
            HomeCommand::Select(name) => self.panel_select(PLUGIN, &name).map(|()| String::new()),
            _ => self.plugin_command(PLUGIN, "command", json!({ "text": text })),
        }
    }
}

/// A change written as a command.
#[derive(Debug, Clone, PartialEq)]
pub enum Wanted {
    Toggle,
    Change(Change),
}

/// `tileception msg home ...`.
#[derive(Debug, Clone, PartialEq)]
pub enum HomeCommand {
    Status,
    /// Why a device is unreachable, and what to check.
    Why(String),
    Set(String, Wanted),
    Select(String),
    Fold(bool),
    /// `true` for one list, `false` for rooms.
    View(bool),
    AllOff,
    /// Writes `enabled` under `[home]` in config.toml.
    Enable(bool),
}

/// Parses what follows `home`. Device names may have spaces; a value comes
/// last.
pub fn parse(rest: &str) -> Result<HomeCommand, String> {
    let rest = rest.trim();
    let (sub, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let tail = tail.trim();
    let named = || if tail.is_empty() { Err(format!("home {sub} needs a device name")) } else { Ok(tail.to_string()) };
    let valued = || -> Result<(String, String), String> {
        let (name, value) = tail
            .rsplit_once(char::is_whitespace)
            .ok_or_else(|| format!("home {sub} needs a device name and a value"))?;
        Ok((name.trim().to_string(), value.trim().to_string()))
    };
    let percent = |value: &str| {
        value
            .trim_end_matches('%')
            .parse::<u32>()
            .ok()
            .filter(|number| *number <= 100)
            .ok_or_else(|| format!("{value:?} is not a percentage"))
    };
    Ok(match sub {
        "" | "status" => HomeCommand::Status,
        "why" => HomeCommand::Why(named()?),
        "toggle" => HomeCommand::Set(named()?, Wanted::Toggle),
        "on" => HomeCommand::Set(named()?, Wanted::Change(Change::Switch(true))),
        "off" => HomeCommand::Set(named()?, Wanted::Change(Change::Switch(false))),
        "select" => HomeCommand::Select(named()?),
        "brightness" => {
            let (name, value) = valued()?;
            HomeCommand::Set(name, Wanted::Change(Change::Brightness(percent(&value)?.max(1))))
        }
        "colour" | "color" | "warmth" => {
            let (name, value) = valued()?;
            HomeCommand::Set(name, Wanted::Change(Change::ColourTemp(percent(&value)?)))
        }
        "target" => {
            let (name, value) = valued()?;
            let degrees = value
                .trim_end_matches('\u{b0}')
                .parse::<f64>()
                .map_err(|_| format!("{value:?} is not a temperature"))?;
            HomeCommand::Set(name, Wanted::Change(Change::Target(degrees)))
        }
        "mode" => {
            let (name, value) = valued()?;
            let mode = match value.to_lowercase().as_str() {
                "cool" => "cold".to_string(),
                "heat" => "hot".to_string(),
                "dry" => "wet".to_string(),
                "fan" => "wind".to_string(),
                other => other.to_string(),
            };
            HomeCommand::Set(name, Wanted::Change(Change::Mode(mode)))
        }
        "fan" => {
            let (name, value) = valued()?;
            let speed = match value.to_lowercase().as_str() {
                "mid" => "middle".to_string(),
                "quiet" => "mute".to_string(),
                other => other.to_string(),
            };
            HomeCommand::Set(name, Wanted::Change(Change::Fan(speed)))
        }
        "all-off" => HomeCommand::AllOff,
        "fold" => HomeCommand::Fold(true),
        "unfold" => HomeCommand::Fold(false),
        "rooms" => HomeCommand::View(false),
        "all" => HomeCommand::View(true),
        "enable" => HomeCommand::Enable(true),
        "disable" => HomeCommand::Enable(false),
        other => return Err(format!("unknown home command {other:?}")),
    })
}

pub const CLI_USAGE: &str = "\
Usage: tileception home <command>

  import      fetch your Tuya devices (names, rooms, local keys) from the
              Tuya IoT Platform once; afterwards they are controlled on the
              LAN only. Asks for the project's Access ID, data center and
              Access Secret. Options: --client-id ID, --region eu|we|us|ue|in|cn|sg,
              --secret-file PATH, --cloud-sensors / --no-cloud-sensors,
              --startup-refresh / --no-startup-refresh,
              --file PATH
  status      connect to the imported devices for a few seconds and list them
              (--file PATH)
  simulate    start simulated devices on localhost and write a device file
              for them (--file PATH); runs until interrupted
";

/// `tileception home ...`.
pub fn run(args: &[String]) -> Result<(), String> {
    let flag = |name: &str| args.iter().position(|arg| arg == name).and_then(|index| args.get(index + 1)).cloned();
    let has = |name: &str| args.iter().any(|arg| arg == name);
    let file = flag("--file").map(PathBuf::from).or_else(store::default_path).ok_or("no config directory")?;
    match args.first().map(String::as_str) {
        Some("import") => import(
            &file,
            flag("--client-id"),
            flag("--region"),
            flag("--secret-file"),
            match () {
                _ if has("--cloud-sensors") => Some(true),
                _ if has("--no-cloud-sensors") => Some(false),
                _ => None,
            },
            match () {
                _ if has("--startup-refresh") => Some(true),
                _ if has("--no-startup-refresh") => Some(false),
                _ => None,
            },
        ),
        Some("status") => status(&file),
        Some("simulate") => simulate(&file),
        None | Some("-h" | "--help" | "help") => {
            print!("{CLI_USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other:?}\n\n{CLI_USAGE}")),
    }
}

fn ask(question: &str) -> Result<String, String> {
    use std::io::Write;
    print!("{question}");
    std::io::stdout().flush().map_err(|err| err.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|err| err.to_string())?;
    Ok(line.trim().to_string())
}

/// Reads a line showing `*` for each character typed: the terminal stops
/// echoing and hands over every key at once; Backspace, Ctrl+U and Ctrl+C
/// work, and the terminal is always put back.
/// Without a terminal (input from a pipe) it reads a plain line.
fn ask_hidden(question: &str) -> Result<zeroize::Zeroizing<String>, String> {
    use std::io::{Read, Write};
    let stty = |arguments: &[&str]| {
        std::process::Command::new("stty")
            .args(arguments)
            .stdin(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let Some(saved) = stty(&["-g"]) else {
        return ask(question).map(zeroize::Zeroizing::new);
    };
    if stty(&["-echo", "-icanon", "-isig", "min", "1"]).is_none() {
        return ask(question).map(zeroize::Zeroizing::new);
    }
    let mut out = std::io::stdout();
    let mut answer = zeroize::Zeroizing::new(String::new());
    let result = (|| {
        write!(out, "{question}").and_then(|()| out.flush()).map_err(|err| err.to_string())?;
        let mut byte = [0u8; 1];
        loop {
            if std::io::stdin().read(&mut byte).map_err(|err| err.to_string())? == 0 {
                break;
            }
            match byte[0] {
                b'\n' | b'\r' => break,
                // Ctrl+C or Ctrl+D: the terminal is put back before leaving.
                0x03 | 0x04 => return Err("cancelled".to_string()),
                // Backspace or Delete.
                0x08 | 0x7f => {
                    if answer.pop().is_some() {
                        write!(out, "\x08 \x08").map_err(|err| err.to_string())?;
                    }
                }
                // Ctrl+U clears the line.
                0x15 => {
                    for _ in 0..answer.chars().count() {
                        write!(out, "\x08 \x08").map_err(|err| err.to_string())?;
                    }
                    answer.clear();
                }
                printable if printable >= 0x20 => {
                    answer.push(char::from(printable));
                    write!(out, "*").map_err(|err| err.to_string())?;
                }
                _ => {}
            }
            out.flush().map_err(|err| err.to_string())?;
        }
        Ok::<(), String>(())
    })();
    stty(&[saved.as_str()]);
    println!();
    result.map(|()| answer)
}

/// Fetch inventory and merge discovered or cached LAN addresses. Callers
/// decide whether to retain credentials and when to save the result.
pub(crate) fn import_devices(
    file: &std::path::Path,
    credentials: tileception_home::cloud::Credentials,
    scan: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<tileception_home::store::DeviceFile, String> {
    let mut imported = tileception_home::cloud::import(credentials, progress)?;
    let heard = if scan {
        progress("Looking for devices on this network (15 seconds)...");
        tileception_home::discovery::scan(Duration::from_secs(15))
    } else {
        progress("Using discovery from the running connection...");
        Vec::new()
    };
    let previous = store::load(file).ok();
    for device in &mut imported.devices {
        let found = heard.iter().find(|(device_id, ..)| *device_id == device.device_id);
        let earlier =
            previous.as_ref().and_then(|old| old.devices.iter().find(|old| old.device_id == device.device_id));
        if let Some((_, host, version)) = found {
            device.host = Some(host.to_string());
            device.version = version.map(|version| version.as_str().to_string());
        } else if let Some(earlier) = earlier {
            device.host = earlier.host.clone();
            device.version = earlier.version.clone();
        }
    }
    Ok(imported)
}

fn import(
    file: &std::path::Path,
    client_id: Option<String>,
    region: Option<String>,
    secret_file: Option<String>,
    cloud_sensors: Option<bool>,
    startup_refresh: Option<bool>,
) -> Result<(), String> {
    println!("Importing devices from the Tuya IoT Platform (iot.tuya.com, Cloud > Development > your project).");
    let client_id = match client_id {
        Some(given) => given,
        None => ask("Access ID: ")?,
    };
    let secret = match secret_file {
        Some(path) => zeroize::Zeroizing::new(
            std::fs::read_to_string(&path).map_err(|err| format!("{path}: {err}"))?.trim().to_string(),
        ),
        None => ask_hidden("Access Secret: ")?,
    };
    if client_id.is_empty() || secret.is_empty() {
        return Err("the Access ID and the Access Secret are both needed".into());
    }
    let region = match region {
        Some(region) => region,
        None => {
            println!("Data centers: {}", tileception_home::cloud::REGIONS);
            ask("Data center (shown on the project's overview): ")?
        }
    };
    if tileception_home::cloud::host(&region).is_none() {
        return Err(format!("unknown data center {region:?}; use {}", tileception_home::cloud::REGIONS));
    }
    let credentials = tileception_home::cloud::Credentials {
        client_id: client_id.clone(),
        secret: secret.clone(),
        region: region.clone(),
    };
    let mut imported = import_devices(file, credentials, true, &mut |line| println!("{line}"))?;
    let cloud_only: Vec<String> = imported
        .devices
        .iter()
        .filter(|device| {
            device.host.is_none() && tileception_home::model::Model::for_entry(device).kind == Kind::Sensor
        })
        .map(|device| device.name.clone())
        .collect();
    let poll_sensors = if cloud_only.is_empty() {
        false
    } else {
        match cloud_sensors {
            Some(choice) => choice,
            None => {
                let (names, verb, unheard, them) = match cloud_only.as_slice() {
                    [one] => (one.clone(), "is a sensor that only reports", "it was", "it"),
                    _ => (cloud_only.join(", "), "are sensors that only report", "they were", "them"),
                };
                println!(
                    "\n{names} {verb} to the cloud ({unheard} not heard on this network). Reading {them} every 10 minutes needs the Access Secret stored in {} (readable by you only).",
                    file.display()
                );
                matches!(ask(&format!("Read {them} from the cloud? [y/N] "))?.to_lowercase().as_str(), "y" | "yes")
            }
        }
    };
    let startup_refresh = match startup_refresh {
        Some(choice) => choice,
        None => {
            if !poll_sensors {
                println!(
                    "\nRefreshing devices, homes and rooms at startup needs the Access Secret stored in {} (readable by you only).",
                    file.display()
                );
            }
            matches!(ask("Refresh devices, homes and rooms at startup? [y/N] ")?.to_lowercase().as_str(), "y" | "yes")
        }
    };
    if poll_sensors || startup_refresh {
        imported.cloud =
            Some(store::CloudAccess { client_id, region, secret: secret.to_string(), poll_sensors, startup_refresh });
    }
    store::save(file, &imported)?;

    println!("\nSaved {} devices to {}:", imported.devices.len(), file.display());
    for device in &imported.devices {
        let place = [device.home.clone(), device.room.clone()].into_iter().flatten().collect::<Vec<_>>().join(", ");
        let found = match (&device.host, &device.version) {
            (Some(host), Some(version)) => format!("on this network at {host}, protocol {version}"),
            (Some(host), None) => format!("last seen at {host}"),
            _ if cloud_only.contains(&device.name) && poll_sensors => "read from the cloud".to_string(),
            _ => "not heard on this network (off, or elsewhere)".to_string(),
        };
        println!("  {}  [{}]  {found}", device.name, if place.is_empty() { "-".into() } else { place });
    }
    println!("\nSet enabled = true under [home] in config.toml to show them in the notification panel (Super+N).");
    Ok(())
}

fn status(file: &std::path::Path) -> Result<(), String> {
    let devices = store::load(file)?;
    let options = Options { discovery: devices.source != "simulated", history: None };
    let service = HomeService::start(&devices, options, || {});
    let until = Instant::now() + Duration::from_secs(12);
    while Instant::now() < until {
        let snapshot = service.snapshot();
        if snapshot.devices.iter().all(|device| !matches!(device.reach, Reach::Connecting)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    for device in service.snapshot().devices {
        let place = [device.home.clone(), device.room.clone()].into_iter().flatten().collect::<Vec<_>>().join(", ");
        let reach = match &device.reach {
            Reach::Online => "online".to_string(),
            Reach::Connecting => "still connecting".to_string(),
            Reach::Cloud { .. } => "cloud".to_string(),
            Reach::Unreachable { why, .. } => format!("unreachable ({why})"),
            Reach::Unsupported(why) => format!("not available ({why})"),
        };
        println!("{}  [{place}]  {reach}  {}", device.name, crate::bundled::home::device_detail(&device));
    }
    Ok(())
}

fn simulate(file: &std::path::Path) -> Result<(), String> {
    let household = tileception_home::sim::household().map_err(|err| err.to_string())?;
    store::save(file, &household.file)?;
    println!("Simulating {} devices; device file: {}", household.file.devices.len(), file.display());
    println!("Point [home] devices at it in a test config. Interrupt to stop.");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Switches one use of the saved cloud access in the device file: polling
/// cloud-only sensors, or refreshing the devices at startup. The other use,
/// the credentials and the devices stay as they are. Returns the new state.
pub(crate) fn switch_cloud_use(path: &std::path::Path, sensors: bool) -> Result<bool, String> {
    let mut file = store::load(path)?;
    let access = file.cloud.as_mut().ok_or("Import devices with saved cloud access first")?;
    let switched = if sensors { &mut access.poll_sensors } else { &mut access.startup_refresh };
    *switched = !*switched;
    let now = *switched;
    store::save(path, &file)?;
    Ok(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_file(dir: &str, cloud: Option<store::CloudAccess>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tileception-home-{dir}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(store::FILE_NAME);
        let file = store::DeviceFile {
            source: "tuya".into(),
            devices: vec![store::DeviceEntry {
                device_id: "lamp".into(),
                name: "Desk lamp".into(),
                key: "0123456789abcdef".into(),
                ..Default::default()
            }],
            cloud,
        };
        store::save(&path, &file).expect("saved");
        path
    }

    #[test]
    fn switching_a_cloud_use_leaves_the_rest_of_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let access = store::CloudAccess {
            client_id: "id".into(),
            region: "eu".into(),
            secret: "secret".into(),
            poll_sensors: true,
            startup_refresh: true,
        };
        let path = device_file("cloud-use", Some(access.clone()));
        let before = store::load(&path).expect("loads");

        assert_eq!(switch_cloud_use(&path, true), Ok(false));
        let after = store::load(&path).expect("loads");
        let cloud = after.cloud.clone().expect("cloud access kept");
        assert!(!cloud.poll_sensors && cloud.startup_refresh);
        assert_eq!((cloud.client_id.as_str(), cloud.region.as_str(), cloud.secret.as_str()), ("id", "eu", "secret"));
        assert_eq!(after.devices, before.devices);

        assert_eq!(switch_cloud_use(&path, false), Ok(false));
        let cloud = store::load(&path).expect("loads").cloud.expect("cloud access kept");
        assert!(!cloud.poll_sensors && !cloud.startup_refresh);

        assert_eq!(switch_cloud_use(&path, true), Ok(true));
        assert_eq!(switch_cloud_use(&path, false), Ok(true));
        assert_eq!(store::load(&path).expect("loads"), before);
        // The file holds the Access Secret: it stays readable by its owner only.
        let mode = std::fs::metadata(&path).expect("exists").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn cloud_uses_need_saved_cloud_access() {
        let path = device_file("no-cloud", None);
        let before = std::fs::read_to_string(&path).expect("reads");
        assert!(switch_cloud_use(&path, true).is_err());
        assert!(switch_cloud_use(&path, false).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("reads"), before);
        assert!(switch_cloud_use(&path.with_file_name("missing.json"), true).is_err());
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn parses_home_commands() {
        assert_eq!(parse(""), Ok(HomeCommand::Status));
        assert_eq!(parse("why Hallway spot"), Ok(HomeCommand::Why("Hallway spot".into())));
        assert_eq!(parse("enable"), Ok(HomeCommand::Enable(true)));
        assert_eq!(parse("disable"), Ok(HomeCommand::Enable(false)));
        assert_eq!(parse("toggle Couch 2"), Ok(HomeCommand::Set("Couch 2".into(), Wanted::Toggle)));
        assert_eq!(
            parse("brightness Couch 2 40%"),
            Ok(HomeCommand::Set("Couch 2".into(), Wanted::Change(Change::Brightness(40))))
        );
        assert_eq!(
            parse("mode Airco cool"),
            Ok(HomeCommand::Set("Airco".into(), Wanted::Change(Change::Mode("cold".into()))))
        );
        assert_eq!(
            parse("target Airco 21.5"),
            Ok(HomeCommand::Set("Airco".into(), Wanted::Change(Change::Target(21.5))))
        );
        assert!(parse("brightness Couch 200").is_err());
        assert!(parse("toggle").is_err());
    }
}
