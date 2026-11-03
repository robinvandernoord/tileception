//! Simulated Tuya devices that speak the LAN protocol on localhost, for
//! tests and for trying the panel without real devices
//! (`tileception home simulate`).

use std::{
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use serde_json::{Map, Value, json};

use crate::{
    device::reported_dps,
    protocol::{Codec, Side, Version, command},
    store::{DataPoint, DeviceEntry, DeviceFile},
};

type Clients = Arc<Mutex<Vec<mpsc::Sender<Map<String, Value>>>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One simulated device, listening on a port of 127.0.0.1 until dropped.
pub struct SimDevice {
    pub device_id: String,
    pub key: [u8; 16],
    pub version: Version,
    pub port: u16,
    dps: Arc<Mutex<Map<String, Value>>>,
    clients: Clients,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SimDevice {
    pub fn spawn(
        device_id: &str,
        key: [u8; 16],
        version: Version,
        dps: Map<String, Value>,
    ) -> std::io::Result<SimDevice> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let dps = Arc::new(Mutex::new(dps));
        let clients: Clients = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let shared = (device_id.to_string(), dps.clone(), clients.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name(format!("sim-{device_id}"))
            .spawn(move || accept(&listener, key, version, &shared))?;
        Ok(SimDevice { device_id: device_id.to_string(), key, version, port, dps, clients, stop, thread: Some(thread) })
    }

    pub fn dps(&self) -> Map<String, Value> {
        lock(&self.dps).clone()
    }

    /// A change made elsewhere (the wall switch, the app): reported to
    /// every connected client.
    pub fn change(&self, dps: Map<String, Value>) {
        lock(&self.dps).extend(dps.clone());
        lock(&self.clients).retain(|client| client.send(dps.clone()).is_ok());
    }
}

impl Drop for SimDevice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

type Context = (String, Arc<Mutex<Map<String, Value>>>, Clients, Arc<AtomicBool>);

fn accept(listener: &TcpListener, key: [u8; 16], version: Version, context: &Context) {
    let stop = &context.3;
    let mut connections = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let context = context.clone();
                if let Ok(thread) = std::thread::Builder::new()
                    .name(format!("sim-{}-conn", context.0))
                    .spawn(move || serve(stream, key, version, &context))
                {
                    connections.push(thread);
                }
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    for thread in connections {
        let _ = thread.join();
    }
}

fn serve(mut stream: TcpStream, key: [u8; 16], version: Version, context: &Context) {
    let (device_id, dps, clients, stop) = context;
    let (pushes, pushed) = mpsc::channel();
    lock(clients).push(pushes);
    let mut codec = Codec::new(version, Side::Device, key);
    let mut nonces: Option<([u8; 16], [u8; 16])> = None;
    let mut buffer = Vec::new();
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    while !stop.load(Ordering::Relaxed) {
        while let Ok(changed) = pushed.try_recv() {
            let body = status_body(device_id, version, &changed);
            if stream.write_all(&codec.encode(command::STATUS, &body)).is_err() {
                return;
            }
        }
        let mut chunk = [0; 2048];
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(_) => return,
        }
        loop {
            let (frame, used) = match codec.decode(&buffer) {
                Ok(Some(decoded)) => decoded,
                Ok(None) => break,
                // A client with the wrong key gets nothing back, like a real device.
                Err(_) => return,
            };
            buffer.drain(..used);
            let mut replies = Vec::new();
            match frame.command {
                command::SESSION_START if version == Version::V35 => {
                    let (remote, reply) = codec.session_reply(frame.seq, &frame.payload);
                    let mut local = [0; 16];
                    if frame.payload.len() == 16 {
                        local.copy_from_slice(&frame.payload);
                    }
                    nonces = Some((local, remote));
                    replies.push(reply);
                }
                command::SESSION_FINISH => {
                    let Some((local, remote)) = nonces.take() else { return };
                    let Ok(session) = codec.session_accept(&local, &remote, &frame.payload) else { return };
                    codec.set_session_key(session);
                }
                command::QUERY | command::QUERY_NEW => {
                    let body = status_body(device_id, version, &lock(dps));
                    replies.push(codec.encode_seq(frame.seq, frame.command, &body));
                }
                command::CONTROL | command::CONTROL_NEW => {
                    replies.push(codec.encode_seq(frame.seq, frame.command, b""));
                    if let Some(changed) = reported_dps(&frame.payload) {
                        // Data points set to null are a "device22" query.
                        let changed: Map<String, Value> =
                            changed.into_iter().filter(|(_, value)| !value.is_null()).collect();
                        lock(dps).extend(changed.clone());
                        let reported = if changed.is_empty() { lock(dps).clone() } else { changed.clone() };
                        let body = status_body(device_id, version, &reported);
                        replies.push(codec.encode(command::STATUS, &body));
                        // Other clients hear about it too.
                        lock(clients).retain(|client| client.send(changed.clone()).is_ok());
                        while pushed.try_recv().is_ok() {}
                    }
                }
                command::HEARTBEAT => replies.push(codec.encode_seq(frame.seq, command::HEARTBEAT, b"")),
                _ => {}
            }
            for reply in replies {
                if stream.write_all(&reply).is_err() {
                    return;
                }
            }
        }
    }
}

fn status_body(device_id: &str, version: Version, dps: &Map<String, Value>) -> Vec<u8> {
    let body = match version {
        Version::V33 => json!({"devId": device_id, "dps": dps, "t": 0}),
        Version::V35 => json!({"protocol": 4, "t": 0, "data": {"devId": device_id, "dps": dps}}),
    };
    body.to_string().into_bytes()
}

fn mapping(points: &[(&str, &str, &str, Value)]) -> std::collections::BTreeMap<String, DataPoint> {
    points
        .iter()
        .map(|(device_id, code, kind, values)| {
            ((*device_id).to_string(), DataPoint { code: (*code).into(), kind: (*kind).into(), values: values.clone() })
        })
        .collect()
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// A small household like a real one: spots and bulbs on 3.3, an air
/// conditioner on 3.5, a battery sensor that only reports to the cloud and
/// a lamp that is switched off at the wall.
/// Name, room, category, version, data points and their mapping.
type Spec = (
    &'static str,
    &'static str,
    &'static str,
    Version,
    Map<String, Value>,
    std::collections::BTreeMap<String, DataPoint>,
);

pub struct Household {
    pub devices: Vec<SimDevice>,
    pub file: DeviceFile,
}

pub fn household() -> std::io::Result<Household> {
    let light = || {
        mapping(&[
            ("20", "switch_led", "Boolean", Value::Null),
            ("21", "work_mode", "Enum", json!({"range": ["white", "colour", "scene", "music"]})),
            ("22", "bright_value_v2", "Integer", json!({"min": 10, "max": 1000, "scale": 0, "step": 1})),
            ("23", "temp_value_v2", "Integer", json!({"min": 0, "max": 1000, "scale": 0, "step": 1})),
        ])
    };
    let airco = mapping(&[
        ("1", "switch", "Boolean", Value::Null),
        ("2", "temp_set", "Integer", json!({"min": 160, "max": 300, "scale": 1, "step": 5, "unit": "C"})),
        ("3", "temp_current", "Integer", json!({"min": -200, "max": 600, "scale": 1, "step": 1, "unit": "C"})),
        ("4", "mode", "Enum", json!({"range": ["auto", "cold", "hot", "wet", "wind"]})),
        ("5", "fan_speed_enum", "Enum", json!({"range": ["strong", "auto", "low", "middle", "high", "mute"]})),
    ]);
    let sensor = mapping(&[
        ("1", "va_temperature", "Integer", json!({"min": -200, "max": 600, "scale": 1, "step": 1})),
        ("2", "va_humidity", "Integer", json!({"min": 0, "max": 100, "scale": 0, "step": 1})),
    ]);
    let lit = |switched: bool, bright: i64, temp: i64| {
        object(json!({"20": switched, "21": "white", "22": bright, "23": temp}))
    };
    let specs: Vec<Spec> = vec![
        ("Ceiling spots", "Living room", "dj", Version::V33, lit(true, 700, 200), light()),
        ("Floor lamp", "Living room", "dj", Version::V33, lit(false, 1000, 0), light()),
        (
            "Airco",
            "Living room",
            "kt",
            Version::V35,
            object(json!({"1": true, "2": 210, "3": 235, "4": "cold", "5": "auto"})),
            airco,
        ),
        ("Desk lamp", "Office", "dj", Version::V33, lit(true, 400, 500), light()),
        ("Bedside lamp", "Bedroom", "dj", Version::V33, lit(false, 300, 0), light()),
    ];
    let mut devices = Vec::new();
    let mut file = DeviceFile { source: "simulated".into(), devices: Vec::new(), cloud: None };
    for (index, (name, room, category, version, dps, points)) in specs.into_iter().enumerate() {
        let device_id = format!("sim{index:02}tileceptionhome");
        let key: [u8; 16] = *format!("simulatedkey{index:04}").as_bytes().first_chunk().unwrap_or(&[b'0'; 16]);
        let device = SimDevice::spawn(&device_id, key, version, dps)?;
        file.devices.push(DeviceEntry {
            device_id,
            name: name.into(),
            home: Some("Simulated home".into()),
            room: Some(room.into()),
            category: category.into(),
            key: String::from_utf8_lossy(&key).into_owned(),
            host: Some("127.0.0.1".into()),
            port: Some(device.port),
            version: Some(version.as_str().into()),
            mapping: points,
            ..Default::default()
        });
        devices.push(device);
    }
    // Switched off at the wall: nothing listens on its port.
    let closed = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
    file.devices.push(DeviceEntry {
        device_id: "sim90tileceptionhome".into(),
        name: "Hallway spot".into(),
        home: Some("Simulated home".into()),
        room: Some("Bedroom".into()),
        category: "dj".into(),
        key: "simulatedkey0090".into(),
        host: Some("127.0.0.1".into()),
        port: Some(closed),
        version: Some("3.3".into()),
        mapping: light(),
        ..Default::default()
    });
    file.devices.push(DeviceEntry {
        device_id: "sim91tileceptionhome".into(),
        name: "T&H sensor".into(),
        home: Some("Simulated home".into()),
        room: Some("Bedroom".into()),
        category: "wsdcg".into(),
        key: "simulatedkey0091".into(),
        mapping: sensor,
        ..Default::default()
    });
    Ok(Household { devices, file })
}
