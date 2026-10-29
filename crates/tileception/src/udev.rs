//! Runs Tileception as the session's compositor: DRM/KMS for output, libinput for
//! input, libseat for device access and VT switching.
//!
//! Independent scanout devices per GPU, with one primary compositor renderer.
//! Outputs on other GPUs show frames it draws; see `mirror`.

use std::time::{Duration, Instant};

use crate::{Tileception, cursor::Cursor, mirror::Mirror};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
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
            control::{ModeTypeFlags, connector, crtc},
        },
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{backend::GlobalId, protocol::wl_surface::WlSurface},
    },
    utils::DeviceFd,
    wayland::{
        compositor::SurfaceData,
        dmabuf::{DmabufFeedbackBuilder, DmabufState, get_dmabuf},
        drm_syncobj::{DrmSyncobjState, supports_syncobj_eventfd},
    },
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
type OutputManager = DrmOutputManager<Allocator, Exporter, (), DrmDeviceFd>;
type Surface = DrmOutput<Allocator, Exporter, (), DrmDeviceFd>;

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
/// How often a still screen is looked at for changes nothing asked to draw.
const STILL_POLL: Duration = Duration::from_millis(100);

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

struct Gpu {
    node: DrmNode,
    gbm: GbmDevice<DrmDeviceFd>,
    manager: OutputManager,
    scanner: DrmScanner,
    surfaces: Vec<Lit>,
    _drm_token: RegistrationToken,
    /// Secondary GPUs upload completed primary-renderer frames for scanout.
    renderer: Option<GlesRenderer>,
}

struct Lit {
    crtc: crtc::Handle,
    output: Output,
    drm_output: Surface,
    global: GlobalId,
    frame_pending: bool,
    /// Client buffers held until scanout completes. See `frame_buffers`.
    in_flight: Vec<Buffer>,
    lock_queued: bool,
    lock_shown: bool,
    /// Frames from the primary renderer, for an output on a secondary GPU.
    mirror: Option<Mirror>,
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
        let token = handle.insert_source(notifier, move |event, _, state| match event {
            DrmEvent::VBlank(crtc) => state.on_vblank(node, crtc),
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

pub fn init(
    event_loop: &mut EventLoop<'static, Tileception>,
    state: &mut Tileception,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, notifier) = LibSeatSession::new().map_err(|err| format!("no seat session: {err}"))?;
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
            }
            InputEvent::DeviceRemoved { device } => state.touchpad_removed(device),
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
                for gpu in &mut udev.gpus {
                    gpu.manager.pause();
                    for lit in &mut gpu.surfaces {
                        lit.frame_pending = false;
                        lit.in_flight.clear();
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
            if let Some(udev) = state.udev.as_mut() {
                udev.render_pending = false;
                for gpu in &mut udev.gpus {
                    if let Err(err) = gpu.manager.activate(false) {
                        tracing::error!("drm could not resume: {err}");
                    }
                    for lit in &mut gpu.surfaces {
                        lit.frame_pending = false;
                        lit.in_flight.clear();
                        lit.drm_output.with_compositor(|compositor| {
                            // A pre-suspend page flip may never deliver its vblank. Drop
                            // both Smithay's queued frames and the stale scanout state.
                            if let Err(err) = compositor.clear() {
                                tracing::error!("could not clear stale scanout: {err}");
                            }
                            compositor.reset_buffers();
                            if let Err(err) = compositor.reset_state() {
                                tracing::error!("could not reset resumed output: {err}");
                            }
                        });
                    }
                }
            }
            state.idle_activity();
            state.discover_gpus();
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
        self.scan_connectors();
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
                self.add_gpu(node.dev_id(), &path);
            }
        }
    }

