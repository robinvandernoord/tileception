//! Runs Tileception as the session's compositor: DRM/KMS for output, libinput for
//! input, libseat for device access and VT switching.
//!
//! One GPU and, for now, one output: the first connected monitor. Further
//! monitors are lit by nothing until worlds per display land.

use std::time::{Duration, Instant};

use crate::{Tileception, cursor::Cursor};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay, context::ContextPriority},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, ImportMemWl, gles::GlesRenderer},
        session::{
            Event as SessionEvent, Session,
            libseat::{LibSeatSession, LibSeatSessionNotifier},
        },
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::{
            Dispatcher, EventLoop, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::{
            Device as _,
            control::{ModeTypeFlags, connector, crtc},
        },
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::backend::GlobalId,
    },
    utils::DeviceFd,
    wayland::{
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        drm_syncobj::{DrmSyncobjState, supports_syncobj_eventfd},
    },
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;
type OutputManager = DrmOutputManager<Allocator, Exporter, (), DrmDeviceFd>;
type Surface = DrmOutput<Allocator, Exporter, (), DrmDeviceFd>;

const COLOR_FORMATS: &[Fourcc] = &[Fourcc::Argb8888, Fourcc::Abgr8888];

pub struct Udev {
    pub session: LibSeatSession,
    pub renderer: GlesRenderer,
    pub cursor: Cursor,
    node: DrmNode,
    manager: OutputManager,
    scanner: DrmScanner,
    /// The monitor Tileception drives.
    surface: Option<Lit>,
    _drm_token: RegistrationToken,
    render_pending: bool,
    frame_pending: bool,
}

struct Lit {
    crtc: crtc::Handle,
    output: Output,
    drm_output: Surface,
    global: GlobalId,
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
    let (mut session, notifier) = LibSeatSession::new().map_err(|e| format!("no seat session: {e}"))?;
    let notifier = wait_until_active(&session, notifier)?;
    let seat = session.seat();

    let gpu_path = primary_gpu(&seat)?.or_else(|| all_gpus(&seat).ok()?.into_iter().next()).ok_or("no GPU found")?;
    let node = DrmNode::from_path(&gpu_path)?;
    let primary = node.node_with_type(NodeType::Primary).and_then(|n| n.ok()).unwrap_or(node);
    let card = primary.dev_path().unwrap_or(gpu_path);
    tracing::info!("using GPU {}", card.display());

    let fd = session.open(&card, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;

    // SAFETY: This GBM device is owned by Smithay and has no externally managed EGL display.
    #[allow(unsafe_code, reason = "Smithay requires an explicit opt-in to initialize a native EGL display")]
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = EGLContext::new_with_priority(&egl, ContextPriority::High)?;
    // SAFETY: The newly created context belongs exclusively to this event-loop thread.
    #[allow(unsafe_code, reason = "Smithay requires the caller to guarantee exclusive EGL context ownership")]
    let renderer = unsafe { GlesRenderer::new(context)? };
    state.shm_state.update_formats(renderer.shm_formats());

    // Clients render into dmabufs; announce what the GPU can import.
    let formats = renderer.dmabuf_formats();
    let feedback = DmabufFeedbackBuilder::new(primary.dev_id(), formats).build()?;
    let mut dmabuf_state = DmabufState::new();
    let global = dmabuf_state.create_global_with_default_feedback::<Tileception>(&state.display_handle, &feedback);
    state.dmabuf = Some((dmabuf_state, global));

    // Explicit sync, which NVIDIA needs to avoid flicker in GPU clients.
    if supports_syncobj_eventfd(&fd) {
        state.syncobj_state = Some(DrmSyncobjState::new::<Tileception>(&state.display_handle, fd.clone()));
        tracing::info!("explicit sync enabled");
    }

    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(primary));
    let manager =
        DrmOutputManager::new(drm, allocator, exporter, Some(gbm), COLOR_FORMATS.iter().copied(), render_formats);

    let drm_token = event_loop.handle().insert_source(drm_notifier, |event, _, state| match event {
        DrmEvent::VBlank(crtc) => state.on_vblank(crtc),
        DrmEvent::Error(err) => tracing::error!("drm: {err:?}"),
    })?;

