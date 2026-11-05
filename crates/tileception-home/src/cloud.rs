//! Imports and startup inventory refreshes from Tuya's cloud: devices with
//! their names, rooms, local keys and data point mappings. Device controls
//! use the LAN; cloud-only sensors are polled separately.

use std::{collections::BTreeMap, time::Duration};

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::store::{DataPoint, DeviceEntry, DeviceFile};

/// What the Tuya IoT Platform project gives: its Access ID, Access Secret
/// and data center.
pub struct Credentials {
    pub client_id: String,
    pub secret: Zeroizing<String>,
    pub region: String,
}

/// The API host of a data center, by the short names Tuya's tools use.
pub fn host(region: &str) -> Option<&'static str> {
    Some(match region.trim().to_ascii_lowercase().as_str() {
        "eu" | "central-europe" => "openapi.tuyaeu.com",
        "we" | "eu-w" | "western-europe" => "openapi-weaz.tuyaeu.com",
        "us" | "az" | "western-america" => "openapi.tuyaus.com",
        "ue" | "us-e" | "eastern-america" => "openapi-ueaz.tuyaus.com",
        "in" | "india" => "openapi.tuyain.com",
        "cn" | "china" => "openapi.tuyacn.com",
        "sg" | "singapore" => "openapi-sg.iotbing.com",
        _ => return None,
    })
}

pub const REGIONS: &str = "eu (Central Europe), we (Western Europe), us, ue (Eastern America), in, cn or sg";

struct Client {
    agent: ureq::Agent,
    host: &'static str,
    credentials: Credentials,
    token: Option<Zeroizing<String>>,
}

impl Client {
    fn optional_get(&self, path: &str, required: bool) -> Result<Option<Value>, String> {
        match self.get(path) {
            Ok(value) => Ok(Some(value)),
            Err(err) if required => Err(err),
            Err(_) => Ok(None),
        }
    }

    /// Signs and sends a GET; `path` includes its query, keys sorted.
    fn get(&self, path: &str) -> Result<Value, String> {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis())
            .to_string();
        let empty_body = hex(&Sha256::digest(b""));
        let token = self.token.as_deref().map_or("", String::as_str);
        let to_sign = format!("{}{token}{time}GET\n{empty_body}\n\n{path}", self.credentials.client_id);
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(self.credentials.secret.as_bytes()).map_err(|err| err.to_string())?;
        mac.update(to_sign.as_bytes());
        let sign = hex(&mac.finalize().into_bytes()).to_ascii_uppercase();
        let mut request = self
            .agent
            .get(format!("https://{}{path}", self.host))
            .header("client_id", &self.credentials.client_id)
            .header("sign", &sign)
            .header("t", &time)
            .header("sign_method", "HMAC-SHA256");
        if let Some(token) = &self.token {
            request = request.header("access_token", token.as_str());
        }
        let mut response = request.call().map_err(|err| format!("{}: {err}", self.host))?;
        let value: Value = response.body_mut().read_json().map_err(|err| format!("{}: {err}", self.host))?;
        if value.get("success").and_then(Value::as_bool) == Some(true) {
            Ok(value.get("result").cloned().unwrap_or(Value::Null))
        } else {
            let code = value.get("code").map(Value::to_string).unwrap_or_default();
            let message = value.get("msg").and_then(Value::as_str).unwrap_or("unknown error");
            Err(format!("Tuya refused {}: {message} (code {code})", path.split('?').next().unwrap_or(path)))
        }
    }
}

impl Client {
    /// Signs in with the project's credentials.
    fn sign_in(credentials: Credentials) -> Result<Client, String> {
        let host = host(&credentials.region).ok_or_else(|| format!("unknown data center; use {REGIONS}"))?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut client = Client { agent, host, credentials, token: None };
        let token = client.get("/v1.0/token?grant_type=1")?;
        client.token = text(&token, "access_token").map(Zeroizing::new);
        if client.token.is_none() {
            return Err("Tuya gave no access token".into());
        }
        Ok(client)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::trim).filter(|found| !found.is_empty()).map(str::to_string)
}

/// Fetches every device linked to the project. `progress` hears what is
/// happening, for the command line.
pub fn import(credentials: Credentials, progress: &mut dyn FnMut(&str)) -> Result<DeviceFile, String> {
    fetch(credentials, progress, false)
}

