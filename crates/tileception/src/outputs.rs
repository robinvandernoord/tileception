//! Independent desktops per output. Input selects a desktop; rendering only
//! borrows its context, without changing the keyboard's active output.
//!
//! A screen can also be split into side-by-side areas, each with a world
//! (tree, camera, saved views, minimized windows) and selection of its own.
//! Programs still see one `wl_output`; everything else about the screen (bar,
//! wallpaper, layers, fullscreen) stays shared. The same swapping as between
//! outputs puts an area's world in `Tileception::world`.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use smithay::{
    output::{Mode, Output},
    reexports::calloop::timer::{TimeoutAction, Timer},
    reexports::wayland_server::{backend::GlobalId, protocol::wl_surface::WlSurface},
    utils::{Logical, Point, Transform},
    wayland::seat::WaylandFocus,
};
use tileception_core::{Placement, Rect, WindowId, World};

use crate::scale::OutputConfig;
use crate::{Tileception, capture::PendingCapture};

#[derive(Debug, Clone)]
pub struct ConnectedMonitor {
    pub name: String,
    pub enabled: bool,
    pub modes: Vec<Mode>,
    pub current_mode: Option<Mode>,
    pub preferred_mode: Option<Mode>,
    pub transform: Transform,
}

/// A monotonic deadline, shared by Settings and the backend's timeout source.
#[derive(Debug, Clone, Copy)]
pub struct OutputPreviewStatus {
    pub token: u64,
    pub deadline: Instant,
}

#[derive(Clone)]
struct RuntimeOutput {
    output: Output,
    enabled: bool,
    mode: Mode,
    transform: Transform,
}

struct OutputPreview {
    token: u64,
    previous: BTreeMap<String, OutputConfig>,
    runtime: Vec<RuntimeOutput>,
    deadline: Instant,
    homes: Vec<(WindowId, AreaId)>,
    active: Option<AreaId>,
    desktops: Vec<PreviewDesktop>,
}

struct PreviewDesktop {
    area: AreaId,
    world: World,
    pointer: Point<f64, Logical>,
    fullscreen: Option<WindowId>,
}

#[derive(Default)]
pub struct Outputs {
    connected: BTreeMap<String, Output>,
    globals: BTreeMap<String, GlobalId>,
    preview: Option<OutputPreview>,
    generation: u64,
    pub active: Option<Output>,
    parked: BTreeMap<String, Desktop>,
    /// Split screens' areas, by connector name. Unsplit screens have none.
    areas: BTreeMap<String, Areas>,
    /// Dragging the gap between areas `.0` and `.0 + 1` of the active
    /// screen, last at horizontal position `.1`.
    pub boundary_drag: Option<(usize, f64)>,
    /// The areas of the last screen, from when it went, so they come back
    /// with it.
    remembered: Option<Remembered>,
}

/// A split screen's areas while no screen is there: their windows wait in
/// one world, and this says where each goes back.
struct Remembered {
    name: String,
    active: usize,
    areas: Vec<RememberedArea>,
}

struct RememberedArea {
    layout: tileception_core::world::LayoutSnapshot,
    minimized: Vec<WindowId>,
    focus: Option<WindowId>,
}

