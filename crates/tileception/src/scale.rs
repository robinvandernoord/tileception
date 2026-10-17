//! Output scaling: how many physical pixels one logical pixel takes.
//!
//! Everything in Tileception (the layout, the pointer, window sizes, the
//! interface) is in logical pixels. Clients are told the scale through
//! wl_surface's preferred buffer scale and wp-fractional-scale, and draw
//! sharp buffers for it; the compositor converts to physical pixels only
//! where it makes render elements, and rasterises its own text and icons at
//! the physical size. Xwayland clients draw at scale 1 and are enlarged.

use serde::Deserialize;
use smithay::{
    backend::renderer::utils::RendererSurfaceStateUserData,
    delegate_fractional_scale, delegate_viewporter,
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::Transform,
    wayland::{
        compositor::{SurfaceData, TraversalAction, send_surface_state, with_states, with_surface_tree_downward},
        fractional_scale::{FractionalScaleHandler, with_fractional_scale},
    },
};

use crate::Tileception;

/// One screen's settings, under `[outputs."<connector>"]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub scale: ScaleSetting,
}

/// "auto", or a fixed scale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ScaleSetting {
    #[default]
    #[serde(deserialize_with = "auto")]
    Auto,
    Fixed(f64),
}

fn auto<'de, D: serde::Deserializer<'de>>(d: D) -> Result<(), D::Error> {
    let word = String::deserialize(d)?;
    if word == "auto" {
        Ok(())
    } else {
        Err(serde::de::Error::custom(format!("expected \"auto\" or a number, not {word:?}")))
    }
}

/// The smallest and largest scale accepted.
const MIN_SCALE: f64 = 0.5;
const MAX_SCALE: f64 = 4.0;

/// Checks `[outputs]`: scales must be numbers in range.
pub fn validate(outputs: &std::collections::BTreeMap<String, OutputConfig>) -> Result<(), String> {
    for (name, output) in outputs {
        if let ScaleSetting::Fixed(scale) = output.scale
            && !(MIN_SCALE..=MAX_SCALE).contains(&scale)
        {
            return Err(format!("[outputs.{name:?}] scale must be from {MIN_SCALE} to {MAX_SCALE}, not {scale}"));
        }
    }
    Ok(())
}

/// A scale the fractional-scale protocol can say exactly (it counts in
/// 120ths), within range.
pub fn tidy(scale: f64) -> f64 {
    ((scale.clamp(MIN_SCALE, MAX_SCALE) * 120.0).round() / 120.0).clamp(MIN_SCALE, MAX_SCALE)
}

/// A scale for a screen of `size_mm` showing `mode` pixels: about 135 pixels
/// per inch on laptop-sized panels and 110 on monitors (read from further
/// away), in steps of a quarter, from 1 to 3, keeping at least 1024x640
/// logical pixels. It rounds down unless the next step is close, so common
/// 1080p laptops stay at 1. Screens that report no size, or a size that is only an
/// aspect ratio (projectors, some TVs), get 1.
pub fn guess(size_mm: (i32, i32), mode: (i32, i32)) -> f64 {
    let (wmm, hmm) = (f64::from(size_mm.0), f64::from(size_mm.1));
    let (w, h) = (f64::from(mode.0), f64::from(mode.1));
    if wmm <= 0.0 || hmm <= 0.0 || w <= 0.0 || h <= 0.0 {
        return 1.0;
    }
    let inches = wmm.hypot(hmm) / 25.4;
    // 16x9 or 160x90: an aspect ratio, not a size. Under 8 inches is not a
    // screen anyone runs a desktop on.
    if !(8.0..=120.0).contains(&inches) {
        return 1.0;
    }
    let dpi = w.hypot(h) / inches;
    let target = if inches < 20.0 { 135.0 } else { 110.0 };
    let mut scale = ((dpi / target) * 4.0 + 0.3).floor() / 4.0;
    scale = scale.clamp(1.0, 3.0);
    while scale > 1.0 && (w / scale < 1024.0 || h / scale < 640.0) {
        scale -= 0.25;
    }
    scale
}

impl Tileception {
    /// The scale an output should have: `TILECEPTION_SCALE`, else its
    /// `[outputs]` entry, else `[outputs."*"]`, else a guess from its size.
    pub fn wanted_scale(&self, output: &Output) -> f64 {
        if let Some(scale) = std::env::var("TILECEPTION_SCALE").ok().and_then(|s| s.trim().parse::<f64>().ok()) {
            return tidy(scale);
        }
        let outputs = &self.config.file.outputs;
        let setting = outputs.get(&output.name()).or_else(|| outputs.get("*")).map(|o| o.scale).unwrap_or_default();
        match setting {
            ScaleSetting::Fixed(scale) => tidy(scale),
            ScaleSetting::Auto => {
                let size = output.physical_properties().size;
                let mode = output.current_mode().map(|m| m.size).unwrap_or_default();
                guess((size.w, size.h), (mode.w, mode.h))
            }
        }
    }

    /// Gives the output its scale and lays everything out again at the new
    /// logical size. Call after lighting an output, resizing it and
    /// reloading the config.
    pub fn apply_output_scale(&mut self) {
        let Some(output) = self.output.clone() else {
            self.apply_layout();
            return;
        };
        let scale = self.wanted_scale(&output);
        if output.current_scale().fractional_scale() != scale {
            let wl_scale = if scale.fract() == 0.0 {
                smithay::output::Scale::Integer(scale as i32)
            } else {
                smithay::output::Scale::Fractional(scale)
            };
            output.change_current_state(None, None, Some(wl_scale), None);
        }
        let changed = self.scale != scale;
        self.scale = scale;
        if changed {
            tracing::info!("{} at scale {scale}", output.name());
            self.text.borrow_mut().set_scale(scale as f32);
            self.glyphs.set_scale(scale);
            self.launcher.icons.set_scale(scale);
            // Drawn for the old scale.
            self.wallpaper = None;
            self.wallpaper_failed = None;
        }
        let screen = self.output_rect();
        self.world.set_screen(screen);
        self.pointer.x = self.pointer.x.clamp(screen.x, screen.right() - 1.0);
        self.pointer.y = self.pointer.y.clamp(screen.y, screen.bottom() - 1.0);
        self.update_usable_area();
        self.resize_lock_surface();
        self.apply_layout();
        if changed {
            self.send_scale_everywhere();
        }
        self.schedule_render();
    }

