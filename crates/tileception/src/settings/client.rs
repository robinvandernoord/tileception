//! A real xdg-shell client for Settings, Updates and the popped-out Doctor. Only the UI model lives in the
//! compositor; buffers and input travel through the regular Wayland protocols.

use std::{
    cell::{Cell, RefCell},
    fs::File,
    io::{Read, Write},
    os::{
        fd::AsFd,
        unix::{fs::FileExt, net::UnixStream},
    },
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Offscreen,
            gles::{GlesRenderer, GlesTexture},
        },
    },
    input::keyboard::xkb,
    reexports::{
        calloop::{RegistrationToken, channel},
        rustix::{
            event::{PollFd, PollFlags, poll},
            fs::{MemfdFlags, memfd_create},
        },
    },
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::{
    wp::{
        cursor_shape::v1::client::{wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1},
        viewporter::client::{wp_viewport, wp_viewporter},
    },
    xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base},
};

use smithay::utils::{Buffer as BufferCoords, Rectangle};

use super::{Mods, Point, Tileception};
use crate::{
    capture::{Redraw, read_region},
    render::OutputElement,
};

pub(crate) enum Event {
    Resize(i32, i32),
    Motion(Point),
    Leave,
    Button(bool),
    /// The middle button went down: it pastes into a text field.
    MiddleClick,
    Scroll(f64),
    Key {
        sym: u32,
        mods: Mods,
        pressed: bool,
        code: u32,
    },
    Unfocus,
    /// The window was asked to close, as by a title bar or keybinding.
    CloseRequested,
    /// The client is gone.
    Closed,
}

/// Which built-in window a client shows: its title, and where its input goes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Settings,
    Updates,
    Doctor,
}

impl Kind {
    fn title(self) -> (&'static str, &'static str) {
        match self {
            Kind::Settings => ("Settings", "tileception-settings"),
            Kind::Updates => ("Updates", "tileception-updates"),
            Kind::Doctor => ("Wizard", "tileception-wizard"),
        }
    }
}

/// A part of a window's picture that changed, with its rows as native-endian
/// ARGB8888.
type Part = (Rectangle<i32, BufferCoords>, Vec<u8>);

/// What changed in a window's picture since the frame before.
struct Frame {
    /// The whole picture's size in physical pixels.
    size: (i32, i32),
    parts: Vec<Part>,
}

/// What a window's last frame left behind on the compositor's side, so that
/// the next frame redraws and reads back only what changed since.
struct Canvas {
    texture: GlesTexture,
    redraw: Redraw,
}

enum Request {
    Frame(Frame),
    Move,
    Cursor(wp_cursor_shape_device_v1::Shape),
}

pub(crate) struct Client {
    requests: mpsc::Sender<Request>,
    wake: Mutex<UnixStream>,
    pub client_id: smithay::reexports::wayland_server::backend::ClientId,
    kind: Kind,
    canvas: RefCell<Option<Canvas>>,
    /// The shapes of this window's frames, apart from any output's.
    shapes: crate::shapes::Frames,
    dirty: Cell<bool>,
    last_draw: Cell<Option<(Instant, f64)>>,
    /// The size of the frame drawn last, in physical pixels.
    last_size: Cell<Option<(i32, i32)>>,
    last_cursor: Cell<Option<smithay::input::pointer::CursorIcon>>,
    pub source: RegistrationToken,
}

impl Client {
    pub fn start(state: &mut super::Tileception, kind: Kind) -> Result<Self, String> {
        let (server, socket) = UnixStream::pair().map_err(|error| error.to_string())?;
        let client = state
            .display_handle
            .insert_client(server, Arc::new(crate::state::ClientState::default()))
            .map_err(|error| error.to_string())?;
        let (send, receive) = channel::channel();
        let client_id = client.id();
        let source = state
            .loop_handle
            .insert_source(receive, move |event, _, state| {
                let channel::Event::Msg(event) = event else { return };
                match kind {
                    Kind::Settings => {
                        if state.settings.client.as_ref().is_some_and(|client| client.client_id == client_id) {
                            state.settings_client_event(event);
                        }
                    }
                    Kind::Updates => state.updates_client_event(&client_id, event),
                    Kind::Doctor => state.doctor_client_event(&client_id, event),
                }
            })
            .map_err(|error| error.to_string())?;
        let (requests, receiver) = mpsc::channel();
        let (wake, reader) = UnixStream::pair().map_err(|error| error.to_string())?;
        wake.set_nonblocking(true).map_err(|error| error.to_string())?;
        let size = match kind {
            Kind::Settings => state.settings.size,
            Kind::Updates => state.updates.window.size,
            Kind::Doctor => state.doctor.size,
        };
        std::thread::spawn(move || {
            if let Err(error) = run(socket, reader, receiver, send.clone(), size, kind) {
                tracing::warn!("{} client: {error}", kind.title().0);
            }
            let _ = send.send(Event::Closed);
        });
        Ok(Self {
            requests,
            wake: Mutex::new(wake),
            client_id: client.id(),
            kind,
            canvas: RefCell::new(None),
            shapes: Default::default(),
            dirty: Cell::new(true),
            last_draw: Cell::new(None),
            last_size: Cell::new(None),
            last_cursor: Cell::new(None),
            source,
        })
    }

