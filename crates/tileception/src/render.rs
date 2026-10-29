//! Builds the render elements for one frame, in the Tide style.
//!
//! Every window is drawn from its live buffer. While the camera moves, or in
//! Overview, a window is scaled into the rectangle the layout gives it. Once
//! everything has settled and the client has drawn at the new size, the window
//! is drawn 1:1 and receives pointer input ("live").
//!
//! Elements are listed topmost first. A window's frame (rounded corners,
//! border, focus glow) is drawn just above the window itself.
//!
//! Positions are worked out in logical pixels and turned into physical ones
//! here, as elements are made: client surfaces are enlarged by the output's
//! scale, and the compositor's own images are already rasterised at it.

use smithay::{
    backend::renderer::{
        element::{
            AsRenderElements, Id, Kind,
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            texture::TextureRenderElement,
            utils::{CropRenderElement, RescaleRenderElement},
        },
        gles::{GlesRenderer, GlesTexture, element::PixelShaderElement},
    },
    utils::{IsAlive, Physical, Point, Rectangle, Scale},
    wayland::shell::wlr_layer::Layer,
};
use tileception_core::{Node, NodeId, Rect, WindowId, WindowRole};

use crate::{
    cursor::Cursor,
    shapes::{Shape, Shapes},
    state::Tileception,
    theme::Colour,
};

smithay::backend::renderer::element::render_elements! {
    pub OutputElement<=GlesRenderer>;
    Live = RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    Scaled = CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    Solid = SolidColorRenderElement,
    Memory = MemoryRenderBufferRenderElement<GlesRenderer>,
    Shape = PixelShaderElement,
    Texture = TextureRenderElement<GlesTexture>,
}

/// How long a session takes to fade in from black, in seconds: it follows the
/// greeter's fade out, so logging in is no hard cut.
const FADE_IN: f64 = 0.35;

/// The session's first frames fading in from black.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum FadeIn {
    #[default]
    Off,
    /// Until the first frame is drawn.
    Waiting,
    Since(std::time::Instant),
}

/// How one window is drawn this frame.
#[derive(Debug, Clone, Copy)]
pub struct Drawn {
    pub window_id: WindowId,
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
    /// Live but larger than its slot, so cut off at `content`.
    pub clipped: bool,
}