/// Refreshes the inventory using credentials already retained by the user.
/// Every request must succeed before replacing the cache. LAN addresses and
/// protocol versions survive the cloud import, which does not supply them.
pub fn refresh(cached: &DeviceFile) -> Result<DeviceFile, String> {
    let access = cached.cloud.as_ref().ok_or("no saved Tuya credentials")?;
    let credentials = Credentials {
        client_id: access.client_id.clone(),
        secret: Zeroizing::new(access.secret.clone()),
        region: access.region.clone(),
    };
    let mut refreshed = fetch(credentials, &mut |_| {}, true)?;
    refreshed.cloud = cached.cloud.clone();
    for device in &mut refreshed.devices {
        if let Some(previous) = cached.devices.iter().find(|old| old.device_id == device.device_id) {
            device.host = previous.host.clone();
            device.version = previous.version.clone();
            // Some listing endpoints omit keys even on successful requests.
            if device.key.is_empty() {
                device.key = previous.key.clone();
            }
        }
    }
    Ok(refreshed)
}

fn fetch(credentials: Credentials, progress: &mut dyn FnMut(&str), strict: bool) -> Result<DeviceFile, String> {
    progress("Signing in to Tuya...");
    let client = Client::sign_in(credentials)?;

    progress("Listing devices...");
    let mut listed = Vec::new();
    let mut row_key: Option<String> = None;
    loop {
        let path = match &row_key {
            Some(key) => format!("/v1.0/iot-01/associated-users/devices?last_row_key={key}&size=50"),
            None => "/v1.0/iot-01/associated-users/devices?size=50".to_string(),
        };
        let page = client.get(&path)?;
        listed.extend(page.get("devices").and_then(Value::as_array).cloned().unwrap_or_default());
        row_key = text(&page, "last_row_key");
        if page.get("has_more").and_then(Value::as_bool) != Some(true) || row_key.is_none() {
            break;
        }
    }
    if listed.is_empty() {
        return Err(
            "no devices are linked to this project: link your app account under Devices, Link App Account".into()
        );
    }

    // The per-user listing has local keys and gateway ids the other may lack.
    let mut details: BTreeMap<String, Value> = BTreeMap::new();
    let users: std::collections::BTreeSet<String> = listed.iter().filter_map(|device| text(device, "uid")).collect();
    for uid in users {
        let mut row_key: Option<String> = None;
        loop {
            let mut path = format!("/v1.3/iot-03/devices?page_size=75&source_id={uid}&source_type=tuyaUser");
            if let Some(key) = &row_key {
                path = format!(
                    "/v1.3/iot-03/devices?last_row_key={key}&page_size=75&source_id={uid}&source_type=tuyaUser"
                );
            }
            let Some(page) = client.optional_get(&path, strict)? else { break };
            for device in page.get("list").and_then(Value::as_array).into_iter().flatten() {
                if let Some(device_id) = text(device, "id") {
                    details.insert(device_id, device.clone());
                }
            }
            row_key = text(&page, "last_row_key");
            if page.get("has_more").and_then(Value::as_bool) != Some(true) || row_key.is_none() {
                break;
            }
        }
    }

    progress("Reading homes and rooms...");
    let mut rooms: BTreeMap<String, String> = BTreeMap::new();
    let mut home_names: BTreeMap<String, String> = BTreeMap::new();
    let homes: std::collections::BTreeSet<String> =
        listed.iter().filter_map(|device| text(device, "owner_id")).collect();
    for home in homes {
        if let Some(found) = client.optional_get(&format!("/v1.0/homes/{home}"), strict)?
            && let Some(name) = text(&found, "name")
        {
            home_names.insert(home.clone(), name);
        }
        let Some(list) = client.optional_get(&format!("/v1.0/homes/{home}/rooms"), strict)? else {
            progress("Rooms are not available (is Smart Home Basic Service subscribed?); devices stay in one list.");
            break;
        };
        for room in list.get("rooms").and_then(Value::as_array).into_iter().flatten() {
            let (Some(room_id), Some(name)) = (room.get("room_id").map(Value::to_string), text(room, "name")) else {
                continue;
            };
            let room_id = room_id.trim_matches('"').to_string();
            if let Some(Value::Array(devices)) =
                client.optional_get(&format!("/v1.0/homes/{home}/rooms/{room_id}/devices"), strict)?
            {
                for device in devices {
                    if let Some(device_id) = text(&device, "id") {
                        rooms.insert(device_id, name.clone());
                    }
                }
            }
        }
    }

    progress("Reading data points...");
    let mut mappings: BTreeMap<String, BTreeMap<String, DataPoint>> = BTreeMap::new();
    let mut devices = Vec::new();
    for device in &listed {
        let Some(device_id) = text(device, "id") else { continue };
        let detail = details.get(&device_id).unwrap_or(device);
        let product = text(device, "product_id").unwrap_or_default();
        let mapping = match mappings.get(&product) {
            Some(mapping) => mapping.clone(),
            None => {
                let mapping = client
                    .optional_get(&format!("/v1.1/devices/{device_id}/specifications"), strict)?
                    .map(|spec| specification(&spec))
                    .unwrap_or_default();
                if !product.is_empty() {
                    mappings.insert(product.clone(), mapping.clone());
                }
                mapping
            }
        };
        devices.push(DeviceEntry {
            name: text(device, "name").unwrap_or_else(|| device_id.clone()),
            home: text(device, "owner_id").and_then(|home| home_names.get(&home).cloned()),
            room: rooms.get(&device_id).cloned(),
            category: text(device, "category").unwrap_or_default(),
            product: text(device, "product_name").unwrap_or_default(),
            key: text(detail, "local_key").or_else(|| text(device, "local_key")).unwrap_or_default(),
            gateway: text(detail, "gateway_id").filter(|gateway| *gateway != device_id),
            node_id: text(detail, "node_id").filter(|node| *node != device_id),
            mapping,
            device_id,
            ..Default::default()
        });
    }
    Ok(DeviceFile { source: "tuya".into(), devices, cloud: None })
}

