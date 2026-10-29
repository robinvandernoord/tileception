//! Rounded rectangles drawn by one small pixel shader: an optional gradient
//! fill, a border, a soft outer glow, and a corner mask in the background
//! colour that makes square client windows look rounded.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use smithay::{
    backend::renderer::{
        element::Kind,
        gles::{GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType, element::PixelShaderElement},
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

/// The compiled shader, made once per renderer, and the output scale that
/// shapes are drawn at: rectangles and sizes are given in logical pixels.
#[derive(Debug, Clone)]
pub struct Shapes {
    program: GlesPixelProgram,
    scale: f64,
    reused: Rc<RefCell<Reused>>,
}

/// Where a shape is and how it looks, as exact bits: the same key draws the
/// same pixels.
type Key = ([i32; 4], [u32; 28]);

/// The elements of the last frame and of this one. Each new element gets a
/// new id, which the damage tracker takes for something that changed, so a
/// shape that stays the same must keep its element or every frame redraws
/// everything under it.
#[derive(Debug, Default)]
struct Reused {
    previous: HashMap<Key, Vec<PixelShaderElement>>,
    current: HashMap<Key, Vec<PixelShaderElement>>,
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
        let program = renderer.compile_custom_pixel_shader(SHADER, &uniforms).map_err(|err| err.to_string())?;
        Ok(Shapes { program, scale: 1.0, reused: Default::default() })
    }

    /// Starts a frame: shapes drawn in the last one can be drawn again as they were.
    pub fn next_frame(&self) {
        let mut reused = self.reused.borrow_mut();
        reused.previous = std::mem::take(&mut reused.current);
    }

    pub fn at_scale(self, scale: f64) -> Shapes {
        Shapes { scale, ..self }
    }

    /// A render element drawing `shape` over `rect` (logical pixels). The
    /// element reaches beyond the rectangle by the glow radius.
    pub fn element(&self, rect: &Rect, shape: Shape) -> Option<PixelShaderElement> {
        self.element_faded(rect, shape, 1.0)
    }

    /// The same, at a fraction of its opacity, for fading in and out.
    pub fn element_faded(&self, rect: &Rect, shape: Shape, alpha: f32) -> Option<PixelShaderElement> {
        // The shader works in physical pixels.
        let scale = self.scale;
        let rect = &Rect::new(rect.x * scale, rect.y * scale, rect.w * scale, rect.h * scale);
        let scale_f32 = scale as f32;
        let shape = Shape {
            radius: shape.radius * scale_f32,
            border_width: shape.border_width * scale_f32,
            glow_radius: shape.glow_radius * scale_f32,
            ..shape
        };
        let margin = shape.glow_radius.ceil();
        let left = (rect.x - margin as f64).floor() as i32;
        let top = (rect.y - margin as f64).floor() as i32;
        let right = (rect.right() + margin as f64).ceil() as i32;
        let bottom = (rect.bottom() + margin as f64).ceil() as i32;
        if right <= left || bottom <= top {
            return None;
        }
        let area: Rectangle<i32, Logical> = Rectangle::new((left, top).into(), (right - left, bottom - top).into());
        // The shape's offset inside the element area, keeping sub-pixel position.
        let offset = ((rect.x - left as f64) as f32, (rect.y - top as f64) as f32);
        let radius = shape.radius.min(rect.w as f32 / 2.0).min(rect.h as f32 / 2.0);
        let rgba = |colour: Colour| colour.0;
        let (size, fill_a, fill_b) = ((rect.w as f32, rect.h as f32), rgba(shape.fill.0), rgba(shape.fill.1));
        let (border, glow, mask) = (rgba(shape.border), rgba(shape.glow), rgba(shape.corner_mask));
        let mut bits = [0u32; 28];
        let floats = [size.0, size.1, offset.0, offset.1, radius, shape.border_width, shape.glow_radius, alpha]
            .into_iter()
            .chain(fill_a)
            .chain(fill_b)
            .chain(border)
            .chain(glow)
            .chain(mask);
        for (bit, float) in bits.iter_mut().zip(floats) {
            *bit = float.to_bits();
        }
        let key = ([left, top, right, bottom], bits);
        let make = || {
            let uniforms = vec![
                Uniform::new("u_rect_size", size),
                Uniform::new("u_offset", offset),
                Uniform::new("u_radius", radius),
                Uniform::new("u_fill_a", fill_a),
                Uniform::new("u_fill_b", fill_b),
                Uniform::new("u_border_w", shape.border_width),
                Uniform::new("u_border", border),
                Uniform::new("u_glow_r", shape.glow_radius),
                Uniform::new("u_glow", glow),
                Uniform::new("u_mask", mask),
            ];
            PixelShaderElement::new(self.program.clone(), area, None, alpha, uniforms, Kind::Unspecified)
        };
        // The same shape twice in a frame takes the last frame's second one,
        // so no two elements share an id.
        let mut reused = self.reused.borrow_mut();
        let element =
            reused.previous.get_mut(&key).filter(|elements| !elements.is_empty()).map(|elements| elements.remove(0));
        let element = element.unwrap_or_else(make);
        reused.current.entry(key).or_default().push(element.clone());
        Some(element)
    }
}