/// The areas of one split screen.
struct Areas {
    /// Each area's share of the width, left to right, adding up to 1.
    shares: Vec<f64>,
    /// The shares `[outputs]` asks for. Dragging changes only `shares`, for
    /// this session; a reload resets them only when this setting changes.
    configured: Vec<f64>,
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

/// The narrowest an area may be dragged, as a share of the screen.
const MIN_SHARE: f64 = 0.1;

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

/// `shares` with the boundary right of area `index` moved by `delta` (a
/// share of the width), each of the two areas kept at least `MIN_SHARE`
/// wide, or as wide as it is when narrower. `None` when nothing moves.
fn moved_boundary(shares: &[f64], index: usize, delta: f64) -> Option<Vec<f64>> {
    let (left, right) = (*shares.get(index)?, *shares.get(index + 1)?);
    let pair = left + right;
    // Areas configured narrower than the minimum keep their width as the floor.
    let low = MIN_SHARE.min(left);
    let high = (pair - MIN_SHARE.min(right)).max(low);
    let moved = (left + delta).clamp(low, high);
    if (moved - left).abs() < 1e-6 {
        return None;
    }
    let mut shares = shares.to_vec();
    shares[index] = moved;
    shares[index + 1] = pair - moved;
    Some(shares)
}

/// Area `index` of a split screen `out`, the whole height: its slice of the
/// usable room, the outer areas reaching the screen's edges.
fn area_room(out: Rect, areas: &Areas, index: usize) -> Rect {
    let part = slice(areas.usable, &areas.shares, index);
    let left = if index == 0 { out.x } else { part.x };
    let right = if index + 1 >= areas.shares.len() { out.right() } else { part.right() };
    Rect::new(left, out.y, (right - left).max(1.0), out.h)
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
    /// Includes connected but disabled monitors. Headless accepts custom modes
    /// within the same bounds as configuration validation.
    pub fn connected_monitors(&self) -> Vec<ConnectedMonitor> {
        self.outputs
            .connected
            .values()
            .map(|output| ConnectedMonitor {
                name: output.name(),
                enabled: self.output_named(&output.name()).is_some(),
                modes: output.modes(),
                current_mode: output.current_mode(),
                preferred_mode: output.preferred_mode(),
                transform: output.current_transform(),
            })
            .collect()
    }

    fn runtime_outputs(&self) -> Vec<RuntimeOutput> {
        self.outputs
            .connected
            .values()
            .filter_map(|output| {
                Some(RuntimeOutput {
                    output: output.clone(),
                    enabled: self.output_named(&output.name()).is_some(),
                    mode: output.current_mode()?,
                    transform: output.current_transform(),
                })
            })
            .collect()
    }

    fn proposed_outputs(
        &self,
        proposed: &BTreeMap<String, OutputConfig>,
        allow_fallback: bool,
    ) -> Result<Vec<RuntimeOutput>, String> {
        crate::scale::validate(proposed)?;
        let mut runtime = Vec::new();
        for output in self.outputs.connected.values() {
            let explicit = proposed.get(&output.name());
            let mut setting = explicit.or_else(|| proposed.get("*")).cloned().unwrap_or_default();
            if output.name() == "winit" && explicit.is_none() {
                if setting.mode.parse()?.is_some() || setting.transform != Default::default() {
                    tracing::debug!("ignoring wildcard mode and transform for nested output");
                }
                setting.mode = Default::default();
                setting.transform = Default::default();
            }
            let requested = setting.mode.parse()?;
            let mode = requested
                .or(output.preferred_mode())
                .or(output.current_mode())
                .ok_or_else(|| format!("{} has no available mode", output.name()))?;
            if output.name() == "winit" {
                if !setting.enabled || requested.is_some() || setting.transform != Default::default() {
                    return Err("nested backend does not support output mode, transform or enable changes".into());
                }
            } else if !output.name().starts_with("headless") && !output.modes().contains(&mode) {
                return Err(format!(
                    "{} does not support {}x{}@{}",
                    output.name(),
                    mode.size.w,
                    mode.size.h,
                    f64::from(mode.refresh) / 1000.0
                ));
            }
            runtime.push(RuntimeOutput {
                output: output.clone(),
                enabled: setting.enabled,
                mode,
                transform: if output.name() == "winit" { output.current_transform() } else { setting.transform.into() },
            });
        }
        if !runtime.is_empty() && !runtime.iter().any(|output| output.enabled) {
            if !allow_fallback {
                return Err("cannot disable every connected monitor".into());
            }
            let index = runtime
                .iter()
                .position(|target| ["eDP", "LVDS", "DSI"].iter().any(|prefix| target.output.name().starts_with(prefix)))
                .unwrap_or(0);
            runtime[index].enabled = true;
            tracing::warn!(output = %runtime[index].output.name(), "configuration disables every connected monitor; enabling fallback");
        }
        Ok(runtime)
    }

    fn apply_runtime_outputs(&mut self, runtime: &[RuntimeOutput]) -> Result<(), String> {
        // Configure and enable destinations before moving windows off disabled screens.
        for target in runtime.iter().filter(|target| target.enabled) {
            self.configure_drm_output(&target.output, target.mode, target.transform, true)?;
            target.output.change_current_state(Some(target.mode), Some(target.transform), None, None);
            if self.output_named(&target.output.name()).is_none() {
                self.add_output(target.output.clone());
                self.advertise_output(&target.output);
            }
        }
        for target in runtime.iter().filter(|target| !target.enabled) {
            self.configure_drm_output(&target.output, target.mode, target.transform, false)?;
            if self.output_named(&target.output.name()).is_some() {
                self.remove_output(&target.output);
                self.withdraw_output(&target.output);
            }
            target.output.change_current_state(Some(target.mode), Some(target.transform), None, None);
        }
        self.configure_output_layouts();
        self.schedule_render();
        Ok(())
    }

    /// Applies only at runtime. Confirmation returns the map to persist.
    pub fn preview_outputs(&mut self, proposed: BTreeMap<String, OutputConfig>) -> Result<u64, String> {
        if self.outputs.preview.is_some() {
            return Err("an output preview is already awaiting confirmation".into());
        }
        let runtime = self.proposed_outputs(&proposed, false)?;
        if runtime.is_empty() {
            return Err("no connected monitors".into());
        }
        let previous_runtime = self.runtime_outputs();
        let homes: Vec<_> =
            self.windows.keys().filter_map(|window| Some((*window, self.area_for_window(*window)?))).collect();
        let active = self.active_area();
        let desktops: Vec<_> = self
            .all_areas()
            .into_iter()
            .filter_map(|area| {
                self.with_area(&area, |state| PreviewDesktop {
                    area: area.clone(),
                    world: state.world.clone(),
                    pointer: state.pointer,
                    fullscreen: state.fullscreen,
                })
            })
            .collect();
        let focused = active.as_ref().and_then(|active| {
            desktops.iter().find(|saved| &saved.area == active).and_then(|saved| saved.world.focus())
        });
        let previous = std::mem::replace(&mut self.config.file.outputs, proposed);
        if let Err(error) = self.apply_runtime_outputs(&runtime) {
            self.config.file.outputs = previous;
            return match self.apply_runtime_outputs(&previous_runtime) {
                Ok(()) => {
                    self.restore_output_homes(&homes);
                    self.restore_preview_desktops(desktops, active);
                    Err(error)
                }
                Err(rollback) => Err(format!("{error}; restoring displays also failed: {rollback}")),
            };
        }
        // Moving windows off a disabled screen must not steal the preview
        // caller's keyboard focus, including when its own screen was disabled.
        if let Some(focused) = focused
            && let Some(area) = self.area_for_window(focused)
            && self.activate_area(&area)
        {
            self.world.focus_window(focused, self.now());
            self.apply_layout();
            self.force_keyboard_focus_sync();
        }
        self.outputs.generation = self.outputs.generation.wrapping_add(1);
        let token = self.outputs.generation;
        self.outputs.preview = Some(OutputPreview {
            token,
            previous,
            runtime: previous_runtime,
            deadline: Instant::now() + Duration::from_secs(15),
            homes,
            active,
            desktops,
        });
        if let Err(error) =
            self.loop_handle.insert_source(Timer::from_duration(Duration::from_secs(15)), move |_, _, state| {
                if state.outputs.preview.as_ref().is_some_and(|preview| preview.token == token)
                    && let Err(error) = state.revert_outputs(token)
                {
                    tracing::error!("output preview timeout recovery: {error}");
                }
                TimeoutAction::Drop
            })
        {
            self.revert_outputs(token)?;
            return Err(format!("could not schedule output preview timeout: {error}"));
        }
        Ok(token)
    }

    /// None also means the deadline passed, even before calloop dispatches its timer.
    pub fn output_preview_status(&self) -> Option<OutputPreviewStatus> {
        self.outputs
            .preview
            .as_ref()
            .filter(|preview| Instant::now() < preview.deadline)
            .map(|preview| OutputPreviewStatus { token: preview.token, deadline: preview.deadline })
    }

    fn check_output_preview(&mut self, token: u64) -> Result<(), String> {
        if self.outputs.preview.as_ref().is_none_or(|preview| preview.token != token) {
            return Err("output preview expired or was superseded".into());
        }
        if self.outputs.preview.as_ref().is_some_and(|preview| Instant::now() >= preview.deadline) {
            self.revert_outputs(token)?;
            return Err("output preview expired".into());
        }
        Ok(())
    }

    /// Call only after the user confirms. `persist` must write atomically: on
    /// error it must leave the previous file intact. The callback runs once,
    /// after checking the deadline, with no intervening event-loop dispatch.
    /// Failed persistence restores runtime, config and window placement.
    pub fn confirm_outputs_with_persistence(
        &mut self,
        token: u64,
        persist: impl FnOnce(&BTreeMap<String, OutputConfig>) -> Result<(), String>,
    ) -> Result<BTreeMap<String, OutputConfig>, String> {
        self.check_output_preview(token)?;
        let confirmed = self.config.file.outputs.clone();
        if let Err(error) = persist(&confirmed) {
            return match self.revert_outputs(token) {
                Ok(()) => Err(format!("could not save output settings: {error}; preview reverted")),
                Err(rollback) => {
                    Err(format!("could not save output settings: {error}; restoring displays also failed: {rollback}"))
                }
            };
        }
        self.outputs.preview = None;
        Ok(confirmed)
    }

    pub fn revert_outputs(&mut self, token: u64) -> Result<(), String> {
        if self.outputs.preview.as_ref().is_none_or(|preview| preview.token != token) {
            return Err("output preview expired or was superseded".into());
        }
        let preview = self.outputs.preview.take().expect("preview token was checked");
        self.config.file.outputs = preview.previous.clone();
        if let Err(error) = self.apply_runtime_outputs(&preview.runtime) {
            // Keep the recovery snapshot so an explicit revert or reload can retry.
            self.outputs.preview = Some(preview);
            return Err(error);
        }
        self.restore_output_homes(&preview.homes);
        self.restore_preview_desktops(preview.desktops, preview.active);
        Ok(())
    }

    fn restore_preview_desktops(&mut self, desktops: Vec<PreviewDesktop>, active: Option<AreaId>) {
        for saved in desktops {
            self.with_area(&saved.area, |state| {
                let now = state.now();
                let extra: Vec<_> = state
                    .windows
                    .keys()
                    .copied()
                    .filter(|window| {
                        (state.world.contains_window(*window) || state.world.is_minimized(*window))
                            && !saved.world.contains_window(*window)
                            && !saved.world.is_minimized(*window)
                    })
                    .map(|window| (window, state.world.is_minimized(window)))
                    .collect();
                state.world = saved.world;
                let vanished: Vec<_> = remembered_windows(&state.world.snapshot().root)
                    .into_iter()
                    .chain(state.world.minimized())
                    .filter(|window| !state.windows.contains_key(window))
                    .collect();
                for window in vanished {
                    state.world.remove_window(window, now);
                }
                for (window, minimized) in extra {
                    state.world.add_window(window, Placement::Dock, now);
                    if minimized {
                        state.world.minimize(window, now);
                    }
                }
                state.pointer = saved.pointer;
                state.fullscreen = saved.fullscreen.filter(|window| state.windows.contains_key(window));
                state.apply_layout();
            });
        }
        if let Some(active) = active {
            self.activate_area(&active);
        }
        self.force_keyboard_focus_sync();
    }

    fn restore_output_homes(&mut self, homes: &[(WindowId, AreaId)]) {
        for (window, area) in homes {
            if self.windows.contains_key(window)
                && self.area_for_window(*window).as_ref() != Some(area)
                && let Err(error) = self.move_window_to_area(*window, area, true)
            {
                tracing::warn!("could not restore window after output preview: {error}");
            }
        }
    }

    /// Hotplug first restores a pending preview before changing the connector set.
    pub fn cancel_output_preview(&mut self) {
        if let Some(token) = self.outputs.preview.as_ref().map(|preview| preview.token)
            && let Err(error) = self.revert_outputs(token)
        {
            tracing::error!("could not cancel output preview: {error}");
        }
    }

    pub fn forget_connected_output(&mut self, output: &Output) {
        self.withdraw_output(output);
        self.outputs.connected.remove(&output.name());
    }

    pub fn advertise_output(&mut self, output: &Output) {
        self.outputs.globals.entry(output.name()).or_insert_with(|| output.create_global::<Self>(&self.display_handle));
    }

    fn withdraw_output(&mut self, output: &Output) {
        if let Some(global) = self.outputs.globals.remove(&output.name()) {
            self.display_handle.remove_global::<Self>(global);
        }
    }

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
        self.outputs.connected.insert(output.name(), output.clone());
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
        // Checked first: most setups split nothing, and naming the screen allocates.
        if self.outputs.areas.is_empty() {
            return None;
        }
        self.outputs.areas.get(&self.output.as_ref()?.name())
    }

