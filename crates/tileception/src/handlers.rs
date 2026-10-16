//! Wayland protocol handlers.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma},
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_drm_syncobj, delegate_output,
    delegate_primary_selection, delegate_seat, delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{find_popup_root_surface, get_popup_toplevel_coords, PopupKind},
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    reexports::{
        calloop::Interest,
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
        wayland_server::{
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
            Client, Resource,
        },
    },
    utils::{Rectangle, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            add_blocker, add_pre_commit_hook, get_parent, is_sync_subsurface, with_states, BufferAssignment,
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
        },
        dmabuf::{get_dmabuf, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        drm_syncobj::{DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState},
        output::OutputHandler,
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            primary_selection::{set_primary_focus, PrimarySelectionHandler, PrimarySelectionState},
            SelectionHandler, SelectionSource, SelectionTarget,
        },
        shell::xdg::{
            decoration::XdgDecorationHandler, PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler,
            XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};

use std::os::unix::io::OwnedFd;

use crate::state::{ClientState, Tileception};

impl CompositorHandler for Tileception {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(xwayland) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &xwayland.compositor_state;
        }
        &client.get_data::<ClientState>().expect("clients are inserted with ClientState").compositor_state
    }

    /// Holds a client's commit back until its GPU work is done, so we never
    /// draw a half-rendered buffer.
    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
            let mut acquire = None;
            let dmabuf = with_states(surface, |data| {
                acquire.clone_from(&data.cached_state.get::<DrmSyncobjCachedState>().pending().acquire_point);
                data.cached_state.get::<SurfaceAttributes>().pending().buffer.as_ref().and_then(|b| match b {
                    BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).cloned().ok(),
                    _ => None,
                })
            });
            let Some(dmabuf) = dmabuf else { return };
            let Some(client) = surface.client() else { return };
            let release = move |state: &mut Tileception| {
                let dh = state.display_handle.clone();
                state.client_compositor_state(&client).blocker_cleared(state, &dh);
            };
            if let Some(point) = acquire {
                if let Ok((blocker, source)) = point.generate_blocker() {
                    let release = release.clone();
                    if state
                        .loop_handle
                        .insert_source(source, move |_, _, state| {
                            release(state);
                            Ok(())
                        })
                        .is_ok()
                    {
                        add_blocker(surface, blocker);
                        return;
                    }
                }
            }
            if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
                if state
                    .loop_handle
                    .insert_source(source, move |_, _, state| {
                        release(state);
                        Ok(())
                    })
                    .is_ok()
                {
                    add_blocker(surface, blocker);
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if self.commit_layer(surface) {
            self.popups.commit(surface);
            return;
        }
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(id) = self.window_id(&root) {
                let window = &self.windows[&id];
                window.on_commit();
                let unplaced = root == *surface && window.toplevel().is_some_and(|t| !t.is_initial_configure_sent());
                let geo = window.geometry();
                if unplaced {
                    self.place_window(id);
                } else if self.world.float_needs_size(id) && geo.size.w > 0 && geo.size.h > 0 {
                    // A new floating window has drawn at the size it wants.
                    self.world.size_float(id, geo.size.w as f64, geo.size.h as f64);
                    self.apply_layout();
                } else if let Some(rect) = self.world.float_rect(id) {
                    // A floating window that grew past its rectangle (a dialog
                    // filling in) gets the room, rather than being shrunk into
                    // a preview that takes no clicks. Not while being resized.
                    let (w, h) = (geo.size.w as f64, geo.size.h as f64);
                    let resizing = self.resize_drag.is_some_and(|d| d.window == id);
                    if !resizing && (w > rect.w + 0.5 || h > rect.h + 0.5) {
                        self.world.grow_float(id, w, h);
                        self.apply_layout();
                    }
                }
            }
        }

        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                // The initial configure is always allowed.
                let _ = popup.send_configure();
            }
        }
    }
}

impl BufferHandler for Tileception {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Tileception {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_compositor!(Tileception);
delegate_shm!(Tileception);

impl SeatHandler for Tileception {
    type KeyboardFocus = crate::focus::KeyboardFocus;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Tileception> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&crate::focus::KeyboardFocus>) {
        use smithay::wayland::seat::WaylandFocus;
        let dh = &self.display_handle;
        let client = focused.and_then(|f| f.wl_surface()).and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }
}

delegate_seat!(Tileception);

impl SelectionHandler for Tileception {
    // Some(clip) is owned by the compositor; None bridges an X11 selection.
    type SelectionUserData = Option<crate::launcher::Clip>;

