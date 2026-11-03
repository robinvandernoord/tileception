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
fn controls_simulated_devices_on_both_versions() {
    let household = sim::household().expect("simulated devices");
    let service = HomeService::start(&household.file, Options::default(), || {});

    let snapshot = wait_for(&service, "everything reachable to answer", |snapshot| {
        ["Ceiling spots", "Floor lamp", "Airco", "Desk lamp", "Bedside lamp"]
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
    assert!(matches!(device(&snapshot, "T&H sensor").reach, Reach::Unsupported(_)));

    // From the panel, on 3.3 and 3.5.
    let floor = device(&snapshot, "Floor lamp").device_id.clone();
    service.send(Request::Set { device_id: floor, change: Change::Switch(true) });
    let airco_id = airco.device_id.clone();
    service.send(Request::Set { device_id: airco_id, change: Change::Target(22.5) });
    wait_for(&service, "the changes to be confirmed", |snapshot| {
        let floor = device(snapshot, "Floor lamp");
        let airco = device(snapshot, "Airco");
        floor.state.is_on == Some(true) && !floor.pending && airco.state.target == Some(22.5) && !airco.pending
    });
    assert_eq!(household.devices[1].dps()["20"], true);
    assert_eq!(household.devices[2].dps()["2"], 225);

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
