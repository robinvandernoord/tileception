//! Local control of Tuya smart home devices, off the compositor thread.
//!
//! Devices come from the file the cloud import writes ([`store`]): names,
//! rooms, local keys and data point mappings. After that everything stays on
//! the LAN. A worker thread per device keeps a connection open and updates
//! its part of a shared [`Snapshot`]; a listener picks up the devices'
//! announcements to follow address changes. Each change calls the `wake`
//! callback, after which the compositor reads the snapshot.

pub mod cloud;
mod device;
pub mod discovery;
pub mod model;
pub mod protocol;
pub mod sim;
pub mod store;

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use model::Model;
pub use model::{Change, Kind, State};
use protocol::Version;
use serde_json::{Map, Value};
use store::{DeviceEntry, DeviceFile};

/// Whether a device answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Reach {
    Connecting,
    Online,
    /// Since when, and why, for the panel and the log.
    Unreachable {
        since: SystemTime,
        why: String,
    },
    /// Read from the cloud, last at this time (battery sensors that only
    /// wake to report there).
    Cloud {
        updated: Option<SystemTime>,
    },
    /// Not reachable at all, such as a device without a local key.
    Unsupported(String),
}

/// A device as the panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub device_id: String,
    pub name: String,
    pub home: Option<String>,
    pub room: Option<String>,
    pub kind: Kind,
    pub state: State,
    pub reach: Reach,
    /// A change was sent and the device has not confirmed it yet.
    pub pending: bool,
    pub dimmable: bool,
    pub tunable: bool,
    /// Lowest, highest and step of the target temperature.
    pub target_range: Option<(f64, f64, f64)>,
    pub modes: Vec<String>,
    pub fan_speeds: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// In the file's order, which follows the Tuya app.
    pub devices: Vec<Device>,
}

/// A request from the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Set {
        device_id: String,
        change: Change,
    },
    /// Switches off every light and plug that is on, never climate devices.
    AllOff,
}

#[derive(Default)]
struct Inner {
    snapshot: Snapshot,
    /// Addresses and versions heard in announcements, by device id.
    heard: HashMap<String, (IpAddr, Option<Version>)>,
}

/// The state the workers share, with the compositor's wake-up call.
#[derive(Clone)]
pub(crate) struct Shared {
    inner: Arc<Mutex<Inner>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn changed(&self, slot: usize, change: impl FnOnce(&mut Device)) {
        let differs = {
            let mut inner = self.lock();
            let Some(device) = inner.snapshot.devices.get_mut(slot) else { return };
            let before = device.clone();
            change(device);
            *device != before
        };
        if differs {
            (self.wake)();
        }
    }

    /// Where a device is: its announced address before the stored one.
    pub(crate) fn address(&self, entry: &DeviceEntry) -> Option<(IpAddr, Option<Version>)> {
        let stored_version = entry.version.as_deref().and_then(Version::parse);
        if let Some((host, version)) = self.lock().heard.get(&entry.device_id).copied() {
            return Some((host, version.or(stored_version)));
        }
        let host = entry.host.as_deref()?.parse().ok()?;
        Some((host, stored_version))
    }

    pub(crate) fn heard(&self, device_id: &str, host: IpAddr, version: Option<Version>) {
        self.lock().heard.insert(device_id.to_string(), (host, version));
    }

    pub(crate) fn set_reach(&self, slot: usize, reach: Reach) {
        self.changed(slot, |device| {
            // An outage keeps the time it started.
            if let (Reach::Unreachable { since: old, .. }, Reach::Unreachable { why, .. }) = (&device.reach, &reach) {
                device.reach = Reach::Unreachable { since: *old, why: why.clone() };
                device.pending = false;
                return;
            }
            device.reach = reach;
            if device.reach != Reach::Online {
                device.pending = false;
            }
        });
    }

    /// Takes data points into a device's state; `sent` marks them pending
    /// until the device confirms.
    pub(crate) fn apply(&self, slot: usize, model: &Model, dps: &Map<String, Value>, sent: bool) {
        self.changed(slot, |device| {
            device.state.update(model, dps);
            device.pending = sent;
        });
    }

    pub(crate) fn state(&self, slot: usize) -> State {
        self.lock().snapshot.devices.get(slot).map(|device| device.state.clone()).unwrap_or_default()
    }

    pub(crate) fn settle(&self, slot: usize) {
        self.changed(slot, |device| device.pending = false);
    }
}

