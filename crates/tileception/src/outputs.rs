//! Independent desktops per output. Input selects a desktop; rendering only
//! borrows its context, without changing the keyboard's active output.
//!
//! A screen can also be split into side-by-side areas, each with a world
//! (tree, camera, saved views, minimized windows) and selection of its own.
//! Programs still see one `wl_output`; everything else about the screen (bar,
//! wallpaper, layers, fullscreen) stays shared. The same swapping as between
//! outputs puts an area's world in `Tileception::world`.

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
    /// Split screens' areas, by connector name. Unsplit screens have none.
    areas: BTreeMap<String, Areas>,
}

/// The areas of one split screen.
struct Areas {
    /// Each area's share of the width, left to right, adding up to 1.
    shares: Vec<f64>,
    /// The area input and bindings go to while this screen is active.
    active: usize,
    /// The area whose world is in use: in `Tileception` while this screen
    /// is, otherwise in its parked desktop. Equal to `active` except inside
    /// `with_area`.
    context: usize,
    /// The other areas, `None` at `context`.
    parked: Vec<Option<Area>>,
    /// The room the areas share: the screen less what bars reserve.
    usable: Rect,
}

struct Area {
    world: World,
    selection: crate::selection::Selection,
}

/// One area of a screen; an unsplit screen is its own area 0.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaId {
    pub output: Output,
    pub index: usize,
}

