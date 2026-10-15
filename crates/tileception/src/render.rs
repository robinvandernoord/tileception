//! Builds the render elements for one frame.
//!
//! Every window is drawn from its live buffer. While the camera moves, or in
//! Overview, a window is scaled into the rectangle the layout gives it. Once
//! everything has settled and the client has drawn at the new size, the window
//! is drawn 1:1 and receives pointer input ("live").

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            utils::{CropRenderElement, RescaleRenderElement},
            AsRenderElements, Id, Kind,
        },
        gles::GlesRenderer,
        ImportAll, ImportMem,
    },
    utils::{IsAlive, Physical, Point, Rectangle, Scale},
};
use tileception_core::{Node, Rect, WindowId, WindowRole};

use smithay::wayland::shell::wlr_layer::Layer;

use crate::{state::Tileception, udev::Udev};

smithay::backend::renderer::element::render_elements! {
    pub OutputElement<R> where R: ImportAll + ImportMem;
    Live = WaylandSurfaceRenderElement<R>,
    Scaled = CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<R>>>,
    Solid = SolidColorRenderElement,
    Memory = MemoryRenderBufferRenderElement<R>,
}

pub const BACKGROUND: [f32; 4] = [0.11, 0.12, 0.14, 1.0];
const TILE: [f32; 4] = [0.16, 0.17, 0.20, 1.0];
const DESKTOP: [f32; 4] = [0.20, 0.24, 0.30, 1.0];
const FOCUS: [f32; 4] = [0.36, 0.56, 0.95, 1.0];
const FOCUS_WIDTH: f64 = 2.0;
const TAB: [f32; 4] = [0.22, 0.24, 0.28, 1.0];
const TAB_SELECTED: [f32; 4] = [0.34, 0.37, 0.43, 1.0];
/// Premultiplied: the focus colour at 30% opacity.
const DROP: [f32; 4] = [0.36 * 0.3, 0.56 * 0.3, 0.95 * 0.3, 0.3];

/// How one window is drawn this frame.
#[derive(Debug, Clone, Copy)]
pub struct Drawn {
    pub id: WindowId,
    pub role: WindowRole,
    /// The rectangle the layout gives the window.
    pub slot: Rect,
    /// Where the window's visible content ends up on screen.
    pub content: Rect,
    /// Where the top left of the window's buffer ends up, before scaling.
    pub origin: Point<f64, smithay::utils::Logical>,
    pub scale: (f64, f64),
    /// Drawn 1:1 and accepting pointer input.
    pub live: bool,
}

impl Tileception {
    /// Every window on screen, topmost first.
    pub fn drawn_windows(&self, now: f64) -> Vec<Drawn> {
        let view = self.world.view(now);
        let moving = self.world.is_animating(now);
        let overview = self.world.in_overview();
        let screen = self.world.screen();
        let focus = self.world.focus();

        if let Some(id) = self.fullscreen {
            if let Some(window) = self.windows.get(&id) {
                let geo = window.geometry();
                let out = self.output_rect();
                let content = Rect::new(out.x, out.y, geo.size.w as f64, geo.size.h as f64);
                let origin = (content.x - geo.loc.x as f64, content.y - geo.loc.y as f64).into();
                return vec![Drawn {
                    id,
                    role: WindowRole::Tiled,
                    slot: out,
                    content,
                    origin,
                    scale: (1.0, 1.0),
                    live: true,
                }];
            }
        }

        let dragged = self.move_drag.filter(|d| !d.free).map(|d| d.window);
        let mut out = Vec::new();
        for p in self.world.placements(view) {
            if !p.rect.intersects(&screen) || Some(p.window) == dragged {
                continue;
            }
            let Some(window) = self.windows.get(&p.window) else { continue };
            let geo = window.geometry();
            if geo.size.w <= 0 || geo.size.h <= 0 {
                continue;
            }
            let (gw, gh) = (geo.size.w as f64, geo.size.h as f64);
            // Floating windows hang still while the camera moves.
            let moving = moving && p.role == WindowRole::Tiled;
            let (sx, sy) = if moving {
                // Stretch into the moving slot; the client catches up after.
                (p.rect.w / gw, p.rect.h / gh)
            } else {
                // Fit without enlarging: a window too big for its slot shows
                // as a preview, a smaller one sits in the middle.
                let s = (p.rect.w / gw).min(p.rect.h / gh).min(1.0);
                (s, s)
            };
            let content = Rect::new(
                p.rect.x + (p.rect.w - gw * sx) / 2.0,
                p.rect.y + (p.rect.h - gh * sy) / 2.0,
                gw * sx,
                gh * sy,
            );
            let origin = (content.x - geo.loc.x as f64 * sx, content.y - geo.loc.y as f64 * sy).into();
            let live = !moving && !overview && sx >= 0.999 && sy >= 0.999;
            out.push(Drawn { id: p.window, role: p.role, slot: p.rect, content, origin, scale: (sx, sy), live });
        }
        // Floating windows above tiles, the focused one above its peers.
        out.sort_by_key(|d| (d.role != WindowRole::Floating, Some(d.id) != focus));
        out
    }

