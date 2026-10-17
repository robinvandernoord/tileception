use std::{collections::HashMap, ffi::OsString, sync::Arc, time::Instant};

use smithay::{
    desktop::{PopupManager, Window, space::SpaceElement},
    input::{Seat, SeatState, keyboard::XkbConfig, pointer::CursorImageStatus},
    output::Output,
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::{
        compositor::{CompositorClientState, CompositorState, with_states},
        dmabuf::{DmabufGlobal, DmabufState},
        drm_syncobj::DrmSyncobjState,
        output::OutputManagerState,
        selection::{data_device::DataDeviceState, primary_selection::PrimarySelectionState},
        shell::wlr_layer::WlrLayerShellState,
        shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgShellState, decoration::XdgDecorationState},
        shm::ShmState,
        socket::ListeningSocketSource,
        xwayland_shell::XWaylandShellState,
    },
    xwayland::{X11Surface, X11Wm},
};
use tileception_core::{DropTarget, NodeId, Placement, Rect, WindowId, World};

use smithay::{utils::IsAlive, wayland::seat::WaylandFocus};

use crate::{config::Config, focus::KeyboardFocus};

/// Dragging the gap between two children of a split.
#[derive(Debug, Clone, Copy)]
pub struct GapDrag {
    pub split: NodeId,
    pub index: usize,
    pub last: Point<f64, Logical>,
    pub horizontal: bool,
}

/// A window being dragged to a new place.
#[derive(Debug, Clone, Copy)]
pub struct MoveDrag {
    pub window: WindowId,
    /// Where the pointer holds the picture, as a fraction of its size.
    pub grab: (f64, f64),
    /// Size of the picture that follows the pointer.
    pub size: (f64, f64),
    pub target: Option<DropTarget>,
    /// A floating window moving as itself rather than as a picture.
    pub free: bool,
    pub last: Point<f64, Logical>,
}

/// Resizing a window by dragging with the modifier and the right button.
#[derive(Debug, Clone, Copy)]
pub struct ResizeDrag {
    pub window: WindowId,
    pub side_x: tileception_core::Side,
    pub side_y: tileception_core::Side,
    pub last: Point<f64, Logical>,
}

/// A press that turns into a drag once the pointer moves far enough.
#[derive(Debug, Clone, Copy)]
pub struct PendingPress {
    pub at: Point<f64, Logical>,
    pub window: Option<WindowId>,
}

pub struct Tileception {
    pub start: Instant,
    pub loop_handle: LoopHandle<'static, Tileception>,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Tileception>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    /// Clipboard managers and `wl-copy`/`wl-paste` without a window.
    pub wlr_data_control_state: smithay::wayland::selection::wlr_data_control::DataControlState,
    pub ext_data_control_state: smithay::wayland::selection::ext_data_control::DataControlState,
    /// The window list for taskbars and switchers.
    pub toplevels: crate::toplevels::Toplevels,
    pub pointer_constraints_state: smithay::wayland::pointer_constraints::PointerConstraintsState,
    pub relative_pointer_state: smithay::wayland::relative_pointer::RelativePointerManagerState,
    pub xwayland_shell_state: XWaylandShellState,
    pub xwm: Option<X11Wm>,
    pub xdisplay: Option<u32>,
    /// X11 menus and tooltips, which place themselves, topmost last.
    pub x11_overlays: Vec<X11Surface>,
    pub popups: PopupManager,
    pub seat: Seat<Self>,
    pub dmabuf: Option<(DmabufState, DmabufGlobal)>,
    pub syncobj_state: Option<DrmSyncobjState>,
    pub cursor_status: CursorImageStatus,
    /// Set when running as the session's compositor.
    pub udev: Option<Box<crate::udev::Udev>>,

