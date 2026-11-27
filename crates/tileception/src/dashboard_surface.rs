//! Dashboard surfaces: items a plugin draws itself, for animation or
//! anything a picture cannot do.
//!
//! A plugin whose `[dashboard]` block sets `surface = true` is started with
//! `WAYLAND_SOCKET`, one end of a socket pair. The other end joins a Wayland
//! display of its own, which has only `wl_compositor`, `wl_subcompositor`,
//! `wl_shm`, `zwp_linux_dmabuf_v1` where the main display has it,
//! `wp_viewporter`, `wp_fractional_scale_manager_v1`, a pointer-only
//! `wl_seat` and `tileception_dashboard_v1`. Nothing else Tileception offers
//! exists there: no windows, keyboard, clipboard, outputs or screen capture.
//! A client on the usual socket cannot reach the dashboard global at all.
//!
//! A plugin that draws on the GPU passes its buffers as dmabufs, with the
//! formats and device the main display names, and never copies them through
//! shared memory. One that binds the fractional scale manager is told the
//! output's real scale, and draws one buffer pixel to one screen pixel at
//! 1.5 as at 2. Shared memory is limited in `dashboard_shm`.
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
            Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource,
            backend::{ClientId, ObjectId},
            protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
        },
    },
    wayland::{
        compositor::{self, CompositorState, TraversalAction, with_surface_tree_downward},
        dmabuf::DmabufGlobal,
        fractional_scale::FractionalScaleManagerState,
        shm::ShmState,
        viewporter::ViewporterState,
    },
};
use tileception_core::{Point, Rect};