/// How often cloud-only devices are read.
const POLL_INTERVAL: Duration = Duration::from_secs(600);

/// A cloud-only device to read.
pub(crate) struct Polled {
    pub slot: usize,
    pub device_id: String,
    pub model: crate::model::Model,
    /// Data point numbers by code, as the cloud reports codes.
    pub points: BTreeMap<String, String>,
}

/// Reads cloud-only devices every ten minutes, on its own thread.
pub(crate) fn start_polling(
    access: crate::store::CloudAccess,
    polled: Vec<Polled>,
    shared: crate::Shared,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    use std::sync::atomic::Ordering;
    std::thread::Builder::new()
        .name("home-cloud".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let credentials = Credentials {
                    client_id: access.client_id.clone(),
                    secret: Zeroizing::new(access.secret.clone()),
                    region: access.region.clone(),
                };
                match Client::sign_in(credentials) {
                    Ok(client) => {
                        for device in &polled {
                            match client.get(&format!("/v1.0/iot-03/devices/{}/status", device.device_id)) {
                                Ok(Value::Array(status)) => {
                                    let dps = status
                                        .iter()
                                        .filter_map(|item| {
                                            let point = device.points.get(&text(item, "code")?)?;
                                            Some((point.clone(), item.get("value")?.clone()))
                                        })
                                        .collect();
                                    shared.apply(device.slot, &device.model, &dps, false);
                                    shared.set_reach(
                                        device.slot,
                                        crate::Reach::Cloud { updated: Some(std::time::SystemTime::now()) },
                                    );
                                }
                                Ok(_) => {}
                                Err(err) => tracing::info!("home: reading {} from the cloud: {err}", device.device_id),
                            }
                        }
                    }
                    Err(err) => tracing::info!("home: cloud sign-in: {err}"),
                }
                let until = std::time::Instant::now() + POLL_INTERVAL;
                while std::time::Instant::now() < until && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        })
        .ok()
}

/// The data points in a specification: status and functions, by number.
fn specification(spec: &Value) -> BTreeMap<String, DataPoint> {
    let mut mapping = BTreeMap::new();
    for list in ["functions", "status"] {
        for point in spec.get(list).and_then(Value::as_array).into_iter().flatten() {
            let Some(code) = text(point, "code") else { continue };
            let device_id = point.get("dp_id").map(Value::to_string).unwrap_or_else(|| code.clone());
            let device_id = device_id.trim_matches('"').to_string();
            if mapping.contains_key(&device_id) {
                continue;
            }
            let kind = text(point, "type").unwrap_or_default();
            let values = match point.get("values") {
                Some(Value::String(json)) => serde_json::from_str(json).unwrap_or(Value::Null),
                Some(other) => other.clone(),
                None => Value::Null,
            };
            mapping.insert(device_id, DataPoint { code, kind, values });
        }
    }
    mapping
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specifications_become_mappings() {
        let spec = serde_json::json!({
            "functions": [
                {"code": "switch_led", "dp_id": 20, "type": "Boolean", "values": "{}"},
                {"code": "bright_value_v2", "dp_id": 22, "type": "Integer",
                 "values": "{\"min\":10,\"max\":1000,\"scale\":0,\"step\":1}"}
            ],
            "status": [{"code": "switch_led", "dp_id": 20, "type": "Boolean", "values": "{}"}]
        });
        let mapping = specification(&spec);
        assert_eq!(mapping.len(), 2);
        assert_eq!(mapping["22"].code, "bright_value_v2");
        assert_eq!(mapping["22"].values["max"], 1000);
    }

    #[test]
    fn regions_have_hosts() {
        assert_eq!(host("EU"), Some("openapi.tuyaeu.com"));
        assert_eq!(host("we"), Some("openapi-weaz.tuyaeu.com"));
        assert_eq!(host("mars"), None);
    }
}
