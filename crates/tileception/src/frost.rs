//! Frost: Tileception's own panels (the bar, launcher, notifications, menus
//! and cards) turn translucent and blur what is behind them. Windows stay
//! solid. Off unless `[theme] frost = true`.
//!
//! Each frame, the panels that want frost register their background shape
//! with [`Frost::region`]. What lies under the panels (the windows, layers and
//! wallpaper) is rendered into an offscreen texture, only the elements near a
//! panel and only where they changed, and blurred with a dual Kawase blur at
//! half, quarter and eighth size. Each panel then gets a backdrop: a piece of
//! the blurred picture cut to its rounded shape, drawn just beneath its
//! background. Panels do not blur one another.
//!
//! The blur is redone only when something changed near a panel, so a still
//! desktop with the bar open draws nothing new.

use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Color32F, Frame, Offscreen, Renderer,
            damage::OutputDamageTracker,
            element::{Element, Id, Kind, RenderElement},
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType,
            },
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
    },
    utils::{Buffer as BufferCoords, Physical, Rectangle, Scale, Size, Transform},
};
use tileception_core::Rect;

use crate::{render::OutputElement, state::Tileception};

/// How opaque frosted panel backgrounds are.
pub const PANEL_OPACITY: f32 = 0.72;

/// How far the blur reaches, in physical pixels: changes this close to a
/// panel redo its blur.
const REACH: i32 = 96;

const HEADER: &str = r#"
//_DEFINES_
precision mediump float;
uniform sampler2D tex;
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
"#;

/// Dual Kawase, the step down to half size.
const DOWNSAMPLE: &str = r#"
uniform vec2 u_halfpixel;
uniform float u_offset;
void main() {
    vec2 h = u_halfpixel * u_offset;
    vec4 sum = texture2D(tex, v_coords) * 4.0;
    sum += texture2D(tex, v_coords - h);
    sum += texture2D(tex, v_coords + h);
    sum += texture2D(tex, v_coords + vec2(h.x, -h.y));
    sum += texture2D(tex, v_coords - vec2(h.x, -h.y));
    gl_FragColor = sum / 8.0;
}
"#;

