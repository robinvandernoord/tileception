//! Independent desktops per output. Input selects a desktop; rendering only
//! borrows its context, without changing the keyboard's active output.

use std::{collections::BTreeMap, path::PathBuf};

use smithay::{
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
    wayland::seat::WaylandFocus,
};
use tileception_core::{Placement, Rect, WindowId, World};

use crate::{Tileception, capture::PendingCapture};

#[derive(Default)]
pub struct Outputs {
    pub active: Option<Output>,
    parked: BTreeMap<String, Desktop>,
}

struct Desktop {
    output: Output,
    world: World,
    scale: f64,
    pointer: Point<f64, Logical>,
    fullscreen: Option<WindowId>,
    layer_focus: Option<WlSurface>,
    selection: crate::selection::Selection,
    strip: crate::minimized::Strip,
    bar: crate::bar::Bar,
    wallpaper: Option<crate::wallpaper::Wallpaper>,
    wallpaper_failed: Option<PathBuf>,
    captures: Vec<PendingCapture>,
    text: std::cell::RefCell<crate::text::Text>,
}

impl Tileception {
    pub fn all_outputs(&self) -> Vec<Output> {
        let mut outputs: Vec<_> = self.outputs.parked.values().map(|desktop| desktop.output.clone()).collect();
        outputs.extend(self.output.clone());
        outputs.sort_by_key(Output::name);
        outputs
    }

    pub fn is_active_output(&self) -> bool {
        self.output == self.outputs.active
    }

    pub fn output_named(&self, name: &str) -> Option<Output> {
        self.all_outputs().into_iter().find(|output| output.name() == name)
    }

    /// Adds a fresh desktop, retaining the current desktop on first connection.
    pub fn add_output(&mut self, output: Output) {
        if self.output.is_none() {
            self.output = Some(output.clone());
            self.outputs.active = Some(output);
            self.apply_output_scale();
        } else {
            let name = output.name();
            let world = World::new(self.config.world_settings(), Rect::new(0.0, 0.0, 1280.0, 800.0));
            self.outputs.parked.insert(
                name,
                Desktop {
                    output: output.clone(),
                    world,
                    scale: 1.0,
                    pointer: (0.0, 0.0).into(),
                    fullscreen: None,
                    layer_focus: None,
                    selection: Default::default(),
                    strip: Default::default(),
                    bar: Default::default(),
                    wallpaper: None,
                    wallpaper_failed: None,
                    captures: Vec::new(),
                    text: Default::default(),
                },
            );
            self.with_output(&output, |state| state.apply_output_scale());
        }
        self.arrange_outputs();
    }

    /// Switches the context used by existing layout/rendering code. This does
    /// not activate an output or route input there. Always restored by with_output.
    fn exchange_output(&mut self, output: &Output) -> bool {
        if self.output.as_ref() == Some(output) {
            return true;
        }
        let Some(current) = self.output.clone() else { return false };
        let Some(mut desktop) = self.outputs.parked.remove(&output.name()) else { return false };
        self.output = Some(desktop.output.clone());
        desktop.output = current.clone();
        macro_rules! exchange {
            ($($field:ident),+) => { $(std::mem::swap(&mut self.$field, &mut desktop.$field);)+ };
        }
        exchange!(world, scale, pointer, fullscreen, layer_focus, selection, strip, wallpaper, wallpaper_failed, text);
        self.bar.exchange_output(&mut desktop.bar);
        std::mem::swap(&mut self.pending_captures, &mut desktop.captures);
        self.outputs.parked.insert(current.name(), desktop);
        self.text.borrow_mut().set_scale(self.scale as f32);
        self.glyphs.set_scale(self.scale);
        self.launcher.icons.set_scale(self.scale);
        true
    }

    pub fn with_output<T>(&mut self, output: &Output, operation: impl FnOnce(&mut Self) -> T) -> Option<T> {
        let previous = self.output.clone()?;
        if !self.exchange_output(output) {
            return None;
        }
        let result = operation(self);
        self.exchange_output(&previous);
        Some(result)
    }

    pub fn activate_output(&mut self, output: &Output) -> bool {
        if self.outputs.active.as_ref() == Some(output) {
            return true;
        }
        if self.output_named(&output.name()).is_none() {
            return false;
        }
        self.close_launcher();
        self.power_menu = None;
        self.close_calendar();
        self.close_tray();
        self.selection.anchor = None;
        self.resize_drag = None;
        self.gap_drag = None;
        self.pending_press = None;
        self.wheel_accum = (0.0, 0.0);
        self.outputs.active = Some(output.clone());
        // Deactivate the old output's window before changing layout context.
        self.apply_layout();
        if !self.exchange_output(output) {
            return false;
        }
        self.apply_layout();
        self.force_keyboard_focus_sync();
        self.schedule_render();
        true
    }

