//! The display backlight: read from sysfs, written through logind so no
//! special permissions are needed.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{changed, lock, Backlight, Change, Event, Notifier, Percent, Shared};

const ROOT: &str = "/sys/class/backlight";
/// Changes made elsewhere (a firmware hotkey, `brightnessctl`) show up within this.
const POLL: Duration = Duration::from_secs(2);
/// Never go fully dark: some panels switch off at 0.
const MIN: Percent = 1;

pub struct Worker {
    requests: mpsc::Sender<Option<Change>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn start(shared: Shared, notifier: Notifier) -> Self {
        let (requests, incoming) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("tileception-backlight".into())
            .spawn(move || run(Path::new(ROOT), &shared, &notifier, &incoming))
            .map_err(|e| tracing::error!("could not start the backlight worker: {e}"))
            .ok();
        Self { requests, thread }
    }

    pub fn send(&self, change: Change) {
        let _ = self.requests.send(Some(change));
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.requests.send(None);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The backlight to control: firmware interfaces know the panel best, then
/// platform drivers, then raw GPU registers (the order GNOME and systemd use).
pub fn pick_device(root: &Path) -> Option<PathBuf> {
    let rank = |kind: &str| match kind.trim() {
        "firmware" => 0,
        "platform" => 1,
        "raw" => 2,
        _ => 3,
    };
    let mut found: Vec<(i32, PathBuf)> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| read_number(&path.join("max_brightness")).is_some_and(|max| max > 0))
        .map(|path| (std::fs::read_to_string(path.join("type")).map_or(3, |kind| rank(&kind)), path))
        .collect();
    found.sort();
    found.into_iter().next().map(|(_, path)| path)
}

fn read_number(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn name(device: &Path) -> String {
    device.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The device's level now, as a percentage of its range.
fn level(device: &Path) -> Option<(Percent, u32)> {
    let max = read_number(&device.join("max_brightness")).filter(|m| *m > 0)?;
    let now = read_number(&device.join("brightness"))?;
    let percent = (u64::from(now) * 100 + u64::from(max) / 2) / u64::from(max);
    Some((percent as Percent, max))
}

fn run(root: &Path, shared: &Shared, notifier: &Notifier, incoming: &mpsc::Receiver<Option<Change>>) {
    let device = pick_device(root);
    match &device {
        Some(device) => tracing::info!("backlight: {}", device.display()),
        None => tracing::info!("no adjustable backlight"),
    }
    let mut logind = None;
    loop {
        if let Some(device) = &device {
            let current = level(device).map(|(percent, _)| Backlight { device: name(device), percent });
            let mut snapshot = lock(shared);
            if snapshot.backlight != current {
                snapshot.backlight = current;
                drop(snapshot);
                notifier.send(Event::Backlight);
            }
        }
        let change = match incoming.recv_timeout(POLL) {
            Ok(Some(change)) => change,
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
        };
        let Some(device) = &device else {
            // Nothing changes, but the compositor still hears back.
            notifier.send(Event::Backlight);
            continue;
        };
        let Some((current, max)) = level(device) else { continue };
        let next = changed(current, change, 100).max(MIN);
        let value = ((u64::from(next) * u64::from(max) + 50) / 100).max(1) as u32;
        if let Err(error) = set(&mut logind, device, value) {
            tracing::warn!("could not set the brightness: {error}");
        }
    }
}

fn set(logind: &mut Option<zbus::blocking::Connection>, device: &Path, value: u32) -> Result<(), String> {
    let via_logind = (|| {
        if logind.is_none() {
            *logind = Some(zbus::blocking::Connection::system()?);
        }
        let Some(connection) = logind.as_ref() else { return Ok(()) };
        connection.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1/session/auto",
            Some("org.freedesktop.login1.Session"),
            "SetBrightness",
            &("backlight", name(device), value),
        )?;
        Ok::<_, zbus::Error>(())
    })();
    match via_logind {
        Ok(()) => Ok(()),
        Err(error) => {
            *logind = None;
            // Writable directly when a udev rule allows it.
            std::fs::write(device.join("brightness"), value.to_string())
                .map_err(|e| format!("logind: {error}; sysfs: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(root: &Path, name: &str, kind: &str, max: u32, now: u32) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("create fake device");
        std::fs::write(dir.join("type"), format!("{kind}\n")).expect("write type");
        std::fs::write(dir.join("max_brightness"), format!("{max}\n")).expect("write max");
        std::fs::write(dir.join("brightness"), format!("{now}\n")).expect("write brightness");
    }

    #[test]
    fn prefers_firmware_then_platform_then_raw() {
        let root = std::env::temp_dir().join(format!("tileception-backlight-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(pick_device(&root), None);
        fake(&root, "intel_backlight", "raw", 96000, 48000);
        assert_eq!(pick_device(&root).as_deref(), Some(root.join("intel_backlight").as_path()));
        fake(&root, "thinkpad_screen", "platform", 15, 15);
        assert_eq!(pick_device(&root).as_deref(), Some(root.join("thinkpad_screen").as_path()));
        fake(&root, "acpi_video0", "firmware", 100, 30);
        fake(&root, "broken", "firmware", 0, 0);
        assert_eq!(pick_device(&root).as_deref(), Some(root.join("acpi_video0").as_path()));
        assert_eq!(level(&root.join("intel_backlight")), Some((50, 96000)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