/// Dual Kawase, the step up to twice the size.
const UPSAMPLE: &str = r#"
uniform vec2 u_halfpixel;
uniform float u_offset;
void main() {
    vec2 h = u_halfpixel * u_offset;
    vec4 sum = texture2D(tex, v_coords + vec2(-h.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(-h.x, h.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, h.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(h.x, h.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(h.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(h.x, -h.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, -h.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(-h.x, -h.y)) * 2.0;
    gl_FragColor = sum / 12.0;
}
"#;

/// The blurred picture cut to a panel's rounded rectangle, a little more
/// saturated so colours behind it still read.
const BACKDROP: &str = r#"
uniform vec4 u_src;
uniform vec2 u_rect_size;
uniform float u_radius;
uniform float u_saturation;

float rounded_box(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    vec2 p = (v_coords - u_src.xy) / u_src.zw * u_rect_size;
    vec2 half_size = u_rect_size * 0.5;
    float inside = clamp(0.5 - rounded_box(p - half_size, half_size, u_radius), 0.0, 1.0);
    vec4 color = texture2D(tex, v_coords);
    float grey = dot(color.rgb, vec3(0.2126, 0.7152, 0.0722));
    color.rgb = mix(vec3(grey), color.rgb, u_saturation);
    gl_FragColor = vec4(color.rgb, 1.0) * inside * alpha;
}
"#;

/// A panel background that wants frost behind it, registered while drawing.
#[derive(Debug, Clone)]
pub struct Region {
    /// The id of the panel's background element; the backdrop goes just
    /// beneath it.
    pub panel: Id,
    /// Logical pixels.
    pub rect: Rect,
    pub radius: f32,
    pub alpha: f32,
    /// A popup, which blurs the frosted panels it covers too.
    pub over_panels: bool,
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
    source: GlesTexture,
    tracker: OutputDamageTracker,
    rendered_once: bool,
    /// Half, quarter and eighth size; the first ends up holding the blur.
    levels: Vec<(GlesTexture, Size<i32, Physical>)>,
    /// Bumped whenever the blur is redone.
    generation: u64,
    /// Backdrops by the panel background they go under, kept so an
    /// unchanged backdrop is not redrawn.
    backdrops: HashMap<Id, Kept>,
}

#[derive(Debug)]
struct Kept {
    backdrop: Id,
    commit: CommitCounter,
    key: (u64, [i32; 4], u32, u32),
}

/// The frost state kept across frames.
#[derive(Debug, Default)]
pub struct Frost {
    regions: Vec<Region>,
    programs: Option<Programs>,
    failed: bool,
    outputs: HashMap<String, Cache>,
}

impl Frost {
    /// Starts collecting the regions of a new frame.
    pub fn begin(&mut self) {
        self.regions.clear();
    }

    pub fn region(&mut self, region: Region) {
        self.regions.push(region);
    }

    /// Lets go of every texture, when frost is turned off.
    pub fn release(&mut self) {
        self.outputs.clear();
        self.regions.clear();
    }

    fn programs(&mut self, renderer: &mut GlesRenderer) -> Option<&Programs> {
        if self.programs.is_none() && !self.failed {
            match compile(renderer) {
                Ok(programs) => self.programs = Some(programs),
                Err(error) => {
                    tracing::error!("frost shaders: {error}");
                    self.failed = true;
                }
            }
        }
        self.programs.as_ref()
    }
}

fn compile(renderer: &mut GlesRenderer) -> Result<Programs, GlesError> {
    let kawase = [UniformName::new("u_halfpixel", UniformType::_2f), UniformName::new("u_offset", UniformType::_1f)];
    let downsample = renderer.compile_custom_texture_shader(format!("#version 100\n{HEADER}{DOWNSAMPLE}"), &kawase)?;
    let upsample = renderer.compile_custom_texture_shader(format!("#version 100\n{HEADER}{UPSAMPLE}"), &kawase)?;
    let backdrop = renderer.compile_custom_texture_shader(
        format!("#version 100\n{HEADER}{BACKDROP}"),
        &[
            UniformName::new("u_src", UniformType::_4f),
            UniformName::new("u_rect_size", UniformType::_2f),
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
    /// Whether panels are drawn frosted.
    pub fn frost_on(&self) -> bool {
        self.theme.frost
    }

    /// Adds a backdrop beneath every frosted panel in `elements` (topmost
    /// first); `scene` is where the windows, layers and wallpaper start.
    pub fn add_frost(&self, renderer: &mut GlesRenderer, elements: &mut Vec<OutputElement>, scene: usize) {
        let mut frost = self.frost.borrow_mut();
        if !self.frost_on() {
            frost.release();
            return;
        }
        let regions = std::mem::take(&mut frost.regions);
        let scale = self.scale;
        // Where each backdrop goes: just beneath its panel's background.
        let placed: Vec<Placed> = regions
            .into_iter()
            .filter_map(|region| {
                let index =
                    elements[..scene.min(elements.len())].iter().position(|element| element.id() == &region.panel)?;
                let rect = crate::render::physical(&region.rect, scale);
                (!rect.is_empty()).then_some((index, region, rect))
            })
            .collect();
        if placed.is_empty() {
            return;
        }
        // A popup over another frosted panel would show a hole where that
        // panel is: it blurs everything beneath it instead, panels included,
        // from a second picture kept only while such a popup is open.
        let covers = |(index, region, rect): &Placed| {
            region.over_panels
                && placed.iter().any(|(other, _, beneath)| other > index && rect.intersection(*beneath).is_some())
        };
        let (over, placed): (Vec<Placed>, Vec<Placed>) = placed.iter().cloned().partition(covers);
        let out = self.output_rect();
        let size: Size<i32, Physical> =
            ((out.w * scale).round().max(1.0) as i32, (out.h * scale).round().max(1.0) as i32).into();
        let name = self.output.as_ref().map(|output| output.name()).unwrap_or_default();
        let over_name = format!("{name} over panels");
        if frost.programs(renderer).is_none() {
            return;
        }
        let clear = self.clear_colour();
        frost.backdrops(renderer, &name, size, clear, scale, elements, scene, placed);
        if over.is_empty() {
            frost.outputs.remove(&over_name);
            return;
        }
        // Found again: the backdrops just added moved them.
        let over: Vec<Placed> = over
            .into_iter()
            .filter_map(|(_, region, rect)| {
                let index = elements.iter().position(|element| element.id() == &region.panel)?;
                Some((index, region, rect))
            })
            .collect();
        let Some(lowest) = over.iter().map(|(index, _, _)| *index).max() else { return };
        frost.backdrops(renderer, &over_name, size, clear, scale, elements, lowest + 1, over);
    }
}

/// A frosted panel's index in the frame, its region and its physical rectangle.
type Placed = (usize, Region, Rectangle<i32, Physical>);

impl Frost {
    /// Blurs what lies from `start` on in `elements` into the picture named
    /// `name`, and inserts a piece of it beneath each panel in `placed`.
    #[allow(clippy::too_many_arguments, reason = "one frame's worth of state, passed along once")]
    fn backdrops(
        &mut self,
        renderer: &mut GlesRenderer,
        name: &str,
        size: Size<i32, Physical>,
        clear: [f32; 4],
        scale: f64,
        elements: &mut Vec<OutputElement>,
        start: usize,
        mut placed: Vec<Placed>,
    ) {
        if placed.is_empty() {
            return;
        }
        let start = start.min(elements.len());
        let blurred = match self.update(renderer, name, size, clear, &elements[start..], &placed) {
            Ok(generation) => generation,
            Err(error) => {
                tracing::warn!("frost: {error}");
                return;
            }
        };
        let Some(programs) = self.programs.as_ref() else { return };
        let program = programs.backdrop.clone();
        let Some(cache) = self.outputs.get_mut(name) else { return };
        let (texture, level_size) = cache.levels[0].clone();
        let mut used = HashMap::new();
        // Insert from the bottom up so earlier indices stay valid.
        placed.sort_by_key(|(index, _, _)| std::cmp::Reverse(*index));
        for (index, region, rect) in placed {
            let rect = rect.intersection(Rectangle::from_size(size)).unwrap_or_default();
            if rect.is_empty() {
                continue;
            }
            // The blurred picture is half size.
            let ratio = (level_size.w as f64 / size.w as f64, level_size.h as f64 / size.h as f64);
            let src = Rectangle::<f64, BufferCoords>::new(
                (rect.loc.x as f64 * ratio.0, rect.loc.y as f64 * ratio.1).into(),
                (rect.size.w as f64 * ratio.0, rect.size.h as f64 * ratio.1).into(),
            );
            let key = (
                blurred,
                [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h],
                region.radius.to_bits(),
                region.alpha.to_bits(),
            );
            let mut kept = cache.backdrops.remove(&region.panel).unwrap_or_else(|| Kept {
                backdrop: Id::new(),
                commit: CommitCounter::default(),
                key,
            });
            if kept.key != key {
                kept.commit.increment();
                kept.key = key;
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
                Uniform::new("u_radius", (region.radius * scale as f32).min(rect.size.w.min(rect.size.h) as f32 / 2.0))
                    .into_owned(),
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
            };
            used.insert(region.panel, kept);
            elements.insert(index + 1, OutputElement::Backdrop(backdrop));
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
        name: &str,
        size: Size<i32, Physical>,
        clear: [f32; 4],
        scene: &[OutputElement],
        placed: &[(usize, Region, Rectangle<i32, Physical>)],
    ) -> Result<u64, String> {
        let stale = self.outputs.get(name).is_none_or(|cache| cache.size != size);
        if stale {
            self.outputs.remove(name);
            let cache = new_cache(renderer, size).map_err(|error| error.to_string())?;
            self.outputs.insert(name.to_string(), cache);
        }
        let Some(programs) = self.programs.as_ref() else { return Err("no shaders".into()) };
        let Some(cache) = self.outputs.get_mut(name) else { return Err("no cache".into()) };

        // Only what lies near a panel matters.
        let near: Vec<Rectangle<i32, Physical>> = placed
            .iter()
            .map(|(_, _, rect)| {
                Rectangle::new(
                    (rect.loc.x - REACH, rect.loc.y - REACH).into(),
                    (rect.size.w + 2 * REACH, rect.size.h + 2 * REACH).into(),
                )
            })
            .collect();
        let one = Scale::from(1.0);
        let wanted: Vec<&OutputElement> =
            scene.iter().filter(|element| near.iter().any(|area| area.overlaps(element.geometry(one)))).collect();
        let age = if cache.rendered_once && cache.clear == clear { 1 } else { 0 };
        let damage = {
            let mut target = renderer.bind(&mut cache.source).map_err(|error| error.to_string())?;
            let result = cache
                .tracker
                .render_output(renderer, &mut target, age, &wanted, clear)
                .map_err(|error| format!("{error:?}"))?;
            result.damage.cloned().unwrap_or_default()
        };
        let changed = !cache.rendered_once || damage.iter().any(|rect| near.iter().any(|area| area.overlaps(*rect)));
        cache.rendered_once = true;
        cache.clear = clear;
        if !changed {
            return Ok(cache.generation);
        }

        // Down to an eighth, then back up to half size.
        let offset = 2.0f32;
        let levels = &mut cache.levels;
        let count = levels.len();
        // Down passes, each from the level before.
        for index in 0..count {
            let (before, after) = levels.split_at_mut(index);
            let source = match before.last() {
                Some((texture, level)) => (texture, *level),
                None => (&cache.source, size),
            };
            pass(renderer, &programs.downsample, source, &mut after[0], offset)?;
        }
        // Up passes, each from the smaller level after.
        for index in (0..count.saturating_sub(1)).rev() {
            let (before, after) = levels.split_at_mut(index + 1);
            pass(renderer, &programs.upsample, (&after[0].0, after[0].1), &mut before[index], offset)?;
        }
        cache.generation += 1;
        Ok(cache.generation)
    }
}

fn new_cache(renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Result<Cache, GlesError> {
    let buffer = |renderer: &mut GlesRenderer, size: Size<i32, Physical>| -> Result<GlesTexture, GlesError> {
        renderer.create_buffer(Fourcc::Abgr8888, (size.w.max(1), size.h.max(1)).into())
    };
    let source = buffer(renderer, size)?;
    let mut levels = Vec::new();
    for shift in 1..=3 {
        let level = Size::from(((size.w >> shift).max(1), (size.h >> shift).max(1)));
        levels.push((buffer(renderer, level)?, level));
    }
    Ok(Cache {
        size,
        clear: [0.0; 4],
        source,
        tracker: OutputDamageTracker::new(size, 1.0, Transform::Normal),
        rendered_once: false,
        levels,
        generation: 0,
        backdrops: HashMap::new(),
    })
}

/// Draws all of `source` scaled into all of `target` with a blur program.
fn pass(
    renderer: &mut GlesRenderer,
    program: &GlesTexProgram,
    source: (&GlesTexture, Size<i32, Physical>),
    target: &mut (GlesTexture, Size<i32, Physical>),
    offset: f32,
) -> Result<(), String> {
    let (texture, source_size) = source;
    let target_size = target.1;
    let mut bound = renderer.bind(&mut target.0).map_err(|error| error.to_string())?;
    let mut frame = renderer.render(&mut bound, target_size, Transform::Normal).map_err(|error| error.to_string())?;
    let whole = Rectangle::from_size(target_size);
    frame.clear(Color32F::new(0.0, 0.0, 0.0, 0.0), &[whole]).map_err(|error| error.to_string())?;
    let uniforms = [
        Uniform::new("u_halfpixel", [0.5 / source_size.w as f32, 0.5 / source_size.h as f32]),
        Uniform::new("u_offset", offset),
    ];
    frame
        .render_texture_from_to(
            texture,
            Rectangle::from_size((source_size.w as f64, source_size.h as f64).into()),
            whole,
            &[whole],
            &[],
            Transform::Normal,
            1.0,
            Some(program),
            &uniforms,
        )
        .map_err(|error| error.to_string())?;
    frame.finish().map(|_| ()).map_err(|error| error.to_string())
}