    pub world: World,
    pub windows: HashMap<WindowId, Window>,
    next_window: u64,
    pub output: Option<Output>,
    /// Physical pixels per logical pixel on the output (see `scale.rs`).
    pub scale: f64,
    pub fractional_scale_state: smithay::wayland::fractional_scale::FractionalScaleManagerState,
    pub viewporter_state: smithay::wayland::viewporter::ViewporterState,
    pub pointer: Point<f64, Logical>,
    pub config: Config,
    /// Running in a window of another desktop rather than as the session.
    pub nested: bool,
    /// Running as the desktop session (not nested or headless), so desktop
    /// services such as notifications may be started.
    pub session_services: bool,
    /// Private buses for the notification service and media players (tests),
    /// which also allow starting them nested or headless.
    pub notification_bus: Option<String>,
    pub media_bus: Option<String>,
    pub gap_drag: Option<GapDrag>,
    pub move_drag: Option<MoveDrag>,
    pub resize_drag: Option<ResizeDrag>,
    /// Scrolling not yet turned into wheel binding steps.
    pub wheel_accum: (f64, f64),
    /// A modifier pressed on its own, which fires its tap binding if it is
    /// let go before anything else happens.
    pub tap_armed: Option<crate::config::Mods>,
    pub tap_fired: Vec<crate::commands::Command>,
    pub pending_press: Option<PendingPress>,
    pub terminal: String,
    pub ipc_socket: Option<std::path::PathBuf>,
    keyboard_focus: Option<WindowId>,
    keyboard_dirty: bool,
    pub layer_shell_state: WlrLayerShellState,
    /// A layer surface (launcher, lock screen) holding the keyboard.
    pub layer_focus: Option<WlSurface>,
    /// The window covering the whole output, if any.
    pub fullscreen: Option<WindowId>,
    pub session_lock_state: smithay::wayland::session_lock::SessionLockManagerState,
    pub lock: crate::lock::LockState,
    pub builtin_locker: crate::builtin_lock::BuiltinLocker,
    pub pending_captures: Vec<crate::capture::PendingCapture>,
    pub theme: crate::theme::Theme,
    pub text: std::cell::RefCell<crate::text::Text>,
    pub shapes: std::cell::OnceCell<crate::shapes::Shapes>,
    pub power_menu: Option<crate::power::PowerMenu>,
    pub launcher: crate::launcher::Launcher,
    /// Line icons for the compositor's own interface.
    pub glyphs: crate::ui::Glyphs,
    pub notifications: crate::notifications::Notifications,
    /// Volume, media players and brightness.
    pub media: crate::media::Media,
    /// The panel for asking an agent about the setup.
    pub doctor: crate::doctor::Doctor,
    /// Light or dark for applications, through the desktop portal.
    pub appearance: Option<crate::appearance::AppearanceService>,
    pub selection: crate::selection::Selection,
    /// The strip of minimized windows at the bottom edge.
    pub strip: crate::minimized::Strip,
    /// The bar along the top edge.
    pub bar: crate::bar::Bar,
    pub persistence: crate::persistence::Persistence,
    pub idle: crate::idle::Idle,
    pub wallpaper: Option<crate::wallpaper::Wallpaper>,
    /// A wallpaper that could not be loaded, so it is not retried every frame.
    pub wallpaper_failed: Option<std::path::PathBuf>,
    /// Set when running without a screen.
    pub headless: Option<Box<crate::headless::Headless>>,
}

