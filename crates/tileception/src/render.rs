//! Builds the render elements for one frame, in the Tide style.
//!
//! Every window is drawn from its live buffer. While the camera moves, or in
//! Overview, a window is scaled into the rectangle the layout gives it. Once
//! everything has settled and the client has drawn at the new size, the window
//! is drawn 1:1 and receives pointer input ("live").
//!
//! Elements are listed topmost first. A window's frame (rounded corners,
//! border, focus glow) is drawn just above the window itself.

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            utils::{CropRenderElement, RescaleRenderElement},
            AsRenderElements, Id, Kind,
        },
        gles::{element::PixelShaderElement, GlesRenderer},
    },
    utils::{IsAlive, Physical, Point, Rectangle, Scale},
    wayland::shell::wlr_layer::Layer,
};
use tileception_core::{Node, NodeId, Rect, WindowId, WindowRole};

use crate::{
    shapes::{Shape, Shapes},
    state::Tileception,
    theme::Colour,
    udev::Udev,
};

smithay::backend::renderer::element::render_elements! {
    pub OutputElement<=GlesRenderer>;
    Live = WaylandSurfaceRenderElement<GlesRenderer>,
    Scaled = CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    Solid = SolidColorRenderElement,
    Memory = MemoryRenderBufferRenderElement<GlesRenderer>,
    Shape = PixelShaderElement,
}

/// How one window is drawn this frame.
#[derive(Debug, Clone, Copy)]
pub struct Drawn {
    pub id: WindowId,
    pub role: WindowRole,
    pub leaf: NodeId,
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
    /// The colour behind everything.
    pub fn clear_colour(&self) -> [f32; 4] {
        self.theme.background.0
    }

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
                let leaf = self.world.tree().leaf_of(id).unwrap_or(self.world.tree().root());
                return vec![Drawn {
                    id,
                    role: WindowRole::Tiled,
                    leaf,
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
            out.push(Drawn {
                id: p.window,
                role: p.role,
                leaf: p.leaf,
                slot: p.rect,
                content,
                origin,
                scale: (sx, sy),
                live,
            });
        }
        // Floating windows above tiles, the focused one above its peers.
        out.sort_by_key(|d| (d.role != WindowRole::Floating, Some(d.id) != focus));
        out
    }

    /// The whole frame: everything below plus the wallpaper at the bottom.
    pub fn frame_elements(&mut self, renderer: &mut GlesRenderer, now: f64) -> Vec<OutputElement> {
        if matches!(self.lock, crate::lock::LockState::BuiltIn) {
            self.wallpaper_buffer();
        }
        let wallpaper = if self.is_locked() { None } else { self.wallpaper_buffer() };
        let mut elements = self.render_elements(renderer, now);
        if let Some(buffer) = wallpaper {
            let o = self.output_rect();
            push_image(&mut elements, renderer, &buffer, (o.x, o.y));
        }
        elements
    }

    pub fn render_elements(&self, renderer: &mut GlesRenderer, now: f64) -> Vec<OutputElement> {
        if self.is_locked() {
            // Only the locker; black where it has not drawn yet.
            let mut elements = self.lock_elements(renderer);
            elements.push(solid(&self.output_rect(), [0.0, 0.0, 0.0, 1.0]));
            return elements;
        }
        let shapes = self.shapes(renderer);
        let theme = &self.theme;
        let drawn = self.drawn_windows(now);
        let focus = self.world.focus();
        let view = self.world.view(now);
        let mut elements: Vec<OutputElement> = Vec::new();
        elements.extend(self.power_menu_elements(renderer, shapes.as_ref()));
        elements.extend(self.launcher_elements(renderer));
        elements.extend(self.notification_elements(renderer));
        elements.extend(self.selection_elements(renderer));

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
            push_shape(&mut elements, shapes.as_ref(), &r, self.frame(true, false));
            elements.extend(self.picture(renderer, drag.window, &r));
            push_shape(
                &mut elements,
                shapes.as_ref(),
                &r,
                Shape { glow_radius: 24.0, glow: Colour([0.0, 0.0, 0.0, 0.45]), ..Default::default() },
            );
            if let Some(preview) = drag.target.and_then(|t| self.world.drop_preview(t, view)) {
                let shape = Shape {
                    radius: theme.radius,
                    fill: (theme.accent.alpha(0.16), theme.accent_end.alpha(0.16)),
                    border_width: 2.0,
                    border: theme.accent,
                    ..Default::default()
                };
                push_shape(&mut elements, shapes.as_ref(), &preview, shape);
            }
        }

