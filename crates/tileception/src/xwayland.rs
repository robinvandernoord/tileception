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
                        tracing::info!("Xwayland ready on DISPLAY=:{display_number}");
                        if !state.nested {
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
            self.spawn("sh", &["-c".to_string(), entry.command_line(&format!("{} -e", self.terminal))]);
        }
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
        self.x11_overlays.retain(|overlay| overlay != &window);
        if let Some(window_id) = self.x11_window_id(&window) {
            self.forget_window(window_id);
        }
        if !window.is_override_redirect() {
            let _ = window.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.x11_overlays.retain(|overlay| overlay != &window);
        if let Some(window_id) = self.x11_window_id(&window) {
            self.forget_window(window_id);
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
        if let Some(output) = self.x11_window_id(&window).and_then(|window| self.output_for_window(window)) {
            self.with_output(&output, |state| state.apply_layout());
            return;
        }
        let mut geo = window.geometry();
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
        if let Some(window_id) = self.x11_window_id(&window)
            && let Some(output) = self.output_for_window(window_id)
        {
            self.activate_output(&output);
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
            && let Some(output) = self.output_for_window(window_id)
        {
            self.with_output(&output, |state| {
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
                    clip.send(fd);
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
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }
}
