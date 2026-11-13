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
    pub position: Point<f64, Logical>,
    pub window: Option<WindowId>,
}

pub struct Tileception {
    pub start: Instant,
    pub loop_handle: LoopHandle<'static, Tileception>,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    /// Start the installed binary in this process once the loop stops.
    pub restart: bool,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Tileception>,
    pub data_device_state: DataDeviceState,
    pub clipboard: crate::clipboard::Clipboard,
    pub primary_selection_state: PrimarySelectionState,
    /// Clipboard managers and `wl-copy`/`wl-paste` without a window.
    pub wlr_data_control_state: smithay::wayland::selection::wlr_data_control::DataControlState,
    pub ext_data_control_state: smithay::wayland::selection::ext_data_control::DataControlState,
    /// The window list for taskbars and switchers.
    pub toplevels: crate::toplevels::Toplevels,
    pub pointer_constraints_state: smithay::wayland::pointer_constraints::PointerConstraintsState,
    pub relative_pointer_state: smithay::wayland::relative_pointer::RelativePointerManagerState,
    pub pointer_gestures_state: smithay::wayland::pointer_gestures::PointerGesturesState,
    pub xwayland_shell_state: XWaylandShellState,
    pub xwm: Option<X11Wm>,
    pub xdisplay: Option<u32>,
    /// X11 menus and tooltips, which place themselves, topmost last.
    pub x11_overlays: Vec<X11Surface>,
    /// Where X11 menus really are, by X window, when smithay does not know
    /// (see `Tileception::overlay_geometry`).
    pub x11_overlay_geometry: std::collections::HashMap<u32, smithay::utils::Rectangle<i32, smithay::utils::Logical>>,
    /// A connection of our own to Xwayland, for asking where windows are.
    pub x11_query: Option<x11rb::rust_connection::RustConnection>,
    /// Where X11 windows asked to be before smithay would place them, by X
    /// window, applied when they map.
    pub x11_refused_configure: std::collections::HashMap<u32, smithay::utils::Rectangle<i32, smithay::utils::Logical>>,
    pub popups: PopupManager,
    pub seat: Seat<Self>,
    pub dmabuf: Option<(DmabufState, DmabufGlobal)>,
    pub syncobj_state: Option<DrmSyncobjState>,
    pub cursor_status: CursorImageStatus,
    /// A frame is already asked for when the animated pointer's next picture is due.
    pub cursor_wake_pending: bool,
    /// Set when running as the session's compositor.
    pub udev: Option<Box<crate::udev::Udev>>,