    /// The output's size in physical pixels.
    pub fn output_pixels(&self) -> (i32, i32) {
        let size = self.output.as_ref().and_then(|o| o.current_mode()).map(|m| m.size).unwrap_or_default();
        if size.w > 0 && size.h > 0 {
            (size.w, size.h)
        } else {
            let r = self.world.screen();
            ((r.w * self.scale).round() as i32, (r.h * self.scale).round() as i32)
        }
    }

    /// Tells a surface and its subsurfaces about the output and its scale.
    pub fn send_scale(&self, surface: &WlSurface) {
        let output = self.output.clone();
        let scale = self.scale;
        with_surface_tree_downward(
            surface,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |surface, states, _| {
                // The surface's states are locked here, so `has_buffer`
                // (which locks them again) would deadlock.
                if let Some(output) = &output
                    && buffered(states)
                {
                    output.enter(surface);
                }
                send_scale_state(surface, states, scale);
            },
            |_, _, _| true,
        );
    }

    /// Every window, popup, layer and lock surface, after the scale changed.
    fn send_scale_everywhere(&self) {
        let mut roots: Vec<WlSurface> = Vec::new();
        for window in self.windows.values() {
            if let Some(toplevel) = window.toplevel() {
                roots.push(toplevel.wl_surface().clone());
                roots.extend(
                    smithay::desktop::PopupManager::popups_for_surface(toplevel.wl_surface())
                        .map(|(popup, _)| popup.wl_surface().clone()),
                );
            } else if let Some(surface) = window.x11_surface().and_then(|x| x.wl_surface()) {
                roots.push(surface);
            }
        }
        if let Some(output) = &self.output {
            let map = smithay::desktop::layer_map_for_output(output);
            roots.extend(map.layers().map(|layer| layer.wl_surface().clone()));
        }
        roots.extend(self.lock_surface().cloned());
        for surface in roots {
            self.send_scale(&surface);
        }
    }
}

/// Whether a surface has drawn something, so it may be told it is on the
/// output.
pub fn has_buffer(surface: &WlSurface) -> bool {
    with_states(surface, buffered)
}

/// `has_buffer`, given the surface's states.
fn buffered(states: &SurfaceData) -> bool {
    states.data_map.get::<RendererSurfaceStateUserData>().is_some_and(|s| s.lock().is_ok_and(|s| s.buffer().is_some()))
}

/// The preferred buffer scale (rounded up, for clients without fractional
/// scaling) and the fractional scale.
pub fn send_scale_state(surface: &WlSurface, states: &SurfaceData, scale: f64) {
    send_surface_state(surface, states, scale.ceil() as i32, Transform::Normal);
    with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
}

impl FractionalScaleHandler for Tileception {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self.scale;
        with_states(&surface, |states| send_scale_state(&surface, states, scale));
    }
}

delegate_fractional_scale!(Tileception);
delegate_viewporter!(Tileception);

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn guesses_from_size_and_resolution() {
        // 14" and 15.6" 1080p laptops and a 24" 1080p monitor: 1.
        assert_eq!(guess((310, 174), (1920, 1080)), 1.0);
        assert_eq!(guess((344, 194), (1920, 1080)), 1.0);
        assert_eq!(guess((531, 299), (1920, 1080)), 1.0);
        // A 15.6" 4K laptop: about 282 dpi.
        assert_eq!(guess((344, 194), (3840, 2160)), 2.0);
        // A 13.3" 2560x1600 laptop: about 227 dpi.
        assert_eq!(guess((286, 179), (2560, 1600)), 1.75);
        // A 27" 4K monitor: about 163 dpi.
        assert_eq!(guess((597, 336), (3840, 2160)), 1.5);
        // A 14" 2880x1800 panel.
        assert_eq!(guess((302, 189), (2880, 1800)), 1.75);
        // No size, or only an aspect ratio.
        assert_eq!(guess((0, 0), (3840, 2160)), 1.0);
        assert_eq!(guess((16, 9), (3840, 2160)), 1.0);
        assert_eq!(guess((160, 90), (3840, 2160)), 1.0);
        // Never so large that less than 1024x640 is left.
        assert_eq!(guess((120, 75), (1280, 800)), 1.0);
    }

    #[test]
    fn scales_are_tidied() {
        assert_eq!(tidy(1.5), 1.5);
        assert_eq!(tidy(1.333), 160.0 / 120.0);
        assert_eq!(tidy(9.0), MAX_SCALE);
        assert_eq!(tidy(0.1), MIN_SCALE);
    }

    #[test]
    fn config_takes_auto_or_a_number() {
        let parse = |text: &str| toml::from_str::<OutputConfig>(text).map(|o| o.scale);
        assert_eq!(parse("scale = \"auto\"").unwrap(), ScaleSetting::Auto);
        assert_eq!(parse("scale = 1.5").unwrap(), ScaleSetting::Fixed(1.5));
        assert_eq!(parse("scale = 2").unwrap(), ScaleSetting::Fixed(2.0));
        assert!(parse("scale = \"big\"").is_err());
        assert_eq!(parse("").unwrap(), ScaleSetting::Auto);
    }
}
