//! Screen and window sharing through ext-image-copy-capture, with screens and
//! windows (from ext-foreign-toplevel-list) as sources.
//!
//! xdg-desktop-portal-wlr 0.8 prefers this protocol over wlr-screencopy, and
//! offers windows only through it. Its wlr-screencopy path also stalls after
//! the first frame from 0.8.2 on, because only this path wakes its PipeWire
//! stream after each frame.
//!
//! A screen's frames are copied with the other captures of that screen (see
//! [`crate::capture`]); a window is drawn on its own into the program's buffer
//! by [`Tileception::process_window_copies`], once a frame.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use smithay::{
    backend::{
        allocator::{Format, Fourcc},
        renderer::{damage::OutputDamageTracker, element::AsRenderElements, gles::GlesRenderer},
    },
    output::{Output, WeakOutput},
    reexports::{
        wayland_protocols::ext::{
            foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
            image_capture_source::v1::server::{
                ext_foreign_toplevel_image_capture_source_manager_v1::{
                    self, ExtForeignToplevelImageCaptureSourceManagerV1,
                },
                ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
                ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1},
            },
            image_copy_capture::v1::server::{
                ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
                ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
                ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
                ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
            },
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            protocol::{wl_buffer::WlBuffer, wl_output::Transform as WlTransform, wl_shm},
        },
    },
    utils::{Buffer, Rectangle, Scale, Transform},
    wayland::foreign_toplevel_list::ForeignToplevelHandle,
};
use tileception_core::WindowId;

use crate::{
    Tileception,
    capture::{ClientBuffer, ClientCopy, CopyFailure, CopyFrame, ManagerDamage, copy_elements},
    render::{OutputElement, enlarged, physical_point},
};

/// The largest window copied, in pixels on a side.
const LARGEST: i32 = 16384;

/// GPU buffers programs may give for a copy: the render device and the
/// formats the renderer can draw into there.
#[derive(Clone, Debug)]
pub struct CaptureFormats {
    pub device: libc::dev_t,
    /// Each format with the layouts (modifiers) it can be drawn into in.
    pub formats: Vec<(Fourcc, Vec<u64>)>,
}

impl CaptureFormats {
    /// The XRGB formats the renderer can draw into, as copies use.
    pub fn new(device: libc::dev_t, render_formats: impl IntoIterator<Item = Format>) -> Self {
        let modifiers: Vec<u64> = render_formats
            .into_iter()
            .filter(|format| format.code == Fourcc::Xrgb8888)
            .map(|format| u64::from(format.modifier))
            .collect();
        let formats = if modifiers.is_empty() { Vec::new() } else { vec![(Fourcc::Xrgb8888, modifiers)] };
        Self { device, formats }
    }
}

/// Window copies waiting for the window to change, and the GPU buffers
/// copies may use.
#[derive(Default)]
pub struct ImageCopy {
    pub formats: Option<CaptureFormats>,
    windows: Vec<WindowCopy>,
}

/// How often a window that does not change is sent again: video calls may
/// drop a stream's first frames, and a still window sends no others.
const REFRESH: Duration = Duration::from_secs(1);

struct WindowCopy {
    frame: CopyFrame,
    buffer: ClientBuffer,
    window: WindowId,
    session: ExtImageCopyCaptureSessionV1,
    /// When the program asked for it, to send the window again after
    /// [`REFRESH`] when it does not change.
    asked: std::time::Instant,
}

/// What a source shows.
#[derive(Clone, Debug)]
enum Source {
    Screen(WeakOutput),
    Window(WindowId),
}

/// A source, or nothing for one made from a screen or window already gone.
pub struct SourceData(Option<Source>);

pub struct SessionData {
    source: Option<Source>,
    cursor: bool,
    /// Whether no frame was copied yet: the first is copied right away.
    first: AtomicBool,
    /// What was last sent of a screen, to say what changed since.
    history: ManagerDamage,
    inner: Mutex<SessionInner>,
}