    pub world: World,
    pub windows: HashMap<WindowId, Window>,
    next_window: u64,
    pub output: Option<Output>,
    pub outputs: crate::outputs::Outputs,
    /// Physical pixels per logical pixel on the output (see `scale.rs`).
    pub scale: f64,
    pub fractional_scale_state: smithay::wayland::fractional_scale::FractionalScaleManagerState,
    pub viewporter_state: smithay::wayland::viewporter::ViewporterState,
    pub cursor_shape_state: smithay::wayland::cursor_shape::CursorShapeManagerState,
    pub pointer: Point<f64, Logical>,
    pub config: Config,
    /// Running in a window of another desktop rather than as the session.
    pub nested: bool,
    /// Running as the desktop session (not nested or headless), so desktop
    /// services such as notifications may be started.
    pub session_services: bool,
    pub fade_in: crate::render::FadeIn,
    /// XDG entries waiting for Xwayland startup to finish.
    pub pending_autostart: Vec<tileception_apps::AutostartEntry>,
    /// Private buses for the notification service and media players (tests),
    /// which also allow starting them nested or headless.
    pub notification_bus: Option<String>,
    pub media_bus: Option<String>,
    pub gap_drag: Option<GapDrag>,
    pub move_drag: Option<MoveDrag>,
    pub resize_drag: Option<ResizeDrag>,
    /// A program's drag and drop in progress here.
    pub drag: Option<crate::drag::LocalDrag>,
    /// Scrolling not yet turned into wheel binding steps.
    pub wheel_accum: (f64, f64),
    /// The touchpad gesture in progress.
    pub gesture: Option<crate::gestures::Gesture>,
    pub touch: crate::touch::Touch,
    /// Touchpads, set up again from `[touchpad]` when the config reloads.
    pub touchpads: Vec<smithay::reexports::input::Device>,
    /// A modifier pressed on its own, which fires its tap binding if it is
    /// let go before anything else happens.
    pub tap_armed: Option<crate::config::Mods>,
    pub tap_fired: Vec<crate::commands::Command>,
    /// Super+D: held to peek, tapped to clear the desk.
    pub desk: crate::desk::Desk,
    pub dashboard: crate::dashboard::Dashboard,
    pub dashboard_surfaces: crate::dashboard_surface::Surfaces,
    pub key_repeat: crate::key_repeat::KeyRepeat,
    /// Set by a lock screen or greeter key to repeat while it is held.
    pub greeter_repeat: Option<crate::key_repeat::Repeat>,
    pub pending_press: Option<PendingPress>,
    pub terminal: String,
    pub ipc_socket: Option<std::path::PathBuf>,
    pub(crate) keyboard_focus: Option<WindowId>,
    keyboard_dirty: bool,
    pub layer_shell_state: WlrLayerShellState,
    /// A layer surface (launcher, lock screen) holding the keyboard.
    pub layer_focus: Option<WlSurface>,
    /// The window covering the whole output, if any.
    pub fullscreen: Option<WindowId>,
    pub session_lock_state: smithay::wayland::session_lock::SessionLockManagerState,
    pub lock: crate::lock::LockState,
    pub builtin_locker: crate::builtin_lock::BuiltinLocker,
    /// Set in `--greeter` mode.
    pub greeter: Option<Box<crate::greeter::Greeter>>,
    pub pending_captures: Vec<crate::capture::PendingCapture>,
    /// The pointer image for captures that ask for it, loaded on first use.
    pub capture_cursor: Option<crate::cursor::Cursor>,
    pub theme: crate::theme::Theme,
    pub text: std::cell::RefCell<crate::text::Text>,
    pub shapes: std::cell::OnceCell<crate::shapes::Shapes>,
    /// Blurred backdrops for frosted panels.
    pub frost: std::cell::RefCell<crate::frost::Frost>,
    pub power_menu: Option<crate::power::PowerMenu>,
    pub feedback: Option<crate::feedback::Feedback>,
    /// A colour pick in progress, which takes the pointer and keyboard.
    pub colour_pick: Option<crate::colour_pick::ColourPick>,
    pub launcher: crate::launcher::Launcher,
    /// Plugins, once started.
    pub plugins: Option<crate::plugins::Host>,
    pub settings: crate::settings::Settings,
    pub(crate) ui_pressed: Option<crate::ui::PointerTarget>,
    /// A middle click pasted into one of the compositor's own fields: its
    /// release is not for anything else.
    pub(crate) middle_pasted: bool,
    /// Line icons for the compositor's own interface.
    pub glyphs: crate::ui::Glyphs,
    pub notifications: crate::notifications::Notifications,
    /// Volume, media players and brightness.
    pub media: crate::media::Media,
    /// Plugin blocks in the notification panel.
    pub panel: crate::panel::Panel,
    /// Commands answered later, by plugins: the one just handled, and the
    /// connections waiting for their answer.
    pub ipc: crate::ipc::Waiting,
    /// The panel for asking an agent about the setup.
    pub doctor: crate::doctor::Doctor,
    /// The rubber duck on the bottom of the screen.
    pub duck: crate::duck::Duck,
    /// Light or dark for applications, through the desktop portal.
    pub appearance: Option<crate::appearance::AppearanceService>,
    pub screensaver: Option<crate::screensaver::ScreenSaverService>,
    /// Applications typing through the desktop portal (KeePassXC's auto-type).
    pub typing: crate::remote_desktop::Typing,
    /// Applications' global shortcuts through the desktop portal.
    pub shortcuts: crate::global_shortcuts::Shortcuts,
    pub selection: crate::selection::Selection,
    /// The strip of minimized windows at the bottom edge.
    pub strip: crate::minimized::Strip,
    /// The bar along the top edge.
    pub bar: crate::bar::Bar,
    pub tray: crate::tray::Tray,
    pub persistence: crate::persistence::Persistence,
    pub idle: crate::idle::Idle,
    pub battery: crate::battery::Battery,
    pub updates: crate::updates::Updates,
    /// The password prompt, for polkit or the update manager's `sudo`.
    pub(crate) password_prompt: Option<crate::password::Prompt>,
    pub polkit: crate::polkit::Polkit,
    /// This instance may be the polkit agent at all.
    pub polkit_wanted: bool,
    pub network: crate::network::Network,
    pub sleep: crate::sleep::Sleep,
    pub handoff: crate::handoff::Handoff,
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
        let display_handle = display.handle();

