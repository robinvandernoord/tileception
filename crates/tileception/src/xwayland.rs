//! Xwayland: X11 programs run as clients of an X server that is itself a
//! Wayland client of Tileception. Their windows are arranged like any other.

use std::{os::unix::io::OwnedFd, process::Stdio};

use smithay::{
    delegate_xwayland_shell,
    utils::{Logical, Rectangle},
    wayland::{
        selection::{
            SelectionTarget,
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
                set_primary_selection,
            },
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
        xwm::{Reorder, ResizeEdge, XwmId},
    },
};

use crate::{Tileception, focus::KeyboardFocus};

impl Tileception {
    /// Starts Xwayland; X11 programs can run once it reports ready.
    pub fn start_xwayland(&mut self) {
        let spawned = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(String, String)>(),
            true,
            Stdio::null(),
            Stdio::null(),
            |_| (),
        );
        let (xwayland, client) = match spawned {
            Ok(started) => started,
            Err(err) => {
                tracing::warn!("Xwayland unavailable, X11 programs will not run: {err}");
                self.start_pending_autostart();
                return;
            }
        };
        let inserted = self.loop_handle.insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => {
                match X11Wm::start_wm(state.loop_handle.clone(), x11_socket, client.clone()) {
                    Ok(window_manager) => {
                        state.xwm = Some(window_manager);
                        state.xdisplay = Some(display_number);
                        state.plugins_display_ready(display_number);
                        tracing::info!("Xwayland ready on DISPLAY=:{display_number}");
                        // Only a real session speaks for the user's D-Bus and
                        // systemd; nested and headless ones would take it over.
                        if state.session_services {
                            let _ = std::process::Command::new("dbus-update-activation-environment")
                                .env("DISPLAY", format!(":{display_number}"))
                                .args(["--systemd", "DISPLAY"])
                                .status();
                        }
                    }
                    Err(err) => tracing::error!("could not start the X11 window manager: {err}"),
                }
                state.start_pending_autostart();
            }
            XWaylandEvent::Error => {
                tracing::warn!("Xwayland failed to start");
                state.start_pending_autostart();
            }
        });
        if let Err(err) = inserted {
            tracing::error!("could not watch Xwayland: {err}");
            self.start_pending_autostart();
        }
    }

    /// Drain once readiness or failure is known, so Wayland apps also start
    /// when Xwayland is unavailable.
    fn start_pending_autostart(&mut self) {
        for entry in std::mem::take(&mut self.pending_autostart) {
            tracing::info!(file = %entry.file.display(), "starting XDG autostart entry");
            self.spawn("sh", &["-c".to_string(), entry.command_line(&self.terminal_prefix())]);
        }
    }

    /// Where an X11 menu or tooltip is. A window that becomes one only as it
    /// is mapped (ImageMagick's menus do) keeps the geometry it was created
    /// with in smithay, missing any move or resize before the map. What X
    /// said at the map stands in until X reports a change.
    pub(crate) fn overlay_geometry(&self, overlay: &X11Surface) -> Rectangle<i32, Logical> {
        self.x11_overlay_geometry.get(&overlay.window_id()).copied().unwrap_or_else(|| overlay.geometry())
    }

    /// Place an X11 menu in the same scaled coordinates as its owning window.
    /// Unrelated overlays keep their native screen coordinates.
    pub(crate) fn overlay_transform(
        &self,
        overlay: &X11Surface,
    ) -> Option<(smithay::utils::Point<f64, Logical>, (f64, f64))> {
        let geometry = self.overlay_geometry(overlay);
        let owner = self
            .drawn_windows(self.now())
            .into_iter()
            .filter_map(|drawn| {
                let window = self.windows.get(&drawn.window_id)?;
                let x11 = window.x11_surface()?;
                let same = overlay.is_transient_for().map_or_else(
                    || {
                        self.same_x11_client(x11.window_id(), overlay.window_id())
                            .unwrap_or_else(|| overlay.pid().is_some() && overlay.pid() == x11.pid())
                    },
                    |parent| parent == x11.window_id(),
                );
                same.then_some((drawn, x11.geometry()))
            })
            .max_by_key(|(drawn, parent)| {
                (parent.contains(geometry.loc), self.world.focus() == Some(drawn.window_id), drawn.live)
            });
        match owner {
            Some((drawn, _)) if !drawn.live => None,
            Some((drawn, parent)) => {
                let offset = (geometry.loc - parent.loc).to_f64();
                Some((
                    (drawn.origin.x + offset.x * drawn.scale.0, drawn.origin.y + offset.y * drawn.scale.1).into(),
                    drawn.scale,
                ))
            }
            None => Some((geometry.loc.to_f64(), (1.0, 1.0))),
        }
    }

    /// Our own connection to Xwayland, made when first needed, for asking:
    /// X sends changes made through it to the window manager to decide, as
    /// it does any client's. The window manager's is smithay's, and it lets
    /// go of the server before calling us, so requests here cannot wait on
    /// it.
    fn x11_query(&mut self) -> Option<&x11rb::rust_connection::RustConnection> {
        if self.x11_query.is_none()
            && let Some(display) = self.xdisplay
        {
            match x11rb::rust_connection::RustConnection::connect(Some(&format!(":{display}"))) {
                Ok((connection, _)) => self.x11_query = Some(connection),
                Err(error) => tracing::warn!("could not connect to Xwayland: {error}"),
            }
        }
        self.x11_query.as_ref()
    }

    /// Asks Xwayland where a just-mapped menu or tooltip is.
    fn ask_overlay_geometry(&mut self, overlay: &X11Surface) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let Some(connection) = self.x11_query() else { return };
        let reply = connection.get_geometry(overlay.window_id()).map(|cookie| cookie.reply());
        match reply {
            Ok(Ok(geometry)) => {
                let rect = Rectangle::new(
                    (i32::from(geometry.x), i32::from(geometry.y)).into(),
                    (i32::from(geometry.width), i32::from(geometry.height)).into(),
                );
                if rect != overlay.geometry() {
                    self.x11_overlay_geometry.insert(overlay.window_id(), rect);
                }
            }
            // Gone already: nothing to place.
            Ok(Err(_)) => {}
            Err(error) => {
                tracing::warn!("lost the connection asking Xwayland where its menus are: {error}");
                self.x11_query = None;
            }
        }
    }

    /// The X server's top-level windows, the lowest first.
    pub(crate) fn x11_stacking(&mut self) -> Vec<u32> {
        use x11rb::{connection::Connection as _, protocol::xproto::ConnectionExt as _};
        let Some(connection) = self.x11_query() else { return Vec::new() };
        let Some(root) = connection.setup().roots.first().map(|screen| screen.root) else { return Vec::new() };
        match connection.query_tree(root).map(|cookie| cookie.reply()) {
            Ok(Ok(tree)) => tree.children,
            Ok(Err(_)) => Vec::new(),
            Err(error) => {
                tracing::warn!("lost the connection asking Xwayland how its windows stack: {error}");
                self.x11_query = None;
                Vec::new()
            }
        }
    }

    /// Waits until the X server has `frame` over `others`, a few
    /// milliseconds at most: a raise asked for through the window manager's
    /// connection may not have reached it yet.
    pub(crate) fn settle_x11_raise(&mut self, frame: u32, others: &[u32]) {
        for _ in 0..10 {
            let stacking = self.x11_stacking();
            let height = |window: u32| stacking.iter().position(|stacked| *stacked == window);
            let Some(top) = height(frame) else { return };
            if others.iter().all(|other| height(*other).is_none_or(|other| other < top)) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        tracing::debug!("the X server has not raised window {frame:#x} yet");
    }

    /// Raises an X11 window over the others, with the menus and tooltips
    /// open over it again, as they are drawn: the X server finds the window
    /// a click is in by this stacking, and a menu under a window would take
    /// a click in it for one beside it.
    pub(crate) fn raise_x11(&mut self, window: &X11Surface) {
        let Some(xwm) = self.xwm.as_mut() else { return };
        let _ = xwm.raise_window(window);
        for overlay in &self.x11_overlays {
            let _ = xwm.raise_window(overlay);
        }
    }

    /// Whether two X windows were made by the same X client, the program
    /// behind them: the X server hands each client its own range of ids.
    /// Unknown without our connection to ask how the ranges go.
    pub(crate) fn same_x11_client(&self, one: u32, other: u32) -> Option<bool> {
        use x11rb::connection::Connection as _;
        let mask = self.x11_query.as_ref()?.setup().resource_id_mask;
        Some(one & !mask == other & !mask)
    }

    fn x11_window_id(&self, surface: &X11Surface) -> Option<tileception_core::WindowId> {
        self.windows.iter().find(|(_, window)| window.x11_surface() == Some(surface)).map(|(window_id, _)| *window_id)
    }
}

