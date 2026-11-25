//! Runs Tileception as the session's compositor: DRM/KMS for output, libinput for
//! input, libseat for device access and VT switching.
//!
//! Independent scanout devices per GPU, with one primary compositor renderer.
//! Outputs on other GPUs show frames it draws; see `mirror`.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime},
};

use crate::frame_timing::FrameDone;
use crate::{Tileception, cursor::Cursor, mirror::Mirror};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, NodeType,
            compositor::{FrameFlags, PrimaryPlaneElement},
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay, context::ContextPriority},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            ImportDma, ImportMemWl,
            gles::GlesRenderer,
            utils::{Buffer, RendererSurfaceStateUserData},
        },
        session::{
            Event as SessionEvent, Session,
            libseat::{LibSeatSession, LibSeatSessionNotifier},
        },
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    desktop::layer_map_for_output,
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::{
            Dispatcher, EventLoop, LoopHandle, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::{
            Device as _,
            control::{Device as _, Mode as DrmMode, ModeTypeFlags, connector, crtc},
        },
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::DeviceFd,
    wayland::{
        compositor::SurfaceData,
        dmabuf::{DmabufFeedbackBuilder, DmabufState, get_dmabuf},
        drm_syncobj::{DrmSyncobjState, supports_syncobj_eventfd},
    },
};
use smithay::{
    desktop::utils::OutputPresentationFeedback,
    reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
    utils::{Clock, Monotonic},
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

type Allocator = GbmAllocator<DrmDeviceFd>;

/// The mode a display asks for, else the first it offers.
fn preferred_mode(modes: &[DrmMode]) -> Option<DrmMode> {
    modes.iter().find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED)).or(modes.first()).copied()
}
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
/// Each queued frame carries the presentation feedback and fifo barriers of
/// what it shows, until its page flip.
type OutputManager = DrmOutputManager<Allocator, Exporter, FrameDone, DrmDeviceFd>;
type Surface = DrmOutput<Allocator, Exporter, FrameDone, DrmDeviceFd>;

const COLOR_FORMATS: &[Fourcc] = &[Fourcc::Argb8888, Fourcc::Abgr8888];

/// How long before the next refresh a frame is drawn: time enough to build
/// it and for the GPU to finish it.
const RENDER_BUDGET: Duration = Duration::from_millis(6);
/// About one frame at 60 Hz.
const FRAME: Duration = Duration::from_millis(16);
/// Empty frames in a row, about a quarter of a second, before polling slows
/// down after an animation: it may have a still frame now and then.
const QUIET_FRAMES: u32 = 15;
/// The same after a frame that was asked for (a commit, input): what it
/// starts mostly asks again or shows at once, so it is looked after briefly.
const ASKED_FRAMES: u32 = 3;
/// The shortest wait for an animation's next change.
const DUE_FLOOR: Duration = Duration::from_millis(4);
/// How often a still screen is looked at for changes nothing asked to draw.
const STILL_POLL: Duration = Duration::from_millis(100);
/// How long after waking from suspend to wait for an in-flight page flip before
/// treating that output's scanout as stuck and resetting it.
const WAKE_STALE_FLIP: Duration = Duration::from_millis(500);
/// How long after `initialize_output` or `use_mode` to wait for an in-flight
/// page flip before assuming Smithay's bandwidth fallback committed
/// synchronously without generating a vblank event.
const BANDWIDTH_FALLBACK_STALE_FLIP: Duration = Duration::from_millis(100);
/// Initial retry attempts logged at warning/error level before switching to
/// quiet capped-backoff retries while a post-wake scanout stays stuck.
const WAKE_CLEAR_RETRIES: u8 = 3;

fn monotonic_now() -> Duration {
    Clock::<Monotonic>::new().now().into()
}