        let compositor_state = CompositorState::new::<Self>(&display_handle);
        crate::capture::init_screencopy(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);
        let wlr_data_control_state = smithay::wayland::selection::wlr_data_control::DataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );
        let ext_data_control_state = smithay::wayland::selection::ext_data_control::DataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );
        let xwayland_shell_state = XWaylandShellState::new::<Self>(&display_handle);

        let mut seat: Seat<Self> = seat_state.new_wl_seat(&display_handle, "seat0");
        let keyboard_config = &config.file.keyboard;
        let (model, layout, variant, options) = keyboard_config.names();
        let xkb = XkbConfig { layout: &layout, variant: &variant, model: &model, options, ..Default::default() };
        let (delay, rate) = (keyboard_config.repeat_delay.unwrap_or(400), keyboard_config.repeat_rate.unwrap_or(30));
        if seat.add_keyboard(xkb, delay, rate).is_err() {
            tracing::warn!("keyboard layout {layout:?} {variant:?} did not compile, using us");
            seat.add_keyboard(Default::default(), delay, rate).expect("default keymap compiles");
        }
        seat.add_pointer();
        seat.add_touch();

        let socket_name = Self::listen(display, event_loop);

        let launcher = crate::launcher::Launcher::default();
        let terminal = std::env::var("TILECEPTION_TERMINAL")
            .ok()
            .or_else(|| config.file.general.terminal.clone())
            .unwrap_or_else(|| default_terminal(launcher.apps()));
        let settings = config.world_settings();
        let theme = config.theme.clone();

        Tileception {
            start: Instant::now(),
            loop_handle: event_loop.handle(),
            socket_name,
            display_handle: display_handle.clone(),
            loop_signal: event_loop.get_signal(),
            restart: false,
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            clipboard: Default::default(),
            primary_selection_state,
            wlr_data_control_state,
            ext_data_control_state,
            toplevels: crate::toplevels::Toplevels::new(&display_handle),
            pointer_constraints_state: smithay::wayland::pointer_constraints::PointerConstraintsState::new::<Self>(
                &display_handle,
            ),
            relative_pointer_state: smithay::wayland::relative_pointer::RelativePointerManagerState::new::<Self>(
                &display_handle,
            ),
            pointer_gestures_state: smithay::wayland::pointer_gestures::PointerGesturesState::new::<Self>(
                &display_handle,
            ),
            xwayland_shell_state,
            xwm: None,
            xdisplay: None,
            x11_overlays: Vec::new(),
            x11_overlay_geometry: std::collections::HashMap::new(),
            x11_query: None,
            x11_refused_configure: std::collections::HashMap::new(),
            popups: PopupManager::default(),
            seat,
            dmabuf: None,
            syncobj_state: None,
            cursor_status: CursorImageStatus::default_named(),
            cursor_wake_pending: false,
            udev: None,
            world: World::new(settings, Rect::new(0.0, 0.0, 1280.0, 800.0)),
            windows: HashMap::new(),
            next_window: 1,
            output: None,
            scale: 1.0,
            fractional_scale_state: smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<Self>(
                &display_handle,
            ),
            viewporter_state: smithay::wayland::viewporter::ViewporterState::new::<Self>(&display_handle),
            cursor_shape_state: smithay::wayland::cursor_shape::CursorShapeManagerState::new::<Self>(&display_handle),
            pointer: (0.0, 0.0).into(),
            config,
            nested,
            gap_drag: None,
            move_drag: None,
            drag: None,
            resize_drag: None,
            wheel_accum: (0.0, 0.0),
            gesture: None,
            touch: Default::default(),
            touchpads: Vec::new(),
            tap_armed: None,
            tap_fired: Vec::new(),
            desk: Default::default(),
            dashboard: Default::default(),
            dashboard_surfaces: Default::default(),
            key_repeat: Default::default(),
            greeter_repeat: None,
            pending_press: None,
            terminal,
            ipc_socket: None,
            outputs: Default::default(),
            keyboard_focus: None,
            keyboard_dirty: false,
            layer_shell_state: WlrLayerShellState::new::<Self>(&display_handle),
            layer_focus: None,
            fullscreen: None,
            session_lock_state: smithay::wayland::session_lock::SessionLockManagerState::new::<Self, _>(
                &display_handle,
                |_| true,
            ),
            lock: Default::default(),
            builtin_locker: Default::default(),
            greeter: None,
            pending_captures: Vec::new(),
            capture_cursor: None,
            theme,
            text: Default::default(),
            shapes: Default::default(),
            frost: Default::default(),
            power_menu: None,
            feedback: None,
            colour_pick: None,
            launcher,
            plugins: None,
            settings: Default::default(),
            ui_pressed: None,
            middle_pasted: false,
            glyphs: Default::default(),
            notifications: Default::default(),
            media: Default::default(),
            panel: Default::default(),
            ipc: Default::default(),
            doctor: Default::default(),
            duck: Default::default(),
            session_services: false,
            fade_in: Default::default(),
            pending_autostart: Vec::new(),
            notification_bus: None,
            media_bus: None,
            appearance: None,
            screensaver: None,
            typing: Default::default(),
            shortcuts: Default::default(),
            selection: Default::default(),
            strip: Default::default(),
            bar: Default::default(),
            tray: Default::default(),
            persistence: crate::persistence::Persistence::load(),
            idle: Default::default(),
            battery: Default::default(),
            updates: Default::default(),
            password_prompt: None,
            polkit: Default::default(),
            polkit_wanted: false,
            network: Default::default(),
            sleep: Default::default(),
            handoff: Default::default(),
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

    /// How to start the configured terminal: the options its desktop entry
    /// declares (`X-TerminalArgExec`, `X-TerminalArgDir`), else those of
    /// well-known terminals.
    pub(crate) fn terminal_args(&self) -> tileception_apps::TerminalArgs {
        let program = self.terminal.split_whitespace().next().unwrap_or_default();
        let declared = self
            .launcher
            .apps()
            .by_program(std::path::Path::new(program))
            .map(|app| app.terminal_args.clone())
            .unwrap_or_default();
        let known = known_terminal_args(terminal_name(&self.terminal));
        tileception_apps::TerminalArgs { exec: declared.exec.or(known.exec), dir: declared.dir.or(known.dir) }
    }

    /// What goes before a command line to run it in the terminal, such as
    /// `ghostty -e`.
    pub(crate) fn terminal_prefix(&self) -> String {
        let exec = self.terminal_args().exec.unwrap_or_else(|| "-e".into());
        format!("{} {}", self.terminal.trim(), shell_word(&exec))
    }

    /// Run a shell line in the configured terminal, keeping quoted arguments.
    pub(crate) fn run_in_terminal(&self, line: &str) -> Result<(), String> {
        if self.terminal.trim().is_empty() {
            return Err("No terminal configured".into());
        }
        let line = shell_word(line);
        // LXTerminal takes the whole command as one string.
        let command = if terminal_name(&self.terminal) == "lxterminal" {
            format!("exec {} --command {}", self.terminal.trim(), shell_word(&format!("sh -c {line}")))
        } else {
            format!("exec {} sh -c {line}", self.terminal_prefix())
        };
        self.spawn("sh", &["-c".into(), command]);
        Ok(())
    }

    /// Opens the configured terminal in a folder. Terminals get the folder
    /// as their own option too: started without a terminal on stdin, some
    /// (Ghostty) take it for a desktop launch, open in the home folder and
    /// hand the window to an instance already running.
    pub(crate) fn open_terminal_in(&self, directory: &str) {
        let options = self
            .terminal_args()
            .dir_args(directory)
            .unwrap_or_default()
            .iter()
            .map(|arg| shell_word(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let command = format!("cd {} || exit; exec {} {options}", shell_word(directory), self.terminal.trim());
        self.spawn("sh", &["-c".into(), command]);
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
            .envs(self.ipc_socket.iter().map(|socket| ("TILECEPTION_SOCKET", socket)));
        if self.session_services {
            command.env("XDG_SESSION_TYPE", "wayland");
            command.env(
                "XDG_CURRENT_DESKTOP",
                std::env::var_os("XDG_CURRENT_DESKTOP").unwrap_or_else(|| "Tileception".into()),
            );
        }
        // X11 programs go to our Xwayland, never to a host X server.
        match self.xdisplay {
            Some(number) => command.env("DISPLAY", format!(":{number}")),
            None => command.env_remove("DISPLAY"),
        };
        command
    }

    pub fn window_id(&self, surface: &WlSurface) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, window)| window.wl_surface().is_some_and(|candidate| *candidate == *surface))
            .map(|(window_id, _)| *window_id)
    }

    pub(crate) fn next_id(&mut self) -> WindowId {
        let window_id = WindowId(self.next_window);
        self.next_window += 1;
        window_id
    }

    pub fn new_window(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|pending| {
            pending.states.set(xdg_toplevel::State::TiledLeft);
            pending.states.set(xdg_toplevel::State::TiledRight);
            pending.states.set(xdg_toplevel::State::TiledTop);
            pending.states.set(xdg_toplevel::State::TiledBottom);
        });
        let window_id = self.next_id();
        self.settings_client_window(&toplevel, window_id);
        self.updates_client_window(&toplevel, window_id);
        self.windows.insert(window_id, Window::new_wayland_window(toplevel));
    }

    pub fn new_x11_window(&mut self, surface: X11Surface) {
        let window_id = self.next_id();
        self.windows.insert(window_id, Window::new_x11_window(surface));
        self.place_window(window_id);
    }

    /// Puts a new window into the layout: dialogs and fixed-size windows
    /// float, everything else docks. Wayland windows call this on their first
    /// commit, once they have said what they are.
    pub fn place_window(&mut self, window_id: WindowId) {
        if self.send_child_away(window_id) {
            return;
        }
        let parent_area = self
            .windows
            .get(&window_id)
            .and_then(|window| window.toplevel())
            .and_then(|toplevel| toplevel.parent())
            .and_then(|parent| self.window_id(&parent))
            .and_then(|parent| self.area_for_window(parent));
        if let Some(area) = parent_area
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.place_window(window_id));
            return;
        }
        let Some(window) = self.windows.get(&window_id) else { return };
        let rule = crate::rules::decide(&self.config.file.rules, &app_id(window), &window_title(window));
        let float = rule.float.unwrap_or_else(|| {
            self.is_settings_window(window_id) || self.is_updates_window(window_id) || wants_float(window)
        });
        let placement = if float { Placement::Float } else { Placement::Dock };
        tracing::info!(window = window_id.0, app = app_id(window), ?placement, "new window");
        // The new window takes the keyboard. Behind a fullscreen window it
        // would do so unseen, so that one returns to the layout first.
        if let Some(full) = self.fullscreen.filter(|full| *full != window_id && !rule.minimized) {
            self.set_fullscreen(full, false);
        }
        let now = self.now();
        self.world.add_window(window_id, placement, now);
        self.restore_window_layout(window_id);
        if let Some([width, height]) = rule.size.filter(|_| float) {
            self.world.size_float(window_id, f64::from(width), f64::from(height));
        }
        if let Some(x11) = self.windows.get(&window_id).and_then(|placed| placed.x11_surface()) {
            let geometry = x11.geometry();
            if self.world.float_needs_size(window_id) && geometry.size.w > 0 && geometry.size.h > 0 {
                self.world.size_float(window_id, geometry.size.w as f64, geometry.size.h as f64);
            }
        }
        self.apply_layout();
        if let Some(toplevel) = self.windows.get(&window_id).and_then(|placed| placed.toplevel()) {
            toplevel.send_configure();
            // The layout held the keyboard back until this first configure.
            self.force_keyboard_focus_sync();
        }
        if rule.minimized {
            self.minimize_window(window_id);
        } else if rule.fullscreen {
            self.set_fullscreen(window_id, true);
        }
    }

    pub fn remove_window(&mut self, surface: &WlSurface) {
        if let Some(window_id) = self.window_id(surface) {
            self.forget_window(window_id);
        }
    }

    pub fn forget_window(&mut self, window_id: WindowId) {
        if let Some(area) = self.area_for_window(window_id)
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.forget_window(window_id));
            return;
        }
        self.selection.windows.remove(&window_id);
        if self.windows.remove(&window_id).is_none() {
            return;
        }
        self.window_forgotten(window_id);
        if self.move_drag.is_some_and(|drag| drag.window == window_id) {
            self.move_drag = None;
        }
        if self.resize_drag.is_some_and(|drag| drag.window == window_id) {
            self.resize_drag = None;
        }
        if self.fullscreen == Some(window_id) {
            self.fullscreen = None;
        }
        let now = self.now();
        self.world.remove_window(window_id, now);
        if self.keyboard_focus == Some(window_id) {
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
            for placement in self.world.placements(self.world.target_view()) {
                if !placement.rect.intersects(&screen) {
                    continue;
                }
                // Fullscreen windows have their own size; new floating
                // windows choose theirs.
                if Some(placement.window) == self.fullscreen || self.world.float_needs_size(placement.window) {
                    continue;
                }
                let size = slot_size(&placement.rect);
                let Some(window) = self.windows.get(&placement.window) else {
                    // Another machine's window: its owner sizes it, and keeps
                    // its menus on this screen.
                    let room = tileception_handoff::Room {
                        left: (screen.x - placement.rect.x).round() as i32,
                        top: (screen.y - placement.rect.y).round() as i32,
                        width: screen.w.round() as i32,
                        height: screen.h.round() as i32,
                    };
                    self.ask_remote_size(placement.window, size, room);
                    continue;
                };
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|pending| pending.size = Some(size));
                } else if let Some(x11) = window.x11_surface() {
                    // X11 programs place their menus from this position.
                    let loc = (placement.rect.x.round() as i32, placement.rect.y.round() as i32);
                    let _ = x11.configure(Rectangle::new(self.x11_origin() + Point::from(loc), size));
                }
            }
        }
        for (window_id, window) in &self.windows {
            if !self.world.contains_window(*window_id) && !self.world.is_minimized(*window_id) {
                continue;
            }
            window.set_activated(self.is_active_output() && Some(*window_id) == focus);
            // Minimized windows are told they are hidden, so they can stop drawing.
            let hidden = self.world.is_minimized(*window_id);
            if let Some(x11) = window.x11_surface().filter(|x11| x11.is_minimized() != hidden) {
                let _ = x11.set_suspended(hidden);
            }
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|pending| {
                    if hidden {
                        pending.states.set(xdg_toplevel::State::Suspended);
                    } else {
                        pending.states.unset(xdg_toplevel::State::Suspended);
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
        if !self.is_active_output() {
            return;
        }
        let focus = self.world.focus();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        // A closed surface can still hold the keyboard; take it back then.
        let stale = keyboard.current_focus().is_some_and(|focused| !focused.alive());
        if focus == self.keyboard_focus && !self.keyboard_dirty && !stale {
            return;
        }
        self.keyboard_focus = focus;
        self.keyboard_dirty = false;
        let mut remote = None;
        let target = if self.output.is_none() {
            None
        } else if self.is_locked() {
            self.lock_surface().map(|surface| KeyboardFocus::Wayland(surface.clone()))
        } else if self.feedback.is_some()
            || self.password_prompt.is_some()
            || self.power_menu.is_some()
            || self.colour_pick.is_some()
            || self.notifications.answering.is_some()
        {
            None
        } else if let Some(away) = self.away_keyboard_target() {
            // One of this machine's windows, typed into from another machine.
            Some(away)
        } else if self.handoff.forwarding.is_some() {
            None
        } else if let Some(layer) = &self.layer_focus {
            // Over the launcher and the Doctor too: slurp needs its Esc.
            Some(KeyboardFocus::Wayland(layer.clone()))
        } else if self.launcher.open
            || (self.doctor.open && self.doctor.focused)
            || self.duck.typing()
            || self.network.zoom.open
            || self.notifications_keyboard()
        {
            None
        } else {
            remote = focus.filter(|window_id| self.handoff.windows.is_remote(*window_id));
            focus.and_then(|window_id| self.windows.get(&window_id)).and_then(focus_target)
        };
        if let Some(KeyboardFocus::X11(x11)) = &target {
            self.raise_x11(&x11.clone());
        }
        keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
        // Another machine's window takes keys through its owner.
        self.set_remote_keyboard(remote);
    }

    /// Re-evaluates keyboard focus even if the focused window did not change.
    pub fn force_keyboard_focus_sync(&mut self) {
        self.keyboard_dirty = true;
        self.sync_keyboard_focus();
    }

    /// Makes a window cover the whole output, or puts it back in its tile.
    pub fn set_fullscreen(&mut self, window_id: WindowId, enable: bool) {
        if let Some(area) = self.area_for_window(window_id)
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.set_fullscreen(window_id, enable));
            return;
        }
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
        if enable {
            if let Some(old) = self.fullscreen.filter(|old| *old != window_id) {
                self.set_fullscreen(old, false);
            }
        } else if self.fullscreen != Some(window_id) {
            return;
        }
        let Some(window) = self.windows.get(&window_id) else { return };
        let out = self.output_rect();
        let size: Size<i32, Logical> = (out.w as i32, out.h as i32).into();
        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|pending| {
                if enable {
                    pending.states.set(State::Fullscreen);
                    pending.size = Some(size);
                } else {
                    pending.states.unset(State::Fullscreen);
                }
            });
        } else if let Some(x11) = window.x11_surface() {
            let _ = x11.set_fullscreen(enable);
            if enable {
                let _ = x11.configure(Rectangle::new(self.x11_origin(), size));
            }
        }
        self.fullscreen = enable.then_some(window_id);
        if enable {
            self.world.focus_window(window_id, self.now());
        }
        self.apply_layout();
    }

    /// Takes a window out of the layout without closing it, leaving
    /// fullscreen first.
    pub fn minimize_window(&mut self, window_id: WindowId) {
        if let Some(area) = self.area_for_window(window_id)
            && self.context_area().as_ref() != Some(&area)
        {
            self.with_area(&area, |state| state.minimize_window(window_id));
            return;
        }
        if self.fullscreen == Some(window_id) {
            self.set_fullscreen(window_id, false);
        }
        self.selection.windows.remove(&window_id);
        let now = self.now();
        if self.world.minimize(window_id, now) {
            tracing::info!(window = window_id.0, "minimized");
            self.peek_minimized();
        }
        self.apply_layout();
    }

    /// Where this output's top left corner is on the X screen. Xwayland lays
    /// the outputs out as Wayland does, and constrains the X pointer to them:
    /// an X11 window placed in output coordinates on an output that is not
    /// leftmost (or with a gap where an output was) hangs partly off every
    /// output in X, and clicks there land at the nearest output's edge.
    pub fn x11_origin(&self) -> Point<i32, Logical> {
        self.output.as_ref().map_or_else(Default::default, Output::current_location)
    }

    /// The whole output in logical pixels, including what bars reserve.
    pub fn output_rect(&self) -> Rect {
        let Some((output, mode)) = self.output.as_ref().and_then(|active| Some((active, active.current_mode()?)))
        else {
            return self.world.screen();
        };
        // Worked out as layer surfaces are arranged.
        let size = output.current_transform().transform_size(
            mode.size.to_f64().to_logical(output.current_scale().fractional_scale()).to_i32_round::<i32>(),
        );
        if size.w > 0 && size.h > 0 { Rect::new(0.0, 0.0, size.w as f64, size.h as f64) } else { self.world.screen() }
    }

    pub fn close_focused(&self) {
        if let Some(window_id) = self.world.focus() {
            self.close_window(window_id);
        }
    }

    pub fn close_window(&self, window_id: WindowId) {
        if self.close_remote(window_id) {
            return;
        }
        let Some(window) = self.windows.get(&window_id) else { return };
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        } else if let Some(x11) = window.x11_surface() {
            let _ = x11.close();
        }
    }
}

