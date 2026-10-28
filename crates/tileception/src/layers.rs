//! wlr-layer-shell: bars, launchers, notifications and wallpapers.
//!
//! Layers live on the output, outside the tree. Bars that reserve an edge
//! (an exclusive zone) shrink the area the tree is laid out in. A layer that
//! asks for the keyboard exclusively, such as a launcher, gets it until it
//! goes away.

use smithay::{
    backend::renderer::{
        element::{AsRenderElements, surface::WaylandSurfaceRenderElement},
        gles::GlesRenderer,
    },
    delegate_layer_shell,
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Point, Scale},
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
            WlrLayerShellState,
        },
    },
};
use tileception_core::Rect;

use crate::{Tileception, render::OutputElement};

impl WlrLayerShellHandler for Tileception {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output.as_ref().and_then(Output::from_resource).or_else(|| self.output.clone());
        let Some(output) = output else {
            surface.send_close();
            return;
        };
        tracing::info!("layer surface {namespace:?}");
        let mapped = layer_map_for_output(&output).map_layer(&LayerSurface::new(surface, namespace));
        if let Err(err) = mapped {
            tracing::warn!("could not map layer surface: {err}");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let Some(output) = self.output.clone() else { return };
        let mut map = layer_map_for_output(&output);
        let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
        if let Some(layer) = layer {
            map.unmap_layer(&layer);
        }
        drop(map);
        if self.layer_focus.as_ref() == Some(surface.wl_surface()) {
            // Give the keyboard back to the focused window; without this it
            // stays on the closed launcher and typing goes nowhere.
            self.layer_focus = None;
            self.force_keyboard_focus_sync();
        }
        self.update_usable_area();
    }
}

delegate_layer_shell!(Tileception);

impl Tileception {
    /// Handles a commit if it belongs to a layer surface. Returns whether it did.
    pub fn commit_layer(&mut self, surface: &WlSurface) -> bool {
        let Some(output) = self.output.clone() else { return false };
        let mut map = layer_map_for_output(&output);
        let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned() else {
            return false;
        };
        map.arrange();
        let initial_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .and_then(|d| d.lock().ok().map(|d| d.initial_configure_sent))
                .unwrap_or(true)
        });
        if !initial_sent {
            layer.layer_surface().send_configure();
        }
        drop(map);

        let wants_keyboard = layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
            && matches!(layer.layer(), Layer::Top | Layer::Overlay);
        if wants_keyboard && self.layer_focus.as_ref() != Some(surface) {
            self.layer_focus = Some(surface.clone());
            self.force_keyboard_focus_sync();
        } else if !wants_keyboard && self.layer_focus.as_ref() == Some(surface) {
            self.layer_focus = None;
            self.force_keyboard_focus_sync();
        }
        self.update_usable_area();
        true
    }

    /// Lays the tree out in what the bars leave free.
    pub fn update_usable_area(&mut self) {
        let Some(output) = self.output.clone() else { return };
        let zone = {
            let mut map = layer_map_for_output(&output);
            map.arrange();
            map.non_exclusive_zone()
        };
        let usable = Rect::new(zone.loc.x as f64, zone.loc.y as f64, zone.size.w as f64, zone.size.h as f64);
        let usable = self.bar_reserve(usable);
        if usable.w > 0.0 && usable.h > 0.0 && usable != self.world.screen() {
            self.world.set_screen(usable);
            self.apply_layout();
        }
    }

    /// Layer surfaces of the given kinds, topmost first.
    pub fn layer_elements(&self, renderer: &mut GlesRenderer, layers: &[Layer]) -> Vec<OutputElement> {
        let Some(output) = self.output.as_ref() else { return Vec::new() };
        let map = layer_map_for_output(output);
        let mut out = Vec::new();
        for kind in layers {
            for layer in map.layers_on(*kind).rev() {
                let Some(geo) = map.layer_geometry(layer) else { continue };
                let origin = crate::render::physical_point((geo.loc.x as f64, geo.loc.y as f64), self.scale);
                let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    layer.render_elements(renderer, origin, Scale::from(1.0), 1.0);
                out.extend(crate::render::enlarged(elements, origin, self.scale));
            }
        }
        out
    }

    /// Whether an overlay layer surface (slurp, a launcher) takes the pointer
    /// here, ahead of Tileception's own panels.
    pub fn overlay_under(&self, p: tileception_core::Point) -> bool {
        self.layer_surface_under((p.x, p.y).into(), &[Layer::Overlay]).is_some()
    }

    /// The layer surface under a point, among the given kinds.
    pub fn layer_surface_under(
        &self,
        pos: Point<f64, Logical>,
        layers: &[Layer],
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.output.as_ref()?;
        let map = layer_map_for_output(output);
        for kind in layers {
            if let Some(layer) = map.layer_under(*kind, pos) {
                let geo = map.layer_geometry(layer)?;
                if let Some((s, loc)) = layer.surface_under(pos - geo.loc.to_f64(), WindowSurfaceType::ALL) {
                    return Some((s, (loc + geo.loc).to_f64()));
                }
            }
        }
        None
    }

    pub fn send_layer_frames(&self, time: std::time::Duration) {
        let Some(output) = self.output.as_ref() else { return };
        let map = layer_map_for_output(output);
        for layer in map.layers() {
            layer.send_frame(output, time, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
        }
        drop(map);
        // The lock screen and X11 menus draw their next frame only when told
        // the last one was shown. So does a client's cursor: Xwayland holds
        // back the next cursor image until the previous one's frame callback,
        // so without it X11 cursors stay frozen until something else redraws.
        let cursor = match &self.cursor_status {
            smithay::input::pointer::CursorImageStatus::Surface(surface) => Some(surface.clone()),
            _ => None,
        };
        let extra = self
            .lock_surface()
            .cloned()
            .into_iter()
            .chain(self.x11_overlays.iter().filter_map(|o| o.wl_surface()))
            .chain(cursor);
        for surface in extra {
            smithay::desktop::utils::send_frames_surface_tree(
                &surface,
                output,
                time,
                Some(std::time::Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
    }
}
