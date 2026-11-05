//! Listens for the announcements Tuya devices broadcast every few seconds,
//! to follow a device whose address changed. It only listens: nothing is
//! sent. Port 6666 carries plain JSON (3.1), 6667 3.3 frames and 7000 3.5
//! frames, both under a well-known key.

use std::{
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::{
    Shared,
    protocol::{Side, UDP_KEY, Version, decode_frame},
};

const PORTS: [u16; 3] = [6666, 6667, 7000];

/// The listening threads; they end once `stop` is set.
pub(crate) struct Listener {
    _threads: Vec<std::thread::JoinHandle<()>>,
}

impl Listener {
    pub(crate) fn start(shared: Shared, stop: Arc<AtomicBool>) -> Listener {
        let mut threads = Vec::new();
        for port in PORTS {
            let socket = match bind(port) {
                Ok(socket) => socket,
                Err(err) => {
                    // Another program (a home automation server) may hold it.
                    tracing::info!("home: not listening for announcements on {port}: {err}");
                    continue;
                }
            };
            let (shared, stop) = (shared.clone(), stop.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("home-announce-{port}"))
                .spawn(move || listen(&socket, port, &shared, &stop));
            if let Ok(thread) = spawned {
                threads.push(thread);
            }
        }
        Listener { _threads: threads }
    }
}

/// Listens for `duration` and returns every device heard: its id,
/// address and version. Ports another program holds are skipped.
pub fn scan(duration: Duration) -> Vec<(String, IpAddr, Option<Version>)> {
    let sockets: Vec<(u16, UdpSocket)> =
        PORTS.iter().filter_map(|&port| bind(port).ok().map(|socket| (port, socket))).collect();
    let heard = std::sync::Mutex::new(Vec::<(String, IpAddr, Option<Version>)>::new());
    std::thread::scope(|scope| {
        for (port, socket) in &sockets {
            let heard = &heard;
            scope.spawn(move || {
                let until = std::time::Instant::now() + duration;
                let mut buffer = [0; 2048];
                while std::time::Instant::now() < until {
                    let Ok((len, from)) = socket.recv_from(&mut buffer) else { continue };
                    if let Some(found) = announcement(&buffer[..len], *port, from.ip()) {
                        let mut heard = heard.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        heard.retain(|(device_id, ..)| *device_id != found.0);
                        heard.push(found);
                    }
                }
            });
        }
    });
    heard.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn bind(port: u16) -> std::io::Result<UdpSocket> {
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port)))?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    Ok(socket)
}

fn listen(socket: &UdpSocket, port: u16, shared: &Shared, stop: &AtomicBool) {
    let mut buffer = [0; 2048];
    while !stop.load(Ordering::Relaxed) {
        let Ok((len, from)) = socket.recv_from(&mut buffer) else { continue };
        if let Some((device_id, host, version)) = announcement(&buffer[..len], port, from.ip()) {
            shared.heard(&device_id, host, version);
        }
    }
}

/// The device id, address and version in one announcement.
pub(crate) fn announcement(data: &[u8], port: u16, sender: IpAddr) -> Option<(String, IpAddr, Option<Version>)> {
    let payload = if port == 6666 {
        // Unencrypted: the JSON between the 20-byte header and the 8-byte end.
        data.get(20..data.len().checked_sub(8)?)?.to_vec()
    } else {
        let version = if port == 7000 { Version::V35 } else { Version::V33 };
        decode_frame(data, &UDP_KEY, Side::Client, version).ok()??.0.payload
    };
    let text = String::from_utf8_lossy(&payload);
    let json: serde_json::Value = serde_json::from_str(text.trim_matches(char::from(0))).ok()?;
    let device_id = json.get("gwId").and_then(|value| value.as_str())?.to_string();
    let host = json.get("ip").and_then(|value| value.as_str()).and_then(|host| host.parse().ok()).unwrap_or(sender);
    let version = json.get("version").and_then(|value| value.as_str()).and_then(Version::parse);
    Some((device_id, host, version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Codec, command};

    #[test]
    fn announcements_give_id_address_and_version() {
        for (version, port) in [(Version::V33, 6667), (Version::V35, 7000)] {
            let mut codec = Codec::new(version, Side::Client, UDP_KEY);
            let body = br#"{"ip":"192.168.1.20","gwId":"bf0123456789abcdefgh","active":2,"version":"3.3"}"#;
            let frame = codec.encode(command::QUERY, body);
            let (device_id, host, found) = announcement(&frame, port, "10.0.0.1".parse().expect("ip")).expect("parsed");
            assert_eq!(device_id, "bf0123456789abcdefgh");
            assert_eq!(host, "192.168.1.20".parse::<IpAddr>().expect("ip"));
            assert_eq!(found, Some(Version::V33));
        }
    }
}