fn wake_retry_delay(attempt: u8) -> Duration {
    WAKE_STALE_FLIP.saturating_mul(1u32 << attempt.min(3))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FlipCutoff {
    monotonic: Duration,
    realtime: SystemTime,
}

impl FlipCutoff {
    fn now() -> Self {
        Self { monotonic: monotonic_now(), realtime: SystemTime::now() }
    }
}

/// Drops vblank events timestamped at or before the scanout reset cutoff in
/// either clock domain, and clears the cutoff once a post-reset event arrives
/// (or if the realtime wall clock stepped backwards).
fn retire_vblank_cutoff(
    retired_before: &mut Option<FlipCutoff>,
    metadata: Option<&DrmEventMetadata>,
    now_realtime: SystemTime,
) -> bool {
    let Some(cutoff) = *retired_before else { return false };
    match metadata.map(|meta| meta.time) {
        Some(DrmEventTime::Monotonic(time)) if time <= cutoff.monotonic => true,
        Some(DrmEventTime::Realtime(time)) if now_realtime >= cutoff.realtime && time <= cutoff.realtime => true,
        Some(DrmEventTime::Monotonic(_) | DrmEventTime::Realtime(_)) => {
            *retired_before = None;
            false
        }
        None => false,
    }
}

/// Returns the cutoff to keep on a CRTC when its surface disconnects, so a
/// late page-flip event from the old surface cannot complete a replacement
/// surface's first frame on the same CRTC.
fn disconnect_flip_cutoff(
    frame_pending: bool,
    existing: Option<FlipCutoff>,
    now: impl FnOnce() -> FlipCutoff,
) -> Option<FlipCutoff> {
    if frame_pending { Some(now()) } else { existing }
}

fn schedule_stuck_flip_retry(
    handle: &LoopHandle<'static, Tileception>,
    targets: Vec<StuckFlip>,
    delay: Duration,
    attempt: u8,
) {
    if targets.is_empty() {
        return;
    }
    if let Err(error) = handle.insert_source(Timer::from_duration(delay), move |_, _, state| {
        state.recover_stuck_flips(&targets, attempt);
        TimeoutAction::Drop
    }) {
        tracing::error!("could not schedule stuck-flip recovery timer: {error}");
    }
}

#[derive(Clone, Copy)]
struct StuckFlip {
    node: DrmNode,
    crtc: crtc::Handle,
    surface_id: u64,
    flip_seq: u64,
}

pub struct Udev {
    pub session: LibSeatSession,
    pub renderer: GlesRenderer,
    pub cursor: Cursor,
    gpus: Vec<Gpu>,
    render_pending: bool,
    /// The timer that looks again for changes while frames come out empty.
    poll: Option<RegistrationToken>,
    /// Empty frames in a row, for backing the poll off on a still screen.
    quiet: u32,
    /// Something asked for the frame being made, rather than the poll.
    asked: bool,
}

impl Udev {
    fn lit_mut(&mut self, node: DrmNode, crtc: crtc::Handle) -> Option<&mut Lit> {
        self.gpus
            .iter_mut()
            .find(|gpu| gpu.node == node)
            .and_then(|gpu| gpu.surfaces.iter_mut().find(|lit| lit.crtc == crtc))
    }
}

struct Gpu {
    node: DrmNode,
    gbm: GbmDevice<DrmDeviceFd>,
    manager: OutputManager,
    scanner: DrmScanner,
    surfaces: Vec<Lit>,
    /// Cutoffs carried across disconnect/reconnect on the same CRTC so a stale
    /// vblank from a retired surface never completes its replacement's frame.
    retired_flips: HashMap<crtc::Handle, FlipCutoff>,
    _drm_token: RegistrationToken,
    /// Secondary GPUs upload completed primary-renderer frames for scanout.
    renderer: Option<GlesRenderer>,
}

struct Lit {
    /// Unique surface generation so timers never mistake a newly connected
    /// output on the same CRTC for a retired one.
    surface_id: u64,
    connector: connector::Handle,
    modes: Vec<smithay::reexports::drm::control::Mode>,
    mode: WlMode,
    enabled: bool,
    crtc: crtc::Handle,
    output: Output,
    drm_output: Surface,
    frame_pending: bool,
    /// Monotonic counter incremented each time a frame is queued or completed,
    /// so wake recovery only resets outputs whose flip genuinely never arrived.
    flip_seq: u64,
    /// `flip_seq` currently watched by an active post-wake retry timer, so
    /// repeated `wake_udev` calls do not stack duplicate retry chains.
    wake_retry_seq: Option<u64>,
    /// Timestamps recorded when `compositor.clear()` retires an in-flight flip,
    /// so a late DRM vblank event from before the reset cannot prematurely
    /// complete a replacement frame.
    retired_flip_before: Option<FlipCutoff>,
    /// Vblank pacing timer waiting until just before this output's next refresh.
    render_timer: Option<RegistrationToken>,
    /// Client buffers held until scanout completes. See `frame_buffers`.
    in_flight: Vec<Buffer>,
    lock_queued: bool,
    lock_shown: bool,
    /// Frames from the primary renderer, for an output on a secondary GPU.
    mirror: Option<Mirror>,
    /// Frames that changed nothing, waiting for the refresh that shows what
    /// was committed for them.
    unchanged: Vec<Unchanged>,
    /// Feedback of frames that changed nothing, with the vblank counter
    /// each was made at, waiting for the queued frame's flip: what was
    /// committed for them showed if a refresh came before it.
    flipping: Vec<(OutputPresentationFeedback, u32)>,
}

/// A frame that changed nothing on screen: its feedback and fifo batch
/// wait until the screen has really refreshed once (its vblank counter
/// passed `after`), unless a newer frame takes that refresh.
struct Unchanged {
    token: u64,
    feedback: Option<OutputPresentationFeedback>,
    fifo: Option<u64>,
    /// The screen's vblank counter when the frame was made.
    after: u32,
}

/// Numbers for frames that changed nothing, unique across screens and the
/// screens that replace them, so a timer finds only its own frame.
static NEXT_UNCHANGED: AtomicU64 = AtomicU64::new(1);
static NEXT_SURFACE_ID: AtomicU64 = AtomicU64::new(1);

/// How many times a frame that changed nothing looks for its refresh
/// before giving up on it (a screen that stopped refreshing).
const UNCHANGED_TRIES: u32 = 8;

/// The screen's latest vblank: its counter and when it happened
/// (monotonic), asked of the kernel without waiting.
fn latest_vblank(manager: &OutputManager, crtc: crtc::Handle) -> Option<(u32, Duration)> {
    use smithay::reexports::drm::{Device as _, VblankWaitFlags, VblankWaitTarget, control::Device as _};
    let device = manager.device();
    let pipe = device.resource_handles().ok()?.crtcs().iter().position(|handle| *handle == crtc)?;
    let reply = device
        .wait_vblank(VblankWaitTarget::Relative(0), VblankWaitFlags::empty(), u32::try_from(pipe).ok()?, 0)
        .ok()?;
    Some((reply.frame(), reply.time()?))
}

/// When to look whether the screen has refreshed: just after the refresh
/// expected next.
fn unchanged_wait(output: &Output) -> Duration {
    let period = match crate::frame_timing::refresh_of(output) {
        smithay::wayland::presentation::Refresh::Fixed(period) => period,
        _ => Duration::from_millis(16),
    };
    period + Duration::from_millis(1)
}

/// Reports what was committed for a frame that changed nothing as shown at
/// the first refresh after `after`, given the screen's `latest` vblank.
fn present_unchanged(
    mut feedback: OutputPresentationFeedback,
    after: u32,
    (sequence, time): (u32, Duration),
    refresh: smithay::wayland::presentation::Refresh,
) {
    // The first refresh after the frame, should several have passed.
    let later = sequence.wrapping_sub(after).saturating_sub(1);
    let time = match refresh {
        smithay::wayland::presentation::Refresh::Fixed(period) => time.saturating_sub(period * later),
        _ => time,
    };
    // The kernel's answer says not whether the time was measured, and an
    // earlier refresh's is worked out.
    let flags = wp_presentation_feedback::Kind::Vsync;
    feedback.presented::<_, Monotonic>(time, refresh, u64::from(after.wrapping_add(1)), flags);
}

/// The vblank in `latest` when it came after the counter `after`.
fn refreshed_since(latest: Option<(u32, Duration)>, after: u32) -> Option<(u32, Duration)> {
    latest.filter(|(sequence, _)| sequence.wrapping_sub(after) as i32 > 0)
}

impl Lit {
    /// A frame is being queued: frames that changed nothing stop waiting
    /// for a refresh of their own. One whose refresh already came by
    /// `latest` (the screen's vblank now) is reported shown; the others
    /// wait for the queued frame's flip ([`Lit::flipped`]). Returns their
    /// fifo batches, to go with that frame, and those waiting.
    fn take_unchanged(
        &mut self,
        latest: Option<(u32, Duration)>,
    ) -> (Vec<u64>, Vec<(OutputPresentationFeedback, u32)>) {
        let refresh = crate::frame_timing::refresh_of(&self.output);
        let mut fifo = Vec::new();
        let mut waiting = Vec::new();
        for unchanged in self.unchanged.drain(..) {
            if let Some(feedback) = unchanged.feedback {
                match refreshed_since(latest, unchanged.after) {
                    Some(vblank) => present_unchanged(feedback, unchanged.after, vblank, refresh),
                    None => waiting.push((feedback, unchanged.after)),
                }
            }
            fifo.extend(unchanged.fifo);
        }
        (fifo, waiting)
    }

    /// The queued frame flipped at `vblank` (counter and time), or is gone
    /// (`None`): feedback waiting for it is reported shown at the refresh
    /// after it was made, when that came before the flip, or discarded.
    fn flipped(&mut self, vblank: Option<(u32, Duration)>) {
        let refresh = crate::frame_timing::refresh_of(&self.output);
        for (mut feedback, after) in self.flipping.drain(..) {
            match vblank.filter(|(sequence, _)| sequence.wrapping_sub(after) as i32 > 1) {
                Some(vblank) => present_unchanged(feedback, after, vblank, refresh),
                None => feedback.discarded(),
            }
        }
    }

    /// The screen goes off, sleeps or wakes, or changes mode, or the session
    /// pauses or resumes: frames waiting for its refresh
    /// give up, their feedback discarded and their fifo batches left to
    /// the watchdog.
    fn drop_unchanged(&mut self) {
        let (_, waiting) = self.take_unchanged(None);
        self.flipping.extend(waiting);
        self.flipped(None);
    }

    fn cancel_render_timer(&mut self, handle: &LoopHandle<'static, Tileception>) {
        if let Some(token) = self.render_timer.take() {
            handle.remove(token);
        }
    }

    /// Retires any pending scanout state on this surface after pause, sleep,
    /// disable, or a successful `clear()`.
    fn retire_frame(&mut self, handle: &LoopHandle<'static, Tileception>, cutoff: Option<FlipCutoff>) {
        self.cancel_render_timer(handle);
        if let Some(cutoff) = cutoff {
            self.retired_flip_before = Some(cutoff);
        }
        self.frame_pending = false;
        self.flip_seq = self.flip_seq.wrapping_add(1);
        self.wake_retry_seq = None;
        self.in_flight.clear();
        self.lock_queued = false;
        self.lock_shown = false;
        self.drop_unchanged();
    }

    /// Drops Smithay's queued/stale scanout state after resume or a stuck flip.
    fn reset_scanout(&mut self, loud: bool, context: &str) -> bool {
        self.drm_output.with_compositor(|compositor| {
            let cleared = match compositor.clear() {
                Ok(()) => true,
                Err(err) => {
                    if loud {
                        tracing::error!("could not clear stale scanout {context}: {err}");
                    } else {
                        tracing::debug!("still could not clear stale scanout {context}: {err}");
                    }
                    false
                }
            };
            compositor.reset_buffers();
            if let Err(err) = compositor.reset_state() {
                if loud {
                    tracing::error!("could not reset output {context}: {err}");
                } else {
                    tracing::debug!("still could not reset output {context}: {err}");
                }
            }
            cleared
        })
    }

    /// Marks this surface's current pending flip for watchdog recovery if it is
    /// not already being watched.
    fn watch_pending_flip(&mut self, node: DrmNode) -> Option<StuckFlip> {
        if self.enabled && self.frame_pending && self.wake_retry_seq != Some(self.flip_seq) {
            self.wake_retry_seq = Some(self.flip_seq);
            Some(StuckFlip { node, crtc: self.crtc, surface_id: self.surface_id, flip_seq: self.flip_seq })
        } else {
            None
        }
    }
}

impl Lit {
    /// The primary renderer's frame, shown on this secondary-GPU output.
    fn mirrored(
        &mut self,
        gbm: &GbmDevice<DrmDeviceFd>,
        primary: &mut GlesRenderer,
        secondary: &mut GlesRenderer,
        scene: &[crate::render::OutputElement],
        size: (i32, i32),
        clear: [f32; 4],
    ) -> Result<crate::render::OutputElement, String> {
        if self.mirror.as_ref().is_none_or(|mirror| mirror.size() != size) {
            self.mirror = Some(Mirror::new(gbm, primary, secondary, size)?);
        }
        let mirror = self.mirror.as_mut().ok_or("no mirror")?;
        let frame = mirror.render(primary, secondary, scene, clear);
        if frame.is_err() {
            // Its buffers may hold half a frame: start afresh next time.
            self.mirror = None;
        }
        frame
    }
}

impl Gpu {
    fn open(
        session: &mut LibSeatSession,
        path: &std::path::Path,
        handle: &LoopHandle<'static, Tileception>,
    ) -> Result<(Self, GlesRenderer, DrmDeviceFd), Box<dyn std::error::Error>> {
        let node = DrmNode::from_path(path)?;
        let node = node.node_with_type(NodeType::Primary).and_then(|result| result.ok()).unwrap_or(node);
        let card = node.dev_path().unwrap_or_else(|| path.to_path_buf());
        let opened = session.open(&card, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)?;
        let device_fd = DrmDeviceFd::new(DeviceFd::from(opened));
        let (drm, notifier) = DrmDevice::new(device_fd.clone(), true)?;
        let gbm = GbmDevice::new(device_fd.clone())?;
        // SAFETY: Smithay owns this device; no other EGL display manages it.
        #[allow(unsafe_code, reason = "Smithay requires opt-in for native EGL initialization")]
        let egl = unsafe { EGLDisplay::new(gbm.clone())? };
        let context = EGLContext::new_with_priority(&egl, ContextPriority::High)?;
        // SAFETY: This renderer and context stay on the compositor event-loop thread.
        #[allow(unsafe_code, reason = "Smithay requires exclusive EGL context ownership")]
        let renderer = unsafe { GlesRenderer::new(context)? };
        let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
        let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node));
        let manager = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm.clone()),
            COLOR_FORMATS.iter().copied(),
            render_formats,
        );
        let token = handle.insert_source(notifier, move |event, metadata, state| match event {
            DrmEvent::VBlank(crtc) => state.on_vblank(node, crtc, metadata.take()),
            DrmEvent::Error(error) => tracing::error!("drm {node:?}: {error:?}"),
        })?;
        tracing::info!("opened GPU {}", card.display());
        Ok((
            Self {
                node,
                gbm,
                manager,
                scanner: DrmScanner::new(),
                surfaces: Vec::new(),
                retired_flips: HashMap::new(),
                _drm_token: token,
                renderer: None,
            },
            renderer,
            device_fd,
        ))
    }
}

