//! Locking before the computer sleeps. A delay inhibitor from logind holds
//! suspend and hibernation back until the lock screen has been drawn, so the
//! desktop never shows on waking, however sleep was asked for: the power
//! menu, a closed lid, `systemctl suspend` or a nearly empty battery. logind
//! goes ahead anyway after its delay (five seconds by default).

use std::os::fd::OwnedFd;

use smithay::reexports::calloop::channel;

use crate::Tileception;

enum Event {
    /// Held until the lock shows; dropping it lets sleep go ahead.
    Inhibitor(OwnedFd),
    Preparing,
    Woke,
}

#[derive(Default)]
pub struct Sleep {
    inhibitor: Option<OwnedFd>,
    /// Sleep is waiting for the lock screen.
    preparing: bool,
    /// A frame with the lock is on its way to the screen.
    lock_queued: bool,
}

impl Tileception {
    /// Watches logind for sleep, on its own thread.
    pub fn start_sleep_watch(&mut self) {
        let (sender, receiver) = channel::channel::<Event>();
        let token = self.loop_handle.insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.sleep_event(event);
            }
        });
        if let Err(error) = token {
            return tracing::error!("could not watch for sleep: {error}");
        }
        let started = std::thread::Builder::new().name("tileception-sleep".into()).spawn(move || {
            if let Err(error) = watch(&sender) {
                tracing::warn!("sleep: {error}; the screen is not locked before sleeping");
            }
        });
        if let Err(error) = started {
            tracing::error!("could not start the sleep watcher: {error}");
        }
    }

    fn sleep_event(&mut self, event: Event) {
        match event {
            Event::Inhibitor(descriptor) => self.sleep.inhibitor = Some(descriptor),
            Event::Preparing => {
                tracing::info!("preparing for sleep");
                let locking =
                    self.config.file.idle.lock_before_sleep && !matches!(self.shell().lock(), crate::shell::Part::Off);
                if locking {
                    self.lock_session(false);
                }
                self.sleep.preparing = true;
                if locking {
                    // An outside locker may take a moment to connect.
                    self.schedule_render();
                    self.sleep_check(false);
                } else {
                    self.release_sleep();
                }
            }
            Event::Woke => {
                tracing::info!("woke from sleep");
                self.release_sleep();
                self.refresh_battery();
            }
        }
    }

    /// After rendering: a frame drawn while locked is on its way.
    pub fn sleep_frame_queued(&mut self, queued: bool) {
        if self.sleep.preparing && self.is_locked() {
            if queued {
                self.sleep.lock_queued = true;
            } else {
                // Nothing changed, so the lock already shows.
                self.release_sleep();
            }
        }
    }

    /// The frame queued last now shows.
    pub fn sleep_frame_shown(&mut self) {
        if std::mem::take(&mut self.sleep.lock_queued) {
            self.sleep_check(true);
        }
    }

    /// Lets sleep go on once the lock shows (`drawn`), or at once while the
    /// screens are dark: they show nothing on waking.
    pub fn sleep_check(&mut self, drawn: bool) {
        if self.sleep.preparing && self.is_locked() && (drawn || self.idle.off) {
            self.release_sleep();
        }
    }

    fn release_sleep(&mut self) {
        self.sleep.preparing = false;
        self.sleep.lock_queued = false;
        self.sleep.inhibitor = None;
    }
}

fn watch(events: &channel::Sender<Event>) -> zbus::Result<()> {
    let bus = zbus::blocking::Connection::system()?;
    let manager = zbus::blocking::Proxy::new(
        &bus,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    let inhibit = || -> zbus::Result<Event> {
        let descriptor: zbus::zvariant::OwnedFd =
            manager.call("Inhibit", &("sleep", "Tileception", "Locking the screen first", "delay"))?;
        Ok(Event::Inhibitor(descriptor.into()))
    };
    // Listening first, so no signal falls between the two.
    let signals = manager.receive_signal("PrepareForSleep")?;
    let _ = events.send(inhibit()?);
    for message in signals {
        let starting: bool = message.body().deserialize()?;
        if starting {
            let _ = events.send(Event::Preparing);
        } else {
            let _ = events.send(Event::Woke);
            match inhibit() {
                Ok(event) => {
                    let _ = events.send(event);
                }
                Err(error) => tracing::warn!("sleep: could not hold sleep back again: {error}"),
            }
        }
    }
    Ok(())
}
