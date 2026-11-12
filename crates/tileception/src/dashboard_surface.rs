//! Dashboard surfaces: items a plugin draws itself, for animation or
//! anything a picture cannot do.
//!
//! A plugin whose `[dashboard]` block sets `surface = true` is started with
//! `WAYLAND_SOCKET`, one end of a socket pair. The other end joins a Wayland
//! display of its own, which has only `wl_compositor`, `wl_subcompositor`,
//! `wl_shm`, `wp_viewporter`, a pointer-only `wl_seat` and
//! `tileception_dashboard_v1`. Nothing else Tileception offers exists there:
//! no windows, keyboard, clipboard, outputs or screen capture. A client on
//! the usual socket cannot reach the dashboard global at all.
//!
//! Tileception places each item, tells the plugin its size, clips what it
//! draws to that size and frames it with the item's title and the plugin's
//! name. Frame callbacks go out only while the item is on screen.

use std::{collections::HashMap, os::unix::net::UnixStream, sync::Arc, time::Duration};

use smithay::{
    backend::renderer::{
        element::{Kind, surface::render_elements_from_surface_tree},
        gles::GlesRenderer,
    },
    input::{Seat, pointer::PointerHandle},
    reexports::{
        calloop::{Interest, Mode, PostAction, generic::Generic},
        wayland_server::{
            Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource, backend::ClientId,
            protocol::wl_surface::WlSurface,
        },
    },
    wayland::{
        compositor::{self, CompositorState},
        shm::ShmState,
        viewporter::ViewporterState,
    },
};
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    dashboard::{Shown, Slot},
    render::{OutputElement, clip_elements, enlarged, physical_point},
    state::ClientState,
};

#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery, clippy::restriction, unreachable_pub)]
pub mod protocol {
    use smithay::reexports::{wayland_server, wayland_server::protocol::*};

    pub mod __interfaces {
        use smithay::reexports::wayland_server::{backend as wayland_backend, protocol::__interfaces::*};
        wayland_scanner::generate_interfaces!("../../resources/protocols/tileception-dashboard-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("../../resources/protocols/tileception-dashboard-v1.xml");
}

use protocol::{tileception_dashboard_item_v1::TileceptionDashboardItemV1, tileception_dashboard_v1};

/// The role a surface takes as a dashboard item.
const ROLE: &str = "tileception_dashboard_item_v1";
/// The largest buffer side a dashboard item may commit.
const LARGEST_BUFFER: i32 = 4096;

/// The dashboard's own display and the items drawn on it.
#[derive(Default)]
pub struct Surfaces {
    display: Option<Restricted>,
    /// Items by (plugin, item id).
    items: HashMap<(String, String), Item>,
}

/// The display dashboard plugins connect to, and its globals.
struct Restricted {
    handle: DisplayHandle,
    seat: Seat<Tileception>,
    pointer: PointerHandle<Tileception>,
    // Kept for their globals.
    _compositor: CompositorState,
    _shm: ShmState,
    _viewporter: ViewporterState,
}

struct Item {
    surface: WlSurface,
    resource: TileceptionDashboardItemV1,
    /// The last configure sent: width, height and scale.
    configured: Option<(i32, i32, i32)>,
}

/// Who a dashboard client is.
#[derive(Debug, Clone)]
pub struct Owner {
    pub plugin: String,
}

impl Tileception {
    /// A plugin that draws dashboard surfaces started: its connection joins
    /// the dashboard's display.
    pub(crate) fn dashboard_client(&mut self, plugin: String, stream: UnixStream) {
        if self.dashboard_surfaces.display.is_none() {
            match self.open_dashboard_display() {
                Ok(display) => self.dashboard_surfaces.display = Some(display),
                Err(error) => {
                    tracing::warn!("Plugin {plugin}: no dashboard display: {error}");
                    return;
                }
            }
        }
        let Some(display) = &mut self.dashboard_surfaces.display else { return };
        let data = ClientState { dashboard: Some(Owner { plugin: plugin.clone() }), ..ClientState::default() };
        if let Err(error) = display.handle.insert_client(stream, Arc::new(data)) {
            tracing::warn!("Plugin {plugin}: dashboard connection refused: {error}");
        }
    }

    fn open_dashboard_display(&mut self) -> Result<Restricted, Box<dyn std::error::Error>> {
        let display: Display<Tileception> = Display::new()?;
        let handle = display.handle();
        let compositor = CompositorState::new::<Self>(&handle);
        let shm = ShmState::new::<Self>(&handle, Vec::new());
        let viewporter = ViewporterState::new::<Self>(&handle);
        handle.create_global::<Self, tileception_dashboard_v1::TileceptionDashboardV1, ()>(1, ());
        // A seat with a pointer only: no keyboard exists here.
        let mut seat = self.seat_state.new_wl_seat(&handle, "dashboard");
        let pointer = seat.add_pointer();
        self.loop_handle.insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
            #[allow(unsafe_code, reason = "calloop requires an opt-in for mutable access to a registered I/O source")]
            // SAFETY: dispatch_clients borrows the display without dropping or replacing
            // its registered I/O source, which remains owned by the event loop.
            unsafe {
                if let Err(error) = display.get_mut().dispatch_clients(state) {
                    tracing::warn!("dashboard display: {error}");
                }
            }
            Ok(PostAction::Continue)
        })?;
        Ok(Restricted { handle, seat, pointer, _compositor: compositor, _shm: shm, _viewporter: viewporter })
    }