#[derive(Default)]
struct SessionInner {
    /// The size last announced, which buffers must have.
    size: Option<(i32, i32)>,
    /// The session's frame; a program may have one at a time.
    frame: Option<ExtImageCopyCaptureFrameV1>,
    /// What was last sent of a window, at its size then.
    window: Option<((i32, i32), OutputDamageTracker)>,
}

pub struct FrameData {
    session: ExtImageCopyCaptureSessionV1,
    /// Whether the program made a new frame while this one was waiting
    /// (see `CreateFrame`): it then gets no events at all.
    replaced: AtomicBool,
    inner: Mutex<FrameInner>,
}

#[derive(Default)]
struct FrameInner {
    buffer: Option<WlBuffer>,
    captured: bool,
}

/// A frame being copied for a program.
pub struct ExtFrame(ExtImageCopyCaptureFrameV1);

impl ExtFrame {
    /// Whether the frame still takes events: neither destroyed nor
    /// replaced by a newer one.
    pub fn is_alive(&self) -> bool {
        self.0.is_alive() && !self.0.data::<FrameData>().is_some_and(|data| data.replaced.load(Ordering::Relaxed))
    }

    pub fn fail(&self, reason: CopyFailure) {
        if !self.is_alive() {
            return;
        }
        let reason = match reason {
            CopyFailure::Unknown => ext_image_copy_capture_frame_v1::FailureReason::Unknown,
            CopyFailure::BufferConstraints => ext_image_copy_capture_frame_v1::FailureReason::BufferConstraints,
            CopyFailure::Stopped => {
                if let Some(data) = self.0.data::<FrameData>()
                    && data.session.is_alive()
                {
                    data.session.stopped();
                }
                ext_image_copy_capture_frame_v1::FailureReason::Stopped
            }
        };
        self.0.failed(reason);
    }

    /// Tells the program the buffer is filled. The first frame of a session
    /// always reports the whole buffer as changed.
    pub fn ready(&self, changed: Option<Rectangle<i32, Buffer>>, size: (i32, i32), now: Duration) {
        if !self.is_alive() {
            return;
        }
        let first = self
            .0
            .data::<FrameData>()
            .and_then(|data| data.session.data::<SessionData>())
            .is_some_and(|session| session.first.swap(false, Ordering::Relaxed));
        let whole = Rectangle::from_size(size.into());
        let changed = if first { Some(whole) } else { changed.and_then(|rect| rect.intersection(whole)) };
        self.0.transform(WlTransform::Normal);
        if let Some(rect) = changed {
            self.0.damage(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
        }
        let secs = now.as_secs();
        self.0.presentation_time((secs >> 32) as u32, secs as u32, now.subsec_nanos());
        self.0.ready();
    }
}

pub fn init_image_copy(display: &DisplayHandle) {
    display.create_global::<Tileception, ExtImageCopyCaptureManagerV1, _>(1, ());
    display.create_global::<Tileception, ExtOutputImageCaptureSourceManagerV1, _>(1, ());
    display.create_global::<Tileception, ExtForeignToplevelImageCaptureSourceManagerV1, _>(1, ());
}

impl Tileception {
    /// The size, in pixels, buffers for a source must have; nothing when it
    /// is gone.
    fn source_size(&mut self, source: &Source) -> Option<(i32, i32)> {
        match source {
            Source::Screen(output) => {
                let region = self.screen_region(&output.upgrade()?)?;
                Some((region.size.w, region.size.h))
            }
            Source::Window(window) => self.window_copy_size(*window),
        }
    }

    /// A window's size in its screen's pixels, without its shadow.
    fn window_copy_size(&self, window_id: WindowId) -> Option<(i32, i32)> {
        let scale = self.output_for_window(window_id)?.current_scale().fractional_scale();
        let geometry = self.windows.get(&window_id)?.geometry();
        let pixels = |logical: i32| ((f64::from(logical) * scale).round() as i32).min(LARGEST);
        let size = (pixels(geometry.size.w), pixels(geometry.size.h));
        (size.0 > 0 && size.1 > 0).then_some(size)
    }

