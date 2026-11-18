//! Frost: Tileception's own panels (the bar, launcher, notifications, menus
//! and cards) turn translucent and blur what is behind them. Windows that
//! ask for blur behind them (`blur.rs`) get it too. Off unless
//! `[theme] frost = true`.
//!
//! Each frame, the panels that want frost register their background shape
//! with [`Frost::region`]. What lies under the panels (the windows, layers and
//! wallpaper) is rendered into an offscreen texture, only the elements near a
//! panel and only where they changed, and blurred with a dual Kawase blur at
//! half, quarter and eighth size. Each panel then gets a backdrop: a piece of
//! the blurred picture cut to its rounded shape, drawn just beneath its
//! background. Panels do not blur one another.
//!
//! A client's blur is made the same way from its own picture: everything
//! beneath that window, so a floating terminal over a browser shows the
//! browser frosted. Windows are done from the bottom up, so one blurred
//! window over another shows the lower one with its frost.
//!
//! The blur is redone only when something changed near a panel, so a still
//! desktop with the bar open draws nothing new.

use std::collections::{HashMap, HashSet};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Color32F, Frame, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker,
            element::{
                Element, Id, Kind, RenderElement,
                utils::{Relocate, RelocateRenderElement},
            },
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType,
            },
            utils::{CommitCounter, DamageSet, OpaqueRegions, draw_render_elements},
        },
    },
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform},
};
use tileception_core::Rect;

use crate::{blur::subtract_blur_rect, render::OutputElement, state::Tileception};

/// How opaque frosted panel backgrounds are.
pub const PANEL_OPACITY: f32 = 0.72;

/// How far the blur reaches, in physical pixels: changes this close to a
/// panel redo its blur.
const REACH: i32 = 96;
const OFFSET: f32 = 2.0;
// Maximum source-texel distance of each pass, including bilinear filtering.
const RADII: [i32; 5] = [2, 2, 2, 3, 3];

const HEADER: &str = r#"
//_DEFINES_
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform sampler2D tex;
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
"#;

const SAMPLING: &str = r#"
uniform vec2 u_halfpixel;
uniform vec2 u_limit;
uniform float u_offset;
vec4 sample_at(vec2 point) {
    return texture2D(tex, clamp(point, u_halfpixel, u_limit));
}
"#;

/// Dual Kawase, the step down to half size.
const DOWNSAMPLE: &str = r#"
void main() {
    vec2 h = u_halfpixel * u_offset;
    vec4 sum = sample_at(v_coords) * 4.0;
    sum += sample_at(v_coords - h);
    sum += sample_at(v_coords + h);
    sum += sample_at(v_coords + vec2(h.x, -h.y));
    sum += sample_at(v_coords - vec2(h.x, -h.y));
    gl_FragColor = sum / 8.0;
}
"#;

/// Dual Kawase, the step up to twice the size.
const UPSAMPLE: &str = r#"
void main() {
    vec2 h = u_halfpixel * u_offset;
    vec4 sum = sample_at(v_coords + vec2(-h.x * 2.0, 0.0));
    sum += sample_at(v_coords + vec2(-h.x, h.y)) * 2.0;
    sum += sample_at(v_coords + vec2(0.0, h.y * 2.0));
    sum += sample_at(v_coords + vec2(h.x, h.y)) * 2.0;
    sum += sample_at(v_coords + vec2(h.x * 2.0, 0.0));
    sum += sample_at(v_coords + vec2(h.x, -h.y)) * 2.0;
    sum += sample_at(v_coords + vec2(0.0, -h.y * 2.0));
    sum += sample_at(v_coords + vec2(-h.x, -h.y)) * 2.0;
    gl_FragColor = sum / 12.0;
}
"#;

/// The blurred picture cut to a rounded rectangle, a little more saturated
/// so colours behind it still read. The rectangle is the panel, or the
/// window a client's blur belongs to, which a piece of its region may only
/// partly cover: `u_shape` is where it lies, relative to the backdrop.
const BACKDROP: &str = r#"
uniform vec4 u_src;
uniform vec2 u_rect_size;
uniform vec4 u_shape;
uniform float u_radius;
uniform float u_saturation;