        let strips = self.world.tab_strips(view);
        for d in &drawn {
            let Some(window) = self.windows.get(&d.id) else { continue };
            let focused = Some(d.id) == focus && self.windows.len() > 1;
            let floating = d.role == WindowRole::Floating;
            // A tabbed tile's frame takes in its strip of tabs.
            let mut frame_rect = d.content;
            if let Some(strip) = strips.iter().find(|s| s.tile == d.leaf) {
                frame_rect = Rect::new(d.content.x, strip.rect.y, d.content.w, d.content.bottom() - strip.rect.y);
            }
            push_shape(&mut elements, shapes.as_ref(), &frame_rect, self.frame(focused, !floating));
            if d.live {
                let origin: Point<i32, Physical> = (d.origin.x.round() as i32, d.origin.y.round() as i32).into();
                let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    window.render_elements(renderer, origin, Scale::from(1.0), 1.0);
                elements.extend(surfaces.into_iter().map(OutputElement::Live));
            } else {
                elements.extend(self.picture(renderer, d.id, &d.content));
            }
            if floating {
                // Floating windows cast a soft shadow onto the layout.
                let shadow = Shape {
                    radius: theme.radius,
                    glow_radius: 28.0,
                    glow: Colour([0.0, 0.0, 0.0, 0.4]),
                    ..Default::default()
                };
                push_shape(&mut elements, shapes.as_ref(), &d.content, shadow);
            }
        }

        for strip in &strips {
            elements.extend(self.tab_strip_elements(renderer, shapes.as_ref(), strip, focus));
        }

        // Tile backgrounds, so slots read as places even before a window draws.
        let half_gap = self.world.settings.gap / 2.0;
        let tree = self.world.tree();
        for leaf in tree.leaves(tree.root()) {
            if matches!(tree.node(leaf), Node::Desktop { .. }) {
                continue;
            }
            let Some(r) = self.world.node_rect(leaf, view) else { continue };
            let r = r.inset(half_gap);
            if !r.intersects(&self.world.screen()) {
                continue;
            }
            push_shape(&mut elements, shapes.as_ref(), &r, Shape::filled(theme.surface, theme.radius));
        }