    /// Announces what buffers a session takes, or that it stopped when its
    /// source is gone.
    fn announce(&mut self, session: &ExtImageCopyCaptureSessionV1) {
        let Some(data) = session.data::<SessionData>() else { return };
        let size = data.source.clone().and_then(|source| self.source_size(&source));
        let Some((width, height)) = size else {
            session.stopped();
            return;
        };
        data.inner.lock().expect("capture session").size = Some((width, height));
        session.buffer_size(width as u32, height as u32);
        session.shm_format(wl_shm::Format::Xrgb8888);
        if let Some(formats) = &self.image_copy.formats
            && !formats.formats.is_empty()
        {
            session.dmabuf_device(formats.device.to_ne_bytes().to_vec());
            for (format, modifiers) in &formats.formats {
                let modifiers = modifiers.iter().flat_map(|modifier| modifier.to_ne_bytes()).collect();
                session.dmabuf_format(*format as u32, modifiers);
            }
        }
        session.done();
    }

    /// Starts copying a frame the program asked for into its buffer.
    fn start_copy(
        &mut self,
        frame: ExtImageCopyCaptureFrameV1,
        session: &ExtImageCopyCaptureSessionV1,
        buffer: WlBuffer,
    ) {
        let ext = ExtFrame(frame);
        let Some(data) = session.data::<SessionData>() else {
            ext.fail(CopyFailure::Unknown);
            return;
        };
        let Some(source) = data.source.clone() else {
            ext.fail(CopyFailure::Stopped);
            return;
        };
        let Some(size) = self.source_size(&source) else {
            ext.fail(CopyFailure::Stopped);
            return;
        };
        let announced = data.inner.lock().expect("capture session").size;
        let Some(buffer) = ClientBuffer::from_wl(buffer) else {
            ext.fail(CopyFailure::BufferConstraints);
            return;
        };
        let first = data.first.load(Ordering::Relaxed);
        match source {
            Source::Screen(weak) => {
                // A screen that changed size takes new buffers.
                if announced != Some(size) {
                    self.announce(session);
                    ext.fail(CopyFailure::BufferConstraints);
                    return;
                }
                if buffer.size() != Some(size) {
                    ext.fail(CopyFailure::BufferConstraints);
                    return;
                }
                let Some(output) = weak.upgrade() else {
                    ext.fail(CopyFailure::Stopped);
                    return;
                };
                let copy = ClientCopy {
                    frame: CopyFrame::Ext(ext),
                    buffer,
                    wait_for_damage: true,
                    cursor: data.cursor,
                    history: data.history.clone(),
                    output: weak,
                };
                self.queue_copy(&output, Rectangle::from_size(size.into()), copy, true, first);
            }
            Source::Window(window) => {
                // Windows change size whenever the layout changes, even
                // because the sharing program opened a window of its own.
                // The frame still goes into the buffer given, so the stream
                // does not stall on a failed frame, and the new size is
                // announced for the next buffers.
                if announced != Some(size) {
                    self.announce(session);
                }
                if buffer.size().is_none_or(|(width, height)| width <= 0 || height <= 0) {
                    ext.fail(CopyFailure::BufferConstraints);
                    return;
                }
                if self.headless.is_none()
                    && let Some(output) = self.output_for_window(window)
                {
                    self.with_output(&output, |state| {
                        state.bar.capture = Some((std::time::Instant::now(), true));
                    });
                }
                self.image_copy.windows.push(WindowCopy {
                    frame: CopyFrame::Ext(ext),
                    buffer,
                    window,
                    session: session.clone(),
                    asked: std::time::Instant::now(),
                });
                if first {
                    self.schedule_render();
                }
                // A frame for the copy then, even if nothing else draws one.
                let timer = smithay::reexports::calloop::timer::Timer::from_duration(REFRESH);
                let _ = self.loop_handle.insert_source(timer, |_, _, state| {
                    state.schedule_render();
                    smithay::reexports::calloop::timer::TimeoutAction::Drop
                });
            }
        }
    }

