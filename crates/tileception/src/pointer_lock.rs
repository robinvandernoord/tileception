//! Pointer lock and confinement (pointer-constraints) with relative motion
//! (relative-pointer), for games and 3D programs that steer by mouse movement.

use smithay::{
    delegate_pointer_constraints, delegate_relative_pointer,
    input::pointer::{PointerHandle, RelativeMotionEvent},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
    wayland::pointer_constraints::{PointerConstraint, PointerConstraintsHandler, with_pointer_constraint},
};

use crate::Tileception;

/// What an active constraint does to the next movement.
enum Held {
    Free,
    Locked,
    /// Kept inside this surface (at this origin), and its region if it has one.
    Confined(WlSurface, Point<f64, Logical>, Option<smithay::wayland::compositor::RegionAttributes>),
}

impl Tileception {
    /// Whether constraints may hold the pointer: not while the compositor's
    /// own interface wants it.
    fn constraints_apply(&self) -> bool {
        !self.is_locked() && !self.launcher.open && self.power_menu.is_none() && !self.notifications.open
    }

    /// A relative pointer movement. Reports it to clients that asked for raw
    /// motion, then moves the cursor unless a lock or confinement holds it.
    /// Returns where the cursor should go, or `None` to leave it.
    pub fn constrained_motion(
        &mut self,
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        utime: u64,
        target: Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let under = self.constraint_surface_under(self.pointer);
        let held = match (&under, self.constraints_apply()) {
            (Some((surface, origin)), true) => {
                let local = (self.pointer - *origin).to_i32_round();
                with_pointer_constraint(surface, &pointer, |constraint| match constraint {
                    Some(c) if c.is_active() && c.region().is_none_or(|r| r.contains(local)) => match &*c {
                        PointerConstraint::Locked(_) => Held::Locked,
                        PointerConstraint::Confined(confined) => {
                            Held::Confined(surface.clone(), *origin, confined.region().cloned())
                        }
                    },
                    _ => Held::Free,
                })
            }
            _ => Held::Free,
        };
        // Raw motion goes nowhere while the compositor's interface has the pointer.
        let focus = under.clone().filter(|_| self.constraints_apply());
        pointer.relative_motion(self, focus, &RelativeMotionEvent { delta, delta_unaccel, utime });
        match held {
            Held::Free => Some(target),
            Held::Locked => {
                pointer.frame(self);
                None
            }
            Held::Confined(surface, origin, region) => {
                let leaves_surface = self.constraint_surface_under(target).is_none_or(|(s, _)| s != surface);
                let leaves_region = region.is_some_and(|r| !r.contains((target - origin).to_i32_round()));
                if leaves_surface || leaves_region {
                    pointer.frame(self);
                    None
                } else {
                    Some(target)
                }
            }
        }
    }

    /// Activates a waiting constraint once the pointer is over its surface
    /// (and inside its region).
    pub fn activate_constraint_under(&mut self) {
        if !self.constraints_apply() {
            return;
        }
        let Some((surface, origin)) = self.constraint_surface_under(self.pointer) else { return };
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if pointer.current_focus().as_ref() != Some(&surface) {
            return;
        }
        let local = (self.pointer - origin).to_i32_round();
        with_pointer_constraint(&surface, &pointer, |constraint| {
            if let Some(c) = constraint
                && !c.is_active()
                && c.region().is_none_or(|r| r.contains(local))
            {
                c.activate();
            }
        });
    }

    /// The surface a constraint would belong to under `at`, with its origin.
    /// Programs often draw into a subsurface but put the constraint on their
    /// main surface, so this is the root of the surface tree.
    fn constraint_surface_under(&self, at: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (surface, origin) = self.surface_under(at)?;
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        if root == surface {
            return Some((surface, origin));
        }
        // A window's main surface sits at the origin it is drawn at.
        let drawn = self
            .window_id(&root)
            .and_then(|id| self.drawn_windows(self.now()).into_iter().find(|d| d.id == id && d.live).map(|d| d.origin));
        Some(match drawn {
            Some(origin) => (root, origin),
            None => (surface, origin),
        })
    }
}

impl PointerConstraintsHandler for Tileception {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {
        // Takes effect when the pointer is (or next comes) over the surface.
        self.activate_constraint_under();
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        // Where the program would like the cursor to reappear once unlocked.
        let active = with_pointer_constraint(surface, pointer, |c| c.is_some_and(|c| c.is_active()));
        if !active {
            return;
        }
        if let Some((_, origin)) = self.constraint_surface_under(self.pointer).filter(|(s, _)| s == surface) {
            let at = origin + location;
            self.pointer = at;
            pointer.set_location(at);
        }
    }
}

delegate_pointer_constraints!(Tileception);
delegate_relative_pointer!(Tileception);