        // Bottommost: desktop widgets and wallpaper.
        elements.extend(self.layer_elements(renderer, &[Layer::Bottom, Layer::Background]));
        elements
    }

    /// The compiled shape shader, made on first use with this renderer.
    pub fn shapes(&self, renderer: &mut GlesRenderer) -> Option<Shapes> {
        if self.shapes.get().is_none() {
            match Shapes::compile(renderer) {
                Ok(s) => {
                    let _ = self.shapes.set(s);
                }
                Err(err) => {
                    tracing::error!("shape shader: {err}");
                    return None;
                }
            }
        }
        self.shapes.get().cloned()
    }

    /// A window's frame: rounded corners masked in the background colour (for
    /// tiles), a hairline border, and the accent with a glow when focused.
    fn frame(&self, focused: bool, mask_corners: bool) -> Shape {
        let t = &self.theme;
        Shape {
            radius: t.radius,
            border_width: 1.0,
            border: if focused { t.accent } else { t.border },
            glow_radius: if focused { t.glow } else { 0.0 },
            glow: t.accent.alpha(0.27),
            corner_mask: if mask_corners { t.background } else { Colour::TRANSPARENT },
            ..Default::default()
        }
    }

    fn tab_strip_elements(
        &self,
        renderer: &mut GlesRenderer,
        shapes: Option<&Shapes>,
        strip: &tileception_core::TabStrip,
        focus: Option<WindowId>,
    ) -> Vec<OutputElement> {
        let t = &self.theme;
        let mut out: Vec<OutputElement> = Vec::new();
        let pad = 3.0;
        let inner = strip.rect.inset(pad);
        let n = strip.views.len() as f64;
        let gap = 3.0;
        let tab_w = (inner.w - gap * (n - 1.0)) / n;
        let font_size = t.font_size.min((inner.h as f32 * 0.62).max(6.0));
        for (i, win) in strip.views.iter().enumerate() {
            let tab = Rect::new(inner.x + i as f64 * (tab_w + gap), inner.y, tab_w, inner.h);
            let active = i == strip.selected;
            let focused = active && Some(*win) == focus;
            let title = self.windows.get(win).map(crate::state::window_title).unwrap_or_default();
            let text_colour = if active { t.text } else { t.muted };
            // App mark: a small rounded square, accent on the shown tab.
            let mark = 16.0;
            let mark_rect = Rect::new(tab.x + 8.0, tab.y + (tab.h - mark) / 2.0, mark, mark);
            let close_w = if active { 16.0 } else { 0.0 };
            let text_x = mark_rect.right() + 6.0;
            let text_max = (tab.right() - close_w - 6.0 - text_x) as f32;
            let mut text = self.text.borrow_mut();
            if let Some(img) = text.line(&title, &t.font, font_size, text_colour, text_max, active) {
                let y = tab.y + (tab.h - img.height as f64) / 2.0;
                push_image(&mut out, renderer, &img.buffer, (text_x, y));
            }
            if active {
                if let Some(img) = text.line("\u{00d7}", &t.font, font_size, t.muted, 20.0, false) {
                    let x = tab.right() - close_w + (close_w - img.width as f64) / 2.0 - 2.0;
                    let y = tab.y + (tab.h - img.height as f64) / 2.0;
                    push_image(&mut out, renderer, &img.buffer, (x, y));
                }
            }
            drop(text);
            let mark_shape = if active {
                Shape { radius: 2.0, fill: (t.accent, t.accent_end), ..Default::default() }
            } else {
                Shape::filled(t.muted.alpha(0.5), 2.0)
            };
            let icon =
                self.windows.get(win).and_then(|window| self.launcher.app_icon(&crate::state::app_id(window), 16));
            if let Some(icon) = icon {
                push_image(&mut out, renderer, &icon, (mark_rect.x, mark_rect.y));
            } else {
                push_shape(&mut out, shapes, &mark_rect, mark_shape);
            }
            if active {
                // The shown tab: tinted, with an accent line along the bottom.
                let underline = Rect::new(tab.x + 2.0, tab.bottom() - 2.0, tab.w - 4.0, 2.0);
                let line = Shape { radius: 1.0, fill: (t.accent, t.accent_end), ..Default::default() };
                push_shape(&mut out, shapes, &underline, line);
                let tint = if focused { t.accent.alpha(0.12) } else { t.surface_raised };
                push_shape(&mut out, shapes, &tab, Shape::filled(tint, 4.0));
            }
        }
        push_shape(&mut out, shapes, &strip.rect, Shape::filled(t.background.alpha(0.6), t.radius));
        out
    }

    /// The pointer, drawn by us when Tileception owns the screen: the client's own
    /// cursor surface when it set one, otherwise the theme's arrow.
    pub fn cursor_elements(&mut self, udev: &mut Udev) -> Vec<OutputElement> {
        use smithay::input::pointer::{CursorImageAttributes, CursorImageStatus};
        if matches!(self.lock, crate::lock::LockState::BuiltIn) {
            self.cursor_status = CursorImageStatus::default_named();
        }
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
    fn picture(&self, renderer: &mut GlesRenderer, id: WindowId, content: &Rect) -> Vec<OutputElement> {
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

pub fn push_shape(out: &mut Vec<OutputElement>, shapes: Option<&Shapes>, rect: &Rect, shape: Shape) {
    if let Some(e) = shapes.and_then(|s| s.element(rect, shape)) {
        out.push(OutputElement::Shape(e));
    }
}

pub fn push_image(
    out: &mut Vec<OutputElement>,
    renderer: &mut GlesRenderer,
    buffer: &smithay::backend::renderer::element::memory::MemoryRenderBuffer,
    at: (f64, f64),
) {
    let at = (at.0.round(), at.1.round());
    if let Ok(e) =
        MemoryRenderBufferRenderElement::from_buffer(renderer, at, buffer, None, None, None, Kind::Unspecified)
    {
        out.push(OutputElement::Memory(e));
    }
}

fn physical(r: &Rect) -> Rectangle<i32, Physical> {
    let x0 = r.x.round() as i32;
    let y0 = r.y.round() as i32;
    let x1 = r.right().round() as i32;
    let y1 = r.bottom().round() as i32;
    Rectangle::new((x0, y0).into(), ((x1 - x0).max(0), (y1 - y0).max(0)).into())
}

pub fn solid(r: &Rect, colour: [f32; 4]) -> OutputElement {
    OutputElement::Solid(SolidColorRenderElement::new(Id::new(), physical(r), 0usize, colour, Kind::Unspecified))
}