float rounded_box(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    vec2 p = (v_coords - u_src.xy) / u_src.zw * u_rect_size;
    vec2 half_size = u_shape.zw * 0.5;
    vec2 centre = u_shape.xy + half_size;
    float inside = u_radius == 0.0 ? 1.0 : clamp(0.5 - rounded_box(p - centre, half_size, u_radius), 0.0, 1.0);
    vec4 color = texture2D(tex, v_coords);
    float grey = dot(color.rgb, vec3(0.2126, 0.7152, 0.0722));
    color.rgb = mix(vec3(grey), color.rgb, u_saturation);
    gl_FragColor = vec4(color.rgb, 1.0) * inside * alpha;
}
"#;

/// A panel background that wants frost behind it, registered while drawing.
#[derive(Debug, Clone)]
pub struct Region {
    /// The id of the panel's background element; the backdrop goes just beneath it.
    pub panel: Id,
    /// Logical pixels.
    pub rect: Rect,
    pub radius: f32,
    pub alpha: f32,
    /// A popup, which blurs the frosted panels it covers too.
    pub over_panels: bool,
}

#[derive(Debug)]
struct SurfaceRegion {
    anchor: Id,
    rects: Vec<Rectangle<i32, Physical>>,
    /// The window's rounded outline, which the blur is cut to.
    outline: Option<(Rectangle<i32, Physical>, f32)>,
}

#[derive(Debug)]
struct Programs {
    downsample: GlesTexProgram,
    upsample: GlesTexProgram,
    backdrop: GlesTexProgram,
}

/// One output's picture of what is under the panels, and its blur.
#[derive(Debug)]
struct Cache {
    size: Size<i32, Physical>,
    clear: [f32; 4],
    tracker: OutputDamageTracker,
    rendered_once: bool,
    final_level: GlesTexture,
    coverage: Vec<Rectangle<i32, Physical>>,
    /// Bumped whenever the blur is redone.
    generation: u64,
    damage: Vec<PhysicalRect>,
    /// Backdrops by the panel background they go under, kept so an
    /// unchanged backdrop is not redrawn.
    backdrops: HashMap<Id, Kept>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Picture {
    Panels,
    OverPanels,
    Surface(Id, usize),
}

#[derive(Debug)]
struct Kept {
    backdrop: Id,
    commit: CommitCounter,
    key: (u64, [i32; 4], u32, u32, Option<[i32; 4]>),
    mask: Option<Vec<Rectangle<i32, Physical>>>,
    previous_damage: Option<(CommitCounter, Vec<PhysicalRect>)>,
}

/// The frost state kept across frames.
#[derive(Debug, Default)]
pub struct Frost {
    regions: Vec<Region>,
    surfaces: Vec<SurfaceRegion>,
    programs: Option<Programs>,
    failed: bool,
    outputs: HashMap<String, HashMap<Picture, Cache>>,
    scratch: [Option<GlesTexture>; 2],
}

impl Frost {
    /// Starts collecting the regions of a new frame.
    fn begin(&mut self) {
        self.regions.clear();
        self.surfaces.clear();
    }

    pub fn region(&mut self, region: Region) {
        self.regions.push(region);
    }

    /// A client surface that asked for blur behind it; `outline` is the
    /// rounded rectangle of its window (physical pixels and radius).
    pub fn surface(
        &mut self,
        anchor: Id,
        rects: Vec<Rectangle<i32, Physical>>,
        outline: Option<(Rectangle<i32, Physical>, f32)>,
    ) {
        self.surfaces.push(SurfaceRegion { anchor, rects, outline });
    }

    /// Lets go of every texture, when frost is turned off.
    pub fn release(&mut self) {
        self.outputs.clear();
        self.scratch = Default::default();
        self.regions.clear();
        self.surfaces.clear();
    }

    pub fn remove_output(&mut self, name: &str) {
        self.outputs.remove(name);
        self.scratch = Default::default();
    }