    /// A Wayland client set the clipboard: offer it to X11 programs too.
    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|s| s.mime_types())) {
                tracing::warn!("could not offer the selection to X11: {err:?}");
            }
        }
    }

    /// A Wayland client pastes what an X11 program copied.
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        clip: &Option<crate::launcher::Clip>,
    ) {
        if let Some(clip) = clip {
            clip.send(fd);
            return;
        }
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.send_selection(ty, mime_type, fd, self.loop_handle.clone()) {
                tracing::warn!("could not paste from X11: {err:?}");
            }
        }
    }
}

impl PrimarySelectionHandler for Tileception {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

delegate_primary_selection!(Tileception);

impl DataDeviceHandler for Tileception {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for Tileception {}
impl ServerDndGrabHandler for Tileception {}

delegate_data_device!(Tileception);

impl smithay::wayland::selection::wlr_data_control::DataControlHandler for Tileception {
    fn data_control_state(&self) -> &smithay::wayland::selection::wlr_data_control::DataControlState {
        &self.wlr_data_control_state
    }
}

smithay::delegate_data_control!(Tileception);

impl smithay::wayland::selection::ext_data_control::DataControlHandler for Tileception {
    fn data_control_state(&self) -> &smithay::wayland::selection::ext_data_control::DataControlState {
        &self.ext_data_control_state
    }
}

smithay::delegate_ext_data_control!(Tileception);

impl OutputHandler for Tileception {}
delegate_output!(Tileception);

impl XdgShellHandler for Tileception {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.new_window(surface);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.remove_window(surface.wl_surface());
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    /// Dragging a title bar picks the window up, like Alt+drag.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        if let Some(id) = self.window_id(surface.wl_surface()) {
            self.begin_title_drag(id);
        }
    }

    /// A menu takes the keyboard and pointer until it is dismissed, so a click
    /// outside closes it.
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        match self.window_id(surface.wl_surface()) {
            Some(id) => self.set_fullscreen(id, true),
            // Not placed yet: remember it for the first configure.
            None => surface.with_pending_state(|s| {
                s.states
                    .set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen);
            }),
        }
    }

    /// The minimize button in a program's own title bar.
    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(surface.wl_surface()) {
            self.minimize_window(id);
            self.persist_layout();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id(surface.wl_surface()) {
            self.set_fullscreen(id, false);
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        use smithay::{
            desktop::{PopupKeyboardGrab, PopupPointerGrab, PopupUngrabStrategy},
            input::pointer::Focus,
        };
        let Some(seat) = Seat::<Tileception>::from_resource(&seat) else { return };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        if self.window_id(&root).is_none() {
            return;
        }
        let root_focus = crate::focus::KeyboardFocus::Wayland(root);
        let Ok(mut grab) = self.popups.grab_popup(root_focus, kind, &seat, serial) else { return };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }
}

delegate_xdg_shell!(Tileception);

impl Tileception {
    /// Keeps popups inside the screen, relative to their window's place on it.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else { return };
        let Some(id) = self.window_id(&root) else { return };
        let Some(placement) = self.world.placements(self.world.target_view()).into_iter().find(|p| p.window == id)
        else {
            return;
        };
        let screen = self.world.screen();
        let mut target =
            Rectangle::new((screen.x as i32, screen.y as i32).into(), (screen.w as i32, screen.h as i32).into());
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= (placement.rect.x as i32, placement.rect.y as i32).into();
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

/// Windows tile, so ask clients not to draw their own title bars and shadows.
impl XdgDecorationHandler for Tileception {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| state.decoration_mode = Some(DecorationMode::ServerSide));
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        toplevel.with_pending_state(|state| state.decoration_mode = Some(DecorationMode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.request_mode(toplevel, DecorationMode::ServerSide);
    }
}

delegate_xdg_decoration!(Tileception);

impl DmabufHandler for Tileception {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf.as_mut().expect("dmabuf global exists when clients use it").0
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let renderer = match (self.udev.as_mut(), self.headless.as_mut()) {
            (Some(u), _) => Some(&mut u.renderer),
            (None, Some(h)) => Some(&mut h.renderer),
            _ => None,
        };
        let ok = renderer.is_some_and(|r| r.import_dmabuf(&dmabuf, None).is_ok());
        if ok {
            let _ = notifier.successful::<Tileception>();
        } else {
            notifier.failed();
        }
    }
}

delegate_dmabuf!(Tileception);

impl DrmSyncobjHandler for Tileception {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.syncobj_state.as_mut()
    }
}

delegate_drm_syncobj!(Tileception);