impl Tileception {
    pub fn new(
        event_loop: &mut EventLoop<'static, Tileception>,
        display: Display<Tileception>,
        config: Config,
        nested: bool,
    ) -> Self {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        crate::capture::init_screencopy(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let wlr_data_control_state = smithay::wayland::selection::wlr_data_control::DataControlState::new::<Self, _>(
            &dh,
            Some(&primary_selection_state),
            |_| true,
        );
        let ext_data_control_state = smithay::wayland::selection::ext_data_control::DataControlState::new::<Self, _>(
            &dh,
            Some(&primary_selection_state),
            |_| true,
        );
        let xwayland_shell_state = XWaylandShellState::new::<Self>(&dh);

        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        let k = &config.file.keyboard;
        let pick = |file: &Option<String>, env: &str| file.clone().unwrap_or_else(|| env_or_default(env));
        let (layout, variant, model) = (
            pick(&k.layout, "XKB_DEFAULT_LAYOUT"),
            pick(&k.variant, "XKB_DEFAULT_VARIANT"),
            pick(&k.model, "XKB_DEFAULT_MODEL"),
        );
        let xkb = XkbConfig {
            layout: &layout,
            variant: &variant,
            model: &model,
            options: k.options.clone().or_else(|| std::env::var("XKB_DEFAULT_OPTIONS").ok()),
            ..Default::default()
        };
        let (delay, rate) = (k.repeat_delay.unwrap_or(400), k.repeat_rate.unwrap_or(30));
        if seat.add_keyboard(xkb, delay, rate).is_err() {
            tracing::warn!("keyboard layout {layout:?} {variant:?} did not compile, using us");
            seat.add_keyboard(Default::default(), delay, rate).expect("default keymap compiles");
        }
        seat.add_pointer();

        let socket_name = Self::listen(display, event_loop);

        let terminal = std::env::var("TILECEPTION_TERMINAL")
            .ok()
            .or_else(|| config.file.general.terminal.clone())
            .unwrap_or_else(default_terminal);
        let settings = config.world_settings();
        let theme = config.theme.clone();

        Tileception {
            start: Instant::now(),
            loop_handle: event_loop.handle(),
            socket_name,
            display_handle: dh.clone(),
            loop_signal: event_loop.get_signal(),
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            wlr_data_control_state,
            ext_data_control_state,
            toplevels: crate::toplevels::Toplevels::new(&dh),
            pointer_constraints_state: smithay::wayland::pointer_constraints::PointerConstraintsState::new::<Self>(&dh),
            relative_pointer_state: smithay::wayland::relative_pointer::RelativePointerManagerState::new::<Self>(&dh),
            xwayland_shell_state,
            xwm: None,
            xdisplay: None,
            x11_overlays: Vec::new(),
            popups: PopupManager::default(),
            seat,
            dmabuf: None,
            syncobj_state: None,
            cursor_status: CursorImageStatus::default_named(),
            udev: None,
            world: World::new(settings, Rect::new(0.0, 0.0, 1280.0, 800.0)),
            windows: HashMap::new(),
            next_window: 1,
            output: None,
            scale: 1.0,
            fractional_scale_state: smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<Self>(&dh),
            viewporter_state: smithay::wayland::viewporter::ViewporterState::new::<Self>(&dh),
            pointer: (0.0, 0.0).into(),
            config,
            nested,
            gap_drag: None,
            move_drag: None,
            resize_drag: None,
            wheel_accum: (0.0, 0.0),
            tap_armed: None,
            tap_fired: Vec::new(),
            pending_press: None,
            terminal,
            ipc_socket: None,
            keyboard_focus: None,
            keyboard_dirty: false,
            layer_shell_state: WlrLayerShellState::new::<Self>(&dh),
            layer_focus: None,
            fullscreen: None,
            session_lock_state: smithay::wayland::session_lock::SessionLockManagerState::new::<Self, _>(&dh, |_| true),
            lock: Default::default(),
            builtin_locker: Default::default(),
            pending_captures: Vec::new(),
            theme,
            text: Default::default(),
            shapes: Default::default(),
            power_menu: None,
            launcher: Default::default(),
            glyphs: Default::default(),
            notifications: Default::default(),
            media: Default::default(),
            doctor: Default::default(),
            session_services: false,
            notification_bus: None,
            media_bus: None,
            appearance: None,
            selection: Default::default(),
            strip: Default::default(),
            bar: Default::default(),
            persistence: crate::persistence::Persistence::load(),
            idle: Default::default(),
            wallpaper: None,
            wallpaper_failed: None,
            headless: None,
        }
    }

    fn listen(display: Display<Tileception>, event_loop: &mut EventLoop<'static, Tileception>) -> OsString {
        let socket = ListeningSocketSource::new_auto().expect("a free wayland socket name");
        let name = socket.socket_name().to_os_string();
        let handle = event_loop.handle();
        handle
            .insert_source(socket, |stream, _, state| {
                if let Err(err) = state.display_handle.insert_client(stream, Arc::new(ClientState::default())) {
                    tracing::warn!("rejected client: {err}");
                }
            })
            .expect("socket source");
        handle
            .insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
                #[allow(
                    unsafe_code,
                    reason = "calloop requires an opt-in for mutable access to a registered I/O source"
                )]
                // SAFETY: dispatch_clients borrows the display without dropping or replacing
                // its registered I/O source, which remains owned by the event loop.
                unsafe {
                    display.get_mut().dispatch_clients(state).expect("dispatch clients");
                }
                Ok(PostAction::Continue)
            })
            .expect("display source");
        name
    }

    /// Seconds since start, the clock the layout engine runs on.
    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn spawn(&self, cmd: &str, args: &[String]) {
        self.spawn_child(cmd, args, false);
    }

    /// Like [`Self::spawn`], returning the process id. With `group` the
    /// program leads a process group of its own, so it can be stopped with
    /// everything it started.
    pub fn spawn_child(&self, cmd: &str, args: &[String], group: bool) -> Option<u32> {
        let mut command = self.command(cmd, args);
        if group {
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        match command.spawn() {
            Ok(child) => {
                let pid = child.id();
                watch_child(child, label(cmd, args));
                Some(pid)
            }
            Err(err) => {
                tracing::warn!("could not start {cmd}: {err}");
                None
            }
        }
    }

    /// A program set up to run in this session: its Wayland and X11
    /// displays, toolkit preferences and the IPC socket.
    pub fn command(&self, cmd: &str, args: &[String]) -> std::process::Command {
        let mut command = std::process::Command::new(cmd);
        command
            .args(args)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env("GDK_BACKEND", "wayland,x11")
            .env("QT_QPA_PLATFORM", "wayland;xcb")
            .env("MOZ_ENABLE_WAYLAND", "1")
            .env("ELECTRON_OZONE_PLATFORM_HINT", "auto")
            .envs(self.ipc_socket.iter().map(|p| ("TILECEPTION_SOCKET", p)));
        if self.session_services {
            command.env("XDG_SESSION_TYPE", "wayland");
            command.env(
                "XDG_CURRENT_DESKTOP",
                std::env::var_os("XDG_CURRENT_DESKTOP").unwrap_or_else(|| "Tileception".into()),
            );
        }
        // X11 programs go to our Xwayland, never to a host X server.
        match self.xdisplay {
            Some(n) => command.env("DISPLAY", format!(":{n}")),
            None => command.env_remove("DISPLAY"),
        };
        command
    }

    pub fn window_id(&self, surface: &WlSurface) -> Option<WindowId> {
        self.windows.iter().find(|(_, w)| w.wl_surface().is_some_and(|s| *s == *surface)).map(|(id, _)| *id)
    }

    fn next_id(&mut self) -> WindowId {
        let id = WindowId(self.next_window);
        self.next_window += 1;
        id
    }

    pub fn new_window(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::TiledLeft);
            s.states.set(xdg_toplevel::State::TiledRight);
            s.states.set(xdg_toplevel::State::TiledTop);
            s.states.set(xdg_toplevel::State::TiledBottom);
        });
        let id = self.next_id();
        self.windows.insert(id, Window::new_wayland_window(toplevel));
    }

    pub fn new_x11_window(&mut self, surface: X11Surface) {
        let id = self.next_id();
        self.windows.insert(id, Window::new_x11_window(surface));
        self.place_window(id);
    }

    /// Puts a new window into the layout: dialogs and fixed-size windows
    /// float, everything else docks. Wayland windows call this on their first
    /// commit, once they have said what they are.
    pub fn place_window(&mut self, id: WindowId) {
        let Some(window) = self.windows.get(&id) else { return };
        let rule = crate::rules::decide(&self.config.file.rules, &app_id(window), &window_title(window));
        let float = rule.float.unwrap_or_else(|| wants_float(window));
        let placement = if float { Placement::Float } else { Placement::Dock };
        tracing::info!(window = id.0, app = app_id(window), ?placement, "new window");
        let now = self.now();
        self.world.add_window(id, placement, now);
        self.restore_window_layout(id);
        if let Some([w, h]) = rule.size.filter(|_| float) {
            self.world.size_float(id, f64::from(w), f64::from(h));
        }
        if let Some(x11) = self.windows.get(&id).and_then(|w| w.x11_surface()) {
            let g = x11.geometry();
            if self.world.float_needs_size(id) && g.size.w > 0 && g.size.h > 0 {
                self.world.size_float(id, g.size.w as f64, g.size.h as f64);
            }
        }
        self.apply_layout();
        if let Some(toplevel) = self.windows.get(&id).and_then(|w| w.toplevel()) {
            toplevel.send_configure();
        }
        if rule.minimized {
            self.minimize_window(id);
        } else if rule.fullscreen {
            self.set_fullscreen(id, true);
        }
    }

    pub fn remove_window(&mut self, surface: &WlSurface) {
        if let Some(id) = self.window_id(surface) {
            self.forget_window(id);
        }
    }

    pub fn forget_window(&mut self, id: WindowId) {
        self.selection.windows.remove(&id);
        if self.windows.remove(&id).is_none() {
            return;
        }
        if self.move_drag.is_some_and(|d| d.window == id) {
            self.move_drag = None;
        }
        if self.resize_drag.is_some_and(|d| d.window == id) {
            self.resize_drag = None;
        }
        if self.fullscreen == Some(id) {
            self.fullscreen = None;
        }
        let now = self.now();
        self.world.remove_window(id, now);
        if self.keyboard_focus == Some(id) {
            self.keyboard_focus = None;
        }
        self.apply_layout();
    }

    /// Tells clients about their new sizes and focus after the world changed.
    ///
    /// Windows are resized to where the camera is heading, not where it is, so
    /// they redraw during the animation. In Overview nothing is resized: the
    /// windows keep the size they have when framed and are drawn scaled.
    pub fn apply_layout(&mut self) {
        let focus = self.world.focus();
        if !self.world.in_overview() {
            let screen = self.world.screen();
            for p in self.world.placements(self.world.target_view()) {
                if !p.rect.intersects(&screen) {
                    continue;
                }
                // Fullscreen windows have their own size; new floating
                // windows choose theirs.
                if Some(p.window) == self.fullscreen || self.world.float_needs_size(p.window) {
                    continue;
                }
                let Some(window) = self.windows.get(&p.window) else { continue };
                let size: Size<i32, Logical> =
                    (p.rect.w.round().max(1.0) as i32, p.rect.h.round().max(1.0) as i32).into();
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|s| s.size = Some(size));
                } else if let Some(x11) = window.x11_surface() {
                    // X11 programs place their menus from this position.
                    let loc = (p.rect.x.round() as i32, p.rect.y.round() as i32);
                    let _ = x11.configure(Rectangle::new(loc.into(), size));
                }
            }
        }
        for (id, window) in &self.windows {
            window.set_activated(Some(*id) == focus);
            // Minimized windows are told they are hidden, so they can stop drawing.
            let hidden = self.world.is_minimized(*id);
            if let Some(x11) = window.x11_surface().filter(|x| x.is_minimized() != hidden) {
                let _ = x11.set_suspended(hidden);
            }
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|s| {
                    if hidden {
                        s.states.set(xdg_toplevel::State::Suspended);
                    } else {
                        s.states.unset(xdg_toplevel::State::Suspended);
                    }
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
        }
        self.sync_keyboard_focus();
    }

    /// Points the keyboard at what should have it: a launcher that asked for
    /// it, otherwise the focused window.
    fn sync_keyboard_focus(&mut self) {
        let focus = self.world.focus();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        // A closed surface can still hold the keyboard; take it back then.
        let stale = keyboard.current_focus().is_some_and(|f| !f.alive());
        if focus == self.keyboard_focus && !self.keyboard_dirty && !stale {
            return;
        }
        self.keyboard_focus = focus;
        self.keyboard_dirty = false;
        let target = if self.is_locked() {
            self.lock_surface().map(|s| KeyboardFocus::Wayland(s.clone()))
        } else if self.power_menu.is_some() {
            None
        } else if let Some(layer) = &self.layer_focus {
            // Over the launcher and the Doctor too: slurp needs its Esc.
            Some(KeyboardFocus::Wayland(layer.clone()))
        } else if self.launcher.open || (self.doctor.open && self.doctor.focused) {
            None
        } else {
            focus.and_then(|id| self.windows.get(&id)).and_then(focus_target)
        };
        if let (Some(KeyboardFocus::X11(x11)), Some(xwm)) = (&target, self.xwm.as_mut()) {
            let _ = xwm.raise_window(x11);
        }
        keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
    }

    /// Re-evaluates keyboard focus even if the focused window did not change.
    pub fn force_keyboard_focus_sync(&mut self) {
        self.keyboard_dirty = true;
        self.sync_keyboard_focus();
    }

    /// Makes a window cover the whole output, or puts it back in its tile.
    pub fn set_fullscreen(&mut self, id: WindowId, on: bool) {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
        if on {
            if let Some(old) = self.fullscreen.filter(|old| *old != id) {
                self.set_fullscreen(old, false);
            }
        } else if self.fullscreen != Some(id) {
            return;
        }
        let Some(window) = self.windows.get(&id) else { return };
        let out = self.output_rect();
        let size: Size<i32, Logical> = (out.w as i32, out.h as i32).into();
        if let Some(t) = window.toplevel() {
            t.with_pending_state(|s| {
                if on {
                    s.states.set(State::Fullscreen);
                    s.size = Some(size);
                } else {
                    s.states.unset(State::Fullscreen);
                }
            });
        } else if let Some(x11) = window.x11_surface() {
            let _ = x11.set_fullscreen(on);
            if on {
                let _ = x11.configure(Rectangle::new((0, 0).into(), size));
            }
        }
        self.fullscreen = on.then_some(id);
        if on {
            self.world.focus_window(id, self.now());
        }
        self.apply_layout();
    }

    /// Takes a window out of the layout without closing it, leaving
    /// fullscreen first.
    pub fn minimize_window(&mut self, id: WindowId) {
        if self.fullscreen == Some(id) {
            self.set_fullscreen(id, false);
        }
        self.selection.windows.remove(&id);
        let now = self.now();
        if self.world.minimize(id, now) {
            tracing::info!(window = id.0, "minimized");
            self.peek_minimized();
        }
        self.apply_layout();
    }

    /// The whole output in logical pixels, including what bars reserve.
    pub fn output_rect(&self) -> Rect {
        let Some((output, mode)) = self.output.as_ref().and_then(|o| Some((o, o.current_mode()?))) else {
            return self.world.screen();
        };
        // Worked out as layer surfaces are arranged.
        let size = output.current_transform().transform_size(
            mode.size.to_f64().to_logical(output.current_scale().fractional_scale()).to_i32_round::<i32>(),
        );
        if size.w > 0 && size.h > 0 { Rect::new(0.0, 0.0, size.w as f64, size.h as f64) } else { self.world.screen() }
    }

    pub fn close_focused(&self) {
        if let Some(id) = self.world.focus() {
            self.close_window(id);
        }
    }

    pub fn close_window(&self, id: WindowId) {
        let Some(window) = self.windows.get(&id) else { return };
        if let Some(t) = window.toplevel() {
            t.send_close();
        } else if let Some(x11) = window.x11_surface() {
            let _ = x11.close();
        }
    }
}