/// Dialogs and fixed-size windows float instead of tiling.
pub(crate) fn wants_float(window: &Window) -> bool {
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
        let fixed = matches!((x11.min_size(), x11.max_size()), (Some(min_size), Some(max_size)) if min_size == max_size && min_size.w > 0);
        let dialog = matches!(
            x11.window_type(),
            Some(WmWindowType::Dialog | WmWindowType::Utility | WmWindowType::Splash | WmWindowType::Toolbar)
        );
        return x11.is_transient_for().is_some() || dialog || fixed;
    }
    false
}

/// The size to ask of a window laid out in `slot`: the whole pixels it holds.
/// Rounding up would make the window half a pixel too big, so it would be
/// drawn a hair smaller than it is, resampled and blurry.
pub(crate) fn slot_size(slot: &Rect) -> Size<i32, Logical> {
    // A slot a rounding error short of a whole pixel still gets that pixel.
    let whole = |length: f64| (length + 1e-6).floor().max(1.0) as i32;
    (whole(slot.w), whole(slot.h)).into()
}

/// The largest size a window lets itself be, either side unlimited if `None`.
pub(crate) fn max_size(window: &Window) -> (Option<i32>, Option<i32>) {
    let size = if let Some(toplevel) = window.toplevel() {
        with_states(toplevel.wl_surface(), |states| states.cached_state.get::<SurfaceCachedState>().current().max_size)
    } else if let Some(max) = window.x11_surface().and_then(|x11| x11.max_size()) {
        max
    } else {
        return (None, None);
    };
    // Zero means no limit on that side.
    ((size.w > 0).then_some(size.w), (size.h > 0).then_some(size.h))
}