    /// Copies each shared window that changed into the buffer waiting for
    /// it. Runs once a frame, outside any one screen's context.
    pub fn process_window_copies(&mut self, renderer: &mut GlesRenderer) {
        if self.image_copy.windows.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.image_copy.windows);
        let mut waiting = Vec::new();
        for mut copy in pending {
            if !copy.frame.is_alive() {
                continue;
            }
            let Some(size) = self.window_copy_size(copy.window) else {
                copy.frame.fail(CopyFailure::Stopped);
                continue;
            };
            let Some(data) = copy.session.data::<SessionData>() else {
                copy.frame.fail(CopyFailure::Unknown);
                continue;
            };
            // A resized window is drawn into the buffer it has, from the top
            // left, while the program is told the new size.
            if data.inner.lock().expect("capture session").size != Some(size) {
                self.announce(&copy.session);
            }
            let Some(size) = copy.buffer.size() else {
                copy.frame.fail(CopyFailure::BufferConstraints);
                continue;
            };
            let Some(elements) = self.window_elements(renderer, copy.window) else {
                copy.frame.fail(CopyFailure::Stopped);
                continue;
            };
            let mut inner = data.inner.lock().expect("capture session");
            // A new tracker reports the whole window: on a new size, and to
            // send a window that has not changed in a while again.
            if inner.window.as_ref().is_some_and(|(tracked, _)| *tracked != size) || copy.asked.elapsed() >= REFRESH {
                inner.window = None;
            }
            let (_, tracker) =
                inner.window.get_or_insert_with(|| (size, OutputDamageTracker::new(size, 1.0, Transform::Normal)));
            match copy_elements(renderer, &mut copy.buffer, tracker, elements, size) {
                Ok(Some(changed)) => copy.frame.ready(&changed, size),
                Ok(None) => {
                    drop(inner);
                    waiting.push(copy);
                }
                Err(err) => {
                    inner.window = None;
                    tracing::warn!("window copy: {err}");
                    copy.frame.fail(CopyFailure::Unknown);
                }
            }
        }
        waiting.append(&mut self.image_copy.windows);
        self.image_copy.windows = waiting;
    }