/// How many pixels a resting tiled window may be too big for its slot and still be
/// drawn live, cut off, rather than as a scaled preview.
const MAX_CLIP: f64 = 32.0;

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
        let focus = self.world.focus().filter(|_| self.is_active_output());

        if let Some(window_id) = self.fullscreen
            && let Some(window) = self.windows.get(&window_id)
        {
            let geo = window.geometry();
            let out = self.output_rect();
            let content = Rect::new(out.x, out.y, geo.size.w as f64, geo.size.h as f64);
            let origin = (content.x - geo.loc.x as f64, content.y - geo.loc.y as f64).into();
            let leaf = self.world.tree().leaf_of(window_id).unwrap_or(self.world.tree().root());
            return vec![Drawn {
                window_id,
                role: WindowRole::Tiled,
                leaf,
                slot: out,
                content,
                origin,
                scale: (1.0, 1.0),
                live: true,
                clipped: false,
            }];
        }

        let dragged = self.move_drag.filter(|drag| !drag.free).map(|drag| drag.window);
        let mut out = Vec::new();
        for placement in self.world.placements(view) {
            if !placement.rect.intersects(&screen) || Some(placement.window) == dragged {
                continue;
            }
            let Some(window) = self.windows.get(&placement.window) else { continue };
            let geo = window.geometry();
            if geo.size.w <= 0 || geo.size.h <= 0 {
                continue;
            }
            let (geometry_width, geometry_height) = (geo.size.w as f64, geo.size.h as f64);
            // Floating windows hang still while the camera moves.
            let moving = moving && placement.role == WindowRole::Tiled;
            let (over_w, over_h) = (geometry_width - placement.rect.w, geometry_height - placement.rect.h);
            let tiled = placement.role == WindowRole::Tiled;
            if tiled && !moving && !overview && over_w.max(over_h) > 0.0 && over_w.max(over_h) <= MAX_CLIP {
                // Just too big, usually from a minimum size: drawn 1:1 from
                // the slot's top left and cut off at its edges, so it stays
                // sharp and usable instead of becoming a preview.
                let content = Rect::new(
                    placement.rect.x,
                    placement.rect.y,
                    geometry_width.min(placement.rect.w),
                    geometry_height.min(placement.rect.h),
                );
                let origin = (content.x - geo.loc.x as f64, content.y - geo.loc.y as f64).into();
                out.push(Drawn {
                    window_id: placement.window,
                    role: placement.role,
                    leaf: placement.leaf,
                    slot: placement.rect,
                    content,
                    origin,
                    scale: (1.0, 1.0),
                    live: true,
                    clipped: true,
                });
                continue;
            }
            let (scale_x, scale_y) = if moving {
                // Stretch into the moving slot; the client catches up after.
                (placement.rect.w / geometry_width, placement.rect.h / geometry_height)
            } else {
                // Fit without enlarging: a window too big for its slot shows
                // as a preview, a smaller one sits in the middle.
                let uniform_scale =
                    (placement.rect.w / geometry_width).min(placement.rect.h / geometry_height).min(1.0);
                (uniform_scale, uniform_scale)
            };
            let content = Rect::new(
                placement.rect.x + (placement.rect.w - geometry_width * scale_x) / 2.0,
                placement.rect.y + (placement.rect.h - geometry_height * scale_y) / 2.0,
                geometry_width * scale_x,
                geometry_height * scale_y,
            );
            let origin = (content.x - geo.loc.x as f64 * scale_x, content.y - geo.loc.y as f64 * scale_y).into();
            let live = !moving && !overview && scale_x >= 0.999 && scale_y >= 0.999;
            out.push(Drawn {
                window_id: placement.window,
                role: placement.role,
                leaf: placement.leaf,
                slot: placement.rect,
                content,
                origin,
                scale: (scale_x, scale_y),
                live,
                clipped: false,
            });
        }
        // Floating windows above tiles, the focused one above its peers.
        out.sort_by_key(|drawn| (drawn.role != WindowRole::Floating, Some(drawn.window_id) != focus));
        out
    }

    /// The whole frame: everything below plus the wallpaper at the bottom.
    pub fn frame_elements(&mut self, renderer: &mut GlesRenderer, now: f64) -> Vec<OutputElement> {
        if let Some(shapes) = self.shapes.get() {
            shapes.next_frame();
        }
        if self.draws_lock_screen() {
            self.wallpaper_buffer();
        }
        let wallpaper = if self.is_locked() { None } else { self.wallpaper_buffer() };
        let mut elements = self.render_elements(renderer, now);
        if let Some(buffer) = wallpaper {
            let output_rect = self.output_rect();
            push_image(&mut elements, renderer, &buffer, (output_rect.x, output_rect.y), self.scale);
        }
        if self.fade_in == FadeIn::Waiting {
            self.fade_in = FadeIn::Since(std::time::Instant::now());
        }
        if let FadeIn::Since(started) = self.fade_in {
            let shown = crate::ui::ease_out(started.elapsed().as_secs_f64() / FADE_IN);
            if shown >= 1.0 {
                self.fade_in = FadeIn::Off;
            } else {
                elements.insert(0, solid(&self.output_rect(), [0.0, 0.0, 0.0, (1.0 - shown) as f32], self.scale));
            }
        }
        elements
    }

    pub fn render_elements(&self, renderer: &mut GlesRenderer, now: f64) -> Vec<OutputElement> {
        if self.is_locked() {
            // Only the locker (and the volume card); black where it has not drawn yet.
            let mut elements = self.osd_elements(renderer);
            elements.extend(self.lock_elements(renderer));
            elements.push(solid(&self.output_rect(), [0.0, 0.0, 0.0, 1.0], self.scale));
            return elements;
        }
        let shapes = self.shapes(renderer);
        let theme = &self.theme;
        let drawn_windows = self.drawn_windows(now);
        let focus = self.world.focus().filter(|_| self.is_active_output());
        let view = self.world.view(now);
        let active = self.is_active_output();
        let mut elements: Vec<OutputElement> = Vec::new();
        if active {
            elements.extend(self.osd_elements(renderer));
            elements.extend(self.power_menu_elements(renderer));
        }
        // Overlay layers (slurp, launchers) go over Tileception's own panels,
        // so a region pick can include the Doctor or the notifications.
        elements.extend(self.layer_elements(renderer, &[Layer::Overlay]));
        if active {
            elements.extend(self.tray_elements(renderer));
            // The welcome screen is part of the desktop: notifications go over it.
            if !self.launcher.home {
                elements.extend(self.launcher_elements(renderer));
            }
            elements.extend(self.notification_elements(renderer));
            elements.extend(self.doctor_elements(renderer));
            // Over the strip, which it can stand on.
            elements.extend(self.duck_elements(renderer));
            elements.extend(self.strip_elements(renderer));
            if self.launcher.home {
                elements.extend(self.launcher_elements(renderer));
            }
            elements.extend(self.selection_elements(renderer));
        }
        elements.extend(self.bar_elements(renderer));

        // Then bars. A fullscreen window covers everything but the above.
        if self.fullscreen.is_some() {
            for drawn in &drawn_windows {
                if let Some(window) = self.windows.get(&drawn.window_id) {
                    let origin = physical_point((drawn.origin.x, drawn.origin.y), self.scale);
                    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                        window.render_elements(renderer, origin, Scale::from(1.0), 1.0);
                    elements.extend(enlarged(surfaces, origin, self.scale));
                }
            }
            return elements;
        }
        elements.extend(self.layer_elements(renderer, &[Layer::Top]));

        // X11 menus and tooltips, at the place they chose.
        for overlay in self.x11_overlays.iter().rev().filter(|_| active) {
            let Some(surface) = overlay.wl_surface() else { continue };
            let loc = overlay.geometry().loc;
            let origin = physical_point((loc.x as f64, loc.y as f64), self.scale);
            let surfaces = render_elements_from_surface_tree(renderer, &surface, origin, 1.0, 1.0, Kind::Unspecified);
            elements.extend(enlarged(surfaces, origin, self.scale));
        }

        // Then the window being dragged, then where it would land.
        if let Some(drag) = self.move_drag.filter(|drag| !drag.free) {
            let rect = Rect::new(
                self.pointer.x - drag.grab.0 * drag.size.0,
                self.pointer.y - drag.grab.1 * drag.size.1,
                drag.size.0,
                drag.size.1,
            );
            push_shape(&mut elements, shapes.as_ref(), &rect, self.frame(true, false));
            elements.extend(self.picture(renderer, drag.window, &rect));
            push_shape(
                &mut elements,
                shapes.as_ref(),
                &rect,
                Shape { glow_radius: 24.0, glow: Colour([0.0, 0.0, 0.0, 0.45]), ..Default::default() },
            );
            if let Some(preview) = drag.target.and_then(|target| self.world.drop_preview(target, view)) {
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
        // Menus and tooltips go above every window and frame, even when they
        // reach over a neighbour or past their own window's border.
        let above_windows = elements.len();
        let mut popups = Vec::new();
        for drawn in &drawn_windows {
            let Some(window) = self.windows.get(&drawn.window_id) else { continue };
            let focused = Some(drawn.window_id) == focus && self.windows.len() > 1;
            let floating = drawn.role == WindowRole::Floating;
            // A tabbed tile's frame takes in its strip of tabs.
            let mut frame_rect = drawn.content;
            if let Some(strip) = strips.iter().find(|strip| strip.tile == drawn.leaf) {
                frame_rect =
                    Rect::new(drawn.content.x, strip.rect.y, drawn.content.w, drawn.content.bottom() - strip.rect.y);
            }
            push_shape(&mut elements, shapes.as_ref(), &frame_rect, self.frame(focused, !floating));
            if drawn.live {
                popups.extend(popup_elements(renderer, window, drawn, self.scale));
                elements.extend(live(renderer, window, drawn, self.scale));
            } else {
                elements.extend(self.picture(renderer, drawn.window_id, &drawn.content));
            }
            if floating {
                // Floating windows cast a soft shadow onto the layout.
                let shadow = Shape {
                    radius: theme.radius,
                    glow_radius: 28.0,
                    glow: Colour([0.0, 0.0, 0.0, 0.4]),
                    ..Default::default()
                };
                push_shape(&mut elements, shapes.as_ref(), &drawn.content, shadow);
            }
        }

        elements.splice(above_windows..above_windows, popups);

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
            let Some(rect) = self.world.node_rect(leaf, view) else { continue };
            let rect = rect.inset(half_gap);
            if !rect.intersects(&self.world.screen()) {
                continue;
            }
            push_shape(&mut elements, shapes.as_ref(), &rect, Shape::filled(theme.surface, theme.radius));
        }

        // Bottommost: desktop widgets and wallpaper.
        elements.extend(self.layer_elements(renderer, &[Layer::Bottom, Layer::Background]));
        elements
    }

    /// The compiled shape shader, made on first use with this renderer.
    /// Shapes come out at the output's scale.
    pub fn shapes(&self, renderer: &mut GlesRenderer) -> Option<Shapes> {
        if self.shapes.get().is_none() {
            match Shapes::compile(renderer) {
                Ok(compiled) => {
                    let _ = self.shapes.set(compiled);
                }
                Err(err) => {
                    tracing::error!("shape shader: {err}");
                    return None;
                }
            }
        }
        self.shapes.get().cloned().map(|shapes| shapes.at_scale(self.scale))
    }

    /// A window's frame: rounded corners masked in the background colour (for
    /// tiles), a hairline border, and the accent with a glow when focused.
    /// On a laptop the accent follows the battery: green while charging,
    /// yellow to red as it runs low.
    fn frame(&self, focused: bool, mask_corners: bool) -> Shape {
        let theme = &self.theme;
        let accent = self.battery_border().unwrap_or(theme.accent);
        Shape {
            radius: theme.radius,
            border_width: 1.0,
            border: if focused { accent } else { theme.border },
            glow_radius: if focused { theme.glow } else { 0.0 },
            glow: accent.alpha(0.27),
            corner_mask: if mask_corners { theme.background } else { Colour::TRANSPARENT },
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
        let theme = &self.theme;
        let mut out: Vec<OutputElement> = Vec::new();
        let pad = 3.0;
        let inner = strip.rect.inset(pad);
        let count = strip.views.len() as f64;
        let gap = 3.0;
        let tab_w = (inner.w - gap * (count - 1.0)) / count;
        let font_size = theme.font_size.min((inner.h as f32 * 0.62).max(6.0));
        for (index, win) in strip.views.iter().enumerate() {
            let tab = Rect::new(inner.x + index as f64 * (tab_w + gap), inner.y, tab_w, inner.h);
            let active = index == strip.selected;
            let focused = active && Some(*win) == focus;
            let title = self.windows.get(win).map(crate::state::window_title).unwrap_or_default();
            let text_colour = if active { theme.text } else { theme.muted };
            // App mark: a small rounded square, accent on the shown tab.
            let mark = 16.0;
            let mark_rect = Rect::new(tab.x + 8.0, tab.y + (tab.h - mark) / 2.0, mark, mark);
            let close_w = if active { 16.0 } else { 0.0 };
            let text_x = mark_rect.right() + 6.0;
            let text_max = (tab.right() - close_w - 6.0 - text_x) as f32;
            let mut text = self.text.borrow_mut();
            if let Some(img) = text.line(&title, &theme.font, font_size, text_colour, text_max, active) {
                let top = tab.y + (tab.h - img.height) / 2.0;
                push_image(&mut out, renderer, &img.buffer, (text_x, top), self.scale);
            }
            if active && let Some(img) = text.line("\u{00d7}", &theme.font, font_size, theme.muted, 20.0, false) {
                let left = tab.right() - close_w + (close_w - img.width) / 2.0 - 2.0;
                let top = tab.y + (tab.h - img.height) / 2.0;
                push_image(&mut out, renderer, &img.buffer, (left, top), self.scale);
            }
            drop(text);
            let mark_shape = if active {
                Shape { radius: 2.0, fill: (theme.accent, theme.accent_end), ..Default::default() }
            } else {
                Shape::filled(theme.muted.alpha(0.5), 2.0)
            };
            let icon =
                self.windows.get(win).and_then(|window| self.launcher.app_icon(&crate::state::app_id(window), 16));
            if let Some(icon) = icon {
                push_image(&mut out, renderer, &icon, (mark_rect.x, mark_rect.y), self.scale);
            } else {
                push_shape(&mut out, shapes, &mark_rect, mark_shape);
            }
            if active {
                // The shown tab: tinted, with an accent line along the bottom.
                let underline = Rect::new(tab.x + 2.0, tab.bottom() - 2.0, tab.w - 4.0, 2.0);
                let line = Shape { radius: 1.0, fill: (theme.accent, theme.accent_end), ..Default::default() };
                push_shape(&mut out, shapes, &underline, line);
                let tint = if focused { theme.accent.alpha(0.12) } else { theme.surface_raised };
                push_shape(&mut out, shapes, &tab, Shape::filled(tint, 4.0));
            }
        }
        push_shape(&mut out, shapes, &strip.rect, Shape::filled(theme.background.alpha(0.6), theme.radius));
        out
    }

    /// The pointer, drawn by us when Tileception owns the screen: the client's own
    /// cursor surface when it set one, otherwise the theme's arrow.
    pub fn cursor_elements(&mut self, renderer: &mut GlesRenderer, cursor: &mut Cursor) -> Vec<OutputElement> {
        use smithay::input::pointer::{CursorImageAttributes, CursorImageStatus};
        if self.draws_lock_screen() {
            self.cursor_status = CursorImageStatus::default_named();
        }
        if let CursorImageStatus::Surface(surface) = &self.cursor_status
            && !surface.alive()
        {
            self.cursor_status = CursorImageStatus::default_named();
        }
        let pos = self.pointer;
        let scale = self.scale;
        if cursor.scale != scale {
            *cursor = Cursor::load(scale);
        }
        match &self.cursor_status {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                let hotspot = smithay::wayland::compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<std::sync::Mutex<CursorImageAttributes>>()
                        .and_then(|attributes| attributes.lock().ok().map(|attributes| attributes.hotspot))
                        .unwrap_or_default()
                });
                let origin = physical_point((pos.x - hotspot.x as f64, pos.y - hotspot.y as f64), scale);
                let surfaces = render_elements_from_surface_tree(renderer, surface, origin, 1.0, 1.0, Kind::Cursor);
                enlarged(surfaces, origin, scale).collect()
            }
            CursorImageStatus::Named(_) => {
                // The theme's image is loaded at the scale, its hotspot in its own pixels.
                let hot = cursor.hotspot;
                let position = ((pos.x * scale).round() - hot.x as f64, (pos.y * scale).round() - hot.y as f64);
                MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    position,
                    &cursor.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                )
                .map(|element| vec![OutputElement::Memory(element)])
                .unwrap_or_default()
            }
        }
    }

    /// A window scaled into `content`, which is where its visible geometry goes.
    pub fn picture(&self, renderer: &mut GlesRenderer, window_id: WindowId, content: &Rect) -> Vec<OutputElement> {
        self.picture_faded(renderer, window_id, content, 1.0)
    }

    /// A window picture at some opacity.
    pub fn picture_faded(
        &self,
        renderer: &mut GlesRenderer,
        window_id: WindowId,
        content: &Rect,
        alpha: f32,
    ) -> Vec<OutputElement> {
        let Some(window) = self.windows.get(&window_id) else { return Vec::new() };
        let geo = window.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            return Vec::new();
        }
        // From the window's logical size to the content's physical one.
        let output_scale = self.scale;
        let scale =
            Scale { x: content.w / geo.size.w as f64 * output_scale, y: content.h / geo.size.h as f64 * output_scale };
        let origin = physical_point(
            (
                content.x - geo.loc.x as f64 * content.w / geo.size.w as f64,
                content.y - geo.loc.y as f64 * content.h / geo.size.h as f64,
            ),
            output_scale,
        );
        let crop = physical(content, output_scale);
        let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            window.render_elements(renderer, origin, Scale::from(1.0), alpha);
        surfaces
            .into_iter()
            .filter_map(|surface| {
                let scaled = RescaleRenderElement::from_element(surface, origin, scale);
                CropRenderElement::from_element(scaled, 1.0, crop).map(OutputElement::Scaled)
            })
            .collect()
    }
}