    state.udev = Some(Box::new(Udev {
        session: session.clone(),
        renderer,
        cursor: Cursor::load(state.scale),
        node: primary,
        manager,
        scanner: DrmScanner::new(),
        surface: None,
        _drm_token: drm_token,
        render_pending: false,
        frame_pending: false,
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
                udev.manager.pause();
                udev.frame_pending = false;
                udev.render_pending = false;
            }
        }
        SessionEvent::ActivateSession => {
            tracing::info!("session resumed");
            if libinput.resume().is_err() {
                tracing::error!("libinput could not resume");
            }
            if let Some(udev) = state.udev.as_mut() {
                udev.frame_pending = false;
                udev.render_pending = false;
                if let Err(err) = udev.manager.activate(false) {
                    tracing::error!("drm could not resume: {err}");
                }
                if let Some(lit) = &udev.surface {
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
            state.idle_activity();
            state.schedule_render();
        }
    })?;

    // Monitors coming and going.
    let udev_backend = UdevBackend::new(&seat)?;
    event_loop.handle().insert_source(udev_backend, |event, _, state| match event {
        UdevEvent::Changed { device_id } | UdevEvent::Added { device_id, .. } => {
            if state.udev.as_ref().is_some_and(|u| u.node.dev_id() == device_id) {
                state.scan_connectors();
            }
        }
        UdevEvent::Removed { .. } => {}
    })?;

    state.scan_connectors();
    Ok(())
}

impl Tileception {
    pub fn set_output_sleep(&mut self, asleep: bool) -> bool {
        let Some(udev) = self.udev.as_mut() else { return self.headless.is_some() };
        if !udev.session.is_active() {
            return false;
        }
        let Some(lit) = &udev.surface else { return false };
        if asleep {
            if let Err(error) = lit.drm_output.with_compositor(|compositor| compositor.clear()) {
                tracing::warn!("could not power down the display: {error}");
                return false;
            }
        } else {
            lit.drm_output.reset_buffers();
        }
        udev.frame_pending = false;
        true
    }

    fn scan_connectors(&mut self) {
        let Some(mut udev) = self.udev.take() else { return };
        let events = match udev.scanner.scan_connectors(udev.manager.device()) {
            Ok(events) => events.into_iter().collect::<Vec<_>>(),
            Err(err) => {
                tracing::warn!("connector scan failed: {err}");
                Vec::new()
            }
        };
        for event in events {
            match event {
                DrmScanEvent::Connected { connector, crtc: Some(crtc) } => {
                    if udev.surface.is_none() {
                        self.light(&mut udev, connector, crtc);
                    } else {
                        tracing::info!(
                            "ignoring extra monitor {:?}-{}",
                            connector.interface(),
                            connector.interface_id()
                        );
                    }
                }
                DrmScanEvent::Disconnected { crtc: Some(crtc), .. }
                    if udev.surface.as_ref().is_some_and(|s| s.crtc == crtc) =>
                {
                    if let Some(lit) = udev.surface.take() {
                        self.display_handle.remove_global::<Tileception>(lit.global);
                    }
                    self.output = None;
                }
                _ => {}
            }
        }
        self.udev = Some(udev);
        self.schedule_render();
    }

    fn light(&mut self, udev: &mut Udev, connector: connector::Info, crtc: crtc::Handle) {
        let name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
        let Some(mode) = connector
            .modes()
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or(connector.modes().first())
            .copied()
        else {
            return;
        };
        let wl_mode = WlMode::from(mode);
        let (w, h) = connector.size().unwrap_or((0, 0));
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: (w as i32, h as i32).into(),
                subpixel: connector.subpixel().into(),
                make: "Unknown".into(),
                model: "Unknown".into(),
            },
        );
        let global = output.create_global::<Tileception>(&self.display_handle);
        output.set_preferred(wl_mode);
        output.change_current_state(Some(wl_mode), None, None, Some((0, 0).into()));