    /// A window and its popups, from the window's top left (without its
    /// shadow), at its screen's scale.
    fn window_elements(&self, renderer: &mut GlesRenderer, window_id: WindowId) -> Option<Vec<OutputElement>> {
        let scale = self.output_for_window(window_id)?.current_scale().fractional_scale();
        let window = self.windows.get(&window_id)?;
        let geometry = window.geometry();
        let origin = physical_point((-f64::from(geometry.loc.x), -f64::from(geometry.loc.y)), scale);
        let surfaces = window.render_elements(renderer, origin, Scale::from(1.0), 1.0);
        Some(enlarged(surfaces, origin, scale).collect())
    }
}

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ()> for Tileception {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for Tileception {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_manager_v1::Request::CreateSession { session, source, options } => {
                let options = options.into_result().ok();
                let source = source.data::<SourceData>().and_then(|data| data.0.clone());
                let session = init.init(
                    session,
                    SessionData {
                        source,
                        cursor: options.is_some_and(|options| {
                            options.contains(ext_image_copy_capture_manager_v1::Options::PaintCursors)
                        }),
                        first: AtomicBool::new(true),
                        history: ManagerDamage::default(),
                        inner: Mutex::new(SessionInner::default()),
                    },
                );
                if options.is_none() {
                    manager.post_error(ext_image_copy_capture_manager_v1::Error::InvalidOption, "unknown option");
                    return;
                }
                state.announce(&session);
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession { session, .. } => {
                // Pointer images on their own are not offered; programs ask
                // for the pointer drawn into the frame instead.
                init.init(session, ());
            }
            ext_image_copy_capture_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, ()> for Tileception {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtImageCopyCaptureCursorSessionV1,
        request: ext_image_copy_capture_cursor_session_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } = request {
            let session = init.init(
                session,
                SessionData {
                    source: None,
                    cursor: false,
                    first: AtomicBool::new(true),
                    history: ManagerDamage::default(),
                    inner: Mutex::new(SessionInner::default()),
                },
            );
            session.stopped();
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, SessionData> for Tileception {
    fn request(
        _state: &mut Self,
        _client: &Client,
        session: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        data: &SessionData,
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_session_v1::Request::CreateFrame { frame } = request {
            let frame = init.init(
                frame,
                FrameData { session: session.clone(), replaced: AtomicBool::new(false), inner: Mutex::default() },
            );
            let mut inner = data.inner.lock().expect("capture session");
            // The protocol makes a second frame an error, but
            // xdg-desktop-portal-wlr 0.8 makes one without destroying the
            // first when a new size restarts its stream, and the error
            // would end the share. The waiting frame is dropped instead,
            // without events: the portal's handlers would act on its new
            // frame.
            if let Some(old) = inner.frame.take()
                && old.is_alive()
                && let Some(old_data) = old.data::<FrameData>()
            {
                tracing::debug!("image copy: a new frame replaces one still waiting");
                old_data.replaced.store(true, Ordering::Relaxed);
            }
            inner.frame = Some(frame);
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for Tileception {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        data: &FrameData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        use ext_image_copy_capture_frame_v1::{Error, Request};
        let mut inner = data.inner.lock().expect("capture frame");
        match request {
            Request::AttachBuffer { buffer } => {
                if inner.captured {
                    frame.post_error(Error::AlreadyCaptured, "attach_buffer after capture");
                    return;
                }
                inner.buffer = Some(buffer);
            }
            Request::DamageBuffer { x: left, y: top, width, height } => {
                if inner.captured {
                    frame.post_error(Error::AlreadyCaptured, "damage_buffer after capture");
                } else if left < 0 || top < 0 || width <= 0 || height <= 0 {
                    frame.post_error(Error::InvalidBufferDamage, "invalid buffer damage");
                }
                // Every copy fills the whole buffer, so the program's own
                // damage needs nothing more.
            }
            Request::Capture => {
                if inner.captured {
                    frame.post_error(Error::AlreadyCaptured, "capture sent twice");
                    return;
                }
                let Some(buffer) = inner.buffer.clone() else {
                    frame.post_error(Error::NoBuffer, "capture without a buffer");
                    return;
                };
                inner.captured = true;
                drop(inner);
                state.start_copy(frame.clone(), &data.session, buffer);
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Tileception {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Tileception {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource { source, output } = request {
            let screen = Output::from_resource(&output).map(|output| Source::Screen(output.downgrade()));
            init.init(source, SourceData(screen));
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Tileception {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Tileception {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: ext_foreign_toplevel_image_capture_source_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let ext_foreign_toplevel_image_capture_source_manager_v1::Request::CreateSource { source, toplevel_handle } =
            request
        {
            init.init(source, SourceData(window_of(&toplevel_handle).map(Source::Window)));
        }
    }
}

/// The window a program's window list entry stands for.
fn window_of(handle: &ExtForeignToplevelHandleV1) -> Option<WindowId> {
    ForeignToplevelHandle::from_resource(handle)?.user_data().get::<WindowId>().copied()
}

impl Dispatch<ExtImageCaptureSourceV1, SourceData> for Tileception {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtImageCaptureSourceV1,
        _request: ext_image_capture_source_v1::Request,
        _data: &SourceData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
    }
}