    pub fn render_elements(&self, renderer: &mut GlesRenderer, now: f64) -> Vec<OutputElement<GlesRenderer>> {
        if self.is_locked() {
            // Only the locker; black where it has not drawn yet.
            let mut elements = self.lock_elements(renderer);
            elements.push(solid(&self.output_rect(), [0.0, 0.0, 0.0, 1.0]));
            return elements;
        }
        let drawn = self.drawn_windows(now);
        let focus = self.world.focus();
        let view = self.world.view(now);
        let mut elements: Vec<OutputElement<GlesRenderer>> = Vec::new();

        // Topmost: notifications, launchers and bars. A fullscreen window
        // covers everything but notifications.
        if self.fullscreen.is_some() {
            elements.extend(self.layer_elements(renderer, &[Layer::Overlay]));
            for d in &drawn {
                if let Some(window) = self.windows.get(&d.id) {
                    let origin: Point<i32, Physical> = (d.origin.x.round() as i32, d.origin.y.round() as i32).into();
                    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                        window.render_elements(renderer, origin, Scale::from(1.0), 1.0);
                    elements.extend(surfaces.into_iter().map(OutputElement::Live));
                }
            }
            return elements;
        }
        elements.extend(self.layer_elements(renderer, &[Layer::Overlay, Layer::Top]));

        // X11 menus and tooltips, at the place they chose.
        for overlay in self.x11_overlays.iter().rev() {
            let Some(surface) = overlay.wl_surface() else { continue };
            let loc = overlay.geometry().loc;
            elements.extend(
                render_elements_from_surface_tree(renderer, &surface, (loc.x, loc.y), 1.0, 1.0, Kind::Unspecified)
                    .into_iter()
                    .map(OutputElement::Live),
            );
        }

        // Then the window being dragged, then where it would land.
        if let Some(drag) = self.move_drag.filter(|d| !d.free) {
            let r = Rect::new(
                self.pointer.x - drag.grab.0 * drag.size.0,
                self.pointer.y - drag.grab.1 * drag.size.1,
                drag.size.0,
                drag.size.1,
            );
            elements.extend(self.picture(renderer, drag.window, &r));
            if let Some(preview) = drag.target.and_then(|t| self.world.drop_preview(t, view)) {
                elements.push(solid(&preview, DROP));
            }
        }

        for d in &drawn {
            let Some(window) = self.windows.get(&d.id) else { continue };
            if d.live {
                let origin: Point<i32, Physical> = (d.origin.x.round() as i32, d.origin.y.round() as i32).into();
                let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    window.render_elements(renderer, origin, Scale::from(1.0), 1.0);
                elements.extend(surfaces.into_iter().map(OutputElement::Live));
            } else {
                elements.extend(self.picture(renderer, d.id, &d.content));
            }
            if Some(d.id) == focus && self.windows.len() > 1 {
                elements.push(solid(&inflate(&d.content, FOCUS_WIDTH), FOCUS));
            }
        }

        for strip in self.world.tab_strips(view) {
            let n = strip.views.len() as f64;
            let w = strip.rect.w / n;
            for (i, win) in strip.views.iter().enumerate() {
                let tab = Rect::new(strip.rect.x + i as f64 * w, strip.rect.y, w, strip.rect.h).inset(1.0);
                let colour = if i == strip.selected {
                    if Some(*win) == focus {
                        FOCUS
                    } else {
                        TAB_SELECTED
                    }
                } else {
                    TAB
                };
                elements.push(solid(&tab, colour));
            }
        }

        // Tile backgrounds, so slots read as places even before a window draws.
        let half_gap = self.world.settings.gap / 2.0;
        let tree = self.world.tree();
        for leaf in tree.leaves(tree.root()) {
            let Some(r) = self.world.node_rect(leaf, view) else { continue };
            let r = r.inset(half_gap);
            if !r.intersects(&self.world.screen()) {
                continue;
            }
            let colour = if matches!(tree.node(leaf), Node::Desktop { .. }) { DESKTOP } else { TILE };
            elements.push(solid(&r, colour));
        }

        // Bottommost: desktop widgets and wallpaper.
        elements.extend(self.layer_elements(renderer, &[Layer::Bottom, Layer::Background]));
        elements
    }