    fn remove_gpu(&mut self, device_id: libc::dev_t) {
        let Some(mut udev) = self.udev.take() else { return };
        if let Some(index) = udev.gpus.iter().position(|gpu| gpu.node.dev_id() == device_id) {
            let gpu = udev.gpus.remove(index);
            for lit in gpu.surfaces {
                self.remove_output(&lit.output);
                self.display_handle.remove_global::<Tileception>(lit.global);
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
            if asleep {
                if let Err(error) = lit.drm_output.with_compositor(|compositor| compositor.clear()) {
                    tracing::warn!("could not power down {}: {error}", lit.output.name());
                    success = false;
                }
            } else {
                lit.drm_output.reset_buffers();
            }
            lit.frame_pending = false;
            lit.in_flight.clear();
        }
        success
    }

    fn scan_connectors(&mut self) {
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
                            self.light(&mut gpu.manager, renderer, &mut gpu.surfaces, connector, crtc);
                        }
                    }
                    DrmScanEvent::Disconnected { crtc: Some(crtc), .. } => {
                        if let Some(index) = gpu.surfaces.iter().position(|lit| lit.crtc == crtc) {
                            let lit = gpu.surfaces.remove(index);
                            self.remove_output(&lit.output);
                            self.display_handle.remove_global::<Tileception>(lit.global);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.udev = Some(udev);
        self.schedule_render();
    }

    fn light(
        &mut self,
        manager: &mut OutputManager,
        renderer: &mut GlesRenderer,
        surfaces: &mut Vec<Lit>,
        connector: connector::Info,
        crtc: crtc::Handle,
    ) {
        let name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
        let Some(mode) = connector
            .modes()
            .iter()
            .find(|candidate| candidate.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(connector.modes().first())
            .copied()
        else {
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

        let drm_output = match manager.initialize_output::<_, crate::render::OutputElement>(
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
        ) {
            Ok(initialized) => initialized,
            Err(err) => {
                tracing::error!("could not light {name}: {err}");
                return;
            }
        };
        let size = wl_mode.size;
        tracing::info!("lit {name} at {}x{}@{:.2}", size.w, size.h, wl_mode.refresh as f64 / 1000.0);
        let global = output.create_global::<Tileception>(&self.display_handle);
        self.add_output(output.clone());
        surfaces.push(Lit {
            crtc,
            output,
            drm_output,
            global,
            frame_pending: false,
            in_flight: Vec::new(),
            lock_queued: false,
            lock_shown: false,
            mirror: None,
        });
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
        if udev.render_pending || udev.gpus.iter().flat_map(|gpu| &gpu.surfaces).all(|lit| lit.frame_pending) {
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
        for (_, window) in self.windows.iter().filter(|(window_id, _)| self.world.contains_window(**window_id)) {
            window.with_surfaces(&mut hold);
        }
        for layer in layer_map_for_output(output).layers() {
            layer.with_surfaces(&mut hold);
        }
        buffers
    }

    fn on_vblank(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let Some(udev) = self.udev.as_mut() else { return };
        let Some(gpu) = udev.gpus.iter_mut().find(|gpu| gpu.node == node) else { return };
        let Some(lit) = gpu.surfaces.iter_mut().find(|lit| lit.crtc == crtc) else { return };
        lit.frame_pending = false;
        // The flip waited for the frame's GPU work, so clients may have
        // these buffers back.
        lit.in_flight.clear();
        lit.lock_shown = lit.lock_queued;
        if let Err(err) = lit.drm_output.frame_submitted() {
            tracing::warn!("frame submit: {err}");
        }
        // The sleep inhibitor must cover every output, not just the first
        // monitor whose page flip completes.
        let refresh = lit.output.current_mode().map_or(60_000, |mode| mode.refresh);
        let locked_everywhere = udev.gpus.iter().flat_map(|gpu| &gpu.surfaces).all(|lit| lit.lock_shown);
        if locked_everywhere {
            self.sleep_frame_shown();
        }
        // The next frame is drawn just before it is due rather than right
        // away, so what clients commit in the meantime still makes it.
        // Asking for a frame before then waits for this one.
        let Some(udev) = self.udev.as_mut() else { return };
        if udev.render_pending {
            return;
        }
        let period = Duration::from_secs_f64(1000.0 / f64::from(refresh.max(1_000)));
        udev.render_pending = true;
        let drawn = self.loop_handle.insert_source(
            Timer::from_duration(period.saturating_sub(RENDER_BUDGET)),
            |_, _, state| {
                state.render_udev();
                TimeoutAction::Drop
            },
        );
        if drawn.is_err() {
            self.render_udev();
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
            return;
        }
        let now = self.now();
        let mut queued = false;
        let mut gpus = std::mem::take(&mut udev.gpus);
        let Some(gbm) = gpus.first().map(|primary| primary.gbm.clone()) else {
            self.udev = Some(udev);
            return;
        };
        for gpu in &mut gpus {
            for lit in &mut gpu.surfaces {
                if lit.frame_pending {
                    continue;
                }
                let output = lit.output.clone();
                self.with_output(&output, |state| {
                    state.update_bar();
                    state.process_captures(&mut udev.renderer);
                    let clear = state.clear_colour();
                    let renderer = gpu.renderer.as_mut().unwrap_or(&mut udev.renderer);
                    let mut elements = if state.is_active_output() {
                        state.cursor_elements(renderer, &mut udev.cursor)
                    } else {
                        Vec::new()
                    };
                    let elements = if let Some(secondary) = gpu.renderer.as_mut() {
                        let scene = state.frame_elements(&mut udev.renderer, now);
                        let size = state.output_pixels();
                        // The pointer is drawn by the secondary GPU itself, so
                        // moving it needs nothing new from the primary.
                        lit.mirrored(&gbm, &mut udev.renderer, secondary, &scene, size, clear).map(|frame| {
                            elements.push(frame);
                            elements
                        })
                    } else {
                        elements.extend(state.frame_elements(&mut udev.renderer, now));
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
                            // If KMS cannot wait on the render fence, finish GPU work
                            // before scanout rather than displaying an unfinished frame.
                            if result.needs_sync()
                                && let PrimaryPlaneElement::Swapchain(element) = &result.primary_element
                                && let Err(err) = element.sync.wait()
                            {
                                tracing::warn!("render sync: {err:?}");
                            } else {
                                match lit.drm_output.queue_frame(()) {
                                    Ok(()) => {
                                        queued = true;
                                        lit.frame_pending = true;
                                        lit.lock_queued = state.is_locked();
                                        lit.in_flight = state.frame_buffers(&lit.output);
                                    }
                                    Err(err) => tracing::warn!("queue frame: {err}"),
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!("{}: {error}", lit.output.name()),
                    }
                    let elapsed = state.start.elapsed();
                    // Minimized windows are hidden: no frames, so they need not draw.
                    for (_, window) in
                        state.windows.iter().filter(|(window_id, _)| state.world.contains_window(**window_id))
                    {
                        window.send_frame(&lit.output, elapsed, Some(Duration::ZERO), |_, _| Some(lit.output.clone()));
                    }
                    state.send_layer_frames(elapsed);
                });
            }
        }
        udev.gpus = gpus;
        let asked = std::mem::take(&mut udev.asked);
        udev.quiet = match (queued, asked) {
            (true, false) => 0,
            (true, true) => QUIET_FRAMES - ASKED_FRAMES,
            (false, _) => udev.quiet.saturating_add(1),
        };
        if udev.gpus.iter().flat_map(|gpu| &gpu.surfaces).any(|lit| !lit.frame_pending) {
            // Nothing changed on screen: look again in about a frame, and
            // less often once it has been still for a while. Input and
            // client commits ask for a frame themselves.
            let wait = if udev.quiet <= QUIET_FRAMES { FRAME } else { STILL_POLL };
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
        self.udev = Some(udev);
        self.sleep_frame_queued(queued);
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
