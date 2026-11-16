//! Keyboard shortcuts inhibition (keyboard-shortcuts-inhibit), for virtual
//! machines and remote desktops that want every key, Super shortcuts
//! included, while they hold the keyboard. QEMU and remote desktop clients
//! ask for it when they grab the keyboard and let go when they release it.

use smithay::{
    delegate_keyboard_shortcuts_inhibit,
    wayland::keyboard_shortcuts_inhibit::{
        KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
        KeyboardShortcutsInhibitorSeat,
    },
    wayland::seat::WaylandFocus,
};

use crate::Tileception;

impl KeyboardShortcutsInhibitHandler for Tileception {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    /// Granted at once: it only takes effect while its window has the
    /// keyboard, and clicking another window gives the shortcuts back.
    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        inhibitor.activate();
    }
}

delegate_keyboard_shortcuts_inhibit!(Tileception);

impl Tileception {
    /// Whether keys go to the focused window without Tileception's shortcuts:
    /// it holds an active inhibitor and none of Tileception's own panels has
    /// the keyboard. The lock screen is handled before this is asked.
    pub(crate) fn shortcuts_inhibited(&self) -> bool {
        let panel_has_keys = self.launcher.open
            || self.power_menu.is_some()
            || self.feedback.is_some()
            || self.password_prompt.is_some()
            || self.colour_pick.is_some()
            || self.notifications.open
            || self.network.zoom.open
            || self.duck.typing()
            || (self.doctor.open && self.doctor.focused);
        if panel_has_keys {
            return false;
        }
        let Some(focus) = self.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus()) else {
            return false;
        };
        focus.wl_surface().is_some_and(|surface| {
            self.seat.keyboard_shortcuts_inhibitor_for_surface(&surface).is_some_and(|inhibitor| inhibitor.is_active())
        })
    }
}