impl XWaylandShellHandler for Tileception {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

delegate_xwayland_shell!(Tileception);

impl XwmHandler for Tileception {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("X11 events only arrive once the window manager runs")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(geo) = self.x11_refused_configure.remove(&window.window_id()) {
            let _ = window.configure(geo);
        }
        if let Err(err) = window.set_mapped(true) {
            tracing::warn!("could not map X11 window: {err}");
            return;
        }
        self.new_x11_window(window);
    }

    /// Menus and tooltips place themselves; they are drawn above everything.
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.ask_overlay_geometry(&window);
        self.claim_overlay(&window);
        self.x11_overlays.push(window);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_overlays.retain(|overlay| overlay != &window);
        self.x11_overlay_geometry.remove(&window.window_id());
        self.handoff.windows.overlay_gone(&window);
        if let Some(window_id) = self.x11_window_id(&window) {
            self.forget_window(window_id);
        }
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_overlays.retain(|overlay| overlay != &window);
        self.x11_overlay_geometry.remove(&window.window_id());
        self.x11_refused_configure.remove(&window.window_id());
        self.handoff.windows.overlay_gone(&window);
        if let Some(window_id) = self.x11_window_id(&window) {
            self.forget_window(window_id);
        }
    }

    /// Managed windows get the size of their tile whatever they ask for;
    /// windows shown elsewhere keep the size given there; windows we do not
    /// manage yet get what they ask.
    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        if let Some(window_id) = self.x11_window_id(&window)
            && self.output_for_window(window_id).is_some()
        {
            self.apply_layout();
            return;
        }
        if let Some(window_id) = self.x11_window_id(&window)
            && self.away_configure_request(window_id, w, h)
        {
            return;
        }
        // Building on what it asked before, if smithay refused that.
        let mut geo = self.x11_refused_configure.get(&window.window_id()).copied().unwrap_or_else(|| window.geometry());
        if let Some(left) = x {
            geo.loc.x = left;
        }
        if let Some(top) = y {
            geo.loc.y = top;
        }
        if let Some(width) = w {
            geo.size.w = width as i32;
        }
        if let Some(height) = h {
            geo.size.h = height as i32;
        }
        if let Err(smithay::xwayland::xwm::X11SurfaceError::UnsupportedForOverrideRedirect) = window.configure(geo) {
            // Smithay only learns that a window stopped being a menu once it
            // is mapped (ImageMagick reuses its menu window for dialogs), and
            // refuses to place menus until then. It gets this when mapped.
            self.x11_refused_configure.insert(window.window_id(), geo);
        }
    }

    /// Once a menu is one, smithay follows its changes itself.
    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        if window.is_override_redirect() {
            self.x11_overlay_geometry.remove(&window.window_id());
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(window_id) = self.x11_window_id(&window)
            && let Some(area) = self.area_for_window(window_id)
        {
            self.activate_area(&area);
            self.world.focus_window(window_id, self.now());
            self.run(crate::input::Action::Maximize);
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(window_id) = self.x11_window_id(&window) {
            self.set_fullscreen(window_id, true);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(window_id) = self.x11_window_id(&window) {
            self.set_fullscreen(window_id, false);
        }
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(window_id) = self.x11_window_id(&window) {
            self.minimize_window(window_id);
            self.persist_layout();
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(window_id) = self.x11_window_id(&window)
            && let Some(area) = self.area_for_window(window_id)
        {
            self.with_area(&area, |state| {
                let now = state.now();
                if state.world.restore(window_id, now) {
                    state.apply_layout();
                    state.persist_layout();
                }
            });
        }
    }

    fn resize_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32, _edges: ResizeEdge) {}

    /// Dragging an X11 title bar picks the window up, like Mod+drag.
    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        if let Some(window_id) = self.x11_window_id(&window) {
            self.begin_title_drag(window_id);
        }
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        let focus = self.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus());
        matches!(focus, Some(KeyboardFocus::X11(surface)) if surface.xwm_id() == Some(xwm))
    }

    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        let result = match selection {
            SelectionTarget::Clipboard => {
                let clip = current_data_device_selection_userdata(&self.seat).and_then(|data| data.clone());
                if let Some(crate::launcher::Clip::Remote(remote)) = clip {
                    self.handoff_fetch(&remote, mime_type, fd);
                    return;
                }
                if let Some(clip) = clip {
                    clip.send(&mime_type, fd);
                    return;
                }
                request_data_device_client_selection(&self.seat, mime_type, fd).map_err(|err| format!("{err:?}"))
            }
            SelectionTarget::Primary => {
                let clip = current_primary_selection_userdata(&self.seat).and_then(|data| data.clone());
                if let Some(crate::launcher::Clip::Remote(remote)) = clip {
                    self.handoff_fetch(&remote, mime_type, fd);
                    return;
                }
                // Text selected in one of the compositor's own fields.
                if let Some(clip) = clip {
                    clip.send(&mime_type, fd);
                    return;
                }
                request_primary_client_selection(&self.seat, mime_type, fd).map_err(|err| format!("{err:?}"))
            }
        };
        if let Err(err) = result {
            tracing::warn!("could not hand the Wayland selection to X11: {err}");
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        self.selection_offered(selection, Some(mime_types.clone()));
        match selection {
            SelectionTarget::Clipboard => set_data_device_selection(&self.display_handle, &self.seat, mime_types, None),
            SelectionTarget::Primary => set_primary_selection(&self.display_handle, &self.seat, mime_types, None),
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some_and(|data| data.is_none()) {
                    clear_data_device_selection(&self.display_handle, &self.seat);
                    self.selection_offered(selection, None);
                }
            }
            SelectionTarget::Primary => {
                // Only what X11 offered goes: not text selected since in one
                // of the compositor's own fields.
                if current_primary_selection_userdata(&self.seat).is_some_and(|data| data.is_none()) {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }
}
