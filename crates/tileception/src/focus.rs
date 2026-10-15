//! What the keyboard can be focused on: a Wayland surface, or an X11 window,
//! which also needs X input focus set through the window manager connection.

use std::borrow::Cow;

use smithay::{
    backend::input::KeyState,
    input::{
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
        Seat,
    },
    reexports::wayland_server::{backend::ObjectId, protocol::wl_surface::WlSurface},
    utils::{IsAlive, Serial},
    wayland::seat::WaylandFocus,
    xwayland::X11Surface,
};

use crate::Tileception;

#[derive(Debug, Clone, PartialEq)]
pub enum KeyboardFocus {
    Wayland(WlSurface),
    X11(X11Surface),
}

impl IsAlive for KeyboardFocus {
    fn alive(&self) -> bool {
        match self {
            KeyboardFocus::Wayland(s) => s.alive(),
            KeyboardFocus::X11(s) => s.alive(),
        }
    }
}

impl WaylandFocus for KeyboardFocus {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            KeyboardFocus::Wayland(s) => Some(Cow::Borrowed(s)),
            KeyboardFocus::X11(s) => s.wl_surface().map(Cow::Owned),
        }
    }

    fn same_client_as(&self, object_id: &ObjectId) -> bool {
        match self {
            KeyboardFocus::Wayland(s) => s.same_client_as(object_id),
            KeyboardFocus::X11(s) => s.same_client_as(object_id),
        }
    }
}

impl From<smithay::desktop::PopupKind> for KeyboardFocus {
    fn from(popup: smithay::desktop::PopupKind) -> Self {
        KeyboardFocus::Wayland(popup.wl_surface().clone())
    }
}

/// Pointer focus is always a Wayland surface; X11 windows have one too.
impl From<KeyboardFocus> for WlSurface {
    fn from(focus: KeyboardFocus) -> Self {
        match focus {
            KeyboardFocus::Wayland(s) => s,
            KeyboardFocus::X11(s) => s.wl_surface().expect("mapped X11 windows have a surface"),
        }
    }
}

impl From<WlSurface> for KeyboardFocus {
    fn from(s: WlSurface) -> Self {
        KeyboardFocus::Wayland(s)
    }
}

macro_rules! delegate {
    ($self:ident, $s:ident => $call:expr) => {
        match $self {
            KeyboardFocus::Wayland($s) => $call,
            KeyboardFocus::X11($s) => $call,
        }
    };
}

impl KeyboardTarget<Tileception> for KeyboardFocus {
    fn enter(&self, seat: &Seat<Tileception>, data: &mut Tileception, keys: Vec<KeysymHandle<'_>>, serial: Serial) {
        delegate!(self, s => KeyboardTarget::enter(s, seat, data, keys, serial))
    }

    fn leave(&self, seat: &Seat<Tileception>, data: &mut Tileception, serial: Serial) {
        delegate!(self, s => KeyboardTarget::leave(s, seat, data, serial))
    }

    fn key(
        &self,
        seat: &Seat<Tileception>,
        data: &mut Tileception,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        delegate!(self, s => KeyboardTarget::key(s, seat, data, key, state, serial, time))
    }

    fn modifiers(&self, seat: &Seat<Tileception>, data: &mut Tileception, modifiers: ModifiersState, serial: Serial) {
        delegate!(self, s => KeyboardTarget::modifiers(s, seat, data, modifiers, serial))
    }
}