    /// Whether something holds for the world of every area of the context screen.
    pub fn every_area(&self, test: impl Fn(&World) -> bool) -> bool {
        test(&self.world)
            && self.split().is_none_or(|areas| areas.parked.iter().flatten().all(|area| test(&area.world)))
    }

    /// The world of every area of the context screen, left to right.
    pub fn area_worlds(&self) -> Vec<(usize, &World)> {
        let Some(areas) = self.split() else { return vec![(0, &self.world)] };
        (0..areas.shares.len())
            .filter_map(|index| {
                if index == areas.context {
                    Some((index, &self.world))
                } else {
                    areas.parked.get(index)?.as_ref().map(|area| (index, &area.world))
                }
            })
            .collect()
    }

    /// Whether a window is in the layout of any area of the context screen,
    /// so it is on screen (or next to it) and draws.
    pub fn screen_shows(&self, window: WindowId) -> bool {
        self.world.contains_window(window)
            || self
                .split()
                .is_some_and(|areas| areas.parked.iter().flatten().any(|area| area.world.contains_window(window)))
    }

    /// Every world of every screen and area.
    fn every_world(&self) -> impl Iterator<Item = &World> {
        std::iter::once(&self.world)
            .chain(self.outputs.parked.values().map(|desktop| &desktop.world))
            .chain(self.outputs.areas.values().flat_map(|areas| areas.parked.iter().flatten().map(|area| &area.world)))
    }