    pub fn invalidate(&self) {
        self.dirty.set(true);
    }

    /// Whether to draw a frame of `size` physical pixels now. While the size
    /// keeps changing, at most every [`RESIZE_FRAME_EVERY`]: the window
    /// stretches the last one meanwhile and asks again once it settles.
    pub fn needs_frame(&self, scale: f64, size: (i32, i32)) -> bool {
        let resizing = self.last_size.get().is_some_and(|last| last != size);
        if resizing
            && self
                .last_draw
                .get()
                .is_some_and(|(time, previous_scale)| previous_scale == scale && time.elapsed() < RESIZE_FRAME_EVERY)
        {
            return false;
        }
        let dirty = self.dirty.replace(false);
        if dirty
            || self.last_draw.get().is_none_or(|(time, previous_scale)| {
                previous_scale != scale || time.elapsed() >= Duration::from_secs(1)
            })
        {
            self.last_draw.set(Some((Instant::now(), scale)));
            self.last_size.set(Some(size));
            return true;
        }
        false
    }

    fn request(&self, request: Request) {
        if self.requests.send(request).is_ok() {
            let _ = self.wake.lock().expect("Settings wake mutex is not poisoned").write(&[1]);
        }
    }

    /// Draws `elements` as the window's next frame of `size` physical
    /// pixels and sends the client what changed. A frame that changed
    /// nothing costs no reading back and sends nothing.
    fn render(
        &self,
        renderer: &mut GlesRenderer,
        elements: &[OutputElement],
        size: (i32, i32),
        clear: [f32; 4],
    ) -> Result<(), String> {
        let mut canvas = self.canvas.borrow_mut();
        let result = draw(renderer, &mut canvas, elements, size, clear);
        if result.is_err() {
            // What the texture holds is unknown: start again from nothing.
            *canvas = None;
        }
        let parts = result?;
        if !parts.is_empty() {
            self.request(Request::Frame(Frame { size, parts }));
        }
        Ok(())
    }

    pub fn start_move(&self) {
        self.request(Request::Move);
    }

    pub fn cursor(&self, cursor: smithay::input::pointer::CursorIcon) {
        use smithay::input::pointer::CursorIcon;
        use wp_cursor_shape_device_v1::Shape;
        if self.last_cursor.replace(Some(cursor)) == Some(cursor) {
            return;
        }
        let shape = match cursor {
            CursorIcon::Text => Shape::Text,
            CursorIcon::Pointer => Shape::Pointer,
            CursorIcon::Grab => Shape::Grab,
            CursorIcon::Grabbing => Shape::Grabbing,
            CursorIcon::Crosshair => Shape::Crosshair,
            CursorIcon::EwResize => Shape::EwResize,
            _ => Shape::Default,
        };
        self.request(Request::Cursor(shape));
    }
}

/// Renders into `canvas`, made anew when the size changed, and reads back
/// each part that changed as ARGB8888.
fn draw(
    renderer: &mut GlesRenderer,
    canvas: &mut Option<Canvas>,
    elements: &[OutputElement],
    size: (i32, i32),
    clear: [f32; 4],
) -> Result<Vec<Part>, String> {
    let canvas = match canvas {
        Some(canvas) if canvas.redraw.size() == size => canvas,
        _ => {
            let texture = renderer.create_buffer(Fourcc::Abgr8888, size.into()).map_err(|error| error.to_string())?;
            // The elements are in physical pixels already, as on outputs.
            canvas.insert(Canvas { texture, redraw: Redraw::new(size) })
        }
    };
    let changed = canvas.redraw.draw(renderer, &mut canvas.texture, elements, clear)?;
    changed
        .into_iter()
        .map(|rect| {
            let mut rows = read_region(renderer, &canvas.texture, rect)?;
            // RGBA into native-endian ARGB8888, as the client's buffer holds it.
            for pixel in rows.as_chunks_mut::<4>().0 {
                let [red, green, blue, alpha] = *pixel;
                *pixel = u32::from_be_bytes([alpha, red, green, blue]).to_ne_bytes();
            }
            Ok((rect, rows))
        })
        .collect()
}