/// Dialogs and fixed-size windows float instead of tiling.
fn wants_float(window: &Window) -> bool {
    if let Some(toplevel) = window.toplevel() {
        let (min, max) = with_states(toplevel.wl_surface(), |states| {
            let mut guard = states.cached_state.get::<SurfaceCachedState>();
            let data = guard.current();
            (data.min_size, data.max_size)
        });
        let fixed = min.w > 0 && min.h > 0 && min == max;
        return toplevel.parent().is_some() || fixed;
    }
    if let Some(x11) = window.x11_surface() {
        use smithay::xwayland::xwm::WmWindowType;
        let fixed = matches!((x11.min_size(), x11.max_size()), (Some(a), Some(b)) if a == b && a.w > 0);
        let dialog = matches!(
            x11.window_type(),
            Some(WmWindowType::Dialog | WmWindowType::Utility | WmWindowType::Splash | WmWindowType::Toolbar)
        );
        return x11.is_transient_for().is_some() || dialog || fixed;
    }
    false
}

fn focus_target(window: &Window) -> Option<KeyboardFocus> {
    if let Some(t) = window.toplevel() {
        Some(KeyboardFocus::Wayland(t.wl_surface().clone()))
    } else {
        window.x11_surface().map(|x| KeyboardFocus::X11(x.clone()))
    }
}