    fn init_programs(&mut self, renderer: &mut GlesRenderer) -> bool {
        if self.programs.is_none() && !self.failed {
            match compile(renderer) {
                Ok(programs) => self.programs = Some(programs),
                Err(error) => {
                    tracing::error!("frost shaders: {error}");
                    self.failed = true;
                }
            }
            return true;
        }
        false
    }
}

fn compile(renderer: &mut GlesRenderer) -> Result<Programs, GlesError> {
    let kawase = [
        UniformName::new("u_halfpixel", UniformType::_2f),
        UniformName::new("u_limit", UniformType::_2f),
        UniformName::new("u_offset", UniformType::_1f),
    ];
    let downsample =
        renderer.compile_custom_texture_shader(format!("#version 100\n{HEADER}{SAMPLING}{DOWNSAMPLE}"), &kawase)?;
    let upsample =
        renderer.compile_custom_texture_shader(format!("#version 100\n{HEADER}{SAMPLING}{UPSAMPLE}"), &kawase)?;
    let backdrop = renderer.compile_custom_texture_shader(
        format!("#version 100\n{HEADER}{BACKDROP}"),
        &[
            UniformName::new("u_src", UniformType::_4f),
            UniformName::new("u_rect_size", UniformType::_2f),
            UniformName::new("u_shape", UniformType::_4f),
            UniformName::new("u_radius", UniformType::_1f),
            UniformName::new("u_saturation", UniformType::_1f),
        ],
    )?;
    Ok(Programs { downsample, upsample, backdrop })
}

/// A piece of the blurred picture, cut to a panel's shape.
#[derive(Debug, Clone)]
pub struct Backdrop {
    element_id: Id,
    commit: CommitCounter,
    texture: GlesTexture,
    src: Rectangle<f64, BufferCoords>,
    geometry: Rectangle<i32, Physical>,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
    alpha: f32,
    /// The client's exact mask, relative to the backdrop's geometry.
    mask: Option<Vec<Rectangle<i32, Physical>>>,
    previous_damage: Option<(CommitCounter, Vec<PhysicalRect>)>,
}

impl Element for Backdrop {
    fn id(&self) -> &Id {
        &self.element_id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.src
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else if let Some((previous, damage)) = &self.previous_damage
            && Some(*previous) == commit
        {
            DamageSet::from_slice(damage)
        } else {
            DamageSet::from_slice(&[Rectangle::from_size(self.geometry(scale).size)])
        }
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for Backdrop {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let masked;
        let damage = if let Some(mask) = &self.mask {
            masked = damage
                .iter()
                .flat_map(|rect| mask.iter().filter_map(|part| rect.intersection(*part)))
                .collect::<Vec<_>>();
            if masked.is_empty() {
                return Ok(());
            }
            &masked
        } else {
            damage
        };
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
            Some(&self.program),
            &self.uniforms,
        )
    }
}

impl Tileception {
    /// Whether the theme requests frost and the renderer can draw it.
    pub fn frost_on(&self) -> bool {
        self.theme.frost && self.frost.borrow().programs.is_some()
    }

    pub(crate) fn begin_frost(&mut self, renderer: &mut GlesRenderer) {
        let frost = self.frost.get_mut();
        frost.begin();
        if self.theme.frost && frost.init_programs(renderer) {
            // Bound clients initially get no capabilities. Report the result
            // once initialization succeeds or records a permanent failure.
            self.announce_blur();
        }
    }