    /// Whether the seat is the dashboard's, whose pointer only reaches
    /// dashboard items.
    pub(crate) fn is_dashboard_seat(&self, seat: &Seat<Tileception>) -> bool {
        self.dashboard_surfaces.display.as_ref().is_some_and(|display| &display.seat == seat)
    }

    /// Sends what the dashboard's clients have been told.
    pub(crate) fn flush_dashboard_clients(&mut self) {
        if let Some(display) = &mut self.dashboard_surfaces.display {
            let _ = display.handle.flush_clients();
        }
    }

    /// A commit on a dashboard item's surface. True when it was one.
    pub(crate) fn dashboard_commit(&mut self, surface: &WlSurface) -> bool {
        if compositor::get_role(surface) != Some(ROLE) {
            return false;
        }
        let size = smithay::wayland::compositor::with_states(surface, |states| {
            states
                .data_map
                .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
                .and_then(|state| state.lock().ok().and_then(|state| state.buffer_size()))
        });
        if size.is_some_and(|size| size.w > LARGEST_BUFFER || size.h > LARGEST_BUFFER) {
            surface.post_error(
                smithay::reexports::wayland_server::protocol::wl_surface::Error::InvalidSize,
                format!("dashboard items are at most {LARGEST_BUFFER} pixels a side"),
            );
            return true;
        }
        if self.dashboard_shown() {
            self.schedule_render();
        }
        true
    }

    /// Whether the plugin's item has drawn something to show.
    pub(crate) fn dashboard_surface_drawn(&self, plugin: &str, item: &str) -> bool {
        self.dashboard_surfaces
            .items
            .get(&(plugin.to_string(), item.to_string()))
            .is_some_and(|found| crate::scale::has_buffer(&found.surface))
    }

    /// The items' surfaces in their slots, clipped to their frames.
    pub(crate) fn dashboard_surface_elements(&self, renderer: &mut GlesRenderer, slots: &[Slot]) -> Vec<OutputElement> {
        let alpha = if self.desk.peeking || !self.launcher.open { self.peek_fade() } else { 1.0 };
        let mut elements = Vec::new();
        for (room, item) in self.placed_items(slots) {
            let origin = physical_point((room.x, room.y), self.scale);
            let surfaces =
                render_elements_from_surface_tree(renderer, &item.surface, origin, 1.0, alpha, Kind::Unspecified);
            let drawn: Vec<OutputElement> = enlarged(surfaces, origin, self.scale).collect();
            elements.extend(clip_elements(drawn, &room, self.scale));
        }
        elements
    }

