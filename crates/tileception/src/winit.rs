//! Runs the compositor nested in a window of the current session.

use std::time::Duration;

use crate::Tileception;
use smithay::{
    backend::{
        renderer::damage::OutputDamageTracker,
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};

const REFRESH: i32 = 60_000;

pub fn init(
    event_loop: &mut EventLoop<'static, Tileception>,
    state: &mut Tileception,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init()?;

    let mode = Mode { size: backend.window_size(), refresh: REFRESH };
    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tileception".into(),
            model: "Winit".into(),
        },
    );
    let _global = output.create_global::<Tileception>(&state.display_handle);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.output = Some(output.clone());
    state.apply_output_scale();

    // Render elements are made in physical pixels, so the tracker works at
    // scale 1 whatever the output's scale.
    let mut damage_tracker = OutputDamageTracker::new(mode.size, 1.0, Transform::Flipped180);

    event_loop.handle().insert_source(winit, move |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            output.change_current_state(Some(Mode { size, refresh: REFRESH }), None, None, None);
            damage_tracker = OutputDamageTracker::new(size, 1.0, Transform::Flipped180);
            state.apply_output_scale();
        }
        WinitEvent::Input(event) => state.process_input_event(event),
        WinitEvent::Redraw => {
            let size = backend.window_size();
            let damage = Rectangle::from_size(size);
            let now = state.now();
            state.process_captures(backend.renderer());
            {
                let Ok((renderer, mut framebuffer)) = backend.bind() else { return };
                let elements = state.frame_elements(renderer, now);
                let clear = state.clear_colour();
                if let Err(err) = damage_tracker.render_output(renderer, &mut framebuffer, 0, &elements, clear) {
                    tracing::warn!("render failed: {err:?}");
                }
            }
            if let Err(err) = backend.submit(Some(&[damage])) {
                tracing::warn!("submit failed: {err:?}");
            }

            let elapsed = state.start.elapsed();
            // Minimized windows are hidden: no frames, so they need not draw.
            for (_, window) in state.windows.iter().filter(|(id, _)| !state.world.is_minimized(**id)) {
                window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
            }
            state.send_layer_frames(elapsed);
            state.after_frame();
            backend.window().request_redraw();
        }
        WinitEvent::CloseRequested => state.loop_signal.stop(),
        _ => {}
    })?;

    Ok(())
}