    /// Adds a backdrop beneath every frosted panel and blurred client in
    /// `elements` (topmost first); `scene` is where the windows, layers and
    /// wallpaper start.
    pub fn add_frost(&self, renderer: &mut GlesRenderer, elements: &mut Vec<OutputElement>, scene: usize) {
        let enabled = self.frost_on();
        let mut frost = self.frost.borrow_mut();
        if !enabled {
            frost.release();
            return;
        }
        let scale = self.scale;
        let output = Rectangle::from_size(self.output_pixels().into());
        let name = self.output.as_ref().map(|output| output.name()).unwrap_or_default();
        let clear = self.clear_colour();
        let (panels, over_panels) = place_panels(std::mem::take(&mut frost.regions), elements, scene, scale, output);
        let surfaces = std::mem::take(&mut frost.surfaces);
        let mut placed_surfaces = Vec::new();
        for surface in surfaces {
            let Some(index) = anchor_index(elements, &surface.anchor) else { continue };
            let rects = surface.rects.into_iter().filter_map(|rect| rect.intersection(output));
            for (part, (rect, mut mask)) in group_regions(rects).into_iter().enumerate() {
                for piece in &mut mask {
                    piece.loc -= rect.loc;
                }
                let picture = Picture::Surface(surface.anchor.clone(), part);
                let placed = Placed {
                    index,
                    anchor: surface.anchor.clone(),
                    rect,
                    shape: surface.outline.map(|(outline, _)| outline),
                    radius: surface.outline.map_or(0.0, |(_, radius)| radius),
                    alpha: 1.0,
                    over_panels: false,
                    mask: Some(mask),
                };
                placed_surfaces.push((picture, placed));
            }
        }
        if !placed_surfaces.is_empty() {
            cull_hidden_surfaces(&mut placed_surfaces, &panels, &over_panels, elements, scene, output);
        }
        let used: HashSet<_> = placed_surfaces.iter().map(|(picture, _)| picture.clone()).collect();
        let (below, above): (Vec<_>, Vec<_>) = placed_surfaces.into_iter().partition(|(_, part)| part.index >= scene);
        // The panels sample the scene beneath them. Overlay clients in turn
        // sample the scene with those panel backdrops already in place.
        frost.surface_backdrops(renderer, &name, output, clear, elements, below);
        frost.panel_backdrops(renderer, &name, output, clear, elements, scene, (panels, over_panels));
        let above = above
            .into_iter()
            .filter_map(|(picture, mut surface)| {
                surface.index = anchor_index(elements, &surface.anchor)?;
                Some((picture, surface))
            })
            .collect();
        frost.surface_backdrops(renderer, &name, output, clear, elements, above);
        if let Some(pictures) = frost.outputs.get_mut(&name) {
            pictures.retain(|picture, _| !matches!(picture, Picture::Surface(..)) || used.contains(picture));
        }
        if frost.outputs.values().all(HashMap::is_empty) {
            frost.scratch = Default::default();
        }
    }
}

fn anchor_index(elements: &[OutputElement], anchor: &Id) -> Option<usize> {
    elements.iter().position(|element| element.id() == anchor)
}

/// A backdrop's location in the scene and the part of the output it covers.
#[derive(Clone)]
struct Placed {
    index: usize,
    anchor: Id,
    rect: Rectangle<i32, Physical>,
    /// The rounded rectangle to cut to, when it is not `rect` itself.
    shape: Option<Rectangle<i32, Physical>>,
    radius: f32,
    alpha: f32,
    over_panels: bool,
    mask: Option<Vec<Rectangle<i32, Physical>>>,
}

type PhysicalRect = Rectangle<i32, Physical>;

fn place_panels(
    regions: Vec<Region>,
    elements: &[OutputElement],
    scene: usize,
    scale: f64,
    output: PhysicalRect,
) -> (Vec<Placed>, Vec<Placed>) {
    let placed: Vec<_> = regions
        .into_iter()
        .filter_map(|region| {
            let index = anchor_index(&elements[..scene.min(elements.len())], &region.panel)?;
            let rect = crate::render::physical(&region.rect, scale).intersection(output)?;
            Some(Placed {
                index,
                anchor: region.panel,
                rect,
                shape: None,
                radius: region.radius * scale as f32,
                alpha: region.alpha,
                over_panels: region.over_panels,
                mask: None,
            })
        })
        .collect();
    // Popups that overlap another panel sample the composited panels too.
    placed.iter().cloned().partition(|panel| {
        !panel.over_panels || !placed.iter().any(|other| other.index > panel.index && panel.rect.overlaps(other.rect))
    })
}

fn cull_hidden_surfaces(
    surfaces: &mut Vec<(Picture, Placed)>,
    panels: &[Placed],
    over_panels: &[Placed],
    elements: &[OutputElement],
    scene: usize,
    output: PhysicalRect,
) {
    let one = Scale::from(1.0);
    let opaque: Vec<_> = elements
        .iter()
        .map(|element| {
            let origin = element.geometry(one).loc;
            element
                .opaque_regions(one)
                .into_iter()
                .filter_map(|mut rect| {
                    rect.loc += origin;
                    rect.intersection(output)
                })
                .collect::<Vec<_>>()
        })
        .collect();

    // Each capture reads a suffix of the scene, so only opaque elements
    // inside that suffix can hide a backdrop from it.
    let mut samples = vec![(0, output)];
    samples.extend(panels.iter().map(|panel| (scene, sampling_area(panel.rect))));
    if let Some(lowest) = over_panels.iter().map(|panel| panel.index).max() {
        samples.extend(over_panels.iter().map(|panel| (lowest + 1, sampling_area(panel.rect))));
    }
    surfaces.sort_by_key(|(_, part)| part.index);
    surfaces.retain(|(_, part)| {
        let end = part.index + 1;
        let needed = samples.iter().any(|&(start, area)| {
            let Some(area) = part.rect.intersection(area) else { return false };
            if start >= end {
                return false;
            }
            let mut visible: Vec<_> = part
                .mask
                .iter()
                .flatten()
                .filter_map(|piece| {
                    let mut piece = *piece;
                    piece.loc += part.rect.loc;
                    piece.intersection(area)
                })
                .collect();
            for &cut in opaque[start..end].iter().flatten() {
                let Some(remaining) = subtract_blur_rect(visible, cut) else { return true };
                visible = remaining;
                if visible.is_empty() {
                    return false;
                }
            }
            !visible.is_empty()
        });
        if needed {
            samples.push((end, sampling_area(part.rect)));
        }
        needed
    });
}

/// Share a picture when sampled areas meet. Distant pieces get separate pictures.
fn group_regions(rects: impl Iterator<Item = PhysicalRect>) -> Vec<(PhysicalRect, Vec<PhysicalRect>)> {
    let mut groups: Vec<(PhysicalRect, Vec<PhysicalRect>)> = Vec::new();
    for rect in rects {
        let mut bounds = rect;
        let mut mask = vec![rect];
        let mut index = 0;
        while index < groups.len() {
            if sampling_area(bounds).overlaps(sampling_area(groups[index].0)) {
                let (other, parts) = groups.remove(index);
                bounds = bounds.merge(other);
                mask.extend(parts);
                index = 0;
            } else {
                index += 1;
            }
        }
        groups.push((bounds, mask));
    }
    groups
}

impl Frost {
    #[allow(clippy::too_many_arguments, reason = "one frame's worth of state, passed along once")]
    fn panel_backdrops(
        &mut self,
        renderer: &mut GlesRenderer,
        name: &str,
        output: Rectangle<i32, Physical>,
        clear: [f32; 4],
        elements: &mut Vec<OutputElement>,
        scene: usize,
        (placed, over): (Vec<Placed>, Vec<Placed>),
    ) {
        self.backdrops(renderer, (name, Picture::Panels), output, clear, elements, scene, placed);
        if over.is_empty() {
            if let Some(pictures) = self.outputs.get_mut(name) {
                pictures.remove(&Picture::OverPanels);
            }
            return;
        }
        // Found again: the backdrops just added moved them.
        let over: Vec<Placed> = over
            .into_iter()
            .filter_map(|mut panel| {
                panel.index = anchor_index(elements, &panel.anchor)?;
                Some(panel)
            })
            .collect();
        let Some(lowest) = over.iter().map(|panel| panel.index).max() else { return };
        self.backdrops(renderer, (name, Picture::OverPanels), output, clear, elements, lowest + 1, over);
    }

