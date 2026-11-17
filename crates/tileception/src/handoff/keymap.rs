//! Keyboard layouts across machines.
//!
//! Each machine's keyboard follows only that machine's own pointer, and its
//! keys mean what its own layout says wherever they go: a machine tells the
//! others the layout it types with (xkb's names and which of its layouts is
//! active), and keys from another machine are typed here with that layout,
//! its shortcuts included. Programs see one keyboard, so its keymap changes
//! when a key comes from another keyboard than the last; each keyboard's
//! Caps Lock and Num Lock are kept for when it types again.

use std::collections::{BTreeMap, BTreeSet};

use smithay::{
    input::keyboard::{KeyboardTarget, Layout, XkbConfig},
    utils::SERIAL_COUNTER,
};
use tileception_handoff::{Keymap, Message, PeerId, Source};

use crate::Tileception;

#[derive(Default)]
pub(crate) struct Layouts {
    /// The machine whose keyboard the keymap here is for: another one, or
    /// this one (None).
    typing: Option<PeerId>,
    /// What each machine types with, as it last said.
    theirs: BTreeMap<PeerId, Keymap>,
    /// What each machine was last told this one types with.
    told: BTreeMap<PeerId, Keymap>,
    /// Each keyboard's locks, kept while another types.
    locks: BTreeMap<Option<PeerId>, Locks>,
    /// This machine's own active layout, kept while another keyboard types.
    own_active: u32,
    /// Machines whose layout does not compile here: their keys are typed
    /// with this machine's layout instead.
    broken: BTreeSet<PeerId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Locks {
    caps_lock: bool,
    num_lock: bool,
}

/// What changes when another keyboard starts typing.
#[derive(Debug, PartialEq, Eq)]
enum Change {
    /// The same keymap; only the active layout may differ.
    Layout,
    /// Another keymap, compiled from its names.
    Keymap,
}

fn change(current: &Keymap, next: &Keymap) -> Change {
    if current.same_names(next) { Change::Layout } else { Change::Keymap }
}

impl Layouts {
    pub(crate) fn forget(&mut self, peer: PeerId) {
        self.theirs.remove(&peer);
        self.told.remove(&peer);
        self.locks.remove(&Some(peer));
        self.broken.remove(&peer);
    }
}

impl Tileception {
    /// This machine's own layout, as configured.
    fn own_keymap(&self) -> Keymap {
        let (model, layout, variant, options) = self.config.file.keyboard.names();
        Keymap { model, layout, variant, options, active: 0 }
    }

    /// The keymap a keyboard types with here.
    fn keymap_of(&self, typing: Option<PeerId>) -> Keymap {
        let layouts = &self.handoff.layouts;
        typing
            .filter(|peer| !layouts.broken.contains(peer))
            .and_then(|peer| layouts.theirs.get(&peer).cloned())
            .unwrap_or_else(|| self.own_keymap())
    }

    /// What keys from here are typed with now: the keymap here, at its
    /// active layout. Keys passed on from another machine (into a window of
    /// a third shown here) keep that machine's layout.
    fn keymap_here(&self) -> Keymap {
        let mut keymap = self.keymap_of(self.handoff.layouts.typing);
        if let Some(keyboard) = self.seat.get_keyboard() {
            keymap.active = keyboard.modifier_state().serialized.layout_effective;
        }
        keymap
    }

    /// Tells a machine about to get keys from here what they are typed with,
    /// when that changed since it was last told.
    pub(crate) fn tell_keymap(&mut self, peer: PeerId) {
        let keymap = self.keymap_here();
        if self.handoff.layouts.told.get(&peer) == Some(&keymap) {
            return;
        }
        if self.handoff.send(peer, Message::Keymap(keymap.clone())) {
            self.handoff.layouts.told.insert(peer, keymap);
        }
    }

    /// Another machine's layout changed.
    pub(crate) fn peer_keymap(&mut self, peer: PeerId, keymap: Keymap) {
        let layouts = &mut self.handoff.layouts;
        let previous = layouts.theirs.insert(peer, keymap.clone());
        if layouts.typing != Some(peer) {
            return;
        }
        // Typing with it now: the change applies at once.
        if previous.is_none_or(|previous| change(&previous, &keymap) == Change::Keymap) {
            layouts.broken.remove(&peer);
            let Some(keyboard) = self.seat.get_keyboard() else { return };
            let before = keyboard.modifier_state();
            if !self.load_keymap_for(Some(peer)) {
                return;
            }
            self.set_locks(Locks { caps_lock: before.caps_lock, num_lock: before.num_lock });
        }
        self.set_active_layout(keymap.active);
    }

