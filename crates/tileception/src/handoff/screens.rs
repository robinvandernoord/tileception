//! Stand-ins for other machines' screens, for the X server here.
//!
//! A program here whose windows are shown on another machine still lives on
//! this machine's X screen: it places its menus from where its windows are
//! on it, fits them inside it, and the X server finds the window a click is
//! in by their stacking. So each machine showing windows of ours gets an
//! output here as big as its screen, right of ours and seen by Xwayland
//! alone, and its windows sit on it where that machine shows them.

use std::collections::{BTreeMap, BTreeSet};

use smithay::{
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::backend::GlobalId,
    utils::{Logical, Point, Transform},
};
use tileception_handoff::{PeerId, Room};

use crate::Tileception;

/// Names of the stand-ins start with this, which keeps them from programs
/// other than Xwayland.
pub(crate) const PEER_SCREEN: &str = "handoff-";

struct PeerScreen {
    output: Output,
    global: GlobalId,
    size: (i32, i32),
}

#[derive(Default)]
pub(crate) struct PeerScreens(BTreeMap<PeerId, PeerScreen>);

impl Tileception {
    /// Where `peer`'s screen starts on the X screen here, with its stand-in
    /// made, or resized to `room`.
    pub(crate) fn peer_screen(&mut self, peer: PeerId, room: Room) -> Point<i32, Logical> {
        let size = (room.width.max(1), room.height.max(1));
        let mode = Mode { size: size.into(), refresh: 60_000 };
        match self.handoff.windows.screens.0.get_mut(&peer) {
            Some(screen) if screen.size != size => {
                screen.output.change_current_state(Some(mode), None, None, None);
                screen.output.set_preferred(mode);
                screen.size = size;
            }
            Some(_) => {}
            None => {
                let output = Output::new(
                    format!("{PEER_SCREEN}{peer:016x}"),
                    PhysicalProperties {
                        size: (0, 0).into(),
                        subpixel: Subpixel::Unknown,
                        make: "Tileception".into(),
                        model: "Handoff".into(),
                    },
                );
                output.change_current_state(Some(mode), Some(Transform::Normal), Some(Scale::Integer(1)), None);
                output.set_preferred(mode);
                let global = output.create_global::<Tileception>(&self.display_handle);
                tracing::info!("handoff: a screen of {}x{} for {peer:016x}'s here", size.0, size.1);
                self.handoff.windows.screens.0.insert(peer, PeerScreen { output, global, size });
            }
        }
        self.arrange_peer_screens();
        self.handoff.windows.screens.0.get(&peer).map_or_else(Point::default, |screen| screen.output.current_location())
    }

    /// The stand-ins in a row, right of this machine's own screens.
    fn arrange_peer_screens(&mut self) {
        let bounds = self.desktop_bounds();
        let mut left = bounds.right().round() as i32;
        for screen in self.handoff.windows.screens.0.values() {
            let location = Point::from((left, 0));
            if screen.output.current_location() != location {
                screen.output.change_current_state(None, None, None, Some(location));
            }
            left += screen.size.0;
        }
    }

    /// The stand-ins of machines no longer linked, and showing none of our
    /// windows, go.
    pub(crate) fn drop_peer_screens(&mut self) {
        let mut showing: BTreeSet<PeerId> = self.handoff.windows.away.values().map(|away| away.peer).collect();
        showing.extend(
            self.handoff
                .peers
                .iter()
                .filter(|(_, entry)| matches!(entry.link, super::Link::Linked { .. }))
                .map(|(peer, _)| *peer),
        );
        let gone: Vec<PeerId> =
            self.handoff.windows.screens.0.keys().copied().filter(|peer| !showing.contains(peer)).collect();
        for peer in gone {
            if let Some(screen) = self.handoff.windows.screens.0.remove(&peer) {
                self.display_handle.remove_global::<Tileception>(screen.global);
            }
        }
        self.arrange_peer_screens();
    }

    /// The machine whose screen's stand-in a point on the X screen is on.
    pub(crate) fn peer_screen_at(&self, point: Point<i32, Logical>) -> Option<PeerId> {
        self.handoff.windows.screens.0.iter().find_map(|(peer, screen)| {
            let location = screen.output.current_location();
            let inside = point.x >= location.x
                && point.y >= location.y
                && point.x < location.x + screen.size.0
                && point.y < location.y + screen.size.1;
            inside.then_some(*peer)
        })
    }
}
