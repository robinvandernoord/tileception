//! Firmware updates from fwupd. They are listed with the rest but never
//! ticked by default: a failed flash can leave a device unusable, so each
//! is chosen on purpose.

use serde_json::Value;

use crate::Runner;
use crate::apt::{Kind, Source, Update};

/// `fwupdmgr get-updates --json`: each device with the releases newer than
/// what it runs, the newest first. One update per device.
pub fn parse(text: &str) -> Vec<Update> {
    let Ok(root) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let Some(devices) = root.get("Devices").and_then(Value::as_array) else { return Vec::new() };
    devices
        .iter()
        .filter_map(|device| {
            let text =
                |value: &Value, key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
            let device_id = text(device, "DeviceId");
            let release = device.get("Releases").and_then(Value::as_array)?.first()?;
            if !valid_device(&device_id) {
                return None;
            }
            let vendor = text(device, "Vendor");
            let name = text(device, "Name");
            let title = if vendor.is_empty() || name.starts_with(&vendor) { name } else { format!("{vendor} {name}") };
            let remote = text(release, "RemoteId");
            Some(Update {
                name: device_id,
                source: Source::Firmware,
                title,
                summary: text(release, "Summary"),
                old: Some(text(device, "Version")).filter(|version| !version.is_empty()),
                new: text(release, "Version"),
                origin: if remote.is_empty() { "fwupd".into() } else { format!("fwupd, {remote}") },
                kind: Kind::Firmware,
                size: release.get("Size").and_then(Value::as_u64).unwrap_or(0),
                ..Update::default()
            })
        })
        .collect()
}

/// The release notes of a device's newest firmware, as plain text.
pub fn parse_notes(text: &str, device: &str) -> Option<String> {
    let root: Value = serde_json::from_str(text).ok()?;
    let found = root
        .get("Devices")?
        .as_array()?
        .iter()
        .find(|candidate| candidate.get("DeviceId").and_then(Value::as_str) == Some(device))?;
    let release = found.get("Releases")?.as_array()?.first()?;
    let field = |key: &str| release.get(key).and_then(Value::as_str).unwrap_or_default();
    let mut notes = format!("{} {}\n", field("Name"), field("Version")).trim_start().to_string();
    if !field("Summary").is_empty() {
        notes.push_str(field("Summary"));
        notes.push('\n');
    }
    notes.push('\n');
    notes.push_str(&plain(field("Description")));
    Some(notes)
}

/// fwupd's release descriptions are a little HTML: paragraphs and lists.
fn plain(html: &str) -> String {
    let spaced = html.replace("<li>", "\n  - ").replace("</p>", "\n\n").replace("</ul>", "\n").replace("</ol>", "\n");
    let mut text = String::new();
    let mut inside = false;
    for character in spaced.chars() {
        match character {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => text.push(character),
            _ => {}
        }
    }
    let text = text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"");
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim().to_string()
}

/// Whether this is a device ID as fwupd makes them: forty hex digits.
pub fn valid_device(device: &str) -> bool {
    device.len() == 40 && device.chars().all(|character| character.is_ascii_hexdigit())
}

/// The firmware waiting for devices here. None where fwupd is not
/// installed. fwupd reads metadata it already has, so this needs no network.
pub fn list(runner: &dyn Runner) -> Option<Vec<Update>> {
    if !std::path::Path::new("/usr/bin/fwupdmgr").exists() {
        return None;
    }
    // With nothing to update, fwupdmgr ends with status 2.
    let text = runner
        .run("fwupdmgr", &["get-updates", "--json", "--no-unreported-check", "--no-metadata-check"])
        .unwrap_or_default();
    Some(parse(&text))
}

/// What the newest firmware for a device changes.
pub fn notes(runner: &dyn Runner, device: &str) -> Result<String, String> {
    if !valid_device(device) {
        return Err("not a device".into());
    }
    let text = runner.run("fwupdmgr", &["get-updates", "--json", "--no-unreported-check", "--no-metadata-check"])?;
    parse_notes(&text, device).ok_or_else(|| "fwupd no longer offers firmware for this device".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPDATES: &str = r#"{
  "Devices" : [
    {
      "Name" : "UEFI dbx",
      "DeviceId" : "362301da643102b9f38477387e2193e57abaa590",
      "Vendor" : "Lenovo",
      "Version" : "217",
      "Releases" : [
        {
          "Name" : "Secure Boot dbx",
          "Version" : "371",
          "Summary" : "UEFI Secure Boot Forbidden Signature Database",
          "Description" : "<p>This updates the dbx.</p><ul><li>Revokes old &amp; broken loaders</li></ul>",
          "RemoteId" : "lvfs",
          "Size" : 13501
        },
        { "Version" : "300" }
      ]
    },
    { "Name" : "No releases", "DeviceId" : "0000000000000000000000000000000000000000" },
    { "Name" : "Bad id", "DeviceId" : "--force", "Releases" : [ { "Version" : "2" } ] }
  ]
}"#;

    #[test]
    fn devices_become_updates() {
        let updates = parse(UPDATES);
        assert_eq!(updates.len(), 1);
        let update = &updates[0];
        assert_eq!(update.label(), "Lenovo UEFI dbx");
        assert_eq!(update.old.as_deref(), Some("217"));
        assert_eq!(update.new, "371");
        assert_eq!(update.origin, "fwupd, lvfs");
        assert_eq!(update.size, 13501);
        assert_eq!(update.source, Source::Firmware);
        assert!(parse("No updates available").is_empty());
    }

    #[test]
    fn notes_are_plain_text() {
        let notes = parse_notes(UPDATES, "362301da643102b9f38477387e2193e57abaa590").unwrap_or_default();
        assert!(notes.starts_with("Secure Boot dbx 371\nUEFI Secure Boot"), "{notes}");
        assert!(notes.contains("This updates the dbx."));
        assert!(notes.contains("  - Revokes old & broken loaders"));
        assert_eq!(parse_notes(UPDATES, "elsewhere"), None);
    }
}