    /// Makes the keymap here the one `source`'s keyboard types with. Called
    /// before each key, so it changes only when another keyboard types.
    pub(crate) fn type_with(&mut self, source: Source) {
        let typing = match source {
            Source::Local => None,
            Source::Peer(peer) => Some(peer),
        };
        // The lock screen and the greeter keep their own keymap; sharing is
        // paused while locked anyway.
        if self.handoff.layouts.typing == typing || self.is_locked() {
            return;
        }
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let before = keyboard.modifier_state();
        let from = self.keymap_of(self.handoff.layouts.typing);
        let next = self.keymap_of(typing);
        let layouts = &mut self.handoff.layouts;
        layouts.locks.insert(layouts.typing, Locks { caps_lock: before.caps_lock, num_lock: before.num_lock });
        if layouts.typing.is_none() {
            layouts.own_active = before.serialized.layout_effective;
        }
        layouts.typing = typing;
        if change(&from, &next) == Change::Keymap && !self.load_keymap_for(typing) {
            return;
        }
        // A machine whose layout is not known (yet) types with this one's.
        let layouts = &self.handoff.layouts;
        let known = typing.is_some_and(|peer| layouts.theirs.contains_key(&peer) && !layouts.broken.contains(&peer));
        let active = if known { next.active } else { layouts.own_active };
        let locks = self.handoff.layouts.locks.get(&typing).copied().unwrap_or_default();
        self.set_locks(locks);
        self.set_active_layout(active);
    }

    /// Compiles the keymap `typing`'s keyboard types with. When another
    /// machine's does not compile here, its keys use this machine's.
    fn load_keymap_for(&mut self, typing: Option<PeerId>) -> bool {
        let Some(keyboard) = self.seat.get_keyboard() else { return false };
        let keymap = self.keymap_of(typing);
        let xkb = XkbConfig {
            model: &keymap.model,
            layout: &keymap.layout,
            variant: &keymap.variant,
            options: keymap.options.clone(),
            ..Default::default()
        };
        if keyboard.set_xkb_config(self, xkb).is_ok() {
            return true;
        }
        let Some(peer) = typing else {
            tracing::warn!("handoff: this machine's keyboard layout {:?} did not compile", keymap.layout);
            return false;
        };
        tracing::warn!(
            "handoff: the keyboard layout of {peer:016x} ({:?} {:?}) is not known here; its keys use this one's",
            keymap.layout,
            keymap.variant
        );
        self.handoff.layouts.broken.insert(peer);
        self.load_keymap_for(None)
    }

    fn set_active_layout(&mut self, active: u32) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        if keyboard.modifier_state().serialized.layout_effective != active {
            keyboard.with_xkb_state(self, |mut context| context.set_layout(Layout(active)));
        }
    }

    /// Puts back a keyboard's Caps Lock and Num Lock, telling the program
    /// with the keyboard.
    fn set_locks(&mut self, locks: Locks) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let mut state = keyboard.modifier_state();
        if state.caps_lock == locks.caps_lock && state.num_lock == locks.num_lock {
            return;
        }
        state.caps_lock = locks.caps_lock;
        state.num_lock = locks.num_lock;
        if keyboard.set_modifier_state(state) == 0 {
            return;
        }
        if let Some(focus) = keyboard.current_focus() {
            let seat = self.seat.clone();
            focus.modifiers(&seat, self, keyboard.modifier_state(), SERIAL_COUNTER.next_serial());
        }
    }

    /// A machine went away or a new connection to it began.
    pub(crate) fn keymap_peer_gone(&mut self, peer: PeerId) {
        if self.handoff.layouts.typing == Some(peer) {
            self.type_with(Source::Local);
        }
        self.handoff.layouts.forget(peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap(layout: &str, active: u32) -> Keymap {
        Keymap { model: "pc105".into(), layout: layout.into(), variant: String::new(), options: None, active }
    }

    #[test]
    fn only_another_keymap_is_compiled() {
        assert_eq!(change(&keymap("us,de", 0), &keymap("us,de", 1)), Change::Layout);
        assert_eq!(change(&keymap("us", 0), &keymap("de", 0)), Change::Keymap);
        let mut options = keymap("us", 0);
        options.options = Some("caps:escape".into());
        assert_eq!(change(&keymap("us", 0), &options), Change::Keymap);
    }
}
