//! Super+D. A tap clears the screen's desk, every area of a split screen,
//! into the minimized strips, or brings back what clearing it took away. Held past the key-repeat delay it
//! peeks instead: windows show as outlines until the key is let go.

use std::time::{Duration, Instant};

use smithay::reexports::calloop::{
    RegistrationToken,
    timer::{TimeoutAction, Timer},
};

use crate::{Tileception, commands::Command, input::Action};

#[derive(Default)]
pub struct Desk {
    /// The key bound to `desktop` while it is held.
    key: Option<HeldKey>,
    /// Windows show as outlines while this is set.
    pub peeking: bool,
    /// When the peek last began or ended, for the fade.
    changed: Option<Instant>,
}

/// How long windows take to fade to outlines and back, in seconds.
const FADE: f64 = 0.15;

struct HeldKey {
    keycode: u32,
    /// Starts the peek; gone once it has.
    timer: Option<RegistrationToken>,
}

impl Tileception {
    /// Clears the desk, or brings back what clearing it took away. On a
    /// split screen this is every area's desk: when any of them is clear,
    /// all come back; otherwise all are cleared.
    pub fn toggle_desk(&mut self) {
        if let Some(window_id) = self.fullscreen {
            self.set_fullscreen(window_id, false);
        }
        let now = self.now();
        let clearing = !self.desk_cleared();
        let mut changed = false;
        for index in 0..self.area_count() {
            let toggled = self.with_local_area(index, |state| {
                if clearing { state.world.clear_desk(now) } else { state.world.bring_back_desk(now) }
            });
            changed |= toggled.unwrap_or(false);
        }
        if !changed {
            return;
        }
        if clearing {
            self.selection.windows.clear();
            tracing::info!(windows = self.world.minimized().count(), "cleared the desk");
            self.peek_minimized();
        } else {
            tracing::info!("brought the desk back");
        }
    }

    /// Whether Super+D cleared the desk of the screen, or of any of its areas.
    pub(crate) fn desk_cleared(&mut self) -> bool {
        (0..self.area_count())
            .any(|index| self.with_local_area(index, |state| state.world.desk_cleared()) == Some(true))
    }

    /// A key bound to `desktop` went down: it acts when let go, or peeks
    /// if held.
    pub(crate) fn desk_key_pressed(&mut self, keycode: u32) {
        if self.desk.key.as_ref().is_some_and(|held| held.keycode == keycode) {
            return;
        }
        self.end_desk_key();
        let delay = self.config.file.keyboard.repeat_delay.unwrap_or(400).max(1);
        let timer = Timer::from_duration(Duration::from_millis(delay as u64));
        let token = self.loop_handle.insert_source(timer, |_, _, state| {
            if let Some(held) = &mut state.desk.key {
                held.timer = None;
                state.set_desk_peek(true);
            }
            TimeoutAction::Drop
        });
        let timer = token.map_err(|error| tracing::warn!("could not time the desk key: {error}")).ok();
        self.desk.key = Some(HeldKey { keycode, timer });
    }

    /// A key went up. True when it was the desk key: a tap clears the desk
    /// or brings it back, the end of a hold ends the peek.
    pub(crate) fn desk_key_released(&mut self, keycode: u32) -> bool {
        if !self.desk.key.as_ref().is_some_and(|held| held.keycode == keycode) {
            return false;
        }
        let peeked = self.desk.peeking;
        self.end_desk_key();
        if !peeked && let Err(error) = self.execute(Command::Action(Action::ToggleDesk)) {
            tracing::warn!("{error}");
        }
        true
    }

    /// The desk key went up where this machine did not see it, as while the
    /// keyboard is forwarded to another: ends the hold without acting.
    pub(crate) fn desk_key_lost(&mut self, keycode: u32) {
        if self.desk.key.as_ref().is_some_and(|held| held.keycode == keycode) {
            self.end_desk_key();
        }
    }

    /// Forgets a held desk key and ends its peek, as when the screen locks.
    pub(crate) fn end_desk_key(&mut self) {
        if let Some(timer) = self.desk.key.take().and_then(|held| held.timer) {
            self.loop_handle.remove(timer);
        }
        self.set_desk_peek(false);
    }

    fn set_desk_peek(&mut self, peeking: bool) {
        if self.desk.peeking != peeking {
            self.desk.peeking = peeking;
            self.desk.changed = Some(Instant::now());
            self.schedule_render();
        }
    }

    /// How far windows have faded to outlines: 0 not at all, 1 fully.
    pub(crate) fn peek_fade(&self) -> f32 {
        let progress = self.desk.changed.map_or(1.0, |changed| (changed.elapsed().as_secs_f64() / FADE).min(1.0));
        let eased = crate::ui::ease_out(progress) as f32;
        if self.desk.peeking { eased } else { 1.0 - eased }
    }
}
