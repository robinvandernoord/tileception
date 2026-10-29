//! Follows and controls a fake MPRIS player on a private session bus.
#![allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tileception_media::{MediaService, PlayerCommand, Request, Snapshot, Status};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

/// A private session bus.
struct Bus(Child, String);

impl Bus {
    fn new() -> Bus {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--print-address", "--nofork"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("tests require dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut address).unwrap();
        Bus(child, address.trim().to_owned())
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Root;

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    #[zbus(property)]
    fn identity(&self) -> String {
        "Fake Player".into()
    }
}

#[derive(Clone, Default)]
struct Fake {
    track: Arc<Mutex<usize>>,
    playing: Arc<Mutex<bool>>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

const TRACKS: [(&str, &str); 2] = [("Midnight City", "M83"), ("Teardrop", "Massive Attack")];

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Fake {
    async fn next(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.calls.lock().unwrap().push("Next");
        *self.track.lock().unwrap() += 1;
        let _ = self.metadata_changed(&emitter).await;
    }

    async fn play_pause(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.calls.lock().unwrap().push("PlayPause");
        {
            let mut playing = self.playing.lock().unwrap();
            *playing = !*playing;
        }
        let _ = self.playback_status_changed(&emitter).await;
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        if *self.playing.lock().unwrap() { "Playing" } else { "Paused" }.into()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let (title, artist) = TRACKS[*self.track.lock().unwrap() % TRACKS.len()];
        HashMap::from([
            ("xesam:title".to_string(), Value::from(title).try_into().unwrap()),
            ("xesam:artist".to_string(), Value::from(vec![artist]).try_into().unwrap()),
            ("mpris:length".to_string(), Value::from(240_000_000i64).try_into().unwrap()),
            ("mpris:artUrl".to_string(), Value::from("file:///tmp/My%20Cover.png").try_into().unwrap()),
        ])
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
}

fn wait_for(service: &MediaService, what: &str, test: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = service.snapshot();
        if test(&snapshot) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {snapshot:#?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn follows_and_controls_a_player() {
    let bus = Bus::new();
    let fake = Fake { playing: Arc::new(Mutex::new(true)), ..Fake::default() };
    let _player = zbus::blocking::connection::Builder::address(bus.1.as_str())
        .unwrap()
        .serve_at("/org/mpris/MediaPlayer2", Root)
        .unwrap()
        .serve_at("/org/mpris/MediaPlayer2", fake.clone())
        .unwrap()
        .name("org.mpris.MediaPlayer2.fake")
        .unwrap()
        .build()
        .unwrap();

    let (service, _events) = MediaService::start(Some(bus.1.clone()), || {});
    let snapshot = wait_for(&service, "the player", |snapshot| snapshot.media.active().is_some());
    let player = snapshot.media.active().unwrap();
    assert_eq!(player.identity, "Fake Player");
    assert_eq!(player.title, "Midnight City");
    assert_eq!(player.artist, "M83");
    assert_eq!(player.status, Status::Playing);
    assert_eq!(player.length, Some(Duration::from_secs(240)));
    assert_eq!(player.art.as_deref(), Some(std::path::Path::new("/tmp/My Cover.png")));
    assert!(player.can_next && !player.can_previous);

    service.send(Request::Player(PlayerCommand::Next));
    wait_for(&service, "the next track", |snapshot| {
        snapshot.media.active().is_some_and(|player| player.title == "Teardrop")
    });
    service.send(Request::Player(PlayerCommand::PlayPause));
    wait_for(&service, "pausing", |snapshot| {
        snapshot.media.active().is_some_and(|player| player.status == Status::Paused)
    });
    assert_eq!(*fake.calls.lock().unwrap(), ["Next", "PlayPause"]);

    // A player that leaves is forgotten.
    drop(_player);
    wait_for(&service, "the player to leave", |snapshot| snapshot.media.players.is_empty());
}
