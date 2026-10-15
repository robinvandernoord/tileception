//! ext-session-lock: screen lockers such as swaylock.
//!
//! While locked, only the locker's surface is drawn and gets input; key
//! bindings are off except switching consoles. If the locker dies, the screen
//! stays locked (black) until a new locker unlocks it, as the protocol asks.

use smithay::{
    backend::renderer::{element::surface::render_elements_from_surface_tree, element::Kind, gles::GlesRenderer},
    delegate_session_lock,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Point, Size},
    wayland::session_lock::{LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker},
};

use crate::{render::OutputElement, Tileception};

#[derive(Debug, Default)]
pub enum LockState {
    #[default]
    Unlocked,
    Locked {
        surface: Option<LockSurface>,
    },
}

impl SessionLockHandler for Tileception {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        tracing::info!("session locked");
        self.lock = LockState::Locked { surface: None };
        self.move_drag = None;
        self.resize_drag = None;
        self.gap_drag = None;
        // Nothing but black is drawn from the next frame on.
        confirmation.lock();
        self.force_keyboard_focus_sync();
    }

    fn unlock(&mut self) {
        tracing::info!("session unlocked");
        self.lock = LockState::Unlocked;
        self.force_keyboard_focus_sync();
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        let rect = self.output_rect();
        surface.with_pending_state(|s| s.size = Some((rect.w as u32, rect.h as u32).into()));
        surface.send_configure();
        if let LockState::Locked { surface: slot } = &mut self.lock {
            *slot = Some(surface);
        }
        self.force_keyboard_focus_sync();
    }
}

delegate_session_lock!(Tileception);

impl Tileception {
    /// Unlocks without the locker's consent; see `Command::Unlock`.
    pub fn force_unlock(&mut self) {
        if self.is_locked() {
            tracing::warn!("session unlocked by command");
            self.lock = LockState::Unlocked;
            self.force_keyboard_focus_sync();
        }
    }

    pub fn is_locked(&self) -> bool {
        matches!(self.lock, LockState::Locked { .. })
    }

    pub fn lock_surface(&self) -> Option<&WlSurface> {
        match &self.lock {
            LockState::Locked { surface: Some(s) } => Some(s.wl_surface()),
            _ => None,
        }
    }

    pub fn lock_elements(&self, renderer: &mut GlesRenderer) -> Vec<OutputElement<GlesRenderer>> {
        let Some(surface) = self.lock_surface() else { return Vec::new() };
        render_elements_from_surface_tree(renderer, surface, (0, 0), 1.0, 1.0, Kind::Unspecified)
            .into_iter()
            .map(OutputElement::Live)
            .collect()
    }

    pub fn lock_surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let surface = self.lock_surface()?;
        smithay::desktop::utils::under_from_surface_tree(surface, pos, (0, 0), smithay::desktop::WindowSurfaceType::ALL)
            .map(|(s, loc)| (s, loc.to_f64()))
    }

    /// Keeps the lock surface the size of the output.
    pub fn resize_lock_surface(&self) {
        if let LockState::Locked { surface: Some(s) } = &self.lock {
            let rect = self.output_rect();
            let size: Size<u32, Logical> = (rect.w as u32, rect.h as u32).into();
            s.with_pending_state(|st| st.size = Some(size));
            s.send_configure();
        }
    }
}