impl Tileception {
    /// Draws a built-in window's next frame through its `client` when it
    /// needs one: `size` logical pixels of what `build` gives. Its shapes are
    /// kept from the window's own last frame rather than an output's, so a
    /// shape that did not change keeps its element and is not drawn again.
    pub(crate) fn render_client_window(
        &self,
        renderer: &mut GlesRenderer,
        client: &Client,
        size: (f64, f64),
        build: impl FnOnce(&Self, &mut GlesRenderer) -> Vec<OutputElement>,
    ) {
        let physical = ((size.0 * self.scale).round() as i32, (size.1 * self.scale).round() as i32);
        if !client.needs_frame(self.scale, physical) {
            return;
        }
        let elements = match self.shapes(renderer) {
            Some(shapes) => shapes.drawing_into(&client.shapes, || build(self, renderer)),
            None => build(self, renderer),
        };
        if let Err(error) = client.render(renderer, &elements, physical, self.theme.background.0) {
            tracing::warn!("could not draw the {} window: {error}", client.kind.title().0);
        }
    }
}

struct App {
    events: channel::Sender<Event>,
    surface: wl_surface::WlSurface,
    shell: xdg_surface::XdgSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    viewport: wp_viewport::WpViewport,
    shm: wl_shm::WlShm,
    seat: wl_seat::WlSeat,
    cursor_manager: wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
    cursor: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    cursor_shape: wp_cursor_shape_device_v1::Shape,
    pointer_serial: Option<u32>,
    keyboard: Option<xkb::State>,
    serial: u32,
    size: (i32, i32),
    configured: bool,
    /// The size the window last showed, and when it changed to another:
    /// once it has held still for [`SETTLED`], the compositor is asked for
    /// a sharp frame at that size.
    shown_size: (i32, i32),
    resized: Option<Instant>,
    /// The whole picture as native-endian ARGB8888, `picture_size` physical
    /// pixels, with each frame's changed region copied in.
    picture: Vec<u8>,
    picture_size: (i32, i32),
    /// What changed in the picture since it was last shown: only that is
    /// damaged, so the compositor uploads only that.
    changed: Vec<Rectangle<i32, BufferCoords>>,
}

/// How long a window's size holds still before a resize counts as done.
const SETTLED: Duration = Duration::from_millis(120);
/// While a window's size keeps changing, frames are drawn at most this often;
/// in between it stretches the last one, which costs the compositor nothing.
const RESIZE_FRAME_EVERY: Duration = Duration::from_millis(60);