/// A live window's popups, which may reach past the window's own rectangle.
/// X11 windows draw their menus as separate overlays instead.
fn popup_elements(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    drawn: &Drawn,
    scale: f64,
) -> Vec<OutputElement> {
    let Some(toplevel) = window.toplevel() else { return Vec::new() };
    smithay::desktop::PopupManager::popups_for_surface(toplevel.wl_surface())
        .flat_map(|(popup, offset)| {
            let loc = (window.geometry().loc + offset - popup.geometry().loc).to_f64();
            let origin = physical_point((drawn.origin.x + loc.x, drawn.origin.y + loc.y), scale);
            let surfaces =
                render_elements_from_surface_tree(renderer, popup.wl_surface(), origin, 1.0, 1.0, Kind::Unspecified);
            enlarged(surfaces, origin, scale)
        })
        .collect()
}

/// A live window without its popups, cut off at `drawn.content` when it is
/// larger than its slot.
fn live(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    drawn: &Drawn,
    scale: f64,
) -> Vec<OutputElement> {
    let origin = physical_point((drawn.origin.x, drawn.origin.y), scale);
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = match window.toplevel() {
        Some(toplevel) => {
            render_elements_from_surface_tree(renderer, toplevel.wl_surface(), origin, 1.0, 1.0, Kind::Unspecified)
        }
        None => window.render_elements(renderer, origin, Scale::from(1.0), 1.0),
    };
    if !drawn.clipped {
        return enlarged(surfaces, origin, scale).collect();
    }
    let crop = physical(&drawn.content, scale);
    surfaces
        .into_iter()
        .filter_map(|surface| {
            let scaled = RescaleRenderElement::from_element(surface, origin, scale);
            CropRenderElement::from_element(scaled, 1.0, crop).map(OutputElement::Scaled)
        })
        .collect()
}