/// Owns the workers. Dropping it stops them.
pub struct HomeService {
    shared: Shared,
    workers: Vec<(String, device::Worker)>,
    stop: Arc<AtomicBool>,
    _poller: Option<std::thread::JoinHandle<()>>,
    _discovery: Option<discovery::Listener>,
}

/// Options for [`HomeService::start`].
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Listen for the devices' announcements on the LAN, to follow address
    /// changes. Off for simulated devices and tests.
    pub discovery: bool,
}

impl HomeService {
    pub fn start(file: &DeviceFile, options: Options, wake: impl Fn() + Send + Sync + 'static) -> HomeService {
        let shared = Shared { inner: Arc::default(), wake: Arc::new(wake) };
        let stop = Arc::new(AtomicBool::new(false));
        let mut targets = Vec::new();
        let mut polled = Vec::new();
        {
            let mut inner = shared.lock();
            for (slot, entry) in file.devices.iter().enumerate() {
                let model = Model::for_entry(entry);
                let key = protocol::parse_key(&entry.key);
                let reach = if entry.gateway.is_some() {
                    Reach::Unsupported("behind a gateway".into())
                } else if key.is_none() {
                    Reach::Unsupported("no local key".into())
                } else if entry.host.is_none() && model.kind == Kind::Sensor {
                    if file.cloud.is_some() {
                        polled.push(cloud::Polled {
                            slot,
                            device_id: entry.device_id.clone(),
                            model: model.clone(),
                            points: entry
                                .mapping
                                .iter()
                                .map(|(point, data)| (data.code.clone(), point.clone()))
                                .collect(),
                        });
                        Reach::Cloud { updated: None }
                    } else {
                        Reach::Unsupported("reports to the cloud only".into())
                    }
                } else {
                    Reach::Connecting
                };
                if let (Reach::Connecting, Some(key)) = (&reach, key) {
                    targets.push(device::Target { entry: entry.clone(), model: model.clone(), key, slot });
                }
                inner.snapshot.devices.push(Device {
                    device_id: entry.device_id.clone(),
                    name: entry.name.clone(),
                    room: entry.room.clone().filter(|room| !room.trim().is_empty()),
                    kind: model.kind,
                    state: State::default(),
                    reach,
                    pending: false,
                    dimmable: model.brightness.is_some(),
                    tunable: model.colour_temp.is_some(),
                    target_range: model
                        .target
                        .as_ref()
                        .map(|range| (range.real(range.min), range.real(range.max), range.real_step())),
                    modes: model.mode.as_ref().map(|(_, modes)| modes.clone()).unwrap_or_default(),
                    fan_speeds: model.fan.as_ref().map(|(_, speeds)| speeds.clone()).unwrap_or_default(),
                    home: entry.home.clone(),
                });
            }
        }
        let discovery = options.discovery.then(|| discovery::Listener::start(shared.clone(), stop.clone()));
        let poller = match (&file.cloud, polled.is_empty()) {
            (Some(access), false) => cloud::start_polling(access.clone(), polled, shared.clone(), stop.clone()),
            _ => None,
        };
        let workers = targets
            .into_iter()
            .map(|target| {
                let device_id = target.entry.device_id.clone();
                (device_id, device::start(target, shared.clone(), stop.clone()))
            })
            .collect();
        HomeService { shared, workers, stop, _poller: poller, _discovery: discovery }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.lock().snapshot.clone()
    }

    pub fn send(&self, request: Request) {
        match request {
            Request::Set { device_id, change } => {
                if let Some((_, worker)) = self.workers.iter().find(|(worker_id, _)| *worker_id == device_id) {
                    let _ = worker.requests.send(change);
                }
            }
            Request::AllOff => {
                let lit: Vec<String> = self
                    .snapshot()
                    .devices
                    .into_iter()
                    .filter(|device| {
                        matches!(device.kind, Kind::Light | Kind::Plug) && device.state.is_on == Some(true)
                    })
                    .map(|device| device.device_id)
                    .collect();
                for device_id in lit {
                    self.send(Request::Set { device_id, change: Change::Switch(false) });
                }
            }
        }
    }
}

impl Drop for HomeService {
    /// Asks the threads to stop without waiting for them: a connection
    /// attempt or a cloud request may take seconds to give up, and the
    /// compositor must not stall meanwhile.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