fn run(
    socket: UnixStream,
    mut wake: UnixStream,
    requests: mpsc::Receiver<Request>,
    events: channel::Sender<Event>,
    size: (f64, f64),
    kind: Kind,
) -> Result<(), String> {
    let connection = Connection::from_socket(socket).map_err(|error| error.to_string())?;
    let (globals, mut queue) = registry_queue_init::<App>(&connection).map_err(|error| error.to_string())?;
    let queue_handle = queue.handle();
    let compositor: wl_compositor::WlCompositor =
        globals.bind(&queue_handle, 4..=6, ()).map_err(|error| error.to_string())?;
    let shell_base: xdg_wm_base::XdgWmBase =
        globals.bind(&queue_handle, 1..=6, ()).map_err(|error| error.to_string())?;
    let shm = globals.bind(&queue_handle, 1..=1, ()).map_err(|error| error.to_string())?;
    let seat: wl_seat::WlSeat = globals.bind(&queue_handle, 1..=8, ()).map_err(|error| error.to_string())?;
    let viewporter: wp_viewporter::WpViewporter =
        globals.bind(&queue_handle, 1..=1, ()).map_err(|error| error.to_string())?;
    let surface = compositor.create_surface(&queue_handle, ());
    let shell = shell_base.get_xdg_surface(&surface, &queue_handle, ());
    let toplevel = shell.get_toplevel(&queue_handle, ());
    let (title, app_id) = kind.title();
    toplevel.set_title(title.into());
    toplevel.set_app_id(app_id.into());
    toplevel.set_min_size(480, 360);
    let viewport = viewporter.get_viewport(&surface, &queue_handle, ());
    let cursor_manager = globals.bind(&queue_handle, 1..=1, ()).map_err(|error| error.to_string())?;
    surface.commit();
    let mut app = App {
        events,
        surface,
        shell,
        toplevel,
        viewport,
        shm,
        seat,
        cursor_manager,
        cursor: None,
        cursor_shape: wp_cursor_shape_device_v1::Shape::Default,
        pointer_serial: None,
        keyboard: None,
        serial: 0,
        size: (size.0 as i32, size.1 as i32),
        configured: false,
        shown_size: (size.0 as i32, size.1 as i32),
        resized: None,
        picture: Vec::new(),
        picture_size: (0, 0),
        changed: Vec::new(),
    };
    wake.set_nonblocking(true).map_err(|error| error.to_string())?;
    loop {
        queue.dispatch_pending(&mut app).map_err(|error| error.to_string())?;
        loop {
            match requests.try_recv() {
                Ok(Request::Frame(frame)) => app.patch(frame),
                Ok(Request::Move) => app.toplevel._move(&app.seat, app.serial),
                Ok(Request::Cursor(shape)) => {
                    app.cursor_shape = shape;
                    if let Some(serial) = app.pointer_serial
                        && let Some(cursor) = &app.cursor
                    {
                        cursor.set_shape(serial, shape);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        // The newest picture is shown even when the window has been resized
        // since: stretched to fit, it follows the pointer until a sharp one
        // arrives.
        if app.configured && !app.changed.is_empty() {
            app.draw(&queue_handle)?;
        }
        // A resize that has held still asks for a sharp frame.
        if app.resized.is_some_and(|resized| resized.elapsed() >= SETTLED) {
            app.resized = None;
            let _ = app.events.send(Event::Resize(app.size.0, app.size.1));
        }
        connection.flush().map_err(|error| error.to_string())?;
        let Some(guard) = queue.prepare_read() else { continue };
        let mut fds = [PollFd::new(&connection, PollFlags::IN), PollFd::new(&wake, PollFlags::IN)];
        let timeout = app.resized.map(|resized| {
            let left = SETTLED.saturating_sub(resized.elapsed());
            smithay::reexports::rustix::time::Timespec {
                tv_sec: left.as_secs() as _,
                tv_nsec: left.subsec_nanos().into(),
            }
        });
        if let Err(error) = poll(&mut fds, timeout.as_ref()) {
            if error == smithay::reexports::rustix::io::Errno::INTR {
                continue;
            }
            return Err(error.to_string());
        }
        let ready = [!fds[0].revents().is_empty(), !fds[1].revents().is_empty()];
        if ready[0] {
            guard.read().map_err(|error| error.to_string())?;
        } else {
            drop(guard);
        }
        if ready[1] {
            let mut bytes = [0; 256];
            while let Ok(count) = wake.read(&mut bytes) {
                if count == 0 {
                    return Ok(());
                }
            }
        }
    }
}

impl App {
    /// Copies a frame's changed parts into the picture, which starts again,
    /// empty, at a new size.
    fn patch(&mut self, frame: Frame) {
        let (width, height) = (frame.size.0.max(0), frame.size.1.max(0));
        let whole = Rectangle::from_size((width, height).into());
        if self.picture_size != (width, height) {
            self.picture_size = (width, height);
            self.picture = vec![0; width as usize * height as usize * 4];
            self.changed = vec![whole];
        }
        for (rect, rows) in frame.parts {
            if crate::capture::patch_rows(&mut self.picture, width, rect, &rows) {
                self.changed.push(rect);
            } else {
                tracing::warn!("{width}x{height} picture: part {rect:?} does not fit");
            }
        }
    }

    /// Shows the picture, damaging what changed since it was last shown.
    fn draw(&mut self, queue_handle: &QueueHandle<Self>) -> Result<(), String> {
        let (width, height) = self.picture_size;
        if width == 0 || height == 0 {
            return Ok(());
        }
        let descriptor =
            memfd_create(c"tileception-settings", MemfdFlags::CLOEXEC).map_err(|error| error.to_string())?;
        let mut file = File::from(descriptor);
        file.write_all(&self.picture).map_err(|error| error.to_string())?;
        let pool = self.shm.create_pool(file.as_fd(), self.picture.len() as i32, queue_handle, ());
        let buffer = pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Argb8888, queue_handle, ());
        // Drawn for another size, it is stretched to the window's.
        self.viewport.set_destination(self.size.0, self.size.1);
        self.shell.set_window_geometry(0, 0, self.size.0, self.size.1);
        self.surface.attach(Some(&buffer), 0, 0);
        // The compositor keeps the texture it made from the last buffer and
        // uploads only what is damaged into it.
        for rect in self.changed.drain(..) {
            self.surface.damage_buffer(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
        }
        self.surface.commit();
        pool.destroy();
        Ok(())
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        shell_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        shell: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            shell.ack_configure(serial);
            // The picture shown so far is stretched to the new size at once.
            if app.size != app.shown_size {
                app.shown_size = app.size;
                app.resized = Some(Instant::now());
                app.viewport.set_destination(app.size.0, app.size.1);
                shell.set_window_geometry(0, 0, app.size.0, app.size.1);
            }
            app.surface.commit();
            app.configured = true;
            let _ = app.events.send(Event::Resize(app.size.0, app.size.1));
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(
        app: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                if width > 0 {
                    app.size.0 = width;
                }
                if height > 0 {
                    app.size.1 = height;
                }
            }
            xdg_toplevel::Event::Close => {
                let _ = app.events.send(Event::CloseRequested);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        app: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        queue_handle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(capabilities) } = event {
            if capabilities.contains(wl_seat::Capability::Pointer) {
                let pointer = seat.get_pointer(queue_handle, ());
                app.cursor = Some(app.cursor_manager.get_pointer(&pointer, queue_handle, ()));
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(queue_handle, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let event = match event {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                app.pointer_serial = Some(serial);
                if let Some(cursor) = &app.cursor {
                    cursor.set_shape(serial, app.cursor_shape);
                }
                Event::Motion(Point::new(surface_x, surface_y))
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => Event::Motion(Point::new(surface_x, surface_y)),
            wl_pointer::Event::Leave { .. } => {
                app.pointer_serial = None;
                Event::Leave
            }
            wl_pointer::Event::Button { serial, button: 0x110, state: WEnum::Value(state), .. } => {
                app.serial = serial;
                Event::Button(state == wl_pointer::ButtonState::Pressed)
            }
            wl_pointer::Event::Button {
                button: 0x112, state: WEnum::Value(wl_pointer::ButtonState::Pressed), ..
            } => Event::MiddleClick,
            wl_pointer::Event::Axis { axis: WEnum::Value(wl_pointer::Axis::VerticalScroll), value, .. } => {
                Event::Scroll(value)
            }
            _ => return,
        };
        let _ = app.events.send(event);
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap {
                format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1),
                fd: descriptor,
                size,
            } => {
                // The compositor may give every client the same file, and so
                // the same offset: read from the start, never from where the
                // previous reader stopped.
                let mut bytes = vec![0; size as usize];
                if File::from(descriptor).read_exact_at(&mut bytes, 0).is_ok()
                    && let Ok(text) = String::from_utf8(bytes)
                {
                    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                    app.keyboard = xkb::Keymap::new_from_string(
                        &context,
                        text.trim_end_matches('\0').into(),
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                    .map(|keymap| xkb::State::new(&keymap));
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(keyboard) = &mut app.keyboard {
                    keyboard.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(state), .. } => {
                if let Some(keyboard) = &app.keyboard {
                    let active = |name| keyboard.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE);
                    let mods = Mods {
                        logo: active(xkb::MOD_NAME_LOGO),
                        alt: active(xkb::MOD_NAME_ALT),
                        ctrl: active(xkb::MOD_NAME_CTRL),
                        shift: active(xkb::MOD_NAME_SHIFT),
                    };
                    let sym = keyboard.key_get_one_sym((key + 8).into()).raw();
                    let _ = app.events.send(Event::Key {
                        sym,
                        mods,
                        pressed: state == wl_keyboard::KeyState::Pressed,
                        code: key,
                    });
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                let _ = app.events.send(Event::Unfocus);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(
        _: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wp_viewporter::WpViewporter);
delegate_noop!(App: ignore wp_viewport::WpViewport);
delegate_noop!(App: ignore wp_cursor_shape_manager_v1::WpCursorShapeManagerV1);
delegate_noop!(App: ignore wp_cursor_shape_device_v1::WpCursorShapeDeviceV1);
