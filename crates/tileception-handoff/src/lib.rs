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
pub use picture::{Area, Canvas, Header, pack, pack_pointer, pack_thumbnail, unpack_pointer, unpack_rgba};
pub use service::{DEFAULT_PORT, Event, Messenger, PeerId, Service, Settings, random_id};
pub use wire::{
    CHUNK, DragKind, DragPreview, MAX_POPUPS, MAX_UPDATE_NAMES, Message, PICTURE_BUDGET, PROTOCOL, PendingUpdates,
    Picture, PointerImage, Resumed, Room, Scroll, Target, Thumbnail, Tile, UPDATE_TEXT, UpdatesTask,
};