    /// Items with a slot, with the room inside their frames.
    fn placed_items<'a>(&'a self, slots: &[Slot]) -> Vec<(Rect, &'a Item)> {
        slots
            .iter()
            .filter_map(|slot| match &slot.what {
                Shown::Surface { plugin, surface } => {
                    let item = self.dashboard_surfaces.items.get(&(plugin.clone(), surface.id.clone()))?;
                    Some((crate::dashboard::inner(slot.rect), item))
                }
                _ => None,
            })
            .collect()
    }

    /// The slots the dashboard has now, wherever it shows.
    fn current_dashboard_slots(&self) -> Vec<Slot> {
        if self.dashboard_surfaces.items.is_empty() || !self.dashboard_shown() {
            return Vec::new();
        }
        self.dashboard_slots(self.home_panel())
    }

    /// Tells items their size when it changes; items left out get 0 by 0.
    pub(crate) fn configure_dashboard_items(&mut self) {
        if self.dashboard_surfaces.items.is_empty() || !self.dashboard_shown() {
            return;
        }
        let slots = self.current_dashboard_slots();
        let scale = self.scale.ceil().max(1.0) as i32;
        let mut sizes: HashMap<(String, String), (i32, i32, i32)> = HashMap::new();
        for slot in &slots {
            if let Shown::Surface { plugin, surface } = &slot.what {
                let room = crate::dashboard::inner(slot.rect);
                sizes.insert((plugin.clone(), surface.id.clone()), (room.w as i32, room.h as i32, scale));
            }
        }
        for (key, item) in &mut self.dashboard_surfaces.items {
            let wanted = sizes.get(key).copied().unwrap_or((0, 0, scale));
            if item.configured != Some(wanted) {
                item.resource.configure(wanted.0, wanted.1, wanted.2);
                item.configured = Some(wanted);
            }
        }
    }

    /// Frame callbacks for the items on screen, and only those.
    pub(crate) fn send_dashboard_frames(&self, time: Duration) {
        let Some(output) = self.output.as_ref() else { return };
        if !self.is_active_output() {
            return;
        }
        let slots = self.current_dashboard_slots();
        for (_, item) in self.placed_items(&slots) {
            smithay::desktop::utils::send_frames_surface_tree(
                &item.surface,
                output,
                time,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
    }

    /// The item's surface under the point and where its room starts.
    fn dashboard_surface_under(&self, slots: &[Slot], point: Point) -> Option<(WlSurface, Rect)> {
        self.placed_items(slots)
            .into_iter()
            .find(|(room, _)| room.contains(point))
            .map(|(room, item)| (item.surface.clone(), room))
    }

    /// The pointer over the dashboard: an item's surface gets motion on the
    /// dashboard's own seat; anything else leaves it. True when it took it.
    pub(crate) fn dashboard_pointer_motion(&mut self, point: Point, time: u32) -> bool {
        let Some(display) = &self.dashboard_surfaces.display else { return false };
        let pointer = display.pointer.clone();
        let under = if self.dashboard_shown() && self.launcher.home {
            self.dashboard_surface_under(&self.dashboard_slots(self.home_panel()), point)
        } else {
            None
        };
        let focus =
            under.as_ref().map(|(surface, room)| (surface.clone(), smithay::utils::Point::from((room.x, room.y))));
        let took = focus.is_some();
        pointer.motion(
            self,
            focus,
            &smithay::input::pointer::MotionEvent {
                location: (point.x, point.y).into(),
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        took
    }

    /// A button over an item's surface. True when it took it.
    pub(crate) fn dashboard_pointer_button(&mut self, button: u32, pressed: bool, time: u32) -> bool {
        let Some(display) = &self.dashboard_surfaces.display else { return false };
        let pointer = display.pointer.clone();
        if pointer.current_focus().is_none() {
            return false;
        }
        let state = if pressed {
            smithay::backend::input::ButtonState::Pressed
        } else {
            smithay::backend::input::ButtonState::Released
        };
        pointer.button(
            self,
            &smithay::input::pointer::ButtonEvent {
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time,
                button,
                state,
            },
        );
        pointer.frame(self);
        true
    }

    /// Scrolling over an item's surface. True when it took it.
    pub(crate) fn dashboard_pointer_axis(&mut self, frame: smithay::input::pointer::AxisFrame) -> bool {
        let Some(display) = &self.dashboard_surfaces.display else { return false };
        let pointer = display.pointer.clone();
        if pointer.current_focus().is_none() {
            return false;
        }
        pointer.axis(self, frame);
        pointer.frame(self);
        true
    }

    /// Whether the pointer is over a dashboard item's surface.
    pub(crate) fn dashboard_has_pointer(&self) -> bool {
        self.dashboard_surfaces.display.as_ref().is_some_and(|display| display.pointer.current_focus().is_some())
    }

    /// Where the welcome screen's panel is, for the dashboard beside it.
    pub(crate) fn home_panel(&self) -> Rect {
        if self.launcher.open && self.launcher.home { self.launcher_panel() } else { self.home_centre() }
    }
}

impl GlobalDispatch<tileception_dashboard_v1::TileceptionDashboardV1, ()> for Tileception {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<tileception_dashboard_v1::TileceptionDashboardV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        client.get_data::<ClientState>().is_some_and(|data| data.dashboard.is_some())
    }
}

impl Dispatch<tileception_dashboard_v1::TileceptionDashboardV1, ()> for Tileception {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &tileception_dashboard_v1::TileceptionDashboardV1,
        request: tileception_dashboard_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let tileception_dashboard_v1::Request::GetItem { id, surface, item } = request else { return };
        let Some(owner) = client.get_data::<ClientState>().and_then(|data| data.dashboard.clone()) else { return };
        if compositor::get_role(&surface) != Some(ROLE) && compositor::give_role(&surface, ROLE).is_err() {
            resource.post_error(tileception_dashboard_v1::Error::Role, "the surface already has another role");
            return;
        }
        let key = (owner.plugin, item);
        if state.dashboard_surfaces.items.contains_key(&key) {
            resource.post_error(tileception_dashboard_v1::Error::Taken, "the item already has a surface");
            return;
        }
        let resource = data_init.init(id, key.clone());
        state.dashboard_surfaces.items.insert(key, Item { surface, resource, configured: None });
        state.configure_dashboard_items();
        state.schedule_render();
    }
}

impl Dispatch<TileceptionDashboardItemV1, (String, String)> for Tileception {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &TileceptionDashboardItemV1,
        _request: protocol::tileception_dashboard_item_v1::Request,
        _data: &(String, String),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Only destroy, handled as the object goes.
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &TileceptionDashboardItemV1, data: &(String, String)) {
        if state.dashboard_surfaces.items.get(data).is_some_and(|item| &item.resource == resource) {
            state.dashboard_surfaces.items.remove(data);
            state.schedule_render();
        }
    }
}