/// Not before a window's first configure: foot crashes on an enter then.
pub(crate) fn focus_target(window: &Window) -> Option<KeyboardFocus> {
    if let Some(toplevel) = window.toplevel() {
        toplevel.is_initial_configure_sent().then(|| KeyboardFocus::Wayland(toplevel.wl_surface().clone()))
    } else {
        window.x11_surface().map(|x11| KeyboardFocus::X11(x11.clone()))
    }
}

/// The title a window shows, falling back to its app id.
pub fn window_title(window: &Window) -> String {
    let title = if let Some(toplevel) = window.toplevel() {
        with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|attributes| attributes.title.clone()))
        })
        .unwrap_or_default()
    } else {
        window.x11_surface().map(|x11| x11.title()).unwrap_or_default()
    };
    if title.trim().is_empty() { app_id(window) } else { title }
}

/// The app id (Wayland) or class (X11) of a window.
pub fn app_id(window: &Window) -> String {
    if let Some(toplevel) = window.toplevel() {
        with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|attributes| attributes.app_id.clone()))
        })
        .unwrap_or_default()
    } else {
        window.x11_surface().map(|x11| x11.class()).unwrap_or_default()
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
        self.update_duck();
        self.configure_dashboard_items();
        self.refresh_dashboard_pointer();
        let _ = self.display_handle.flush_clients();
        self.flush_dashboard_clients();
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
        child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn std::io::Read + Send>),
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
                    |remaining| remaining.checked_sub(1),
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