    /// Connector-name order by default; explicit positions are logical pixels.
    pub fn arrange_outputs(&mut self) {
        let mut right = 0;
        for output in self.all_outputs() {
            let configured = self.config.file.outputs.get(&output.name()).and_then(|entry| entry.position);
            let position = configured.unwrap_or([right, 0]);
            output.change_current_state(None, None, None, Some((position[0], position[1]).into()));
            let width = output
                .current_mode()
                .map_or(0, |mode| (f64::from(mode.size.w) / output.current_scale().fractional_scale()).round() as i32);
            right = right.max(position[0].saturating_add(width));
        }
    }

    pub fn configure_outputs(&mut self) {
        for output in self.all_outputs() {
            self.with_output(&output, |state| {
                state.world.settings = state.config.world_settings();
                state.wallpaper = None;
                state.wallpaper_failed = None;
                state.text.borrow_mut().clear();
                state.apply_output_scale();
            });
        }
        self.arrange_outputs();
    }

    pub fn output_for_window(&self, window: WindowId) -> Option<Output> {
        if self.world.contains_window(window) || self.world.is_minimized(window) {
            return self.output.clone();
        }
        self.outputs
            .parked
            .values()
            .find(|desktop| desktop.world.contains_window(window) || desktop.world.is_minimized(window))
            .map(|desktop| desktop.output.clone())
    }

    pub fn output_for_surface(&self, surface: &WlSurface) -> Option<Output> {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        if let crate::lock::LockState::Locked { surfaces } = &self.lock
            && let Some((name, _)) = surfaces.iter().find(|(_, locked)| locked.wl_surface() == &root)
        {
            return self.output_named(name);
        }
        if let Some(output) = self.window_id(&root).and_then(|window| self.output_for_window(window)) {
            return Some(output);
        }
        if let Some(popup) = self.popups.find_popup(&root)
            && let Ok(parent) = smithay::desktop::find_popup_root_surface(&popup)
        {
            return self.window_id(&parent).and_then(|window| self.output_for_window(window));
        }
        self.all_outputs().into_iter().find(|output| {
            smithay::desktop::layer_map_for_output(output)
                .layer_for_surface(&root, smithay::desktop::WindowSurfaceType::ALL)
                .is_some()
        })
    }

    /// Moves a window without closing its client connection. Floats retain their
    /// size; minimized windows stay minimized. Fullscreen is left on transfer.
    pub fn move_window_to_output(&mut self, window: WindowId, target: &Output) -> Result<(), String> {
        let source = self.output_for_window(window).ok_or("window has no output")?;
        if source == *target {
            return Ok(());
        }
        if self.output_named(&target.name()).is_none() {
            return Err("unknown destination output".into());
        }
        let (float, minimized) = self
            .with_output(&source, |state| {
                let float = state.world.floating_geometry(window);
                let minimized = state.world.is_minimized(window);
                if state.fullscreen == Some(window) {
                    state.set_fullscreen(window, false);
                }
                state.selection.windows.remove(&window);
                state.world.remove_window(window, state.now());
                if let Some(window) = state.windows.get(&window) {
                    window.with_surfaces(|surface, _| source.leave(surface));
                }
                state.apply_layout();
                (float, minimized)
            })
            .ok_or("source output disappeared")?;
        self.with_output(target, |state| {
            let now = state.now();
            state.world.add_window(window, if float.is_some() { Placement::Float } else { Placement::Dock }, now);
            if let Some(rect) = float {
                let screen = state.world.screen();
                state.world.set_float_rect(
                    window,
                    Rect::new(
                        screen.x + (screen.w - rect.w.min(screen.w)) / 2.0,
                        screen.y + (screen.h - rect.h.min(screen.h)) / 2.0,
                        rect.w.min(screen.w),
                        rect.h.min(screen.h),
                    ),
                );
            }
            if minimized {
                state.world.minimize(window, now);
            }
            if let Some(surface) =
                state.windows.get(&window).and_then(|window| window.wl_surface()).map(|surface| surface.into_owned())
            {
                state.send_scale(&surface);
            }
            state.apply_layout();
        });
        self.schedule_render();
        Ok(())
    }