    /// Whether a window is minimized, on whichever screen or area it is.
    pub fn minimized_anywhere(&self, window: WindowId) -> bool {
        self.every_world().any(|world| world.is_minimized(window))
    }

    /// Windows drawn on any screen or area, for what must notice a window
    /// that shows but has no focus (idle inhibitors).
    pub fn drawn_everywhere(&mut self, now: f64) -> Vec<crate::render::Drawn> {
        let mut drawn = Vec::new();
        for area in self.all_areas() {
            let shown = self.with_area(&area, |state| {
                let room = state.area_clip().unwrap_or_else(|| state.output_rect());
                let mut shown = state.drawn_windows(now);
                shown.retain(|drawn| drawn.content.intersects(&room));
                shown
            });
            drawn.extend(shown.unwrap_or_default());
        }
        drawn
    }

    /// Saves a view in slot 1..=9 of every area of the context screen, so a
    /// saved view is what the whole screen shows.
    pub fn save_view_everywhere(&mut self, slot: usize) {
        for index in 0..self.area_count() {
            self.with_local_area(index, |state| state.world.save_view(slot));
        }
    }

    /// Goes to slot 1..=9 in every area of the context screen that has it;
    /// the others stay as they are. Returns whether any area had it.
    pub fn recall_view_everywhere(&mut self, slot: usize) -> bool {
        let mut recalled = false;
        for index in 0..self.area_count() {
            recalled |= self
                .with_local_area(index, |state| {
                    let now = state.now();
                    let recalled = state.world.recall_view(slot, now);
                    if recalled {
                        state.apply_layout();
                    }
                    recalled
                })
                .unwrap_or(false);
        }
        recalled
    }

