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
    state.advertise_output(&output);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.add_output(output.clone());
    if let Err(error) = state.configure_outputs() {
        tracing::error!("output configuration rejected; retaining nested display: {error}");
    }

    // Render elements are made in physical pixels, so the tracker works at
    // scale 1 whatever the output's scale.
    let mut damage_tracker = OutputDamageTracker::new(mode.size, 1.0, Transform::Flipped180);

    event_loop.handle().insert_source(winit, move |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            let mode = Mode { size, refresh: REFRESH };
            output.set_preferred(mode);
            output.change_current_state(Some(mode), None, None, None);
            damage_tracker = OutputDamageTracker::new(size, 1.0, Transform::Flipped180);
            state.apply_output_scale();
        }
        WinitEvent::Input(event) => state.process_input_event(event),
        WinitEvent::Redraw => {
            let size = backend.window_size();
            let damage = Rectangle::from_size(size);
            let now = state.now();
            state.process_special_captures(backend.renderer());
            state.process_streams(backend.renderer());
            state.process_window_copies(backend.renderer());
            let renderer = backend.renderer();
            let mut elements = state.drag_elements(renderer);
            let scene = state.frame_elements(renderer, now);
            state.process_captures(renderer, Some(&scene));
            elements.extend(scene);
            let feedback = {
                let Ok((renderer, mut framebuffer)) = backend.bind() else { return };
                let clear = state.clear_colour();
                match damage_tracker.render_output(renderer, &mut framebuffer, 0, &elements, clear) {
                    Ok(result) => state.presentation_feedback(Some(&result.states)),
                    Err(err) => {
                        tracing::warn!("render failed: {err:?}");
                        None
                    }
                }
            };
            if let Err(err) = backend.submit(Some(&[damage])) {
                tracing::warn!("submit failed: {err:?}");
            } else if let Some(feedback) = feedback {
                // The host desktop does not say when it showed the frame.
                crate::frame_timing::presented_now(feedback, crate::frame_timing::refresh_of(&output));
            }
            state.release_all_fifo();

            let elapsed = state.start.elapsed();
            // Minimized windows are hidden: no frames, so they need not draw.
            for (_, window) in state.windows.iter().filter(|(window_id, _)| !state.minimized_anywhere(**window_id)) {
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
