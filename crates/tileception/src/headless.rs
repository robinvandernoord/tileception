//! A backend without a screen: renders only when asked for a capture, on the
//! GPU's render node. For tests and scripted screenshots, alongside a running
//! session, without taking over any display.
//!
//! `TILECEPTION_HEADLESS_SIZE=1920x1080` sets the virtual output's size.

use std::{os::fd::OwnedFd, time::Duration};

use smithay::{
    backend::{
        allocator::gbm::GbmDevice,
        drm::{DrmDeviceFd, DrmNode},
        egl::{EGLContext, EGLDisplay},
        renderer::{gles::GlesRenderer, ImportDma, ImportMemWl},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{
        timer::{TimeoutAction, Timer},
        EventLoop,
    },
    utils::{DeviceFd, Transform},
    wayland::dmabuf::{DmabufFeedbackBuilder, DmabufState},
};
use tileception_core::Rect;

use crate::Tileception;

pub struct Headless {
    pub renderer: GlesRenderer,
    output: Output,
}

pub fn init(
    event_loop: &mut EventLoop<'static, Tileception>,
    state: &mut Tileception,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("TILECEPTION_RENDER_NODE").map(std::path::PathBuf::from).or_else(|_| {
        std::fs::read_dir("/dev/dri")?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("renderD")))
            .ok_or_else(|| std::io::Error::other("no render node in /dev/dri"))
    })?;
    tracing::info!("headless on {}", path.display());
    let file = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
    let fd = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));
    let node = DrmNode::from_path(&path)?;
    let gbm = GbmDevice::new(fd)?;
    // SAFETY: This GBM device is owned by Smithay and has no externally managed EGL display.
    #[allow(unsafe_code, reason = "Smithay requires an explicit opt-in to initialize a native EGL display")]
    let egl = unsafe { EGLDisplay::new(gbm)? };
    // SAFETY: The newly created context belongs exclusively to this event-loop thread.
    #[allow(unsafe_code, reason = "Smithay requires the caller to guarantee exclusive EGL context ownership")]
    let renderer = unsafe { GlesRenderer::new(EGLContext::new(&egl)?)? };
    state.shm_state.update_formats(renderer.shm_formats());
    let feedback = DmabufFeedbackBuilder::new(node.dev_id(), renderer.dmabuf_formats()).build()?;
    let mut dmabuf_state = DmabufState::new();
    let global = dmabuf_state.create_global_with_default_feedback::<Tileception>(&state.display_handle, &feedback);
    state.dmabuf = Some((dmabuf_state, global));

    let (w, h) = std::env::var("TILECEPTION_HEADLESS_SIZE")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((1280, 800));
    let output = Output::new(
        "headless".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tileception".into(),
            model: "Headless".into(),
        },
    );
    let _global = output.create_global::<Tileception>(&state.display_handle);
    let mode = Mode { size: (w, h).into(), refresh: 60_000 };
    output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.world.set_screen(Rect::new(0.0, 0.0, w as f64, h as f64));
    state.output = Some(output.clone());
    state.headless = Some(Box::new(Headless { renderer, output }));

    event_loop.handle().insert_source(Timer::from_duration(Duration::from_millis(16)), |_, _, state| {
        state.headless_tick();
        TimeoutAction::ToDuration(Duration::from_millis(16))
    })?;
    Ok(())
}

impl Tileception {
    fn headless_tick(&mut self) {
        let Some(mut headless) = self.headless.take() else { return };
        self.process_captures(&mut headless.renderer);
        let elapsed = self.start.elapsed();
        for window in self.windows.values() {
            window.send_frame(&headless.output, elapsed, Some(Duration::ZERO), |_, _| Some(headless.output.clone()));
        }
        self.headless = Some(headless);
        self.send_layer_frames(elapsed);
        self.after_frame();
    }
}