/// The title a window shows, falling back to its app id.
pub fn window_title(window: &Window) -> String {
    let title = if let Some(t) = window.toplevel() {
        with_states(t.wl_surface(), |s| {
            s.data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok().and_then(|d| d.title.clone()))
        })
        .unwrap_or_default()
    } else {
        window.x11_surface().map(|x| x.title()).unwrap_or_default()
    };
    if title.trim().is_empty() { app_id(window) } else { title }
}

/// The app id (Wayland) or class (X11) of a window.
pub fn app_id(window: &Window) -> String {
    if let Some(t) = window.toplevel() {
        with_states(t.wl_surface(), |s| {
            s.data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok().and_then(|d| d.app_id.clone()))
        })
        .unwrap_or_default()
    } else {
        window.x11_surface().map(|x| x.class()).unwrap_or_default()
    }
}

impl Tileception {
    /// Housekeeping after each frame.
    pub fn after_frame(&mut self) {
        if matches!(self.lock, crate::lock::LockState::BuiltIn)
            && std::mem::take(&mut self.builtin_locker.suspend_after_lock)
        {
            self.spawn("systemctl", &["suspend".into()]);
        }
        for window in self.windows.values() {
            window.refresh();
        }
        self.popups.cleanup();
        self.update_strip();
        self.update_bar();
        self.update_home();
        self.update_toplevels();
        let _ = self.display_handle.flush_clients();
    }
}