        let mut planes = match udev.manager.device().planes(&crtc) {
            Ok(planes) => planes,
            Err(err) => {
                tracing::warn!("no planes for {name}: {err}");
                return;
            }
        };
        // Overlay planes misbehave on NVIDIA.
        let nvidia = udev
            .manager
            .device()
            .get_driver()
            .map(|d| d.name().to_string_lossy().to_lowercase().contains("nvidia"))
            .unwrap_or(false);
        if nvidia {
            planes.overlay.clear();
        }

        let drm_output = match udev.manager.initialize_output::<_, crate::render::OutputElement>(
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
            &mut udev.renderer,
            &DrmOutputRenderElements::default(),
        ) {
            Ok(o) => o,
            Err(err) => {
                tracing::error!("could not light {name}: {err}");
                self.display_handle.remove_global::<Tileception>(global);
                return;
            }
        };
        let size = wl_mode.size;
        tracing::info!("lit {name} at {}x{}@{:.2}", size.w, size.h, wl_mode.refresh as f64 / 1000.0);
        self.output = Some(output.clone());
        udev.surface = Some(Lit { crtc, output, drm_output, global });
        self.apply_output_scale();
        let screen = self.output_rect();
        self.pointer = (screen.w / 2.0, screen.h / 2.0).into();
    }

    /// Asks for a frame soon, unless one is already on its way.
    pub fn schedule_render(&mut self) {
        let Some(udev) = self.udev.as_mut() else { return };
        if udev.render_pending || udev.frame_pending {
            return;
        }
        udev.render_pending = true;
        self.loop_handle.insert_idle(|state| state.render_udev());
    }

    fn on_vblank(&mut self, _crtc: crtc::Handle) {
        let Some(udev) = self.udev.as_mut() else { return };
        udev.frame_pending = false;
        if let Some(lit) = udev.surface.as_mut()
            && let Err(err) = lit.drm_output.frame_submitted()
        {
            tracing::warn!("frame submit: {err}");
        }
        self.render_udev();
    }

    fn render_udev(&mut self) {
        let Some(mut udev) = self.udev.take() else { return };
        udev.render_pending = false;
        if !udev.session.is_active() || udev.frame_pending || self.idle.off {
            self.udev = Some(udev);
            return;
        }
        self.process_captures(&mut udev.renderer);
        let now = self.now();
        let mut queued = false;
        if let Some(mut lit) = udev.surface.take() {
            let mut elements = self.cursor_elements(&mut udev);
            elements.extend(self.frame_elements(&mut udev.renderer, now));
            let clear = self.clear_colour();
            match lit.drm_output.render_frame(&mut udev.renderer, &elements, clear, FrameFlags::DEFAULT) {
                Ok(result) if !result.is_empty => match lit.drm_output.queue_frame(()) {
                    Ok(()) => queued = true,
                    Err(err) => tracing::warn!("queue frame: {err}"),
                },
                Ok(_) => {}
                Err(err) => tracing::warn!("render: {err:?}"),
            }
            let elapsed = self.start.elapsed();
            // Minimized windows are hidden: no frames, so they need not draw.
            for (_, window) in self.windows.iter().filter(|(id, _)| !self.world.is_minimized(**id)) {
                window.send_frame(&lit.output, elapsed, Some(Duration::ZERO), |_, _| Some(lit.output.clone()));
            }
            udev.surface = Some(lit);
        }
        self.send_layer_frames(self.start.elapsed());
        udev.frame_pending = queued;
        self.udev = Some(udev);
        self.after_frame();
        if !queued {
            // Nothing changed on screen: look again in about a frame.
            self.loop_handle
                .insert_source(Timer::from_duration(Duration::from_millis(16)), |_, _, state| {
                    state.schedule_render();
                    TimeoutAction::Drop
                })
                .ok();
        }
    }

    /// Opens a device for a VT switch, from the keyboard shortcut.
    pub fn change_vt(&mut self, vt: i32) {
        if let Some(udev) = self.udev.as_mut()
            && let Err(err) = udev.session.change_vt(vt)
        {
            tracing::warn!("could not switch to VT {vt}: {err}");
        }
    }
}