/// Client surfaces made at `origin` with their offsets in logical pixels,
/// enlarged by the output's scale about that point. A client that drew for
/// the scale has as many buffer pixels as this takes, so it stays sharp.
pub fn enlarged(
    surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    origin: Point<i32, Physical>,
    scale: f64,
) -> impl Iterator<Item = OutputElement> {
    surfaces
        .into_iter()
        .map(move |surface| OutputElement::Live(RescaleRenderElement::from_element(surface, origin, scale)))
}

pub fn push_shape(out: &mut Vec<OutputElement>, shapes: Option<&Shapes>, rect: &Rect, shape: Shape) {
    if let Some(element) = shapes.and_then(|shapes| shapes.element(rect, shape)) {
        out.push(OutputElement::Shape(element));
    }
}

/// An image rasterised at the output's scale, its top left at `position` in
/// logical pixels, drawn one buffer pixel to one screen pixel.
pub fn push_image(
    out: &mut Vec<OutputElement>,
    renderer: &mut GlesRenderer,
    buffer: &smithay::backend::renderer::element::memory::MemoryRenderBuffer,
    position: (f64, f64),
    scale: f64,
) {
    let position = ((position.0 * scale).round(), (position.1 * scale).round());
    if let Ok(element) =
        MemoryRenderBufferRenderElement::from_buffer(renderer, position, buffer, None, None, None, Kind::Unspecified)
    {
        out.push(OutputElement::Memory(element));
    }
}

/// A point in logical pixels, in whole physical ones.
pub fn physical_point(point: (f64, f64), scale: f64) -> Point<i32, Physical> {
    ((point.0 * scale).round() as i32, (point.1 * scale).round() as i32).into()
}

/// A rectangle in logical pixels, in whole physical ones.
pub fn physical(rect: &Rect, scale: f64) -> Rectangle<i32, Physical> {
    let left = (rect.x * scale).round() as i32;
    let top = (rect.y * scale).round() as i32;
    let right = (rect.right() * scale).round() as i32;
    let bottom = (rect.bottom() * scale).round() as i32;
    Rectangle::new((left, top).into(), ((right - left).max(0), (bottom - top).max(0)).into())
}

pub fn solid(rect: &Rect, colour: [f32; 4], scale: f64) -> OutputElement {
    OutputElement::Solid(SolidColorRenderElement::new(
        Id::new(),
        physical(rect, scale),
        0usize,
        colour,
        Kind::Unspecified,
    ))
}