    /// Adds the client backdrops in stacking order so each samples the layers below it.
    fn surface_backdrops(
        &mut self,
        renderer: &mut GlesRenderer,
        name: &str,
        output: Rectangle<i32, Physical>,
        clear: [f32; 4],
        elements: &mut Vec<OutputElement>,
        mut surfaces: Vec<(Picture, Placed)>,
    ) {
        surfaces.sort_by_key(|(_, surface)| std::cmp::Reverse(surface.index));
        for (picture, surface) in surfaces {
            let bounds = sampling_area(surface.rect).intersection(output).unwrap_or(surface.rect);
            self.backdrops(renderer, (name, picture), bounds, clear, elements, surface.index + 1, vec![surface]);
        }
    }

    /// Blurs what lies from `start` on in `elements` into the picture named
    /// `name`, and inserts a piece of it beneath each panel in `placed`.
    #[allow(clippy::too_many_arguments, reason = "one frame's worth of state, passed along once")]
    fn backdrops(
        &mut self,
        renderer: &mut GlesRenderer,
        (name, picture): (&str, Picture),
        bounds: Rectangle<i32, Physical>,
        clear: [f32; 4],
        elements: &mut Vec<OutputElement>,
        start: usize,
        mut placed: Vec<Placed>,
    ) {
        if placed.is_empty() {
            if let Some(pictures) = self.outputs.get_mut(name) {
                pictures.remove(&picture);
            }
            return;
        }
        let start = start.min(elements.len());
        let blurred = match self.update(renderer, (name, &picture), bounds, clear, &elements[start..], &placed) {
            Ok(generation) => generation,
            Err(error) => {
                tracing::warn!("frost: {error}");
                return;
            }
        };
        let Some(programs) = self.programs.as_ref() else { return };
        let program = programs.backdrop.clone();
        let Some(cache) = self.outputs.get_mut(name).and_then(|pictures| pictures.get_mut(&picture)) else { return };
        let texture = &cache.final_level;
        let level_size = level_size(bounds.size, 1);
        let mut used = HashMap::new();
        // Insert from the bottom up so earlier indices stay valid.
        placed.sort_by_key(|region| std::cmp::Reverse(region.index));
        for region in placed {
            let rect = region.rect;
            let shape = region.shape.unwrap_or(rect);
            let local = rect.loc - bounds.loc;
            // The blurred picture is half size, relative to the capture's origin.
            let ratio = (level_size.w as f64 / bounds.size.w as f64, level_size.h as f64 / bounds.size.h as f64);
            let src = Rectangle::<f64, BufferCoords>::new(
                (local.x as f64 * ratio.0, local.y as f64 * ratio.1).into(),
                (rect.size.w as f64 * ratio.0, rect.size.h as f64 * ratio.1).into(),
            );
            let key = (
                blurred,
                [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h],
                region.radius.to_bits(),
                region.alpha.to_bits(),
                region.shape.map(|shape| [shape.loc.x, shape.loc.y, shape.size.w, shape.size.h]),
            );
            let mut kept = cache.backdrops.remove(&region.anchor).unwrap_or_else(|| Kept {
                backdrop: Id::new(),
                commit: CommitCounter::default(),
                key,
                mask: region.mask.clone(),
                previous_damage: None,
            });
            if kept.key != key || kept.mask != region.mask {
                kept.previous_damage = if kept.key.1 == key.1
                    && kept.key.2 == key.2
                    && kept.key.3 == key.3
                    && kept.key.4 == key.4
                    && kept.mask == region.mask
                    && kept.key.0.checked_add(1) == Some(key.0)
                {
                    let coverage = [Rectangle::new(local, rect.size)];
                    let mut damage = project_damage(&cache.damage, level_size, bounds.size, 1, 0);
                    damage = intersect_damage(&damage, &coverage);
                    for part in &mut damage {
                        part.loc -= local;
                    }
                    if let Some(mask) = &region.mask {
                        damage = intersect_damage(&damage, mask);
                    }
                    Some((kept.commit, damage))
                } else {
                    None
                };
                kept.commit.increment();
                kept.key = key;
                kept.mask = region.mask.clone();
            }
            let (tex_w, tex_h) = (level_size.w as f32, level_size.h as f32);
            let uniforms = vec![
                Uniform::new(
                    "u_src",
                    [
                        src.loc.x as f32 / tex_w,
                        src.loc.y as f32 / tex_h,
                        src.size.w as f32 / tex_w,
                        src.size.h as f32 / tex_h,
                    ],
                )
                .into_owned(),
                Uniform::new("u_rect_size", [rect.size.w as f32, rect.size.h as f32]).into_owned(),
                Uniform::new(
                    "u_shape",
                    [
                        (shape.loc.x - rect.loc.x) as f32,
                        (shape.loc.y - rect.loc.y) as f32,
                        shape.size.w as f32,
                        shape.size.h as f32,
                    ],
                )
                .into_owned(),
                Uniform::new("u_radius", region.radius.min(shape.size.w.min(shape.size.h) as f32 / 2.0)).into_owned(),
                Uniform::new("u_saturation", 1.35f32).into_owned(),
            ];
            let backdrop = Backdrop {
                element_id: kept.backdrop.clone(),
                commit: kept.commit,
                texture: texture.clone(),
                src,
                geometry: rect,
                program: program.clone(),
                uniforms,
                alpha: region.alpha,
                mask: region.mask,
                previous_damage: kept.previous_damage.clone(),
            };
            used.insert(region.anchor, kept);
            elements.insert(region.index + 1, OutputElement::Backdrop(backdrop));
        }
        cache.backdrops = used;
    }
}

impl Frost {
    /// Brings the output's blurred picture up to date with what is under the
    /// panels; returns its generation.
    fn update(
        &mut self,
        renderer: &mut GlesRenderer,
        (name, picture): (&str, &Picture),
        bounds: Rectangle<i32, Physical>,
        clear: [f32; 4],
        scene: &[OutputElement],
        placed: &[Placed],
    ) -> Result<u64, String> {
        let size = bounds.size;
        let pictures = self.outputs.entry(name.to_string()).or_default();
        if pictures.get(picture).is_none_or(|cache| cache.size != size) {
            pictures.remove(picture);
            let cache = new_cache(renderer, size).map_err(|error| error.to_string())?;
            pictures.insert(picture.clone(), cache);
        }
        let Some(programs) = self.programs.as_ref() else { return Err("no shaders".into()) };
        let Some(cache) = pictures.get_mut(picture) else { return Err("no cache".into()) };

        let coverage: Vec<_> = placed
            .iter()
            .flat_map(|region| {
                region
                    .mask
                    .iter()
                    .flatten()
                    .copied()
                    .chain(region.mask.is_none().then(|| Rectangle::from_size(region.rect.size)))
                    .map(|mut part| {
                        part.loc += region.rect.loc - bounds.loc;
                        part
                    })
            })
            .collect();
        let near: Vec<_> = coverage.iter().copied().map(sampling_area).collect();
        let local_bounds = Rectangle::from_size(size);
        let one = Scale::from(1.0);
        let wanted: Vec<_> = scene
            .iter()
            .map(|element| {
                RelocateRenderElement::from_element(element, (-bounds.loc.x, -bounds.loc.y), Relocate::Relative)
            })
            .filter(|element| {
                let rect = element.geometry(one);
                rect.overlaps(local_bounds) && near.iter().any(|area| area.overlaps(rect))
            })
            .collect();
        let age = usize::from(cache.rendered_once && cache.clear == clear && cache.coverage == coverage);
        let mut damage = cache
            .tracker
            .damage_output(age, &wanted)
            .map_err(|error| error.to_string())?
            .0
            .cloned()
            .unwrap_or_default();
        damage.retain(|rect| near.iter().any(|area| area.overlaps(*rect)));
        if damage.is_empty() {
            return Ok(cache.generation);
        }

        let half = level_size(size, 1);
        let quarter = level_size(size, 2);
        let sizes = [size, half, quarter, level_size(size, 3), quarter, half];
        // Find the final pixels affected by the source damage. Keep the extra
        // half-size texels that composition can sample along the mask edges.
        for (index, radius) in RADII.into_iter().enumerate() {
            damage = project_damage(&damage, sizes[index], sizes[index + 1], radius, 0);
        }
        let visible = project_damage(&coverage, size, half, 0, 1);
        let final_damage = disjoint(intersect_damage(&damage, &visible));
        if final_damage.is_empty() {
            return Ok(cache.generation);
        }

        // The intermediate textures are shared, so rebuild every input that
        // the changed final texels will read, working backwards through the passes.
        let mut needed: [Vec<PhysicalRect>; 6] = Default::default();
        needed[5] = final_damage;
        for index in (0..RADII.len()).rev() {
            needed[index] =
                disjoint(project_damage(&needed[index + 1], sizes[index + 1], sizes[index], 0, RADII[index]));
        }

        cache.rendered_once = false;
        let [even, odd] = &mut self.scratch;
        let even = scratch_buffer(renderer, even, size)?;
        let odd = scratch_buffer(renderer, odd, half)?;
        {
            let mut bound = renderer.bind(even).map_err(|error| error.to_string())?;
            let mut frame = renderer.render(&mut bound, size, Transform::Normal).map_err(|error| error.to_string())?;
            frame.clear(clear.into(), &needed[0]).map_err(|error| error.to_string())?;
            draw_render_elements::<GlesRenderer, _, _>(&mut frame, one, &wanted, &needed[0])
                .map_err(|error| error.to_string())?;
            // The next pass samples this texture in the same GL context.
            frame.finish().map(|_| ()).map_err(|error| error.to_string())?;
        }
        for (index, damage) in needed.iter().enumerate().skip(1) {
            let (source, target, program) = match index {
                1 | 3 => (&*even, &mut *odd, &programs.downsample),
                2 => (&*odd, &mut *even, &programs.downsample),
                4 => (&*odd, &mut *even, &programs.upsample),
                _ => (&*even, &mut cache.final_level, &programs.upsample),
            };
            pass(renderer, program, (source, sizes[index - 1]), (target, sizes[index]), damage)?;
        }
        cache.clear = clear;
        cache.coverage = coverage;
        cache.damage = std::mem::take(&mut needed[5]);
        cache.rendered_once = true;
        cache.generation += 1;
        Ok(cache.generation)
    }
}

fn padded(rect: PhysicalRect, radius: i32) -> PhysicalRect {
    Rectangle::new(
        (rect.loc.x - radius, rect.loc.y - radius).into(),
        (rect.size.w + 2 * radius, rect.size.h + 2 * radius).into(),
    )
}

fn sampling_area(rect: PhysicalRect) -> PhysicalRect {
    padded(rect, REACH)
}

fn level_size(size: Size<i32, Physical>, shift: u32) -> Size<i32, Physical> {
    ((size.w >> shift).max(1), (size.h >> shift).max(1)).into()
}

fn intersect_damage(damage: &[PhysicalRect], coverage: &[PhysicalRect]) -> Vec<PhysicalRect> {
    damage.iter().flat_map(|rect| coverage.iter().filter_map(|part| rect.intersection(*part))).collect()
}

fn disjoint(rects: impl IntoIterator<Item = PhysicalRect>) -> Vec<PhysicalRect> {
    let mut union = Vec::new();
    for rect in rects {
        union.extend(rect.subtract_rects(union.iter().copied()));
    }
    union
}

fn project_damage(
    rects: &[PhysicalRect],
    from: Size<i32, Physical>,
    into_size: Size<i32, Physical>,
    pad_before: i32,
    pad_after: i32,
) -> Vec<PhysicalRect> {
    let ratio = Scale::from((into_size.w as f64 / from.w as f64, into_size.h as f64 / from.h as f64));
    rects
        .iter()
        .filter_map(|rect| {
            let from = padded(*rect, pad_before);
            let start: Point<i32, Physical> = from.loc.to_f64().upscale(ratio).to_i32_floor();
            let end = (from.loc + from.size).to_f64().upscale(ratio).to_i32_ceil();
            padded(Rectangle::from_extremities(start, end), pad_after).intersection(Rectangle::from_size(into_size))
        })
        .collect()
}

fn new_cache(renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Result<Cache, GlesError> {
    let half = level_size(size, 1);
    Ok(Cache {
        size,
        clear: [0.0; 4],
        tracker: OutputDamageTracker::new(size, 1.0, Transform::Normal),
        rendered_once: false,
        final_level: renderer.create_buffer(Fourcc::Abgr8888, (half.w, half.h).into())?,
        coverage: Vec::new(),
        generation: 0,
        damage: Vec::new(),
        backdrops: HashMap::new(),
    })
}

fn scratch_buffer<'a>(
    renderer: &mut GlesRenderer,
    slot: &'a mut Option<GlesTexture>,
    size: Size<i32, Physical>,
) -> Result<&'a mut GlesTexture, String> {
    let allocated = slot.as_ref().map(Texture::size).unwrap_or_default();
    if size.w > allocated.w || size.h > allocated.h {
        let size = (size.w.max(allocated.w), size.h.max(allocated.h)).into();
        *slot = Some(renderer.create_buffer(Fourcc::Abgr8888, size).map_err(|error| error.to_string())?);
    }
    slot.as_mut().ok_or_else(|| "no scratch texture".into())
}