/// How a started program is named in the log: the shell line for `sh -c`.
fn label(cmd: &str, args: &[String]) -> String {
    let name = if cmd == "sh" && args.first().map(String::as_str) == Some("-c") {
        args.get(1).cloned().unwrap_or_default()
    } else {
        std::iter::once(cmd.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ")
    };
    name.chars().take(60).collect()
}

/// Lines a started program may write to the log; the rest is dropped, so a
/// chatty or broken program cannot fill the disk through our log.
const CHILD_LOG_LINES: usize = 50;

/// Passes a started program's output to the log (up to a limit) and waits
/// for it to exit, so it does not linger as a zombie.
fn watch_child(mut child: std::process::Child, name: String) {
    use std::io::{BufRead, BufReader};
    let streams: Vec<Box<dyn std::io::Read + Send>> = [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();
    let budget = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(CHILD_LOG_LINES));
    for stream in streams {
        let (name, budget) = (name.clone(), budget.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let left = budget.try_update(
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                    |n| n.checked_sub(1),
                );
                match left {
                    Ok(1) => tracing::info!(target: "program", "[{name}] {line} (further output dropped)"),
                    Ok(_) => tracing::info!(target: "program", "[{name}] {line}"),
                    Err(_) => {} // Over the limit: keep reading so the program never blocks.
                }
            }
        });
    }
    std::thread::spawn(move || match child.wait() {
        Ok(status) if !status.success() => tracing::info!(target: "program", "[{name}] exited with {status}"),
        _ => {}
    });
}

fn env_or_default(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

/// The first installed terminal that runs natively on Wayland.
fn default_terminal() -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    ["ghostty", "foot", "alacritty", "kitty", "lxterminal", "gnome-terminal"]
        .into_iter()
        .find(|t| std::env::split_paths(&path).any(|dir| dir.join(t).is_file()))
        .unwrap_or("lxterminal")
        .to_string()
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