    /// Whether any area of the context screen has a view in slot 1..=9.
    pub fn has_saved_view_anywhere(&self, slot: usize) -> bool {
        self.area_worlds().iter().any(|(_, world)| world.has_saved_view(slot))
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
        area_room(self.output_rect(), areas, index)
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

    /// Hit testing and grab motion must use the same area's window scale.
    pub(crate) fn pointer_scale_in_areas(&mut self, surface: &WlSurface) -> (f64, f64) {
        self.all_areas()
            .into_iter()
            .find_map(|area| self.with_area(&area, |state| state.pointer_surface_scale(surface)).flatten())
            .unwrap_or((1.0, 1.0))
    }

    /// Deliver pointer motion in the hovered area, retaining a grab's scale.
    pub(crate) fn pointer_motion_in_area(&mut self, pos: Point<f64, Logical>) -> crate::input::ClientMotion {
        let grabbed =
            self.seat.get_pointer().and_then(|pointer| pointer.grab_start_data()).and_then(|start| start.focus);
        let grab_scale = grabbed.map(|(surface, _)| self.pointer_scale_in_areas(&surface)).unwrap_or((1.0, 1.0));
        match self.hovered_area() {
            Some(index) => self
                .with_local_area(index, |state| state.scaled_pointer_motion(pos, grab_scale))
                .unwrap_or(crate::input::ClientMotion { focus: None, location: pos }),
            None => self.scaled_pointer_motion(pos, grab_scale),
        }
    }

    /// Another machine's window under the pointer gets the pointer, looked
    /// up in the area the pointer is over, as `surface_under_in_area` does.
    pub fn pointer_to_owner_in_area(&mut self, pos: Point<f64, Logical>, time: u32) -> bool {
        match self.hovered_area() {
            Some(index) => self.with_local_area(index, |state| state.pointer_to_owner(pos, time)).unwrap_or(false),
            None => self.pointer_to_owner(pos, time),
        }
    }

    /// Activates the area under the pointer, for a press in its room (not
    /// on the bar or the strip below it, which the areas share). Returns
    /// whether it switched areas.
    pub fn activate_area_under_pointer(&mut self) -> bool {
        self.activate_area_at(self.pointer)
    }

    /// Activates the area under a point of the context screen, for a press
    /// or a touch in its room. Returns whether it switched areas.
    pub fn activate_area_at(&mut self, pos: Point<f64, Logical>) -> bool {
        let Some(areas) = self.split() else { return false };
        let index = self.area_at(pos.x);
        if index == areas.active || self.fullscreen.is_some() {
            return false;
        }
        let point = tileception_core::Point::new(pos.x, pos.y);
        // The minimap's card shows the active area, wherever it reaches; the
        // gap between areas belongs to neither.
        if !self.usable_rect().contains(point) || self.minimap_covers(point) || self.area_boundary_at(pos).is_some() {
            return false;
        }
        let Some(output) = self.output.clone() else { return false };
        self.activate_area(&AreaId { output, index })
    }

    /// The gap between two areas of the context screen under a point, as the
    /// index of the area left of it.
    pub fn area_boundary_at(&self, pos: Point<f64, Logical>) -> Option<usize> {
        let areas = self.split()?;
        if self.fullscreen.is_some() || pos.y < areas.usable.y || pos.y >= areas.usable.bottom() {
            return None;
        }
        let reach = self.world.settings.gap / 2.0 + 4.0;
        (0..areas.shares.len() - 1).find(|index| {
            let edge = slice(areas.usable, &areas.shares, index + 1).x;
            (pos.x - edge).abs() <= reach
        })
    }

    /// The gap between areas beside a tiled window of the context area, on
    /// `side`, when nothing in its own tree is on that side: as the index of
    /// the area left of the gap. Resizing the window there moves the gap.
    pub fn area_boundary_beside(&self, window: WindowId, side: tileception_core::Side) -> Option<usize> {
        let count = self.split()?.shares.len();
        let tree = self.world.tree();
        let tile = tree.tile_of(window).filter(|_| !tree.is_floating(window))?;
        if tree.boundary(tile, side).is_some() {
            return None;
        }
        let index = self.area_index();
        match side {
            tileception_core::Side::Left => index.checked_sub(1),
            tileception_core::Side::Right => (index + 1 < count).then_some(index),
            _ => None,
        }
    }

    /// Moves the gap between areas `index` and `index + 1` of the context
    /// screen by `delta` pixels, keeping each at least `MIN_SHARE` wide.
    /// Returns whether it moved. The widths last until the session ends;
    /// only `split` in `[outputs]` lasts longer.
    pub fn move_area_boundary(&mut self, index: usize, delta: f64) -> bool {
        let Some(name) = self.output.as_ref().map(Output::name) else { return false };
        let Some(areas) = self.outputs.areas.get_mut(&name) else { return false };
        let Some(shares) = moved_boundary(&areas.shares, index, delta / areas.usable.w.max(1.0)) else {
            return false;
        };
        areas.shares = shares;
        let usable = areas.usable;
        self.set_area_screens(usable);
        self.schedule_render();
        true
    }

    /// `resize` for a window with nothing beside it in its own tree: its area
    /// grows into the next area, or the one before it at the right edge.
    pub fn grow_area_of_focus(&mut self, delta: f64) {
        let Some(window) = self.world.focus().filter(|_| delta != 0.0) else { return };
        let right = self.area_boundary_beside(window, tileception_core::Side::Right);
        let left = self.area_boundary_beside(window, tileception_core::Side::Left);
        let tree = self.world.tree();
        let alone = tree.tile_of(window).is_some_and(|tile| {
            tree.boundary(tile, tileception_core::Side::Left).is_none()
                && tree.boundary(tile, tileception_core::Side::Right).is_none()
        });
        if !alone {
            return;
        }
        match (right, left) {
            (Some(index), _) => self.move_area_boundary(index, delta),
            (None, Some(index)) => self.move_area_boundary(index, -delta),
            (None, None) => false,
        };
    }

    /// Lays each area of the context screen out in its part of `usable`.
    pub fn set_area_screens(&mut self, usable: Rect) {
        self.place_areas(usable, true);
    }

    /// Gives each area of the context screen its part of `usable`, laying
    /// out those that changed when `layout` is set. Without it the caller
    /// lays out once the final room is known (`lay_out_areas`), so windows
    /// are not told a size they are about to lose.
    pub fn place_areas(&mut self, usable: Rect, layout: bool) {
        let Some(name) = self.output.as_ref().map(Output::name) else { return };
        let Some(shares) = self.outputs.areas.get_mut(&name).map(|areas| {
            areas.usable = usable;
            areas.shares.clone()
        }) else {
            if usable != self.world.screen() {
                self.world.set_screen(usable);
                if layout {
                    self.apply_layout();
                }
            }
            return;
        };
        for index in 0..shares.len() {
            let rect = slice(usable, &shares, index);
            self.with_local_area(index, |state| {
                if state.world.screen() != rect {
                    state.world.set_screen(rect);
                    if layout {
                        state.apply_layout();
                    }
                }
            });
        }
    }

    /// Lays out every area of the context screen.
    pub fn lay_out_areas(&mut self) {
        if self.split().is_none() {
            self.apply_layout();
            return;
        }
        for index in 0..self.area_count() {
            self.with_local_area(index, Self::apply_layout);
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
            // Widths dragged this session stay, unless the setting changed.
            if let Some(areas) = self.outputs.areas.get_mut(&name)
                && areas.configured != shares
            {
                areas.configured = shares.clone();
                areas.shares = shares;
            }
            return;
        }
        tracing::info!("{name} in {} areas", shares.len());
        if shares.len() > old {
            self.add_areas(&name, shares);
            self.restore_remembered_areas(&name);
        } else {
            self.combine_areas(shares);
        }
    }

    /// Splits the context screen further, the new areas empty.
    fn add_areas(&mut self, name: &str, shares: Vec<f64>) {
        let settings = self.config.world_settings();
        let screen = self.world.screen();
        let areas = self.outputs.areas.entry(name.to_string()).or_insert_with(|| Areas {
            shares: vec![1.0],
            configured: vec![1.0],
            active: 0,
            context: 0,
            parked: vec![None],
            usable: screen,
        });
        while areas.parked.len() < shares.len() {
            let world = World::new(settings.clone(), screen);
            areas.parked.push(Some(Area { world, selection: Default::default() }));
        }
        areas.configured = shares.clone();
        areas.shares = shares;
    }

    /// Combines the context screen's areas into `shares.len()`: those that go
    /// are taken in by the last one kept, their trees side by side from the
    /// left, and its camera shows the result.
    fn combine_areas(&mut self, shares: Vec<f64>) {
        let Some(name) = self.output.as_ref().map(Output::name) else { return };
        let Some((old_shares, active)) = self.split().map(|areas| (areas.shares.clone(), areas.active)) else {
            return;
        };
        let old = old_shares.len();
        let keep = shares.len().clamp(1, old) - 1;
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
        let Some(areas) = self.outputs.areas.get_mut(&name) else { return };
        areas.parked.truncate(keep + 1);
        areas.active = active.min(keep);
        if keep == 0 {
            self.outputs.areas.remove(&name);
        } else {
            areas.configured = shares.clone();
            areas.shares = shares;
            let active = areas.active;
            self.exchange_area(active);
        }
        self.selection.anchor = None;
    }

    /// Before the last screen goes: notes where each window of its areas
    /// is, so `restore_remembered_areas` can put them back, then combines
    /// the areas so every window stays reachable meanwhile.
    fn remember_and_combine_areas(&mut self) {
        let Some(name) = self.output.as_ref().map(Output::name) else { return };
        let Some(active) = self.split().map(|areas| areas.active) else { return };
        let areas = self
            .area_worlds()
            .into_iter()
            .map(|(_, world)| RememberedArea {
                layout: world.snapshot(),
                minimized: world.minimized().collect(),
                focus: world.focus(),
            })
            .collect();
        self.outputs.remembered = Some(Remembered { name, active, areas });
        self.combine_areas(vec![1.0]);
    }

    /// When the screen whose areas were remembered is split again into as
    /// many areas, its windows go back to the areas they were in, in the
    /// layout they had there. Windows opened meanwhile stay in the first.
    fn restore_remembered_areas(&mut self, name: &str) {
        let Some(remembered) = self.outputs.remembered.take_if(|remembered| remembered.name == name) else {
            return;
        };
        if remembered.areas.len() != self.area_count() || self.area_index() != 0 {
            return;
        }
        let now = self.now();
        let mut moving = Vec::new();
        for (index, saved) in remembered.areas.iter().enumerate().skip(1) {
            let tiled: Vec<WindowId> = remembered_windows(&saved.layout.root)
                .into_iter()
                .filter(|window| self.world.contains_window(*window))
                .collect();
            let minimized: Vec<WindowId> =
                saved.minimized.iter().copied().filter(|window| self.world.is_minimized(*window)).collect();
            for window in tiled.iter().chain(&minimized) {
                self.selection.windows.remove(window);
                self.world.remove_window(*window, now);
            }
            moving.push((index, tiled, minimized));
        }
        for (index, tiled, minimized) in moving {
            let saved = &remembered.areas[index];
            self.with_local_area(index, |state| {
                let remap = tiled.iter().map(|window| (*window, *window)).collect();
                if !tiled.is_empty() && !state.world.restore_layout(&saved.layout, &remap, now) {
                    for window in &tiled {
                        state.world.add_window(*window, Placement::Dock, now);
                    }
                }
                for window in &minimized {
                    state.world.add_window(*window, Placement::Dock, now);
                    state.world.minimize(*window, now);
                }
                if let Some(focus) = saved.focus.filter(|window| state.world.contains_window(*window)) {
                    state.world.focus_window(focus, now);
                }
            });
        }
        if let Some(focus) = remembered.areas[0].focus.filter(|window| self.world.contains_window(*window)) {
            self.world.focus_window(focus, now);
        }
        if let Some(areas) = self.outputs.areas.get_mut(name)
            && remembered.active < areas.shares.len()
        {
            areas.active = remembered.active;
            self.exchange_area(remembered.active);
        }
    }

    /// Makes an area the one input goes to, activating its screen first.
    pub fn activate_area(&mut self, area: &AreaId) -> bool {
        if self.outputs.active.as_ref() != Some(&area.output) && !self.activate_output(&area.output) {
            return false;
        }
        if self.output.as_ref() != Some(&area.output) {
            return false;
        }
        // A fullscreen window covers every area: going to another one leaves it.
        if let Some(full) = self.fullscreen
            && self.area_for_window(full).is_some_and(|holder| holder != *area)
            && self.split().is_some_and(|areas| areas.active != area.index && area.index < areas.shares.len())
        {
            self.set_fullscreen(full, false);
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
        self.close_minimap();
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
            let width = output.current_mode().map_or(0, |mode| {
                (f64::from(output.current_transform().transform_size(mode.size).w)
                    / output.current_scale().fractional_scale())
                .round() as i32
            });
            right = right.max(position[0].saturating_add(width));
        }
    }

    pub fn configure_outputs(&mut self) -> Result<(), String> {
        // A config reload supersedes any preview; its old timer must do nothing.
        self.outputs.preview = None;
        let proposed = self.config.file.outputs.clone();
        let runtime = self.proposed_outputs(&proposed, true)?;
        let previous = self.runtime_outputs();
        if let Err(error) = self.apply_runtime_outputs(&runtime) {
            return match self.apply_runtime_outputs(&previous) {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!("{error}; restoring displays also failed: {rollback}")),
            };
        }
        Ok(())
    }

