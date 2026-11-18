//! Tiled windows drawn with rounded corners: each of a window's surfaces is
//! drawn through a texture shader that leaves out what lies outside the
//! window's rounded rectangle, so the corners show whatever is behind the
//! window (the wallpaper, a frost backdrop, the focus glow).
//!
//! The shader works out where a pixel lies on screen from its texture
//! coordinates, which smithay normalises by the buffer's size after cropping
//! to the element's source. Cropping a [`Rounded`] element, or moving it into
//! an offscreen picture, keeps that mapping, so the corners stay in place.
//! A surface whose buffer is rotated or flipped is drawn without rounding.

use smithay::{
    backend::renderer::{
        Texture,
        element::{
            Element, Id, Kind, RenderElement, UnderlyingStorage,
            surface::{WaylandSurfaceRenderElement, WaylandSurfaceTexture},
            utils::{CropRenderElement, RescaleRenderElement},
        },
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Transform},
};

/// smithay's texture shader, its colour multiplied by how much of the pixel
/// lies inside the rounded rectangle.
const SHADER: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 u_to_screen_scale;
uniform vec2 u_to_screen_offset;
uniform vec4 u_rect;
uniform float u_radius;

float rounded_box(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    vec2 p = v_coords * u_to_screen_scale + u_to_screen_offset;
    vec2 half_size = u_rect.zw * 0.5;
    color *= clamp(0.5 - rounded_box(p - u_rect.xy - half_size, half_size, u_radius), 0.0, 1.0);

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
"#;

/// Compiles the shader for `renderer`.
pub fn compile(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        SHADER,
        &[
            UniformName::new("u_to_screen_scale", UniformType::_2f),
            UniformName::new("u_to_screen_offset", UniformType::_2f),
            UniformName::new("u_rect", UniformType::_4f),
            UniformName::new("u_radius", UniformType::_1f),
        ],
    )
}

type Surface = RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>;

/// One surface of a window, drawn cut to the window's rounded rectangle.
#[derive(Debug)]
pub struct Rounded<E> {
    inner: E,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
    outline: Rectangle<i32, Physical>,
    /// The squares at the rectangle's corners, which are not opaque.
    corners: [Rectangle<i32, Physical>; 4],
}

/// A surface after [`round`]: cut to the rounded rectangle, or as it was
/// when the shader cannot place it.
pub enum Cut {
    Rounded(Rounded<Surface>),
    Plain(Surface),
}

/// Rounds a client surface enlarged about `origin` by `scale`, as
/// `render::enlarged` does, to `outline` (physical pixels) with corners of
/// `radius` physical pixels.
pub fn round(
    program: &GlesTexProgram,
    surface: WaylandSurfaceRenderElement<GlesRenderer>,
    origin: Point<i32, Physical>,
    scale: Scale<f64>,
    outline: Rectangle<i32, Physical>,
    radius: f32,
) -> Cut {
    let texture = match surface.texture() {
        WaylandSurfaceTexture::Texture(texture) => Some((texture.size(), texture.is_y_inverted())),
        WaylandSurfaceTexture::SolidColor(_) => None,
    };
    let normal = surface.transform() == Transform::Normal;
    let enlarged = RescaleRenderElement::from_element(surface, origin, scale);
    let src = enlarged.src();
    let geometry = enlarged.geometry(Scale::from(1.0));
    let Some((texture_size, y_inverted)) = texture else { return Cut::Plain(enlarged) };
    if !normal || src.size.is_empty() || geometry.is_empty() {
        return Cut::Plain(enlarged);
    }
    // Screen pixels per buffer pixel.
    let ratio = (f64::from(geometry.size.w) / src.size.w, f64::from(geometry.size.h) / src.size.h);
    // Smithay negates the normalized Y coordinate for an inverted texture.
    let y_sign = if y_inverted { -1.0 } else { 1.0 };
    let to_screen_scale =
        [(f64::from(texture_size.w) * ratio.0) as f32, (f64::from(texture_size.h) * ratio.1 * y_sign) as f32];
    let to_screen_offset = [
        (f64::from(geometry.loc.x) - src.loc.x * ratio.0) as f32,
        (f64::from(geometry.loc.y) - src.loc.y * ratio.1) as f32,
    ];
    let radius = radius.min(outline.size.w.min(outline.size.h) as f32 / 2.0).max(0.0);
    let uniforms = vec![
        Uniform::new("u_to_screen_scale", to_screen_scale).into_owned(),
        Uniform::new("u_to_screen_offset", to_screen_offset).into_owned(),
        Uniform::new(
            "u_rect",
            [outline.loc.x as f32, outline.loc.y as f32, outline.size.w as f32, outline.size.h as f32],
        )
        .into_owned(),
        Uniform::new("u_radius", radius).into_owned(),
    ];
    Cut::Rounded(Rounded {
        inner: enlarged,
        program: program.clone(),
        uniforms,
        outline,
        corners: corners(outline, radius),
    })
}

/// The squares a corner of `radius` cuts into, at each corner of `outline`.
pub(crate) fn corners(outline: Rectangle<i32, Physical>, radius: f32) -> [Rectangle<i32, Physical>; 4] {
    let side = (radius.ceil() as i32).min(outline.size.w).min(outline.size.h).max(0);
    let size = (side, side);
    let (left, top) = (outline.loc.x, outline.loc.y);
    let (right, bottom) = (left + outline.size.w - side, top + outline.size.h - side);
    [(left, top), (right, top), (left, bottom), (right, bottom)]
        .map(|corner| Rectangle::new(corner.into(), size.into()))
}

impl<E: Element> Element for Rounded<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let origin = self.inner.geometry(scale).loc;
        let mut outline = self.outline;
        outline.loc -= origin;
        let corners = self.corners.map(|mut corner| {
            corner.loc -= origin;
            corner
        });
        let opaque =
            self.inner.opaque_regions(scale).into_iter().filter_map(|rect| rect.intersection(outline)).collect();
        OpaqueRegions::from_slice(&Rectangle::subtract_rects_many_in_place(opaque, corners))
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for Rounded<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), self.uniforms.clone());
        let drawn = self.inner.draw(frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        drawn
    }

    // Scanning the buffer out directly would skip the shader.
    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// A rounded surface cut to a window's picture, as `Tileception::picture` cuts.
pub type CroppedRounded = CropRenderElement<Rounded<Surface>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn tuple(rect: &Rectangle<i32, Physical>) -> (i32, i32, i32, i32) {
        (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h)
    }

    #[test]
    fn the_corners_are_squares_of_the_radius() {
        let outline = Rectangle::new((10, 20).into(), (300, 200).into());
        let squares: Vec<_> = corners(outline, 5.5).iter().map(tuple).collect();
        assert_eq!(squares, vec![(10, 20, 6, 6), (304, 20, 6, 6), (10, 214, 6, 6), (304, 214, 6, 6)]);
    }

    #[test]
    fn no_radius_leaves_no_corners() {
        let outline = Rectangle::new((0, 0).into(), (100, 100).into());
        assert!(corners(outline, 0.0).iter().all(|corner| corner.is_empty()));
    }
}
