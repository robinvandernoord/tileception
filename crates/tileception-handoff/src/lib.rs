//! Sharing one mouse, keyboard and clipboard between Tileception machines.
//!
//! No Smithay here: the compositor decides what input and selections mean,
//! this crate carries them. Machines find each other with mDNS (or fixed
//! addresses), prove they know the same passphrase without revealing it, and
//! talk over an encrypted TCP connection.

mod held;
mod secure;
mod service;
mod wire;

pub use held::{Held, Released, Source};
pub use service::{DEFAULT_PORT, Event, Messenger, PeerId, Service, Settings};
pub use wire::{CHUNK, Message, Scroll, Target};