/// How long to wait for the seat before giving up.
const SEAT_WAIT: Duration = Duration::from_secs(10);

/// Waits until the session is active. A display manager may start us before
/// its greeter has let go of the seat (LightDM does), and until then the GPU
/// cannot be taken over.
fn wait_until_active(
    session: &LibSeatSession,
    notifier: LibSeatSessionNotifier,
) -> Result<LibSeatSessionNotifier, Box<dyn std::error::Error>> {
    if session.is_active() {
        return Ok(notifier);
    }
    tracing::info!("waiting for the session to become active");
    // Only the notifier's events update `is_active`, so run it on a loop of
    // its own and hand it back for the main loop afterwards.
    let mut wait = EventLoop::<()>::try_new()?;
    let dispatcher = Dispatcher::new(notifier, |_, _, _| ());
    let token = wait.handle().register_dispatcher(dispatcher.clone())?;
    let deadline = Instant::now() + SEAT_WAIT;
    while !session.is_active() {
        let left = deadline.checked_duration_since(Instant::now()).ok_or("the session did not become active")?;
        wait.dispatch(Some(left), &mut ())?;
    }
    wait.handle().remove(token);
    tracing::info!("session active");
    Ok(dispatcher.into_source_inner())
}

/// The seat, through logind or seatd. Right after `restart`, logind may not
/// have noticed yet that the compositor before let go of it: try again for
/// a few seconds then.
fn open_seat() -> Result<(LibSeatSession, LibSeatSessionNotifier), String> {
    let restarted = std::env::args().any(|argument| argument == crate::RESTARTED);
    let mut tries = if restarted { 30 } else { 1 };
    loop {
        tries -= 1;
        match LibSeatSession::new() {
            Ok(opened) => return Ok(opened),
            Err(err) if tries == 0 => return Err(format!("no seat session: {err}")),
            Err(err) => {
                tracing::debug!("seat not free yet: {err}");
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

pub fn init(
    event_loop: &mut EventLoop<'static, Tileception>,
    state: &mut Tileception,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, notifier) = open_seat()?;
    let notifier = wait_until_active(&session, notifier)?;
    let seat = session.seat();

    let gpu_path = primary_gpu(&seat)?.or_else(|| all_gpus(&seat).ok()?.into_iter().next()).ok_or("no GPU found")?;
    let (gpu, renderer, device_fd) = Gpu::open(&mut session, &gpu_path, &event_loop.handle())?;
    let primary = gpu.node;
    tracing::info!("using primary renderer on {}", gpu_path.display());
    state.shm_state.update_formats(renderer.shm_formats());

    // Clients render into dmabufs; announce what the GPU can import.
    let formats = renderer.dmabuf_formats();
    let feedback = DmabufFeedbackBuilder::new(primary.dev_id(), formats).build()?;
    let mut dmabuf_state = DmabufState::new();
    let global = dmabuf_state.create_global_with_default_feedback::<Tileception>(&state.display_handle, &feedback);
    state.dmabuf = Some((dmabuf_state, global));
    state.dmabuf_feedback = Some(feedback);
    state.image_copy.formats = Some(crate::image_copy::CaptureFormats::new(
        primary.dev_id(),
        renderer.egl_context().dmabuf_render_formats().iter().copied(),
    ));

    // Explicit sync, which NVIDIA needs to avoid flicker in GPU clients.
    if supports_syncobj_eventfd(&device_fd) {
        state.syncobj_state = Some(DrmSyncobjState::new::<Tileception>(&state.display_handle, device_fd.clone()));
        tracing::info!("explicit sync enabled");
    }

    let mut gpus = vec![gpu];
    for path in all_gpus(&seat)? {
        if DrmNode::from_path(&path).is_ok_and(|node| node.dev_id() == primary.dev_id()) {
            continue;
        }
        match Gpu::open(&mut session, &path, &event_loop.handle()) {
            Ok((mut gpu, renderer, _)) => {
                gpu.renderer = Some(renderer);
                gpus.push(gpu);
            }
            Err(error) => tracing::warn!("could not open secondary GPU {}: {error}", path.display()),
        }
    }
    state.udev = Some(Box::new(Udev {
        session: session.clone(),
        renderer,
        cursor: Cursor::load(state.scale),
        gpus,
        render_pending: false,
        poll: None,
        quiet: 0,
        asked: false,
    }));

    // Input.
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&seat).map_err(|_| "libinput could not take the seat")?;
    let input = LibinputInputBackend::new(libinput.clone());
    event_loop.handle().insert_source(input, |event, _, state| {
        match &event {
            InputEvent::DeviceAdded { device } => {
                tracing::info!("input device: {}", device.name());
                state.touchpad_added(device.clone());
                state.keyboard_added(device.clone());
            }
            InputEvent::DeviceRemoved { device } => {
                state.touchpad_removed(device);
                state.keyboards.retain(|candidate| candidate != device);
            }
            _ => {}
        }
        state.process_input_event(event);
    })?;

    // VT switches pause and resume the session.
    event_loop.handle().insert_source(notifier, move |event, _, state| match event {
        SessionEvent::PauseSession => {
            tracing::info!("session paused");
            libinput.suspend();
            if let Some(udev) = state.udev.as_mut() {
                if let Some(poll) = udev.poll.take() {
                    state.loop_handle.remove(poll);
                }
                for gpu in &mut udev.gpus {
                    gpu.manager.pause();
                    for lit in &mut gpu.surfaces {
                        lit.retire_frame(&state.loop_handle, None);
                    }
                }
                udev.render_pending = false;
            }
        }
        SessionEvent::ActivateSession => {
            tracing::info!("session resumed");
            if libinput.resume().is_err() {
                tracing::error!("libinput could not resume");
            }
            // Clear idle-off state before resetting scanout so `set_output_sleep(false)`
            // cannot overwrite `frame_pending` or `flip_seq` on a failed clear retry.
            state.idle_activity();
            let mut retry_clear = Vec::new();
            if let Some(udev) = state.udev.as_mut() {
                if let Some(poll) = udev.poll.take() {
                    state.loop_handle.remove(poll);
                }
                for gpu in &mut udev.gpus {
                    if let Err(err) = gpu.manager.activate(false) {
                        tracing::error!("drm could not resume: {err}");
                    }
                    for lit in &mut gpu.surfaces {
                        // A pre-suspend page flip may never deliver its vblank. Drop
                        // both Smithay's queued frames and the stale scanout state.
                        let cleared = lit.reset_scanout(true, "after resume");
                        if cleared {
                            lit.retire_frame(&state.loop_handle, Some(FlipCutoff::now()));
                        } else if lit.enabled {
                            // Hold off queuing a replacement frame behind a stale
                            // scanout until `clear()` succeeds on retry.
                            lit.cancel_render_timer(&state.loop_handle);
                            lit.frame_pending = true;
                            lit.flip_seq = lit.flip_seq.wrapping_add(1);
                            lit.wake_retry_seq = Some(lit.flip_seq);
                            retry_clear.push(StuckFlip {
                                node: gpu.node,
                                crtc: lit.crtc,
                                surface_id: lit.surface_id,
                                flip_seq: lit.flip_seq,
                            });
                        }
                    }
                }
            }
            schedule_stuck_flip_retry(&state.loop_handle, retry_clear, WAKE_STALE_FLIP, 0);
            state.discover_gpus();
            state.scan_connectors();
            state.schedule_render();
        }
    })?;

    // Monitors coming and going.
    let udev_backend = UdevBackend::new(&seat)?;
    event_loop.handle().insert_source(udev_backend, |event, _, state| match event {
        UdevEvent::Changed { .. } => state.scan_connectors(),
        UdevEvent::Added { device_id, path } => state.add_gpu(device_id, &path),
        UdevEvent::Removed { device_id } => state.remove_gpu(device_id),
    })?;

    state.scan_connectors();
    Ok(())
}

impl Tileception {
    fn add_gpu(&mut self, device_id: libc::dev_t, path: &std::path::Path) {
        self.add_gpu_only(device_id, path);
        self.scan_connectors();
    }

    fn add_gpu_only(&mut self, device_id: libc::dev_t, path: &std::path::Path) {
        let Some(mut udev) = self.udev.take() else { return };
        if udev.session.is_active() && !udev.gpus.iter().any(|gpu| gpu.node.dev_id() == device_id) {
            match Gpu::open(&mut udev.session, path, &self.loop_handle) {
                Ok((mut gpu, renderer, _)) => {
                    gpu.renderer = Some(renderer);
                    udev.gpus.push(gpu);
                }
                Err(error) => tracing::warn!("could not open GPU {}: {error}", path.display()),
            }
        }
        self.udev = Some(udev);
    }

    fn discover_gpus(&mut self) {
        let Some(udev) = &self.udev else { return };
        let paths = match all_gpus(udev.session.seat()) {
            Ok(paths) => paths,
            Err(error) => {
                tracing::warn!("GPU discovery failed: {error}");
                return;
            }
        };
        for path in paths {
            if let Ok(node) = DrmNode::from_path(&path) {
                self.add_gpu_only(node.dev_id(), &path);
            }
        }
    }

    /// Recovers DRM state after waking from suspend: scans connectors for any
    /// dock/display changes and arms a short watchdog for any output that still
    /// has an in-flight page flip, resetting only outputs whose vblank never
    /// arrives instead of blanking healthy displays on every wake.
    pub fn wake_udev(&mut self) {
        let Some(udev) = self.udev.as_mut() else { return };
        if !udev.session.is_active() {
            return;
        }
        let mut pending = Vec::new();
        for gpu in &mut udev.gpus {
            let node = gpu.node;
            for lit in &mut gpu.surfaces {
                if let Some(target) = lit.watch_pending_flip(node) {
                    pending.push(target);
                }
            }
        }
        schedule_stuck_flip_retry(&self.loop_handle, pending, WAKE_STALE_FLIP, 0);
        self.scan_connectors();
        self.schedule_render();
    }

    fn recover_stuck_flips(&mut self, expected: &[StuckFlip], attempt: u8) {
        let Some(udev) = self.udev.as_mut() else { return };
        if !udev.session.is_active() {
            return;
        }
        let mut recovered = false;
        let mut retry = Vec::new();
        for &target in expected {
            let Some(lit) = udev.lit_mut(target.node, target.crtc).filter(|lit| lit.surface_id == target.surface_id)
            else {
                continue;
            };
            if !lit.enabled || !lit.frame_pending || lit.flip_seq != target.flip_seq {
                continue;
            }
            let loud = attempt <= WAKE_CLEAR_RETRIES;
            if loud {
                tracing::warn!(output = %lit.output.name(), attempt, "page flip stuck after wake; resetting scanout");
            } else {
                tracing::debug!(output = %lit.output.name(), attempt, "retrying stuck post-wake scanout reset");
            }
            let cleared = lit.reset_scanout(loud, "after wake");
            if cleared {
                lit.retire_frame(&self.loop_handle, Some(FlipCutoff::now()));
                recovered = true;
            } else {
                lit.cancel_render_timer(&self.loop_handle);
                if attempt == WAKE_CLEAR_RETRIES {
                    tracing::error!(
                        output = %lit.output.name(),
                        "could not clear stuck scanout after {} attempts; keeping retry watchdog active",
                        attempt + 1
                    );
                }
                // Keep `frame_pending` set and keep retrying at the capped
                // backoff until `clear()` succeeds, a late vblank arrives, or
                // the output/session changes. Clearing `frame_pending` while
                // Smithay still holds the old pending frame would queue a new
                // frame behind it and leave the output stuck with no watchdog.
                retry.push(target);
            }
        }
        schedule_stuck_flip_retry(&self.loop_handle, retry, wake_retry_delay(attempt), attempt.saturating_add(1));
        if recovered {
            self.schedule_render();
        }
    }

    fn remove_gpu(&mut self, device_id: libc::dev_t) {
        self.cancel_output_preview();
        let Some(mut udev) = self.udev.take() else { return };
        if let Some(index) = udev.gpus.iter().position(|gpu| gpu.node.dev_id() == device_id) {
            let gpu = udev.gpus.remove(index);
            for mut lit in gpu.surfaces {
                lit.cancel_render_timer(&self.loop_handle);
                lit.drop_unchanged();
                self.remove_output(&lit.output);
                self.forget_connected_output(&lit.output);
            }
            self.loop_handle.remove(gpu._drm_token);
            if index == 0 {
                tracing::error!("primary rendering GPU removed; ending the session");
                self.loop_signal.stop();
            }
        }
        self.udev = Some(udev);
        self.schedule_render();
    }

    pub fn set_output_sleep(&mut self, asleep: bool) -> bool {
        let Some(udev) = self.udev.as_mut() else { return self.headless.is_some() };
        if !udev.session.is_active() {
            return false;
        }
        if udev.gpus.iter().all(|gpu| gpu.surfaces.is_empty()) {
            return false;
        }
        let mut success = true;
        for lit in udev.gpus.iter_mut().flat_map(|gpu| &mut gpu.surfaces) {
            if !lit.enabled {
                continue;
            }
            let had_pending = lit.frame_pending;
            let cleared = if asleep {
                match lit.drm_output.with_compositor(|compositor| compositor.clear()) {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!("could not power down {}: {error}", lit.output.name());
                        success = false;
                        false
                    }
                }
            } else {
                lit.drm_output.reset_buffers();
                true
            };
            if cleared {
                let cutoff = (asleep && had_pending).then(FlipCutoff::now);
                // The flip that frame waited for may never come, and a refresh
                // after waking shows nothing from before.
                lit.retire_frame(&self.loop_handle, cutoff);
            }
        }
        success
    }

    fn scan_connectors(&mut self) {
        // Scanning without DRM master updates DrmScanner's cache while
        // `initialize_output` fails, leaving newly attached displays dark
        // when the session resumes.
        if !self.udev.as_ref().is_some_and(|udev| udev.session.is_active()) {
            return;
        }
        // Revert any active display preview while `self.udev` is still
        // installed so `configure_drm_output` restores KMS mode/enable state.
        self.cancel_output_preview();
        let Some(mut udev) = self.udev.take() else { return };
        for gpu in &mut udev.gpus {
            let events = match gpu.scanner.scan_connectors(gpu.manager.device()) {
                Ok(events) => events.into_iter().collect::<Vec<_>>(),
                Err(err) => {
                    tracing::warn!("connector scan failed: {err}");
                    Vec::new()
                }
            };
            for event in events {
                match event {
                    DrmScanEvent::Connected { connector, crtc: Some(crtc) } => {
                        if !gpu.surfaces.iter().any(|lit| lit.crtc == crtc) {
                            let renderer = gpu.renderer.as_mut().unwrap_or(&mut udev.renderer);
                            self.light(
                                gpu.node,
                                &mut gpu.manager,
                                renderer,
                                &mut gpu.surfaces,
                                &mut gpu.retired_flips,
                                connector,
                                crtc,
                            );
                        }
                    }
                    DrmScanEvent::Disconnected { crtc: Some(crtc), .. } => {
                        if let Some(index) = gpu.surfaces.iter().position(|lit| lit.crtc == crtc) {
                            let mut lit = gpu.surfaces.remove(index);
                            if let Some(cutoff) =
                                disconnect_flip_cutoff(lit.frame_pending, lit.retired_flip_before, FlipCutoff::now)
                            {
                                gpu.retired_flips.insert(crtc, cutoff);
                            }
                            lit.cancel_render_timer(&self.loop_handle);
                            lit.drop_unchanged();
                            self.remove_output(&lit.output);
                            self.forget_connected_output(&lit.output);
                        }
                    }
                    _ => {}
                }
            }
        }
        // A display that stays connected can change what it offers, as a
        // virtual machine's screen does when its window is resized. Keeping
        // its modes current lets configure_outputs follow the new preferred
        // mode when none is configured.
        for gpu in &mut udev.gpus {
            for lit in &mut gpu.surfaces {
                let Ok(info) = gpu.manager.device().get_connector(lit.connector, false) else { continue };
                if info.state() != connector::State::Connected || info.modes() == lit.modes.as_slice() {
                    continue;
                }
                let Some(preferred) = preferred_mode(info.modes()) else { continue };
                for mode in &lit.modes {
                    lit.output.delete_mode(WlMode::from(*mode));
                }
                for mode in info.modes() {
                    lit.output.add_mode(WlMode::from(*mode));
                }
                lit.output.set_preferred(WlMode::from(preferred));
                lit.modes = info.modes().to_vec();
                let size = WlMode::from(preferred).size;
                tracing::info!("{} now prefers {}x{}", lit.output.name(), size.w, size.h);
            }
        }
        self.udev = Some(udev);
        if let Err(error) = self.configure_outputs() {
            tracing::error!("output configuration rejected; retaining connected displays: {error}");
        }
        self.schedule_render();
    }

    #[allow(clippy::too_many_arguments, reason = "internal DRM hotplug helper borrowing fields of Gpu")]
    fn light(
        &mut self,
        node: DrmNode,
        manager: &mut OutputManager,
        renderer: &mut GlesRenderer,
        surfaces: &mut Vec<Lit>,
        retired_flips: &mut HashMap<crtc::Handle, FlipCutoff>,
        connector: connector::Info,
        crtc: crtc::Handle,
    ) {
        let name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
        let Some(mode) = preferred_mode(connector.modes()) else {
            return;
        };
        let wl_mode = WlMode::from(mode);
        let (width_mm, height_mm) = connector.size().unwrap_or((0, 0));
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: (width_mm as i32, height_mm as i32).into(),
                subpixel: connector.subpixel().into(),
                make: "Unknown".into(),
                model: "Unknown".into(),
            },
        );
        output.set_preferred(wl_mode);
        for mode in connector.modes() {
            output.add_mode(WlMode::from(*mode));
        }
        output.change_current_state(Some(wl_mode), None, None, Some((0, 0).into()));

        let mut planes = match manager.device().planes(&crtc) {
            Ok(planes) => planes,
            Err(err) => {
                tracing::warn!("no planes for {name}: {err}");
                return;
            }
        };
        // Overlay planes misbehave on NVIDIA.
        let nvidia = manager
            .device()
            .get_driver()
            .map(|driver| driver.name().to_string_lossy().to_lowercase().contains("nvidia"))
            .unwrap_or(false);
        if nvidia {
            planes.overlay.clear();
        }

        let init_result = manager.initialize_output::<_, crate::render::OutputElement>(
            crtc,
            mode,
            &[connector.handle()],
            // Render elements are made in physical pixels, so the
            // compositor works at scale 1 whatever the output's scale.
            smithay::output::OutputModeSource::Static {
                size: wl_mode.size,
                scale: 1.0.into(),
                transform: smithay::utils::Transform::Normal,
            },
            Some(planes),
            renderer,
            &DrmOutputRenderElements::default(),
        );
        // Smithay's bandwidth fallback inside `initialize_output` can commit
        // frames synchronously on existing CRTCs on this GPU (even on `Err`),
        // dropping their pending frames without generating a vblank event.
        // Watch any existing pending flips so they recover if no vblank comes.
        let watch: Vec<StuckFlip> = surfaces.iter_mut().filter_map(|lit| lit.watch_pending_flip(node)).collect();
        schedule_stuck_flip_retry(&self.loop_handle, watch, BANDWIDTH_FALLBACK_STALE_FLIP, 0);
        let drm_output = match init_result {
            Ok(initialized) => initialized,
            Err(err) => {
                tracing::error!("could not light {name}: {err}");
                return;
            }
        };
        let size = wl_mode.size;
        tracing::info!("lit {name} at {}x{}@{:.2}", size.w, size.h, wl_mode.refresh as f64 / 1000.0);
        self.advertise_output(&output);
        self.add_output(output.clone());
        surfaces.push(Lit {
            surface_id: NEXT_SURFACE_ID.fetch_add(1, Ordering::Relaxed),
            connector: connector.handle(),
            modes: connector.modes().to_vec(),
            mode: wl_mode,
            enabled: true,
            crtc,
            output,
            drm_output,
            frame_pending: false,
            flip_seq: 0,
            wake_retry_seq: None,
            retired_flip_before: retired_flips.remove(&crtc),
            render_timer: None,
            in_flight: Vec::new(),
            lock_queued: false,
            lock_shown: false,
            mirror: None,
            unchanged: Vec::new(),
            flipping: Vec::new(),
        });
    }

    /// Changes the KMS mode as well as Smithay's rendering transform. Disabled
    /// connectors retain their surface and mode list for later re-enabling.
    pub fn configure_drm_output(
        &mut self,
        output: &Output,
        mode: WlMode,
        transform: smithay::utils::Transform,
        enabled: bool,
    ) -> Result<(), String> {
        let Some(udev) = self.udev.as_mut() else { return Ok(()) };
        for gpu in &mut udev.gpus {
            let Some(index) = gpu.surfaces.iter().position(|lit| &lit.output == output) else { continue };
            if !enabled {
                let lit = &mut gpu.surfaces[index];
                let cutoff = lit.frame_pending.then(FlipCutoff::now);
                lit.drm_output
                    .with_compositor(|compositor| compositor.clear())
                    .map_err(|error| format!("could not disable {}: {error}", output.name()))?;
                lit.enabled = false;
                // Their fifo batches go with the watchdog.
                lit.retire_frame(&self.loop_handle, cutoff);
                return Ok(());
            }
            let drm_mode = gpu.surfaces[index]
                .modes
                .iter()
                .find(|candidate| WlMode::from(**candidate) == mode)
                .copied()
                .ok_or_else(|| format!("{} does not advertise the requested mode", output.name()))?;
            let mode_changed = gpu.surfaces[index].mode != mode;
            if mode_changed {
                let renderer = gpu.renderer.as_mut().unwrap_or(&mut udev.renderer);
                let mode_result = gpu.surfaces[index].drm_output.use_mode::<_, crate::render::OutputElement>(
                    drm_mode,
                    renderer,
                    &DrmOutputRenderElements::default(),
                );
                // Smithay's bandwidth fallback inside `use_mode` can commit
                // frames synchronously on every CRTC on this GPU (even on
                // `Err`), dropping their pending frames without a vblank event.
                // Watch all pending flips on this GPU so none stay stranded.
                let node = gpu.node;
                let watch: Vec<StuckFlip> =
                    gpu.surfaces.iter_mut().filter_map(|surface| surface.watch_pending_flip(node)).collect();
                schedule_stuck_flip_retry(&self.loop_handle, watch, BANDWIDTH_FALLBACK_STALE_FLIP, 0);
                mode_result.map_err(|error| format!("could not set mode on {}: {error}", output.name()))?;
                let lit = &mut gpu.surfaces[index];
                lit.mode = mode;
                // Frames waiting for a refresh of the old mode give up.
                lit.drop_unchanged();
            }
            let lit = &mut gpu.surfaces[index];
            lit.drm_output.with_compositor(|compositor| {
                compositor.set_output_mode_source(smithay::output::OutputModeSource::Static {
                    size: mode.size,
                    scale: 1.0.into(),
                    transform,
                })
            });
            if !lit.enabled || mode_changed || output.current_transform() != transform {
                // `use_mode` usually updates the pending mode and swapchain
                // without retiring an in-flight page flip, while the watchdog
                // above recovers if Smithay's bandwidth fallback committed
                // synchronously. Keep `frame_pending` intact when already
                // enabled so a normal in-flight flip completes cleanly.
                lit.drm_output.reset_buffers();
                if !lit.enabled {
                    lit.retire_frame(&self.loop_handle, None);
                } else {
                    lit.cancel_render_timer(&self.loop_handle);
                    lit.lock_queued = false;
                    lit.lock_shown = false;
                    lit.drop_unchanged();
                }
                lit.mirror = None;
            }
            lit.enabled = true;
            return Ok(());
        }
        Err(format!("{} is no longer connected to DRM", output.name()))
    }

    /// Asks for a frame soon, unless one is already on its way.
    pub fn schedule_render(&mut self) {
        if let Some(udev) = self.udev.as_mut() {
            udev.asked = true;
        }
        self.request_render();
    }

    fn request_render(&mut self) {
        let Some(udev) = self.udev.as_mut() else { return };
        if udev.render_pending
            || udev
                .gpus
                .iter()
                .flat_map(|gpu| &gpu.surfaces)
                .all(|lit| !lit.enabled || lit.frame_pending || lit.render_timer.is_some())
        {
            return;
        }
        udev.render_pending = true;
        self.loop_handle.insert_idle(|state| state.render_udev());
    }

    /// The dmabufs of every surface on screen. Smithay signals a buffer's
    /// explicit-sync release point from the CPU as soon as nothing refers to
    /// it, which can be while the GPU still draws a queued frame from it. A
    /// client that reuses the buffer then draws into what is being composited:
    /// Chromium on NVIDIA showed black patches. Holding the buffers until the
    /// frame is on screen closes that gap, since KMS flips only once the
    /// frame's GPU work is done.
    fn frame_buffers(&self, output: &Output) -> Vec<Buffer> {
        let mut buffers = Vec::new();
        let mut hold = |_: &WlSurface, data: &SurfaceData| {
            if let Some(state) = data.data_map.get::<RendererSurfaceStateUserData>()
                && let Ok(state) = state.lock()
                && let Some(buffer) = state.buffer()
                && get_dmabuf(buffer).is_ok()
            {
                buffers.push(buffer.clone());
            }
        };
        for (_, window) in self.windows.iter().filter(|(window_id, _)| self.screen_shows(**window_id)) {
            window.with_surfaces(&mut hold);
        }
        for layer in layer_map_for_output(output).layers() {
            layer.with_surfaces(&mut hold);
        }
        self.with_dashboard_surfaces(&mut hold);
        buffers
    }

    fn on_vblank(&mut self, node: DrmNode, crtc: crtc::Handle, metadata: Option<DrmEventMetadata>) {
        let Some(udev) = self.udev.as_mut() else { return };
        let Some(lit) = udev.lit_mut(node, crtc) else { return };
        if retire_vblank_cutoff(&mut lit.retired_flip_before, metadata.as_ref(), SystemTime::now()) {
            return;
        }
        if !lit.frame_pending {
            // Still retire any pending flip inside Smithay so `queue_frame`
            // never stalls behind an already-completed flip.
            let fifo = match lit.drm_output.frame_submitted() {
                Ok(Some(done)) => {
                    if let Some(mut feedback) = done.feedback {
                        feedback.discarded();
                    }
                    done.fifo
                }
                Ok(None) | Err(_) => Vec::new(),
            };
            for frame in fifo {
                self.release_fifo_frame(frame);
            }
            return;
        }
        lit.frame_pending = false;
        lit.flip_seq = lit.flip_seq.wrapping_add(1);
        // The flip waited for the frame's GPU work, so clients may have
        // these buffers back.
        lit.in_flight.clear();
        lit.lock_shown = lit.lock_queued;
        let flip = match metadata {
            Some(DrmEventMetadata { time: DrmEventTime::Monotonic(time), sequence }) => Some((sequence, time)),
            _ => None,
        };
        lit.flipped(flip);
        let fifo = match lit.drm_output.frame_submitted() {
            Ok(Some(done)) => {
                if let Some(mut feedback) = done.feedback {
                    let refresh = crate::frame_timing::refresh_of(&lit.output);
                    match metadata {
                        Some(DrmEventMetadata { time: DrmEventTime::Monotonic(time), sequence }) => {
                            let flags = wp_presentation_feedback::Kind::Vsync
                                | wp_presentation_feedback::Kind::HwClock
                                | wp_presentation_feedback::Kind::HwCompletion;
                            feedback.presented::<_, Monotonic>(time, refresh, u64::from(sequence), flags);
                        }
                        _ => crate::frame_timing::presented_now(feedback, refresh),
                    }
                }
                done.fifo
            }
            Ok(None) => Vec::new(),
            Err(err) => {
                tracing::warn!("frame submit: {err}");
                Vec::new()
            }
        };
        // The sleep inhibitor must cover every output, not just the first
        // monitor whose page flip completes.
        let refresh = lit.output.current_mode().map_or(60_000, |mode| mode.refresh);
        let locked_everywhere =
            udev.gpus.iter().flat_map(|gpu| &gpu.surfaces).filter(|lit| lit.enabled).all(|lit| lit.lock_shown);
        if locked_everywhere {
            self.sleep_frame_shown();
        }
        // The next frame for this output is drawn just before it is due rather
        // than right away, so what clients commit in the meantime still makes
        // it, without stalling other outputs' refresh cadence. The timer is
        // set before fifo programs are let go, so their commits wait for it.
        let mut fallback_render = false;
        if let Some(udev) = self.udev.as_mut()
            && let Some(lit) = udev.lit_mut(node, crtc)
            && lit.enabled
        {
            lit.cancel_render_timer(&self.loop_handle);
            let period = Duration::from_secs_f64(1000.0 / f64::from(refresh.max(1_000)));
            match self.loop_handle.insert_source(
                Timer::from_duration(period.saturating_sub(RENDER_BUDGET)),
                move |_, _, state| {
                    if let Some(udev) = state.udev.as_mut()
                        && let Some(lit) = udev.lit_mut(node, crtc)
                    {
                        lit.render_timer = None;
                    }
                    state.request_render();
                    TimeoutAction::Drop
                },
            ) {
                Ok(token) => lit.render_timer = Some(token),
                Err(_) => fallback_render = true,
            }
        }
        if fallback_render {
            self.request_render();
        }
        for frame in fifo {
            self.release_fifo_frame(frame);
        }
    }

    /// Looks whether the screen has refreshed since a frame that changed
    /// nothing was made: if so, what was committed for it showed at the
    /// first refresh after, reported with that refresh's counter and time;
    /// if not yet, it looks again a refresh later. It is discarded when the
    /// screen stopped showing frames, a newer frame took the refresh (it is
    /// gone then), or no refresh came after [`UNCHANGED_TRIES`] looks.
    fn resolve_unchanged(&mut self, node: DrmNode, crtc: crtc::Handle, token: u64, tries: u32) {
        let Some(udev) = self.udev.as_mut() else { return };
        let showing = udev.session.is_active() && !self.idle.off;
        let Some(gpu) = udev.gpus.iter_mut().find(|gpu| gpu.node == node) else { return };
        let latest = latest_vblank(&gpu.manager, crtc);
        let Some(lit) = gpu.surfaces.iter_mut().find(|lit| lit.crtc == crtc) else { return };
        let Some(index) = lit.unchanged.iter().position(|unchanged| unchanged.token == token) else { return };
        let after = lit.unchanged[index].after;
        let refreshed = refreshed_since(latest, after);
        if refreshed.is_none() && showing && lit.enabled && tries < UNCHANGED_TRIES {
            let wait = unchanged_wait(&lit.output);
            let retry = self.loop_handle.insert_source(Timer::from_duration(wait), move |_, _, state| {
                state.resolve_unchanged(node, crtc, token, tries + 1);
                TimeoutAction::Drop
            });
            if retry.is_ok() {
                return;
            }
        }
        let unchanged = lit.unchanged.remove(index);
        let refresh = crate::frame_timing::refresh_of(&lit.output);
        if let Some(mut feedback) = unchanged.feedback {
            match refreshed.filter(|_| showing && lit.enabled) {
                Some(vblank) => present_unchanged(feedback, after, vblank, refresh),
                None => feedback.discarded(),
            }
        }
        if let Some(frame) = unchanged.fifo {
            self.release_fifo_frame(frame);
        }
    }

    fn render_udev(&mut self) {
        let Some(mut udev) = self.udev.take() else { return };
        udev.render_pending = false;
        if let Some(poll) = udev.poll.take() {
            self.loop_handle.remove(poll);
        }
        if !udev.session.is_active() || self.idle.off {
            self.udev = Some(udev);
            // No frame is coming to let them through.
            self.release_all_fifo();
            return;
        }
        let now = self.now();
        // The duck moves on before drawing, so a frame drawn for its next
        // step shows that step.
        self.update_duck();
        let mut queued = false;
        // Fifo barriers of frames that will not flip, let go at the end.
        let mut release_now = Vec::new();
        let mut gpus = std::mem::take(&mut udev.gpus);
        let Some(gbm) = gpus.first().map(|primary| primary.gbm.clone()) else {
            self.udev = Some(udev);
            return;
        };
        self.process_streams(&mut udev.renderer);
        self.process_window_copies(&mut udev.renderer);
        for gpu in &mut gpus {
            for lit in &mut gpu.surfaces {
                if !lit.enabled || lit.frame_pending || lit.render_timer.is_some() {
                    continue;
                }
                let output = lit.output.clone();
                let secondary = gpu.renderer.is_some();
                let node = gpu.node;
                self.with_output(&output, |state| {
                    state.update_bar();
                    state.process_special_captures(&mut udev.renderer);
                    let clear = state.clear_colour();
                    let scene = state.frame_elements(&mut udev.renderer, now);
                    state.process_captures(&mut udev.renderer, Some(&scene));
                    let renderer = gpu.renderer.as_mut().unwrap_or(&mut udev.renderer);
                    let mut elements = if state.is_active_output() {
                        state.cursor_elements(renderer, &mut udev.cursor)
                    } else {
                        Vec::new()
                    };
                    let elements = if let Some(secondary) = gpu.renderer.as_mut() {
                        let size = state.output_pixels();
                        // The pointer is drawn by the secondary GPU itself, so
                        // moving it needs nothing new from the primary.
                        lit.mirrored(&gbm, &mut udev.renderer, secondary, &scene, size, clear).map(|frame| {
                            elements.push(frame);
                            elements
                        })
                    } else {
                        elements.extend(scene);
                        Ok(elements)
                    };
                    let renderer = gpu.renderer.as_mut().unwrap_or(&mut udev.renderer);
                    let rendered = match &elements {
                        Ok(elements) => lit
                            .drm_output
                            .render_frame(renderer, elements, clear, FrameFlags::DEFAULT)
                            .map_err(|error| format!("render: {error:?}")),
                        Err(error) => Err(format!("secondary GPU frame: {error}")),
                    };
                    match rendered {
                        Ok(result) if !result.is_empty => {
                            // A picture made on the primary GPU holds the
                            // surfaces its own pass drew.
                            let states = if secondary {
                                lit.mirror.as_ref().map(|mirror| mirror.states())
                            } else {
                                Some(&result.states)
                            };
                            let feedback = state.presentation_feedback(states);
                            let batch = state.take_fifo_barriers();
                            let mut fifo: Vec<u64> = state.fifo_in_flight(batch).into_iter().collect();
                            // If KMS cannot wait on the render fence, finish GPU work
                            // before scanout rather than displaying an unfinished frame.
                            let unfinished = match &result.primary_element {
                                PrimaryPlaneElement::Swapchain(element) if result.needs_sync() => element
                                    .sync
                                    .wait()
                                    .inspect_err(|err| tracing::warn!("render sync: {err:?}"))
                                    .is_err(),
                                _ => false,
                            };
                            // Looked at once that wait is over, as the
                            // screen may have refreshed during it.
                            let latest =
                                if lit.unchanged.is_empty() { None } else { latest_vblank(&gpu.manager, lit.crtc) };
                            let (unchanged_fifo, waiting) = lit.take_unchanged(latest);
                            fifo.extend(unchanged_fifo);
                            lit.flipping.extend(waiting);
                            let done = FrameDone { feedback, fifo: fifo.clone() };
                            if unfinished {
                                lit.flipped(None);
                                release_now.extend(fifo);
                            } else {
                                match lit.drm_output.queue_frame(done) {
                                    Ok(()) => {
                                        queued = true;
                                        lit.frame_pending = true;
                                        lit.flip_seq = lit.flip_seq.wrapping_add(1);
                                        lit.lock_queued = state.is_locked();
                                        lit.in_flight = state.frame_buffers(&lit.output);
                                    }
                                    Err(err) => {
                                        tracing::warn!("queue frame: {err}");
                                        lit.flipped(None);
                                        release_now.extend(fifo);
                                    }
                                }
                            }
                        }
                        Ok(result) => {
                            // Nothing changed on screen: what was committed is
                            // already shown.
                            let locked = state.is_locked();
                            lit.lock_queued = locked;
                            lit.lock_shown = locked;
                            let states = if secondary {
                                lit.mirror.as_ref().map(|mirror| mirror.states())
                            } else {
                                Some(&result.states)
                            };
                            let feedback = state.presentation_feedback(states);
                            let batch = state.take_fifo_barriers();
                            let fifo = state.fifo_in_flight(batch);
                            if feedback.is_some() || fifo.is_some() {
                                match latest_vblank(&gpu.manager, lit.crtc) {
                                    None => {
                                        // The kernel cannot say when the screen
                                        // refreshes: shown as of now.
                                        if let Some(feedback) = feedback {
                                            let refresh = crate::frame_timing::refresh_of(&lit.output);
                                            crate::frame_timing::presented_now(feedback, refresh);
                                        }
                                        release_now.extend(fifo);
                                    }
                                    Some((after, _)) => {
                                        let token = NEXT_UNCHANGED.fetch_add(1, Ordering::Relaxed);
                                        lit.unchanged.push(Unchanged { token, feedback, fifo, after });
                                        let crtc = lit.crtc;
                                        let timer = Timer::from_duration(unchanged_wait(&lit.output));
                                        let registered = state.loop_handle.insert_source(timer, move |_, _, state| {
                                            state.resolve_unchanged(node, crtc, token, 0);
                                            TimeoutAction::Drop
                                        });
                                        if let Err(error) = registered {
                                            tracing::warn!("presentation timer: {error}");
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!("{}: {error}", lit.output.name());
                            let batch = state.take_fifo_barriers();
                            release_now.extend(state.fifo_in_flight(batch));
                        }
                    }
                    let elapsed = state.start.elapsed();
                    // Minimized windows are hidden: no frames, so they need not draw.
                    // Windows in every area of a split screen draw.
                    for (_, window) in state.windows.iter().filter(|(window_id, _)| state.screen_shows(**window_id)) {
                        window.send_frame(&lit.output, elapsed, Some(Duration::ZERO), |_, _| Some(lit.output.clone()));
                    }
                    state.send_layer_frames(elapsed);
                });
            }
        }
        udev.gpus = gpus;
        let asked = std::mem::take(&mut udev.asked);
        let duck_changed = self.take_duck_changed();
        udev.quiet = match (queued, asked) {
            // The duck's own steps come when duck_due says, not from looking
            // every frame: they leave the count of still frames as it was.
            (true, false) if duck_changed => udev.quiet,
            (true, false) => 0,
            (true, true) => QUIET_FRAMES - ASKED_FRAMES,
            (false, _) => udev.quiet.saturating_add(1),
        };
        if udev
            .gpus
            .iter()
            .flat_map(|gpu| &gpu.surfaces)
            .any(|lit| lit.enabled && !lit.frame_pending && lit.render_timer.is_none())
        {
            // Nothing changed on screen: look again in about a frame, and
            // less often once it has been still for a while. Input and
            // client commits ask for a frame themselves.
            let wait = if udev.quiet <= QUIET_FRAMES { FRAME } else { STILL_POLL };
            let wait = match self.duck_due(Instant::now()) {
                crate::duck::DuckDue::Never => wait,
                crate::duck::DuckDue::Every => FRAME,
                // Not sooner than a few milliseconds, should a due time
                // stay in the past.
                crate::duck::DuckDue::Due(due) => {
                    wait.min(due.saturating_duration_since(Instant::now()).max(DUE_FLOOR))
                }
            };
            udev.poll = self
                .loop_handle
                .insert_source(Timer::from_duration(wait), |_, _, state| {
                    if let Some(udev) = state.udev.as_mut() {
                        udev.poll = None;
                    }
                    state.request_render();
                    TimeoutAction::Drop
                })
                .ok();
        }
        let all_locked_shown = self.is_locked()
            && udev.gpus.iter().flat_map(|gpu| &gpu.surfaces).filter(|lit| lit.enabled).all(|lit| lit.lock_shown);
        self.udev = Some(udev);
        // Let go once the renderer is back, so the commits they free are
        // drawn in the next frame.
        for frame in release_now {
            self.release_fifo_frame(frame);
        }
        self.sleep_frame_queued(queued || !all_locked_shown);
        self.after_frame();
    }

    /// Opens a device for a VT switch, from the keyboard shortcut.
    pub fn change_vt(&mut self, vt_number: i32) {
        if let Some(udev) = self.udev.as_mut()
            && let Err(err) = udev.session.change_vt(vt_number)
        {
            tracing::warn!("could not switch to VT {vt_number}: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_vblank_ignores_events_at_or_before_clear_cutoff() {
        let base_realtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let initial = FlipCutoff { monotonic: Duration::from_secs(10), realtime: base_realtime };

        let before = DrmEventMetadata { sequence: 1, time: DrmEventTime::Monotonic(Duration::from_secs(9)) };
        let exact = DrmEventMetadata { sequence: 2, time: DrmEventTime::Monotonic(Duration::from_secs(10)) };
        let after = DrmEventMetadata { sequence: 3, time: DrmEventTime::Monotonic(Duration::from_millis(10_001)) };

        // Stale events and untimestamped events keep the cutoff armed until a post-cutoff event arrives.
        let now_rt = base_realtime + Duration::from_secs(1);
        let mut cutoff = Some(initial);
        assert!(retire_vblank_cutoff(&mut cutoff, Some(&before), now_rt));
        assert_eq!(cutoff, Some(initial));
        assert!(retire_vblank_cutoff(&mut cutoff, Some(&exact), now_rt));
        assert_eq!(cutoff, Some(initial));
        assert!(!retire_vblank_cutoff(&mut cutoff, None, now_rt));
        assert_eq!(cutoff, Some(initial));
        assert!(!retire_vblank_cutoff(&mut cutoff, Some(&after), now_rt));
        assert_eq!(cutoff, None);
        assert!(!retire_vblank_cutoff(&mut cutoff, Some(&before), now_rt));

        // Realtime clock domain is handled symmetrically.
        let rt_before =
            DrmEventMetadata { sequence: 4, time: DrmEventTime::Realtime(base_realtime - Duration::from_secs(1)) };
        let rt_exact = DrmEventMetadata { sequence: 5, time: DrmEventTime::Realtime(base_realtime) };
        let rt_after =
            DrmEventMetadata { sequence: 6, time: DrmEventTime::Realtime(base_realtime + Duration::from_millis(1)) };
        let mut cutoff = Some(initial);
        assert!(retire_vblank_cutoff(&mut cutoff, Some(&rt_before), now_rt));
        assert_eq!(cutoff, Some(initial));
        assert!(retire_vblank_cutoff(&mut cutoff, Some(&rt_exact), now_rt));
        assert_eq!(cutoff, Some(initial));
        assert!(!retire_vblank_cutoff(&mut cutoff, Some(&rt_after), now_rt));
        assert_eq!(cutoff, None);

        // A backward wall-clock step clears the realtime cutoff instead of stalling completion.
        let stepped_back = base_realtime - Duration::from_secs(5);
        let mut cutoff = Some(initial);
        assert!(!retire_vblank_cutoff(&mut cutoff, Some(&rt_before), stepped_back));
        assert_eq!(cutoff, None);
    }

    #[test]
    fn disconnect_cutoff_protects_replacement_surface_from_stale_vblank() {
        let base_realtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let disconnect_time = FlipCutoff { monotonic: Duration::from_secs(20), realtime: base_realtime };

        // Disconnecting an idle surface with no prior cutoff leaves no cutoff.
        assert_eq!(disconnect_flip_cutoff(false, None, || disconnect_time), None);

        // Disconnecting a surface with an existing cutoff preserves it.
        let prior = FlipCutoff { monotonic: Duration::from_secs(18), realtime: base_realtime - Duration::from_secs(2) };
        assert_eq!(disconnect_flip_cutoff(false, Some(prior), || disconnect_time), Some(prior));

        // Disconnecting a surface with an in-flight flip records a fresh cutoff
        // that drops the old flip's late vblank on the replacement surface,
        // then allows the replacement surface's first real vblank through.
        let mut replacement_cutoff = disconnect_flip_cutoff(true, Some(prior), || disconnect_time);
        assert_eq!(replacement_cutoff, Some(disconnect_time));

        let stale_old_vblank =
            DrmEventMetadata { sequence: 10, time: DrmEventTime::Monotonic(Duration::from_secs(19)) };
        let replacement_vblank =
            DrmEventMetadata { sequence: 11, time: DrmEventTime::Monotonic(Duration::from_millis(20_016)) };
        let now_rt = base_realtime + Duration::from_millis(16);

        assert!(retire_vblank_cutoff(&mut replacement_cutoff, Some(&stale_old_vblank), now_rt));
        assert_eq!(replacement_cutoff, Some(disconnect_time));
        assert!(!retire_vblank_cutoff(&mut replacement_cutoff, Some(&replacement_vblank), now_rt));
        assert_eq!(replacement_cutoff, None);
    }

    #[test]
    fn wake_retry_delay_backs_off_boundedly() {
        assert_eq!(wake_retry_delay(0), Duration::from_millis(500));
        assert_eq!(wake_retry_delay(1), Duration::from_millis(1000));
        assert_eq!(wake_retry_delay(2), Duration::from_millis(2000));
        assert_eq!(wake_retry_delay(3), Duration::from_millis(4000));
        assert_eq!(wake_retry_delay(10), Duration::from_millis(4000));
    }
}