use crate::{
    Tileception,
    dashboard::{Shown, Slot},
    dashboard_shm::ClientShared,
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
pub(crate) const LARGEST_BUFFER: i32 = 4096;

/// The dashboard's own display and the items drawn on it.
#[derive(Default)]
pub struct Surfaces {
    display: Option<Restricted>,
    /// Items by (plugin, item id).
    items: HashMap<(String, String), Item>,
    /// The dmabuf buffers of dashboard clients, which count against the
    /// client's buffers until they are destroyed.
    dmabufs: HashMap<ObjectId, ClientShared>,
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
    _fractional_scale: FractionalScaleManagerState,
    /// None where the main display has no dmabuf either: nested, and
    /// headless on the software renderer.
    _dmabuf: Option<DmabufGlobal>,
}

struct Item {
    surface: WlSurface,
    resource: TileceptionDashboardItemV1,
    /// The last configure sent: width, height and scale.
    configured: Option<(i32, i32, i32)>,
    /// The output scale its surfaces were last told.
    scale: Option<f64>,
}

/// Who a dashboard client is.
#[derive(Debug, Clone)]
pub struct Owner {
    pub plugin: String,
    /// The shared memory objects it has, which are limited.
    pub shared: ClientShared,
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
        let data = ClientState {
            dashboard: Some(Owner { plugin: plugin.clone(), shared: ClientShared::default() }),
            ..ClientState::default()
        };
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
        let fractional_scale = FractionalScaleManagerState::new::<Self>(&handle);
        // The same formats and device as the main display's global, from
        // the same state, so an imported buffer is handled the same way.
        let dmabuf = match (&mut self.dmabuf, &self.dmabuf_feedback) {
            (Some((state, _)), Some(feedback)) => {
                Some(state.create_global_with_default_feedback::<Self>(&handle, feedback))
            }
            _ => None,
        };
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
        Ok(Restricted {
            handle,
            seat,
            pointer,
            _compositor: compositor,
            _shm: shm,
            _viewporter: viewporter,
            _fractional_scale: fractional_scale,
            _dmabuf: dmabuf,
        })
    }

    /// A dashboard client's dmabuf was imported: it counts as one of the
    /// client's buffers, which `admit` made room for.
    pub(crate) fn dashboard_dmabuf(&mut self, buffer: &WlBuffer, shared: ClientShared) {
        // Entries of clients that are gone, whose counts only this map holds.
        self.dashboard_surfaces.dmabufs.retain(|_, counted| Arc::strong_count(counted) > 1);
        self.dashboard_surfaces.dmabufs.insert(buffer.id(), shared);
    }

    /// A buffer is gone; a dashboard client's dmabuf no longer counts.
    /// Buffers of both displays come here, and an object id does not say
    /// which display it is on: a window's buffer may have the same one as a
    /// plugin's. So the entry counts only when the buffer's own display
    /// says it was that plugin's.
    pub(crate) fn dashboard_buffer_destroyed(&mut self, buffer: &WlBuffer) {
        let Some(owner) = crate::dashboard_shm::shared_of_buffer(buffer) else { return };
        let ours = self.dashboard_surfaces.dmabufs.get(&buffer.id()).is_some_and(|shared| Arc::ptr_eq(shared, &owner));
        if ours
            && let Some(shared) = self.dashboard_surfaces.dmabufs.remove(&buffer.id())
            && let Ok(mut counted) = shared.lock()
        {
            counted.shared.buffer_destroyed();
        }
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
        // Every surface of a dashboard client, subsurfaces too.
        let ours = surface
            .client()
            .is_some_and(|client| client.get_data::<ClientState>().is_some_and(|data| data.dashboard.is_some()));
        if !ours {
            return false;
        }
        let pixels = smithay::wayland::compositor::with_states(surface, |states| {
            let scale =
                states.cached_state.get::<smithay::wayland::compositor::SurfaceAttributes>().current().buffer_scale;
            states
                .data_map
                .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
                .and_then(|state| state.lock().ok().and_then(|state| state.buffer_size()))
                .map(|size| (size, scale))
        });
        let too_large = pixels.is_some_and(|(size, scale)| {
            let scale = scale.max(1);
            size.w.saturating_mul(scale) > LARGEST_BUFFER || size.h.saturating_mul(scale) > LARGEST_BUFFER
        });
        if too_large {
            surface.post_error(
                smithay::reexports::wayland_server::protocol::wl_surface::Error::InvalidSize,
                format!("dashboard buffers are at most {LARGEST_BUFFER} pixels a side"),
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
                    let item = self.dashboard_surfaces.items.get(&(plugin.clone(), surface.key.clone()))?;
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

    /// Every surface of the items on screen, subsurfaces too.
    pub(crate) fn with_dashboard_surfaces(&self, mut each: impl FnMut(&WlSurface, &compositor::SurfaceData)) {
        let slots = self.current_dashboard_slots();
        for (_, item) in self.placed_items(&slots) {
            with_surface_tree_downward(
                &item.surface,
                (),
                |_, _, ()| TraversalAction::DoChildren(()),
                |surface, states, ()| each(surface, states),
                |_, _, ()| true,
            );
        }
    }

    /// Tells items their size when it changes; items left out get 0 by 0.
    pub(crate) fn configure_dashboard_items(&mut self) {
        if self.dashboard_surfaces.items.is_empty() || !self.dashboard_shown() {
            return;
        }
        let slots = self.current_dashboard_slots();
        // The whole scale for the configure event; the output's own, which
        // may be fractional, through the surfaces themselves.
        let output_scale = self.scale;
        let scale = output_scale.ceil().max(1.0) as i32;
        let mut sizes: HashMap<(String, String), (i32, i32, i32)> = HashMap::new();
        for slot in &slots {
            if let Shown::Surface { plugin, surface } = &slot.what {
                let room = crate::dashboard::inner(slot.rect);
                sizes.insert((plugin.clone(), surface.key.clone()), (room.w as i32, room.h as i32, scale));
            }
        }
        for (key, item) in &mut self.dashboard_surfaces.items {
            let wanted = sizes.get(key).copied().unwrap_or((0, 0, scale));
            if item.configured != Some(wanted) {
                item.resource.configure(wanted.0, wanted.1, wanted.2);
                item.configured = Some(wanted);
            }
            if item.scale != Some(output_scale) {
                tell_scale(&item.surface, output_scale);
                item.scale = Some(output_scale);
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

    /// The item's surface that takes the pointer here, if any: only where
    /// the welcome screen itself takes it, not under the notification panel
    /// or an overlay drawn over it.
    fn dashboard_target(&self, point: Point) -> Option<(WlSurface, Rect)> {
        if !(self.dashboard_shown() && self.launcher.home && self.launcher_takes(point)) || self.dashboard_holding() {
            return None;
        }
        self.dashboard_surface_under(&self.dashboard_slots(self.home_panel()), point)
    }

    /// The pointer over the dashboard: an item's surface gets motion on the
    /// dashboard's own seat; anything else leaves it. True when it took it.
    pub(crate) fn dashboard_pointer_motion(&mut self, point: Point, time: u32) -> bool {
        let Some(display) = &self.dashboard_surfaces.display else { return false };
        let pointer = display.pointer.clone();
        let under = self.dashboard_target(point);
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

    /// Whether the pointer is over a dashboard item's surface on screen.
    pub(crate) fn dashboard_has_pointer(&self) -> bool {
        self.dashboard_shown()
            && self.dashboard_surfaces.display.as_ref().is_some_and(|display| display.pointer.current_focus().is_some())
    }

    /// Takes the pointer from an item that is no longer under it because
    /// the dashboard went away or its items moved, without waiting for the
    /// mouse to move.
    pub(crate) fn refresh_dashboard_pointer(&mut self) {
        let Some(display) = &self.dashboard_surfaces.display else { return };
        let Some(focus) = display.pointer.current_focus() else { return };
        let point = Point::new(self.pointer.x, self.pointer.y);
        if self.dashboard_target(point).is_some_and(|(surface, _)| surface == focus) {
            return;
        }
        let time = self.start.elapsed().as_millis() as u32;
        self.dashboard_pointer_motion(point, time);
    }

    /// Where the welcome screen's panel is, for the dashboard beside it.
    pub(crate) fn home_panel(&self) -> Rect {
        if self.launcher.open && self.launcher.home { self.launcher_panel() } else { self.home_centre() }
    }
}

/// Tells the item's surface and its subsurfaces the output's scale: the
/// preferred buffer scale, and the fractional scale to those that asked.
fn tell_scale(surface: &WlSurface, scale: f64) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, ()| TraversalAction::DoChildren(()),
        |surface, states, ()| crate::scale::send_scale_state(surface, states, scale),
        |_, _, ()| true,
    );
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
        let tileception_dashboard_v1::Request::GetItem { id: made, surface, item } = request else { return };
        let Some(owner) = client.get_data::<ClientState>().and_then(|data| data.dashboard.clone()) else { return };
        if compositor::get_role(&surface) != Some(ROLE) && compositor::give_role(&surface, ROLE).is_err() {
            resource.post_error(tileception_dashboard_v1::Error::Role, "the surface already has another role");
            return;
        }
        let mine = state.dashboard_surfaces.items.keys().filter(|(plugin, _)| *plugin == owner.plugin).count();
        if mine >= crate::dashboard::PLUGIN_FRAMES {
            resource.post_error(tileception_dashboard_v1::Error::TooMany, "a plugin has at most six surfaces");
            return;
        }
        let key = (owner.plugin, item);
        if state.dashboard_surfaces.items.contains_key(&key) {
            resource.post_error(tileception_dashboard_v1::Error::Taken, "the item already has a surface");
            return;
        }
        let resource = data_init.init(made, key.clone());
        state.dashboard_surfaces.items.insert(key, Item { surface, resource, configured: None, scale: None });
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
