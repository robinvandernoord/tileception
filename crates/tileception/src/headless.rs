//! A backend without a screen: renders only when asked for a capture, on the
//! GPU's render node. For tests and scripted screenshots, alongside a running
//! session, without taking over any display.
//!
//! `TILECEPTION_HEADLESS_SIZE=1920x1080` sets the virtual output's size in
//! pixels; `TILECEPTION_SCALE=1.5` (or `[outputs."headless"]`) its scale.

use std::{os::fd::OwnedFd, time::Duration};

use smithay::{
    backend::{
        allocator::gbm::GbmDevice,
        drm::{DrmDeviceFd, DrmNode},
        egl::{EGLContext, EGLDisplay},
        renderer::{ImportDma, ImportMemWl, gles::GlesRenderer},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::{
        EventLoop,
        timer::{TimeoutAction, Timer},
    },
    utils::{DeviceFd, Transform},
    wayland::dmabuf::{DmabufFeedbackBuilder, DmabufState},
};

use crate::Tileception;

pub struct Headless {
    pub renderer: GlesRenderer,
    outputs: Vec<Output>,
}

pub fn init(
    event_loop: &mut EventLoop<'static, Tileception>,
    state: &mut Tileception,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("TILECEPTION_RENDER_NODE").map(std::path::PathBuf::from).or_else(|_| {
        std::fs::read_dir("/dev/dri")?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("renderD")))
            .ok_or_else(|| std::io::Error::other("no render node in /dev/dri"))
    })?;
    tracing::info!("headless on {}", path.display());
    let file = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
    let descriptor = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));
    let node = DrmNode::from_path(&path)?;
    let gbm = GbmDevice::new(descriptor)?;
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

    let (width, height) = std::env::var("TILECEPTION_HEADLESS_SIZE")
        .ok()
        .and_then(|size| {
            size.split_once('x').and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
        })
        .unwrap_or((1280, 800));
    // Multiple virtual screens exercise the same output/layout routing as DRM.
    // Keep the original name and size variable for existing scripts.
    let sizes = match std::env::var("TILECEPTION_HEADLESS_OUTPUTS") {
        Ok(value) => value
            .split(',')
            .map(|size| {
                let (width, height) = size.trim().split_once('x').ok_or("expected WIDTHxHEIGHT")?;
                let width: i32 = width.parse().map_err(|_| "invalid output width")?;
                let height: i32 = height.parse().map_err(|_| "invalid output height")?;
                if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
                    return Err("headless output dimensions must be 1..16384");
                }
                Ok((width, height))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Err(_) => vec![(width, height)],
    };
    if sizes.is_empty() || sizes.len() > 16 {
        return Err("expected between 1 and 16 headless outputs".into());
    }
    let mut outputs = Vec::new();
    for (index, (width, height)) in sizes.into_iter().enumerate() {
        let output = Output::new(
            if index == 0 { "headless".into() } else { format!("headless-{}", index + 1) },
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Tileception".into(),
                model: "Headless".into(),
            },
        );
        let _global = output.create_global::<Tileception>(&state.display_handle);
        let mode = Mode { size: (width, height).into(), refresh: 60_000 };
        output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
        output.set_preferred(mode);
        state.add_output(output.clone());
        outputs.push(output);
    }
    state.headless = Some(Box::new(Headless { renderer, outputs }));

    event_loop.handle().insert_source(Timer::from_duration(Duration::from_millis(16)), |_, _, state| {
        state.headless_tick();
        TimeoutAction::ToDuration(Duration::from_millis(16))
    })?;
    Ok(())
}

impl Tileception {
    fn headless_tick(&mut self) {
        let Some(mut headless) = self.headless.take() else { return };
        let elapsed = self.start.elapsed();
        for output in &headless.outputs {
            self.with_output(output, |state| {
                state.update_bar();
                state.process_captures(&mut headless.renderer);
                for (_, window) in
                    state.windows.iter().filter(|(window_id, _)| state.world.contains_window(**window_id))
                {
                    window.send_frame(output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
                }
                state.send_layer_frames(elapsed);
            });
        }
        self.headless = Some(headless);
        self.after_frame();
    }
}
