//! Idle locking and DRM power saving, respecting visible idle-inhibiting surfaces.

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
}

impl Default for Idle {
    fn default() -> Self {
        Self { last_activity: Instant::now(), lock_requested: false, off: false, inhibitors: Vec::new() }
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
        self.idle.inhibitors.retain(|surface| surface.alive());
        let visible = self.drawn_windows(self.now());
        let inhibited = !self.is_locked()
            && self.idle.inhibitors.iter().any(|surface| {
                let mut root = surface.clone();
                while let Some(parent) = get_parent(&root) {
                    root = parent;
                }
                visible
                    .iter()
                    .filter(|d| d.content.intersects(&self.output_rect()))
                    .any(|d| self.windows.get(&d.id).and_then(|w| w.wl_surface()).is_some_and(|s| *s == root))
            });
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
        if let Some(i) = self.idle.inhibitors.iter().position(|s| *s == surface) {
            self.idle.inhibitors.remove(i);
        }
    }
}

delegate_idle_inhibit!(Tileception);
