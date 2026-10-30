//! Sharing one mouse, keyboard, clipboard and windows between Tileception
//! machines.
//!
//! No Smithay here: the compositor decides what input and selections mean,
//! this crate carries them. Machines find each other with mDNS (or fixed
//! addresses), prove they know the same passphrase without revealing it, and
//! talk over an encrypted TCP connection.

mod held;
mod picture;
mod secure;
mod service;
mod wire;

pub use held::{Held, Released, Source};
pub use picture::{Area, Canvas, Header, pack, pack_pointer, unpack_pointer};
pub use service::{DEFAULT_PORT, Event, Messenger, PeerId, Service, Settings, random_id};
pub use wire::{
    CHUNK, MAX_POPUPS, Message, PICTURE_BUDGET, Picture, PointerImage, Resumed, Room, Scroll, Target, Tile,
};
