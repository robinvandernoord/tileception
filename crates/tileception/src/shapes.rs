//! Rounded rectangles drawn by one small pixel shader: an optional gradient
//! fill, a border, a soft outer glow, and a corner mask in the background
//! colour that makes square client windows look rounded.

use smithay::{
    backend::renderer::{
        element::Kind,
        gles::{element::PixelShaderElement, GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType},
    },
    utils::{Logical, Rectangle},
};
use tileception_core::Rect;

use crate::theme::Colour;

const SHADER: &str = r#"
precision mediump float;
uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;

uniform vec2 u_rect_size;
uniform vec2 u_offset;
uniform float u_radius;
uniform vec4 u_fill_a;
uniform vec4 u_fill_b;
uniform float u_border_w;
uniform vec4 u_border;
uniform float u_glow_r;
uniform vec4 u_glow;
uniform vec4 u_mask;

float rounded_box(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    // Position relative to the shape's top left, in pixels.
    vec2 p = v_coords * size - u_offset;
    vec2 half_size = u_rect_size * 0.5;
    float d = rounded_box(p - half_size, half_size, u_radius);
    float inside = clamp(0.5 - d, 0.0, 1.0);

    float t = clamp(p.x / max(u_rect_size.x, 1.0), 0.0, 1.0);
    vec4 c = mix(u_fill_a, u_fill_b, t) * inside;

    // The corners of the box outside the rounded shape, painted over the
    // window underneath so it looks rounded.
    float in_box = step(0.0, p.x) * step(0.0, p.y) * step(p.x, u_rect_size.x) * step(p.y, u_rect_size.y);
    vec4 mask = u_mask * in_box * (1.0 - inside);
    c = mask + c * (1.0 - mask.a);

    if (u_glow_r > 0.0) {
        float g = max(d, 0.0) / u_glow_r;
        vec4 glow = u_glow * exp(-g * g * 3.0) * (1.0 - inside);
        c = glow + c * (1.0 - glow.a);
    }
    if (u_border_w > 0.0) {
        float ring = inside - clamp(0.5 - (d + u_border_w), 0.0, 1.0);
        c = u_border * ring + c * (1.0 - u_border.a * ring);
    }
    gl_FragColor = c * alpha;
}
"#;

/// How a rounded rectangle looks. Colours are premultiplied.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    pub radius: f32,
    /// Fill, as a left to right gradient; the same colour twice for a flat fill.
    pub fill: (Colour, Colour),
    pub border_width: f32,
    pub border: Colour,
    pub glow_radius: f32,
    pub glow: Colour,
    /// Painted in the corners outside the rounding, over what is underneath.
    pub corner_mask: Colour,
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            radius: 0.0,
            fill: (Colour::TRANSPARENT, Colour::TRANSPARENT),
            border_width: 0.0,
            border: Colour::TRANSPARENT,
            glow_radius: 0.0,
            glow: Colour::TRANSPARENT,
            corner_mask: Colour::TRANSPARENT,
        }
    }
}

impl Shape {
    pub fn filled(colour: Colour, radius: f32) -> Shape {
        Shape { fill: (colour, colour), radius, ..Default::default() }
    }
}

/// The compiled shader, made once per renderer.
#[derive(Debug, Clone)]
pub struct Shapes {
    program: GlesPixelProgram,
}

impl Shapes {
    pub fn compile(renderer: &mut GlesRenderer) -> Result<Shapes, String> {
        let uniforms = [
            ("u_rect_size", UniformType::_2f),
            ("u_offset", UniformType::_2f),
            ("u_radius", UniformType::_1f),
            ("u_fill_a", UniformType::_4f),
            ("u_fill_b", UniformType::_4f),
            ("u_border_w", UniformType::_1f),
            ("u_border", UniformType::_4f),
            ("u_glow_r", UniformType::_1f),
            ("u_glow", UniformType::_4f),
            ("u_mask", UniformType::_4f),
        ]
        .map(|(name, type_)| UniformName::new(name, type_));
        let program = renderer.compile_custom_pixel_shader(SHADER, &uniforms).map_err(|e| e.to_string())?;
        Ok(Shapes { program })
    }

    /// A render element drawing `shape` over `rect` (screen pixels). The
    /// element reaches beyond the rectangle by the glow radius.
    pub fn element(&self, rect: &Rect, shape: Shape) -> Option<PixelShaderElement> {
        let margin = shape.glow_radius.ceil();
        let x0 = (rect.x - margin as f64).floor() as i32;
        let y0 = (rect.y - margin as f64).floor() as i32;
        let x1 = (rect.right() + margin as f64).ceil() as i32;
        let y1 = (rect.bottom() + margin as f64).ceil() as i32;
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let area: Rectangle<i32, Logical> = Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into());
        // The shape's offset inside the element area, keeping sub-pixel position.
        let offset = ((rect.x - x0 as f64) as f32, (rect.y - y0 as f64) as f32);
        let radius = shape.radius.min(rect.w as f32 / 2.0).min(rect.h as f32 / 2.0);
        let c = |c: Colour| c.0;
        let uniforms = vec![
            Uniform::new("u_rect_size", (rect.w as f32, rect.h as f32)),
            Uniform::new("u_offset", offset),
            Uniform::new("u_radius", radius),
            Uniform::new("u_fill_a", c(shape.fill.0)),
            Uniform::new("u_fill_b", c(shape.fill.1)),
            Uniform::new("u_border_w", shape.border_width),
            Uniform::new("u_border", c(shape.border)),
            Uniform::new("u_glow_r", shape.glow_radius),
            Uniform::new("u_glow", c(shape.glow)),
            Uniform::new("u_mask", c(shape.corner_mask)),
        ];
        Some(PixelShaderElement::new(self.program.clone(), area, None, 1.0, uniforms, Kind::Unspecified))
    }
}