/// Area `index` of `usable`, split by `shares`, on whole pixels.
fn slice(usable: Rect, shares: &[f64], index: usize) -> Rect {
    let edge = |upto: usize| (usable.x + usable.w * shares[..upto].iter().sum::<f64>()).round();
    let left = if index == 0 { usable.x } else { edge(index) };
    let right = if index + 1 >= shares.len() { usable.right() } else { edge(index + 1) };
    Rect::new(left, usable.y, (right - left).max(1.0), usable.h)
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

    /// Whether the context is where input goes: the active screen and, on a
    /// split screen, its active area.
    pub fn is_active_output(&self) -> bool {
        self.output == self.outputs.active && self.split().is_none_or(|areas| areas.context == areas.active)
    }

    pub fn output_named(&self, name: &str) -> Option<Output> {
        self.all_outputs().into_iter().find(|output| output.name() == name)
    }

    /// Adds a fresh desktop, retaining the current desktop on first connection.
    pub fn add_output(&mut self, output: Output) {
        if self.output.is_none() {
            self.output = Some(output.clone());
            self.outputs.active = Some(output);
            self.apply_split();
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
            self.with_output(&output, |state| {
                state.apply_split();
                state.apply_output_scale();
            });
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

    /// Runs an operation in an area's context, like `with_output`.
    pub fn with_area<T>(&mut self, area: &AreaId, operation: impl FnOnce(&mut Self) -> T) -> Option<T> {
        self.with_output(&area.output, |state| state.with_local_area(area.index, operation)).flatten()
    }

    /// Runs an operation with another area of the context screen in use.
    pub fn with_local_area<T>(&mut self, index: usize, operation: impl FnOnce(&mut Self) -> T) -> Option<T> {
        let output = self.output.clone()?;
        let previous = self.area_index();
        let active = self.split().map(|areas| areas.active);
        if !self.exchange_area(index) {
            return None;
        }
        let result = operation(self);
        self.with_output(&output, |state| {
            // Settle on an area the operation activated, else go back.
            let now_active = state.split().map(|areas| areas.active);
            state.exchange_area(if now_active == active { previous } else { now_active.unwrap_or(0) });
        });
        Some(result)
    }

    /// Puts area `index` of the context screen in use. Always restored by
    /// `with_local_area`, or by `activate_area`.
    fn exchange_area(&mut self, index: usize) -> bool {
        let Some(name) = self.output.as_ref().map(Output::name) else { return false };
        let Some(areas) = self.outputs.areas.get_mut(&name) else { return index == 0 };
        if index == areas.context {
            return true;
        }
        let Some(mut area) = areas.parked.get_mut(index).and_then(Option::take) else { return false };
        std::mem::swap(&mut self.world, &mut area.world);
        std::mem::swap(&mut self.selection, &mut area.selection);
        areas.parked[areas.context] = Some(area);
        areas.context = index;
        true
    }

    fn split(&self) -> Option<&Areas> {
        self.outputs.areas.get(&self.output.as_ref()?.name())
    }

    /// Whether something holds for the world of every area of the context screen.
    pub fn every_area(&self, test: impl Fn(&World) -> bool) -> bool {
        test(&self.world)
            && self.split().is_none_or(|areas| areas.parked.iter().flatten().all(|area| test(&area.world)))
    }

    /// How many areas the context screen has.
    pub fn area_count(&self) -> usize {
        self.split().map_or(1, |areas| areas.shares.len())
    }

    /// The area of the context screen in use.
    pub fn area_index(&self) -> usize {
        self.split().map_or(0, |areas| areas.context)
    }

    pub fn context_area(&self) -> Option<AreaId> {
        Some(AreaId { output: self.output.clone()?, index: self.area_index() })
    }

    /// The area a screen shows its bar and panels for, and where input goes
    /// when it is active.
    pub fn resting_area(&self, output: &Output) -> AreaId {
        let index = self.outputs.areas.get(&output.name()).map_or(0, |areas| areas.active);
        AreaId { output: output.clone(), index }
    }

    pub fn active_area(&self) -> Option<AreaId> {
        Some(self.resting_area(self.outputs.active.as_ref()?))
    }

    /// Every area of every screen, screens in connector order, areas from the left.
    pub fn all_areas(&self) -> Vec<AreaId> {
        self.all_outputs()
            .into_iter()
            .flat_map(|output| {
                let count = self.outputs.areas.get(&output.name()).map_or(1, |areas| areas.shares.len());
                (0..count).map(move |index| AreaId { output: output.clone(), index })
            })
            .collect()
    }

    /// Whether any screen is split.
    pub fn has_split(&self) -> bool {
        !self.outputs.areas.is_empty()
    }

    /// `DP-2` for an unsplit screen, `DP-2.1` for the leftmost area of a split one.
    pub fn area_name(&self, area: &AreaId) -> String {
        let name = area.output.name();
        if self.outputs.areas.contains_key(&name) { format!("{name}.{}", area.index + 1) } else { name }
    }

    /// An area by its name, or a screen's active area by the screen's name.
    pub fn area_named(&self, name: &str) -> Option<AreaId> {
        if let Some(output) = self.output_named(name) {
            return Some(self.resting_area(&output));
        }
        let (output, number) = name.rsplit_once('.')?;
        let output = self.output_named(output)?;
        let index = number.parse::<usize>().ok()?.checked_sub(1)?;
        let area = AreaId { output, index };
        self.all_areas().contains(&area).then_some(area)
    }

    /// The room all areas of the context screen share: the screen less bars.
    pub fn usable_rect(&self) -> Rect {
        self.split().map_or_else(|| self.world.screen(), |areas| areas.usable)
    }

    /// Where the context area may draw on a split screen: its part of the
    /// width, the whole height. `None` on an unsplit screen.
    pub fn area_clip(&self) -> Option<Rect> {
        let areas = self.split()?;
        Some(self.area_clip_of(areas, areas.context))
    }

    fn area_clip_of(&self, areas: &Areas, index: usize) -> Rect {
        let out = self.output_rect();
        let part = slice(areas.usable, &areas.shares, index);
        let left = if index == 0 { out.x } else { part.x };
        let right = if index + 1 >= areas.shares.len() { out.right() } else { part.right() };
        Rect::new(left, out.y, (right - left).max(1.0), out.h)
    }

    /// The area of the context screen under a horizontal position.
    pub fn area_at(&self, x_pos: f64) -> usize {
        let Some(areas) = self.split() else { return 0 };
        (0..areas.shares.len())
            .find(|index| x_pos < self.area_clip_of(areas, *index).right())
            .unwrap_or(areas.shares.len() - 1)
    }

    /// The area of the context screen the pointer is over, when it is not the
    /// active one. Hovering there does not activate it; a click does.
    fn hovered_area(&self) -> Option<usize> {
        let areas = self.split()?;
        let index = self.area_at(self.pointer.x);
        (index != areas.active && self.fullscreen.is_none()).then_some(index)
    }

    /// The client surface under a point, looked up in the area the pointer
    /// is over, so programs there get hover and scrolling without the area
    /// taking the keyboard.
    pub fn surface_under_in_area(&mut self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        match self.hovered_area() {
            Some(index) => self.with_local_area(index, |state| state.surface_under(pos)).flatten(),
            None => self.surface_under(pos),
        }
    }

    /// Activates the area under the pointer, for a press in its room (not
    /// on the bar or the strip below it, which the areas share).
    pub fn activate_area_under_pointer(&mut self) {
        let Some(index) = self.hovered_area() else { return };
        if !self.usable_rect().contains(tileception_core::Point::new(self.pointer.x, self.pointer.y)) {
            return;
        }
        if let Some(output) = self.output.clone() {
            self.activate_area(&AreaId { output, index });
        }
    }

    /// Lays each area of the context screen out in its part of `usable`.
    pub fn set_area_screens(&mut self, usable: Rect) {
        let Some(name) = self.output.as_ref().map(Output::name) else { return };
        let Some(shares) = self.outputs.areas.get_mut(&name).map(|areas| {
            areas.usable = usable;
            areas.shares.clone()
        }) else {
            if usable != self.world.screen() {
                self.world.set_screen(usable);
                self.apply_layout();
            }
            return;
        };
        for index in 0..shares.len() {
            let rect = slice(usable, &shares, index);
            self.with_local_area(index, |state| {
                if state.world.screen() != rect {
                    state.world.set_screen(rect);
                    state.apply_layout();
                }
            });
        }
    }

    /// Splits the context screen into the areas `[outputs]` asks for, or
    /// combines them. Areas that go are taken in by the last one kept, their
    /// trees side by side, and its camera shows the result. The caller lays
    /// the screen out again (`apply_output_scale`).
    fn apply_split(&mut self) {
        let Some(name) = self.output.as_ref().map(Output::name) else { return };
        let shares = self
            .config
            .file
            .outputs
            .get(&name)
            .and_then(|entry| entry.split.as_ref())
            .map_or_else(|| vec![1.0], crate::scale::SplitSetting::shares);
        let old = self.area_count();
        if old == shares.len() {
            if let Some(areas) = self.outputs.areas.get_mut(&name) {
                areas.shares = shares;
            }
            return;
        }
        tracing::info!("{name} in {} areas", shares.len());
        if shares.len() > old {
            let settings = self.config.world_settings();
            let screen = self.world.screen();
            let areas = self.outputs.areas.entry(name).or_insert_with(|| Areas {
                shares: vec![1.0],
                active: 0,
                context: 0,
                parked: vec![None],
                usable: screen,
            });
            while areas.parked.len() < shares.len() {
                let world = World::new(settings.clone(), screen);
                areas.parked.push(Some(Area { world, selection: Default::default() }));
            }
            areas.shares = shares;
            return;
        }
        let keep = shares.len() - 1;
        let (old_shares, active) =
            self.split().map(|areas| (areas.shares.clone(), areas.active)).expect("a split screen has areas");
        // The active area's focus keeps the keyboard.
        let focus = self.with_local_area(active, |state| state.world.focus()).flatten();
        self.exchange_area(keep);
        let now = self.now();
        let mut combined = old_shares.get(keep).copied().unwrap_or(1.0);
        for index in keep + 1..old {
            let Some(area) = self.outputs.areas.get_mut(&name).and_then(|areas| areas.parked.get_mut(index)?.take())
            else {
                continue;
            };
            let share = old_shares.get(index).copied().unwrap_or(1.0);
            self.world.absorb(area.world, (combined, share), now);
            self.selection.windows.extend(area.selection.windows);
            combined += share;
        }
        if let Some(window) = focus.filter(|window| self.world.contains_window(*window)) {
            self.world.focus_window(window, now);
        }
        let areas = self.outputs.areas.get_mut(&name).expect("a split screen has areas");
        areas.parked.truncate(shares.len());
        areas.active = active.min(keep);
        if shares.len() == 1 {
            self.outputs.areas.remove(&name);
        } else {
            areas.shares = shares;
            let active = areas.active;
            self.exchange_area(active);
        }
        self.selection.anchor = None;
    }

    /// Makes an area the one input goes to, activating its screen first.
    pub fn activate_area(&mut self, area: &AreaId) -> bool {
        if self.outputs.active.as_ref() != Some(&area.output) && !self.activate_output(&area.output) {
            return false;
        }
        if self.output.as_ref() != Some(&area.output) {
            return false;
        }
        let Some(areas) = self.outputs.areas.get_mut(&area.output.name()) else { return area.index == 0 };
        if areas.active == area.index {
            return true;
        }
        if area.index >= areas.shares.len() {
            return false;
        }
        areas.active = area.index;
        self.selection.anchor = None;
        self.resize_drag = None;
        self.gap_drag = None;
        self.pending_press = None;
        self.wheel_accum = (0.0, 0.0);
        // Deactivate the old area's window before changing worlds.
        self.apply_layout();
        self.exchange_area(area.index);
        self.apply_layout();
        self.force_keyboard_focus_sync();
        self.schedule_render();
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
                for index in 0..state.area_count() {
                    state.with_local_area(index, |state| state.world.settings = state.config.world_settings());
                }
                state.apply_split();
                state.wallpaper = None;
                state.wallpaper_failed = None;
                state.text.borrow_mut().clear();
                state.apply_output_scale();
            });
        }
        self.arrange_outputs();
    }

    pub fn output_for_window(&self, window: WindowId) -> Option<Output> {
        self.area_for_window(window).map(|area| area.output)
    }

    pub fn area_for_window(&self, window: WindowId) -> Option<AreaId> {
        let holds = |world: &World| world.contains_window(window) || world.is_minimized(window);
        let in_parked_area = |name: &str| {
            self.outputs.areas.get(name).and_then(|areas| {
                areas.parked.iter().position(|area| area.as_ref().is_some_and(|area| holds(&area.world)))
            })
        };
        let context = |name: &str| self.outputs.areas.get(name).map_or(0, |areas| areas.context);
        let mut desktops: Vec<(&Output, &World)> = self.output.iter().map(|output| (output, &self.world)).collect();
        desktops.extend(self.outputs.parked.values().map(|desktop| (&desktop.output, &desktop.world)));
        desktops.into_iter().find_map(|(output, world)| {
            let name = output.name();
            let index = if holds(world) { Some(context(&name)) } else { in_parked_area(&name) };
            index.map(|index| AreaId { output: output.clone(), index })
        })
    }

    /// The area showing a surface: its window's or its popup's parent's, else
    /// the area in use on the screen holding its layer or lock surface.
    pub fn area_for_surface(&self, surface: &WlSurface) -> Option<AreaId> {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        if let Some(area) = self.window_id(&root).and_then(|window| self.area_for_window(window)) {
            return Some(area);
        }
        if let Some(popup) = self.popups.find_popup(&root)
            && let Ok(parent) = smithay::desktop::find_popup_root_surface(&popup)
            && let Some(area) = self.window_id(&parent).and_then(|window| self.area_for_window(window))
        {
            return Some(area);
        }
        self.output_for_surface(&root).map(|output| {
            let index = self.outputs.areas.get(&output.name()).map_or(0, |areas| areas.context);
            AreaId { output, index }
        })
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

    /// Moves a window to a screen's active area, as `move_window_to_area`.
    pub fn move_window_to_output(&mut self, window: WindowId, target: &Output) -> Result<(), String> {
        let area = self.resting_area(target);
        self.move_window_to_area(window, &area, false)
    }

    /// Moves a window without closing its client connection. Floats retain
    /// their size, centred in the new area, or with `keep_float` their place
    /// when it stays on the same screen; minimized windows stay minimized.
    /// Fullscreen is left on moving to another screen.
    pub fn move_window_to_area(&mut self, window: WindowId, target: &AreaId, keep_float: bool) -> Result<(), String> {
        let source = self.area_for_window(window).ok_or("window has no output")?;
        if source == *target {
            return Ok(());
        }
        if !self.all_areas().contains(target) {
            return Err("unknown destination output".into());
        }
        let same_screen = source.output == target.output;
        let (float, minimized) = self
            .with_area(&source, |state| {
                let float = state.world.floating_geometry(window);
                let minimized = state.world.is_minimized(window);
                if !same_screen && state.fullscreen == Some(window) {
                    state.set_fullscreen(window, false);
                }
                state.selection.windows.remove(&window);
                state.world.remove_window(window, state.now());
                if !same_screen && let Some(window) = state.windows.get(&window) {
                    window.with_surfaces(|surface, _| source.output.leave(surface));
                }
                state.apply_layout();
                (float, minimized)
            })
            .ok_or("source output disappeared")?;
        self.with_area(target, |state| {
            let now = state.now();
            state.world.add_window(window, if float.is_some() { Placement::Float } else { Placement::Dock }, now);
            if let Some(rect) = float {
                let screen = state.world.screen();
                let rect = if keep_float && same_screen {
                    rect
                } else {
                    Rect::new(
                        screen.x + (screen.w - rect.w.min(screen.w)) / 2.0,
                        screen.y + (screen.h - rect.h.min(screen.h)) / 2.0,
                        rect.w.min(screen.w),
                        rect.h.min(screen.h),
                    )
                };
                state.world.set_float_rect(window, rect);
            }
            if minimized {
                state.world.minimize(window, now);
            }
            if !same_screen
                && let Some(surface) = state
                    .windows
                    .get(&window)
                    .and_then(|window| window.wl_surface())
                    .map(|surface| surface.into_owned())
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
            state.touch_cancel((state.now() * 1000.0) as u32);
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
        // Panels on it move to the active screen.
        if self.doctor.home.as_ref() == Some(output) {
            self.doctor.home = None;
        }
        if self.notifications.home.as_ref() == Some(output) {
            self.notifications.home = None;
        }
        let previous = self.outputs.active.clone();
        let Some(target) = self.all_outputs().into_iter().find(|candidate| candidate != output) else {
            self.output = None;
            self.outputs.active = None;
            // The world in use stays; the other areas' windows come into it.
            self.combine_areas_for_last_screen(output);
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
        self.outputs.areas.remove(&output.name());
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
        // A window dragged over another area of a split screen goes along.
        // Otherwise only a click activates an area, as focus follows clicks.
        if let Some(drag) = self.move_drag
            && self.fullscreen.is_none()
            && self.output.as_ref() == Some(&target)
        {
            let area = AreaId { output: target, index: self.area_at(position.x) };
            if self.active_area().as_ref() != Some(&area) {
                let _ = self.move_window_to_area(drag.window, &area, true);
                self.activate_area(&area);
                if let Some(drag) = self.move_drag.as_mut() {
                    drag.target = None;
                }
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

    /// One line per screen, or per area of a split screen: name, whether
    /// input goes there, size in pixels, logical position and scale.
    pub fn outputs_status(&self) -> String {
        let active = self.active_area();
        self.all_areas()
            .iter()
            .map(|area| {
                let output = &area.output;
                let mode = output.current_mode();
                let position = output.current_location();
                let scale = output.current_scale().fractional_scale();
                let (mut width, height) = (mode.map_or(0, |mode| mode.size.w), mode.map_or(0, |mode| mode.size.h));
                let mut left = position.x;
                if let Some(areas) = self.outputs.areas.get(&output.name()) {
                    let out = self.whole_output_rect(output);
                    let part = slice(areas.usable, &areas.shares, area.index);
                    let start = if area.index == 0 { out.x } else { part.x };
                    let end = if area.index + 1 >= areas.shares.len() { out.right() } else { part.right() };
                    left += start.round() as i32;
                    width = ((end - start) * scale).round() as i32;
                }
                format!(
                    "{}{} {}x{} at {},{} scale {}\n",
                    self.area_name(area),
                    if active.as_ref() == Some(area) { " active" } else { "" },
                    width,
                    height,
                    left,
                    position.y,
                    scale
                )
            })
            .collect()
    }

    /// A screen's whole rectangle in its own logical pixels.
    fn whole_output_rect(&self, output: &Output) -> Rect {
        let Some(mode) = output.current_mode() else { return self.output_rect() };
        let size = output
            .current_transform()
            .transform_size(mode.size.to_f64().to_logical(output.current_scale().fractional_scale()));
        Rect::new(0.0, 0.0, size.w, size.h)
    }

    /// When the last screen goes, its other areas join the one in use, so
    /// their windows are still there when a screen comes back.
    fn combine_areas_for_last_screen(&mut self, output: &Output) {
        let Some(mut areas) = self.outputs.areas.remove(&output.name()) else { return };
        let now = self.now();
        let mut combined = areas.shares.get(areas.context).copied().unwrap_or(1.0);
        for (index, area) in areas.parked.iter_mut().enumerate() {
            let Some(area) = area.take() else { continue };
            let share = areas.shares.get(index).copied().unwrap_or(1.0);
            self.world.absorb(area.world, (combined, share), now);
            combined += share;
        }
    }
}
