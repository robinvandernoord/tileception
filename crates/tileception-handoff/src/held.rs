//! Keys and buttons held by each source of input.
//!
//! The local keyboard and other machines can press keys at the same time.
//! Programs see one keyboard and one pointer, so a key goes down when its
//! first source presses it and up when its last source lets go. When a source
//! goes away, only what it alone still holds is let go. A source pressing a
//! key it already holds (its release got lost, say over a VT switch) is passed
//! on, so a lost release never leaves a key dead.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Local,
    Peer(u64),
}

#[derive(Debug, Default)]
pub struct Held {
    keys: BTreeMap<u32, BTreeSet<Source>>,
    buttons: BTreeMap<u32, BTreeSet<Source>>,
}

/// What to let go of after a source left.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Released {
    pub keys: Vec<u32>,
    pub buttons: Vec<u32>,
}

impl Held {
    /// Whether programs should see this key change.
    pub fn key(&mut self, source: Source, key: u32, pressed: bool) -> bool {
        change(&mut self.keys, source, key, pressed)
    }

    /// Whether programs should see this button change.
    pub fn button(&mut self, source: Source, button: u32, pressed: bool) -> bool {
        change(&mut self.buttons, source, button, pressed)
    }

    /// Whether a source holds a button: it is dragging something.
    pub fn holds_button(&self, source: Source) -> bool {
        self.buttons.values().any(|holders| holders.contains(&source))
    }

    pub fn holds(&self, source: Source) -> bool {
        self.keys.values().chain(self.buttons.values()).any(|holders| holders.contains(&source))
    }

    /// Forgets the buttons a source holds (its drag went to another machine),
    /// returning those nobody else holds.
    pub fn release_buttons(&mut self, source: Source) -> Vec<u32> {
        forget(&mut self.buttons, source)
    }

    /// Forgets a source, returning what nobody else holds and so must be
    /// released for programs.
    pub fn release(&mut self, source: Source) -> Released {
        Released { keys: forget(&mut self.keys, source), buttons: forget(&mut self.buttons, source) }
    }
}

fn change(held: &mut BTreeMap<u32, BTreeSet<Source>>, source: Source, code: u32, pressed: bool) -> bool {
    let holders = held.entry(code).or_default();
    let others = holders.iter().any(|holder| *holder != source);
    let visible = if pressed {
        holders.insert(source);
        !others
    } else {
        holders.remove(&source) && holders.is_empty()
    };
    if holders.is_empty() {
        held.remove(&code);
    }
    visible
}

fn forget(held: &mut BTreeMap<u32, BTreeSet<Source>>, source: Source) -> Vec<u32> {
    let mut released = Vec::new();
    held.retain(|code, holders| {
        if holders.remove(&source) && holders.is_empty() {
            released.push(*code);
        }
        !holders.is_empty()
    });
    released
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT: u32 = 42;
    const KEY_A: u32 = 30;

    #[test]
    fn one_source_passes_through() {
        let mut held = Held::default();
        assert!(held.key(Source::Local, KEY_A, true));
        assert!(held.key(Source::Local, KEY_A, false));
        assert!(!held.holds(Source::Local));
    }

    #[test]
    fn shared_keys_go_up_with_the_last_holder() {
        let mut held = Held::default();
        assert!(held.key(Source::Local, SHIFT, true));
        assert!(!held.key(Source::Peer(1), SHIFT, true));
        assert!(!held.key(Source::Local, SHIFT, false));
        assert!(held.key(Source::Peer(1), SHIFT, false));
    }

    #[test]
    fn releases_that_were_never_pressed_are_dropped() {
        let mut held = Held::default();
        assert!(!held.key(Source::Peer(1), KEY_A, false));
        assert!(held.key(Source::Local, KEY_A, true));
        assert!(!held.key(Source::Peer(1), KEY_A, false));
        assert!(held.holds(Source::Local));
    }

    #[test]
    fn a_lost_release_does_not_swallow_the_next_press() {
        let mut held = Held::default();
        assert!(held.key(Source::Local, KEY_A, true));
        assert!(held.key(Source::Local, KEY_A, true));
        assert!(held.key(Source::Local, KEY_A, false));
        assert!(!held.holds(Source::Local));
    }

    #[test]
    fn a_leaving_source_releases_only_its_own() {
        let mut held = Held::default();
        held.key(Source::Local, SHIFT, true);
        held.key(Source::Peer(1), SHIFT, true);
        held.key(Source::Peer(1), KEY_A, true);
        held.button(Source::Peer(1), 0x110, true);
        held.key(Source::Peer(2), 31, true);
        let released = held.release(Source::Peer(1));
        assert_eq!(released, Released { keys: vec![KEY_A], buttons: vec![0x110] });
        assert!(held.holds(Source::Local));
        assert!(held.holds(Source::Peer(2)));
        assert!(!held.holds(Source::Peer(1)));
        assert!(held.key(Source::Local, SHIFT, false));
    }
}