/// Draws a damaged part of one Kawase level into the next.
fn pass(
    renderer: &mut GlesRenderer,
    program: &GlesTexProgram,
    source: (&GlesTexture, Size<i32, Physical>),
    target: (&mut GlesTexture, Size<i32, Physical>),
    damage: &[PhysicalRect],
) -> Result<(), String> {
    let (texture, source_size) = source;
    let target_size = target.1;
    let mut bound = renderer.bind(target.0).map_err(|error| error.to_string())?;
    let mut frame = renderer.render(&mut bound, target_size, Transform::Normal).map_err(|error| error.to_string())?;
    let whole = Rectangle::from_size(target_size);
    frame.clear(Color32F::TRANSPARENT, damage).map_err(|error| error.to_string())?;
    let allocated = texture.size();
    let uniforms = [
        Uniform::new("u_halfpixel", [0.5 / allocated.w as f32, 0.5 / allocated.h as f32]),
        Uniform::new(
            "u_limit",
            [(source_size.w as f32 - 0.5) / allocated.w as f32, (source_size.h as f32 - 0.5) / allocated.h as f32],
        ),
        Uniform::new("u_offset", OFFSET),
    ];
    frame
        .render_texture_from_to(
            texture,
            Rectangle::from_size((source_size.w as f64, source_size.h as f64).into()),
            whole,
            damage,
            &[],
            Transform::Normal,
            1.0,
            Some(program),
            &uniforms,
        )
        .map_err(|error| error.to_string())?;
    frame.finish().map(|_| ()).map_err(|error| error.to_string())
}