    /// The pointer, drawn by us when Tileception owns the screen: the client's own
    /// cursor surface when it set one, otherwise the theme's arrow.
    pub fn cursor_elements(&mut self, udev: &mut Udev) -> Vec<OutputElement<GlesRenderer>> {
        use smithay::input::pointer::{CursorImageAttributes, CursorImageStatus};
        if let CursorImageStatus::Surface(surface) = &self.cursor_status {
            if !surface.alive() {
                self.cursor_status = CursorImageStatus::default_named();
            }
        }
        let pos = self.pointer;
        match &self.cursor_status {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                let hotspot = smithay::wayland::compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<std::sync::Mutex<CursorImageAttributes>>()
                        .and_then(|a| a.lock().ok().map(|a| a.hotspot))
                        .unwrap_or_default()
                });
                let at: Point<i32, Physical> =
                    ((pos.x - hotspot.x as f64).round() as i32, (pos.y - hotspot.y as f64).round() as i32).into();
                render_elements_from_surface_tree(&mut udev.renderer, surface, at, 1.0, 1.0, Kind::Cursor)
                    .into_iter()
                    .map(OutputElement::Live)
                    .collect()
            }
            CursorImageStatus::Named(_) => {
                let hot = udev.cursor.hotspot;
                let at = (pos.x - hot.x as f64, pos.y - hot.y as f64);
                MemoryRenderBufferRenderElement::from_buffer(
                    &mut udev.renderer,
                    at,
                    &udev.cursor.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .map(|e| vec![OutputElement::Memory(e)])
                .unwrap_or_default()
            }
        }
    }

    /// A window scaled into `content`, which is where its visible geometry goes.
    fn picture(&self, renderer: &mut GlesRenderer, id: WindowId, content: &Rect) -> Vec<OutputElement<GlesRenderer>> {
        let Some(window) = self.windows.get(&id) else { return Vec::new() };
        let geo = window.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            return Vec::new();
        }
        let scale = Scale { x: content.w / geo.size.w as f64, y: content.h / geo.size.h as f64 };
        let origin: Point<i32, Physical> = (
            (content.x - geo.loc.x as f64 * scale.x).round() as i32,
            (content.y - geo.loc.y as f64 * scale.y).round() as i32,
        )
            .into();
        let crop = physical(content);
        let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            window.render_elements(renderer, origin, Scale::from(1.0), 1.0);
        surfaces
            .into_iter()
            .filter_map(|s| {
                let scaled = RescaleRenderElement::from_element(s, origin, scale);
                CropRenderElement::from_element(scaled, 1.0, crop).map(OutputElement::Scaled)
            })
            .collect()
    }
}

fn physical(r: &Rect) -> Rectangle<i32, Physical> {
    let x0 = r.x.round() as i32;
    let y0 = r.y.round() as i32;
    let x1 = r.right().round() as i32;
    let y1 = r.bottom().round() as i32;
    Rectangle::new((x0, y0).into(), ((x1 - x0).max(0), (y1 - y0).max(0)).into())
}

fn inflate(r: &Rect, d: f64) -> Rect {
    Rect::new(r.x - d, r.y - d, r.w + 2.0 * d, r.h + 2.0 * d)
}

fn solid(r: &Rect, colour: [f32; 4]) -> OutputElement<GlesRenderer> {
    OutputElement::Solid(SolidColorRenderElement::new(Id::new(), physical(r), 0usize, colour, Kind::Unspecified))
}