    pub fn remove_output(&mut self, output: &Output) {
        self.with_output(output, |state| {
            state.fail_captures();
            let mut map = smithay::desktop::layer_map_for_output(output);
            let layers = map.layers().cloned().collect::<Vec<_>>();
            for layer in layers {
                layer.layer_surface().send_close();
                map.unmap_layer(&layer);
            }
            state.layer_focus = None;
            for window in state.windows.values() {
                window.with_surfaces(|surface, _| output.leave(surface));
            }
        });
        let previous = self.outputs.active.clone();
        let Some(target) = self.all_outputs().into_iter().find(|candidate| candidate != output) else {
            self.output = None;
            self.outputs.active = None;
            self.move_drag = None;
            self.resize_drag = None;
            self.gap_drag = None;
            self.pending_press = None;
            self.layer_focus = None;
            self.force_keyboard_focus_sync();
            return;
        };
        let windows = self
            .windows
            .keys()
            .copied()
            .filter(|window| self.output_for_window(*window).as_ref() == Some(output))
            .collect::<Vec<_>>();
        for window in windows {
            let _ = self.move_window_to_output(window, &target);
        }
        self.move_drag = None;
        self.resize_drag = None;
        self.gap_drag = None;
        let next = previous.filter(|active| active != output).unwrap_or(target);
        self.activate_output(&next);
        self.outputs.parked.remove(&output.name());
        self.arrange_outputs();
        self.schedule_render();
    }

    /// Global desktop geometry is only used for crossing screens; each world's
    /// pointer and layout coordinates stay local to its output.
    pub fn desktop_motion(&mut self, position: Point<f64, Logical>) -> Point<f64, Logical> {
        let Some(current) = self.output.clone() else { return position };
        let location = current.current_location().to_f64();
        let global = position + location;
        let outputs = self.all_outputs();
        let mut nearest = None;
        for output in outputs {
            let origin = output.current_location().to_f64();
            let Some(mode) = output.current_mode() else { continue };
            let scale = output.current_scale().fractional_scale();
            let size = output.current_transform().transform_size(mode.size.to_f64().to_logical(scale));
            let local = global - origin;
            let clamped: Point<f64, Logical> =
                (local.x.clamp(0.0, (size.w - 1.0).max(0.0)), local.y.clamp(0.0, (size.h - 1.0).max(0.0))).into();
            let distance = (local.x - clamped.x).hypot(local.y - clamped.y);
            if nearest.as_ref().is_none_or(|(_, _, best)| distance < *best) {
                nearest = Some((output, clamped, distance));
            }
        }
        let Some((target, position, _)) = nearest else { return position };
        if target != current {
            if let Some(drag) = self.move_drag {
                let _ = self.move_window_to_output(drag.window, &target);
            }
            self.activate_output(&target);
            if let Some(drag) = self.move_drag.as_mut() {
                drag.target = None;
            }
        }
        position
    }

    pub fn desktop_bounds(&self) -> Rect {
        let mut bounds = None;
        for output in self.all_outputs() {
            let Some(mode) = output.current_mode() else { continue };
            let origin = output.current_location();
            let size = output
                .current_transform()
                .transform_size(mode.size.to_f64().to_logical(output.current_scale().fractional_scale()));
            let rect = Rect::new(f64::from(origin.x), f64::from(origin.y), size.w, size.h);
            bounds = Some(match bounds {
                None => rect,
                Some(previous) => {
                    let previous: Rect = previous;
                    let left = previous.x.min(rect.x);
                    let top = previous.y.min(rect.y);
                    Rect::new(
                        left,
                        top,
                        previous.right().max(rect.right()) - left,
                        previous.bottom().max(rect.bottom()) - top,
                    )
                }
            });
        }
        bounds.unwrap_or_else(|| self.output_rect())
    }

    pub fn outputs_status(&self) -> String {
        self.all_outputs()
            .iter()
            .map(|output| {
                let mode = output.current_mode();
                let position = output.current_location();
                format!(
                    "{}{} {}x{} at {},{} scale {}\n",
                    output.name(),
                    if self.outputs.active.as_ref() == Some(output) { " active" } else { "" },
                    mode.map_or(0, |mode| mode.size.w),
                    mode.map_or(0, |mode| mode.size.h),
                    position.x,
                    position.y,
                    output.current_scale().fractional_scale()
                )
            })
            .collect()
    }
}
