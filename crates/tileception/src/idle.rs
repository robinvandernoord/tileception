//! Idle locking and DRM power saving, respecting visible idle-inhibiting surfaces
//! and applications that hold idle off over D-Bus (`screensaver`).

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use smithay::{
    delegate_idle_inhibit,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::IsAlive,
    wayland::{
        compositor::get_parent,
        idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState},
    },
};

use crate::Tileception;
use smithay::wayland::seat::WaylandFocus;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdleConfig {
    pub lock_after: u64,
    pub off_after: u64,
    /// Lock before suspending or hibernating, however that was asked for.
    pub lock_before_sleep: bool,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self { lock_after: 0, off_after: 600, lock_before_sleep: true }
    }
}

pub struct Idle {
    last_activity: Instant,
    lock_requested: bool,
    pub off: bool,
    inhibitors: Vec<WlSurface>,
    /// Applications holding idle off over D-Bus, wherever their windows are.
    pub bus_inhibitors: crate::screensaver::Inhibitors,
}

impl Default for Idle {
    fn default() -> Self {
        Self {
            last_activity: Instant::now(),
            lock_requested: false,
            off: false,
            inhibitors: Vec::new(),
            bus_inhibitors: crate::screensaver::Inhibitors::default(),
        }
    }
}

impl Tileception {
    pub fn init_idle(&mut self) {
        IdleInhibitManagerState::new::<Self>(&self.display_handle);
        self.loop_handle
            .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
                state.idle_tick();
                TimeoutAction::ToDuration(Duration::from_secs(1))
            })
            .expect("idle timer registration");
    }

    pub fn idle_activity(&mut self) -> bool {
        self.idle.last_activity = Instant::now();
        self.idle.lock_requested = false;
        let was_off = self.idle.off;
        if was_off {
            self.idle.off = false;
            self.set_output_sleep(false);
            self.schedule_render();
            tracing::info!("idle display awake");
        }
        was_off
    }

    fn idle_tick(&mut self) {
        if self.nested || self.headless.is_some() && std::env::var_os("TILECEPTION_IDLE_HEADLESS").is_none() {
            return;
        }
        if self.idle.bus_inhibitors.take_poke() {
            self.idle_activity();
        }
        self.idle.inhibitors.retain(|surface| surface.alive());
        let visible = self.drawn_windows(self.now());
        let inhibited = !self.is_locked()
            && (self.idle.bus_inhibitors.any()
                || self.idle.inhibitors.iter().any(|surface| {
                    let mut root = surface.clone();
                    while let Some(parent) = get_parent(&root) {
                        root = parent;
                    }
                    visible.iter().filter(|drawn| drawn.content.intersects(&self.output_rect())).any(|drawn| {
                        self.windows
                            .get(&drawn.window_id)
                            .and_then(|window| window.wl_surface())
                            .is_some_and(|surface| *surface == root)
                    })
                }));
        if inhibited {
            self.idle.last_activity = Instant::now();
            self.idle.lock_requested = false;
            return;
        }
        let elapsed = self.idle.last_activity.elapsed().as_secs();
        let config = &self.config.file.idle;
        let lock = config.lock_after > 0 && elapsed >= config.lock_after;
        let off = config.off_after > 0 && elapsed >= config.off_after;
        if lock && !self.is_locked() && !self.idle.lock_requested {
            self.lock_session(false);
            self.idle.lock_requested = true;
        }
        if off && !self.idle.off && self.set_output_sleep(true) {
            self.idle.off = true;
            tracing::info!("idle display asleep");
        }
    }
}

impl IdleInhibitHandler for Tileception {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle.inhibitors.push(surface);
    }
    fn uninhibit(&mut self, surface: WlSurface) {
        if let Some(index) = self.idle.inhibitors.iter().position(|inhibitor| *inhibitor == surface) {
            self.idle.inhibitors.remove(index);
        }
    }
}

delegate_idle_inhibit!(Tileception);