    pub(crate) fn configure_output_layouts(&mut self) {
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
        self.settings_fit_screen();
    }

    pub fn output_for_window(&self, window: WindowId) -> Option<Output> {
        self.area_for_window(window).map(|area| area.output)
    }

    pub fn area_for_window(&self, window: WindowId) -> Option<AreaId> {
        let holds = |world: &World| world.contains_window(window) || world.is_minimized(window);
        let desktops = self
            .output
            .iter()
            .map(|output| (output, &self.world))
            .chain(self.outputs.parked.values().map(|desktop| (&desktop.output, &desktop.world)));
        for (output, world) in desktops {
            let areas = (!self.outputs.areas.is_empty()).then(|| self.outputs.areas.get(&output.name())).flatten();
            let index = if holds(world) {
                Some(areas.map_or(0, |areas| areas.context))
            } else {
                areas.and_then(|areas| {
                    areas.parked.iter().position(|area| area.as_ref().is_some_and(|area| holds(&area.world)))
                })
            };
            if let Some(index) = index {
                return Some(AreaId { output: output.clone(), index });
            }
        }
        None
    }

    /// The area showing a surface: its window's or its popup's parent's, else
    /// the area in use on the screen holding its layer or lock surface.
    pub fn area_for_surface(&self, surface: &WlSurface) -> Option<AreaId> {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        let in_use = |output: Output| {
            let index = self.outputs.areas.get(&output.name()).map_or(0, |areas| areas.context);
            AreaId { output, index }
        };
        if let crate::lock::LockState::Locked { surfaces } = &self.lock
            && let Some((name, _)) = surfaces.iter().find(|(_, locked)| locked.wl_surface() == &root)
        {
            return self.output_named(name).map(in_use);
        }
        if let Some(area) = self.window_id(&root).and_then(|window| self.area_for_window(window)) {
            return Some(area);
        }
        if let Some(popup) = self.popups.find_popup(&root)
            && let Ok(parent) = smithay::desktop::find_popup_root_surface(&popup)
        {
            return self.window_id(&parent).and_then(|window| self.area_for_window(window));
        }
        self.all_outputs()
            .into_iter()
            .find(|output| {
                smithay::desktop::layer_map_for_output(output)
                    .layer_for_surface(&root, smithay::desktop::WindowSurfaceType::ALL)
                    .is_some()
            })
            .map(in_use)
    }

