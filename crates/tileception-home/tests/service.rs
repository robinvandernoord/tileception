//! The service against simulated devices on localhost.

use std::time::{Duration, Instant};

use tileception_home::{Change, HomeService, Options, Reach, Request, Snapshot, sim};

fn wait_for(service: &HomeService, what: &str, check: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = service.snapshot();
        if check(&snapshot) {
            return snapshot;
        }
        assert!(Instant::now() < until, "timed out waiting for {what}: {snapshot:#?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn device<'a>(snapshot: &'a Snapshot, name: &str) -> &'a tileception_home::Device {
    snapshot.devices.iter().find(|device| device.name == name).unwrap_or_else(|| panic!("no {name}"))
}

#[test]
fn controls_simulated_devices_on_all_versions() {
    let mut household = sim::household().expect("simulated devices");
    household.file.devices.push(tileception_home::store::DeviceEntry {
        device_id: "unavailable-sub-device".into(),
        name: "Gateway plug".into(),
        gateway: Some(household.file.devices[0].device_id.clone()),
        node_id: Some("a1b2c3".into()),
        ..Default::default()
    });
    let service = HomeService::start(&household.file, Options::default(), || {});

    let snapshot = wait_for(&service, "everything reachable to answer", |snapshot| {
        ["Ceiling spots", "Floor lamp", "Airco", "Desk lamp", "Bedside lamp", "Heater plug"]
            .iter()
            .all(|name| device(snapshot, name).reach == Reach::Online)
    });
    let spots = device(&snapshot, "Ceiling spots");
    assert_eq!((spots.state.is_on, spots.state.brightness), (Some(true), Some(70)));
    let airco = device(&snapshot, "Airco");
    assert_eq!(
        (airco.state.target, airco.state.temperature, airco.state.mode.as_deref()),
        (Some(21.0), Some(23.5), Some("cold"))
    );
    // The plug on protocol 3.4 answers too.
    let heater = device(&snapshot, "Heater plug");
    assert_eq!((heater.state.is_on, heater.state.power), (Some(true), Some(125.0)));
    assert_eq!(device(&snapshot, "Gateway plug").reach, Reach::Unsupported("behind a gateway".into()));
    assert!(matches!(device(&snapshot, "T&H sensor").reach, Reach::Unsupported(_)));

    // From the panel, on 3.3, 3.4 and 3.5.
    let floor = device(&snapshot, "Floor lamp").device_id.clone();
    service.send(Request::Set { device_id: floor, change: Change::Switch(true) });
    let airco_id = airco.device_id.clone();
    service.send(Request::Set { device_id: airco_id, change: Change::Target(22.5) });
    let heater_id = heater.device_id.clone();
    service.send(Request::Set { device_id: heater_id, change: Change::Switch(false) });
    wait_for(&service, "the changes to be confirmed", |snapshot| {
        let floor = device(snapshot, "Floor lamp");
        let airco = device(snapshot, "Airco");
        let heater = device(snapshot, "Heater plug");
        floor.state.is_on == Some(true)
            && !floor.pending
            && airco.state.target == Some(22.5)
            && !airco.pending
            && heater.state.is_on == Some(false)
            && !heater.pending
    });
    assert_eq!(household.devices[1].dps()["20"], true);
    assert_eq!(household.devices[2].dps()["2"], 225);
    assert_eq!(household.devices[5].dps()["1"], false);

    // From the wall switch or the app.
    let mut off = serde_json::Map::new();
    off.insert("20".into(), false.into());
    household.devices[0].change(off);
    wait_for(&service, "the spots to report off", |snapshot| {
        device(snapshot, "Ceiling spots").state.is_on == Some(false)
    });

    service.send(Request::AllOff);
    wait_for(&service, "all lights off", |snapshot| {
        snapshot
            .devices
            .iter()
            .filter(|device| device.kind == tileception_home::Kind::Light && device.reach == Reach::Online)
            .all(|device| device.state.is_on == Some(false))
    });
    // The air conditioner is not a light.
    assert_eq!(device(&service.snapshot(), "Airco").state.is_on, Some(true));

    let hallway = wait_for(&service, "the unplugged lamp to be unreachable", |snapshot| {
        matches!(device(snapshot, "Hallway spot").reach, Reach::Unreachable { .. })
    });
    assert_eq!(device(&hallway, "Hallway spot").state.is_on, None);
}

#[test]
fn samples_the_climate_history() {
    let household = sim::household().expect("simulated devices");
    let dir = std::env::temp_dir().join(format!("tileception-home-history-test-{}", std::process::id()));
    let history_path = dir.join("home-history.json");
    let service =
        HomeService::start(&household.file, Options { discovery: false, history: Some(history_path.clone()) }, || {});
    let snapshot =
        wait_for(&service, "the airco to answer", |snapshot| device(snapshot, "Airco").reach == Reach::Online);
    let airco = device(&snapshot, "Airco");
    assert_eq!(airco.history.len(), 1);
    assert_eq!((airco.history[0].temperature, airco.history[0].target), (Some(23.5), Some(21.0)));

    // A target change is sampled at once and lands in the file.
    let airco_id = airco.device_id.clone();
    service.send(Request::Set { device_id: airco_id, change: Change::Target(22.0) });
    wait_for(&service, "the new target to be sampled", |snapshot| {
        device(snapshot, "Airco").history.len() >= 2
            && device(snapshot, "Airco").history.last().map(|sample| sample.target) == Some(Some(22.0))
    });
    wait_for(&service, "the sampled history to be saved", |_| {
        let loaded = tileception_home::history::load(&history_path);
        loaded
            .get(&airco.device_id)
            .is_some_and(|samples| samples.len() == 2 && samples.last().and_then(|sample| sample.target) == Some(22.0))
    });
    drop(service);
    let _ = std::fs::remove_dir_all(dir);
}
