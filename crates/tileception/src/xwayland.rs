//! Xwayland: X11 programs run as clients of an X server that is itself a
//! Wayland client of Tileception. Their windows are arranged like any other.

use std::{os::unix::io::OwnedFd, process::Stdio};

use smithay::{
    delegate_xwayland_shell,
    utils::{Logical, Rectangle},
    wayland::{
        selection::{
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
                set_primary_selection,
            },
            SelectionTarget,
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
    },
};

use crate::{focus::KeyboardFocus, Tileception};

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
            Ok(x) => x,
            Err(err) => {
                tracing::warn!("Xwayland unavailable, X11 programs will not run: {err}");
                return;
            }
        };
        let inserted = self.loop_handle.insert_source(xwayland, move |event, _, state| match event {
            XWaylandEvent::Ready { x11_socket, display_number } => {
                match X11Wm::start_wm(state.loop_handle.clone(), x11_socket, client.clone()) {
                    Ok(wm) => {
                        state.xwm = Some(wm);
                        state.xdisplay = Some(display_number);
                        tracing::info!("Xwayland ready on DISPLAY=:{display_number}");
                        if !state.nested {
                            std::env::set_var("DISPLAY", format!(":{display_number}"));
                            let _ = std::process::Command::new("dbus-update-activation-environment")
                                .args(["--systemd", "DISPLAY"])
                                .status();
                        }
                    }
                    Err(err) => tracing::error!("could not start the X11 window manager: {err}"),
                }
            }
            XWaylandEvent::Error => tracing::warn!("Xwayland failed to start"),
        });
        if let Err(err) = inserted {
            tracing::error!("could not watch Xwayland: {err}");
        }
    }

    fn x11_window_id(&self, surface: &X11Surface) -> Option<tileception_core::WindowId> {
        self.windows.iter().find(|(_, w)| w.x11_surface() == Some(surface)).map(|(id, _)| *id)
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
        if let Err(err) = window.set_mapped(true) {
            tracing::warn!("could not map X11 window: {err}");
            return;
        }
        self.new_x11_window(window);
    }

    /// Menus and tooltips place themselves; they are drawn above everything.
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_overlays.push(window);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_overlays.retain(|w| w != &window);
        if let Some(id) = self.x11_window_id(&window) {
            self.forget_window(id);
        }
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_overlays.retain(|w| w != &window);
        if let Some(id) = self.x11_window_id(&window) {
            self.forget_window(id);
        }
    }

    /// Managed windows get the size of their tile whatever they ask for;
    /// windows we do not manage yet get what they ask.
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
        if self.x11_window_id(&window).is_some() {
            self.apply_layout();
            return;
        }
        let mut geo = window.geometry();
        if let Some(x) = x {
            geo.loc.x = x;
        }
        if let Some(y) = y {
            geo.loc.y = y;
        }
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        let _ = window.configure(geo);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.x11_window_id(&window) {
            self.world.focus_window(id, self.now());
            self.run(crate::input::Action::Maximize);
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.x11_window_id(&window) {
            self.set_fullscreen(id, true);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(id) = self.x11_window_id(&window) {
            self.set_fullscreen(id, false);
        }
    }

    fn minimize_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn unminimize_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn resize_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32, _edges: ResizeEdge) {}

    /// Dragging an X11 title bar picks the window up, like Mod+drag.
    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        if let Some(id) = self.x11_window_id(&window) {
            self.begin_title_drag(id);
        }
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        let focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        matches!(focus, Some(KeyboardFocus::X11(s)) if s.xwm_id() == Some(xwm))
    }

    fn send_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        let result = match selection {
            SelectionTarget::Clipboard => {
                request_data_device_client_selection(&self.seat, mime_type, fd).map_err(|e| format!("{e:?}"))
            }
            SelectionTarget::Primary => {
                request_primary_client_selection(&self.seat, mime_type, fd).map_err(|e| format!("{e:?}"))
            }
        };
        if let Err(err) = result {
            tracing::warn!("could not hand the Wayland selection to X11: {err}");
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        match selection {
            SelectionTarget::Clipboard => set_data_device_selection(&self.display_handle, &self.seat, mime_types, ()),
            SelectionTarget::Primary => set_primary_selection(&self.display_handle, &self.seat, mime_types, ()),
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &self.seat)
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }
}