/// The first installed terminal that runs natively on Wayland.
/// The terminal program's file name, from the configured command.
fn terminal_name(terminal: &str) -> &str {
    let program = terminal.split_whitespace().next().unwrap_or_default();
    program.rsplit('/').next().unwrap_or(program)
}

/// The options of well-known terminals, for those whose desktop entry does
/// not declare them.
fn known_terminal_args(name: &str) -> tileception_apps::TerminalArgs {
    let exec = match name {
        "gnome-terminal" => "--",
        "xfce4-terminal" => "-x",
        _ => "-e",
    };
    let dir = match name {
        "ghostty" | "foot" | "alacritty" | "gnome-terminal" | "xfce4-terminal" | "lxterminal" => {
            Some("--working-directory=")
        }
        "kitty" => Some("--directory="),
        "konsole" => Some("--workdir"),
        "wezterm" => Some("start --cwd"),
        _ => None,
    };
    tileception_apps::TerminalArgs { exec: Some(exec.into()), dir: dir.map(Into::into) }
}

/// A string as one shell word.
pub(crate) fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The terminal when none is configured: the user's choice in
/// `xdg-terminals.list`, else the first of these that is installed, else
/// any installed terminal emulator.
fn default_terminal(apps: &tileception_apps::AppIndex) -> String {
    if let Some(chosen) = apps.preferred_terminal() {
        return chosen.exec.clone();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    ["ghostty", "foot", "alacritty", "kitty", "lxterminal", "gnome-terminal"]
        .into_iter()
        .find(|terminal| std::env::split_paths(&path).any(|dir| dir.join(terminal).is_file()))
        .map(String::from)
        .or_else(|| apps.terminal_emulators().next().map(|terminal| terminal.exec.clone()))
        .unwrap_or_else(|| "lxterminal".into())
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// Set for a dashboard plugin's own connection.
    pub dashboard: Option<crate::dashboard_surface::Owner>,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_never_asks_for_more_than_its_slot_holds() {
        for (length, expected) in [(312.5, 312), (312.4, 312), (312.9, 312), (313.0, 313), (312.9999999, 313), (0.3, 1)]
        {
            let size = slot_size(&Rect::new(0.0, 0.0, length, length));
            assert_eq!((size.w, size.h), (expected, expected), "slot {length}");
        }
        // Drawn at its slot, such a window needs no scaling, so it stays sharp.
        for length in [312.5, 631.0, 383.0 / 2.0, 1274.0 / 3.0] {
            let size = slot_size(&Rect::new(0.0, 0.0, length, length));
            assert!(f64::from(size.w) <= length + 1e-6, "slot {length}");
        }
    }
}