    pub fn output_for_surface(&self, surface: &WlSurface) -> Option<Output> {
        self.area_for_surface(surface).map(|area| area.output)
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
            // The world in use stays; the other areas' windows come into
            // it until the screen is back.
            self.remember_and_combine_areas();
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
        self.outputs.areas.remove(&output.name());
        self.arrange_outputs();
        self.schedule_render();
    }

    /// Global desktop geometry is only used for crossing screens; each world's
    /// pointer and layout coordinates stay local to its output.
    pub fn desktop_motion(&mut self, position: Point<f64, Logical>) -> Point<f64, Logical> {
        let Some(current) = self.output.clone() else { return position };
        let stretched = self
            .config
            .file
            .general
            .stretch_screen_edges
            .then(|| self.stretched_screen_target(&current, position))
            .flatten();
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
        let Some((target, position, _)) = stretched.map(|(output, point)| (output, point, 0.)).or(nearest) else {
            return position;
        };
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

    fn stretched_screen_target(
        &self,
        current: &Output,
        point: Point<f64, Logical>,
    ) -> Option<(Output, Point<f64, Logical>)> {
        let geometry = |output: &Output| {
            let mode = output.current_mode()?;
            let size = output
                .current_transform()
                .transform_size(mode.size.to_f64().to_logical(output.current_scale().fractional_scale()));
            let location = output.current_location();
            Some(Rect::new(location.x as f64, location.y as f64, size.w, size.h))
        };
        let source = geometry(current)?;
        let horizontal = point.x < 0. || point.x >= source.w;
        let vertical = point.y < 0. || point.y >= source.h;
        if !horizontal && !vertical {
            return None;
        }
        let horizontal = if horizontal && vertical {
            (if point.x < 0. { -point.x } else { point.x - source.w })
                >= (if point.y < 0. { -point.y } else { point.y - source.h })
        } else {
            horizontal
        };
        let forward = if horizontal { point.x >= source.w } else { point.y >= source.h };
        let mut candidates = Vec::new();
        for output in self.all_outputs() {
            if &output == current {
                continue;
            }
            let Some(target) = geometry(&output) else { continue };
            let (gap, overlap, along, start, span) = if horizontal {
                (
                    if forward { target.x - source.right() } else { source.x - target.right() },
                    source.bottom().min(target.bottom()) - source.y.max(target.y),
                    source.y + point.y,
                    target.y,
                    target.h,
                )
            } else {
                (
                    if forward { target.y - source.bottom() } else { source.y - target.bottom() },
                    source.right().min(target.right()) - source.x.max(target.x),
                    source.x + point.x,
                    target.x,
                    target.w,
                )
            };
            if gap < -1. || overlap <= 0. {
                continue;
            }
            let distance = (along - along.clamp(start, start + span)).abs();
            candidates.push((gap.max(0.), distance, output, target));
        }
        candidates.sort_by(|left, right| left.0.total_cmp(&right.0).then_with(|| left.1.total_cmp(&right.1)));
        let (_, _, output, target) = candidates.into_iter().next()?;
        let mapped = if horizontal {
            let fraction = (point.y / (source.h - 1.).max(1.)).clamp(0., 1.);
            let overflow = if forward { point.x - source.w } else { -point.x };
            (if forward { overflow } else { target.w - 1. - overflow }, fraction * (target.h - 1.).max(0.))
        } else {
            let fraction = (point.x / (source.w - 1.).max(1.)).clamp(0., 1.);
            let overflow = if forward { point.y - source.h } else { -point.y };
            (fraction * (target.w - 1.).max(0.), if forward { overflow } else { target.h - 1. - overflow })
        };
        Some((
            output,
            (mapped.0.clamp(0., (target.w - 1.).max(0.)), mapped.1.clamp(0., (target.h - 1.).max(0.))).into(),
        ))
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
                let size = mode.map(|mode| output.current_transform().transform_size(mode.size)).unwrap_or_default();
                let (mut width, height) = (size.w, size.h);
                let mut left = position.x;
                if let Some(areas) = self.outputs.areas.get(&output.name()) {
                    let room = area_room(self.whole_output_rect(output), areas, area.index);
                    left += room.x.round() as i32;
                    width = (room.w * scale).round() as i32;
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
}

/// Every window a layout snapshot holds, tiled or floating.
fn remembered_windows(node: &tileception_core::tree::LayoutNode) -> Vec<WindowId> {
    use tileception_core::tree::LayoutNode;
    match node {
        LayoutNode::Tile { views, .. } => views.clone(),
        LayoutNode::Desktop { floats, .. } => floats.clone(),
        LayoutNode::Split { children, .. } => children.iter().flat_map(remembered_windows).collect(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn slices_cover_the_room_on_whole_pixels() {
        let usable = Rect::new(0.0, 30.0, 2560.0, 1410.0);
        let shares = [1.0 / 3.0; 3];
        let parts: Vec<Rect> = (0..3).map(|index| slice(usable, &shares, index)).collect();
        assert_eq!(parts[0].x, 0.0);
        assert_eq!(parts[2].right(), 2560.0);
        for pair in parts.windows(2) {
            assert_eq!(pair[0].right(), pair[1].x);
            assert_eq!(pair[1].x.fract(), 0.0);
        }
        assert!(parts.iter().all(|part| part.y == 30.0 && part.h == 1410.0));
    }

    #[test]
    fn boundaries_move_within_the_minimum() {
        let moved = moved_boundary(&[0.5, 0.5], 0, 0.1).unwrap();
        assert!((moved[0] - 0.6).abs() < 1e-9 && (moved[1] - 0.4).abs() < 1e-9);
        let moved = moved_boundary(&[0.5, 0.5], 0, 0.9).unwrap();
        assert!((moved[1] - MIN_SHARE).abs() < 1e-9);
        assert!(moved_boundary(&[0.5, 0.5], 1, 0.1).is_none());
        assert!(moved_boundary(&[0.9, 0.1], 0, 0.1).is_none());
        // Narrower than the minimum as configured: no panic, and no shrinking.
        assert!(moved_boundary(&[0.05, 0.05, 0.9], 0, 0.02).is_none());
        let moved = moved_boundary(&[0.05, 0.5, 0.45], 0, -0.1);
        assert!(moved.is_none());
        let moved = moved_boundary(&[0.05, 0.5, 0.45], 0, 0.1).unwrap();
        assert!((moved[0] - 0.15).abs() < 1e-9);
    }

    #[test]
    fn remembered_windows_include_floats() {
        use tileception_core::tree::LayoutNode;
        let layout = LayoutNode::Split {
            axis: tileception_core::Axis::Horizontal,
            children: vec![
                LayoutNode::Tile { views: vec![WindowId(1), WindowId(2)], selected: 0 },
                LayoutNode::Desktop { floats: vec![WindowId(3)], shown: false },
            ],
            weights: vec![1.0, 1.0],
        };
        assert_eq!(remembered_windows(&layout), vec![WindowId(1), WindowId(2), WindowId(3)]);
    }
}
