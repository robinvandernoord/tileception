//! Text for the compositor's own interface (tab titles, menus), rasterised
//! with cosmic-text into images that are cached and drawn as textures.

use std::collections::HashMap;

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

use crate::theme::Colour;

/// Cached images beyond this count are all dropped and redrawn on demand.
const CACHE_LIMIT: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Field {
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub visible: bool,
    pub accent: [u8; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    font: String,
    size_tenths: u32,
    colour: [u8; 4],
    max_width: i32,
    bold: bool,
    field: Option<Field>,
}

/// A line of text as an image, `width` x `height` pixels.
#[derive(Debug, Clone)]
pub struct TextImage {
    pub buffer: MemoryRenderBuffer,
    pub width: i32,
    pub height: i32,
}

pub struct Text {
    fonts: Option<FontSystem>,
    swash: SwashCache,
    cache: HashMap<Key, TextImage>,
}

impl Default for Text {
    fn default() -> Self {
        Text { fonts: None, swash: SwashCache::new(), cache: HashMap::new() }
    }
}

impl Text {
    /// Drops cached images, for example after the theme changed.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// One line of text, shortened with "..." to fit `max_width` pixels.
    pub fn line(
        &mut self,
        text: &str,
        font: &str,
        size: f32,
        colour: Colour,
        max_width: f32,
        bold: bool,
    ) -> Option<TextImage> {
        if text.is_empty() || max_width < 4.0 {
            return None;
        }
        let key = Key {
            text: text.to_string(),
            font: font.to_string(),
            size_tenths: (size * 10.0).round() as u32,
            colour: colour.straight_rgba8(),
            max_width: max_width.floor() as i32,
            bold,
            field: None,
        };
        if let Some(hit) = self.cache.get(&key) {
            return Some(hit.clone());
        }
        if self.cache.len() >= CACHE_LIMIT {
            self.cache.clear();
        }
        let image = self.render(&key)?;
        self.cache.insert(key, image.clone());
        Some(image)
    }

    pub fn field(
        &mut self,
        text: &str,
        font: &str,
        size: f32,
        colour: Colour,
        width: f32,
        field: Field,
    ) -> Option<TextImage> {
        let key = Key {
            text: text.into(),
            font: font.into(),
            size_tenths: (size * 10.0).round() as u32,
            colour: colour.straight_rgba8(),
            max_width: width.floor().max(1.0) as i32,
            bold: false,
            field: Some(field),
        };
        if let Some(image) = self.cache.get(&key) {
            return Some(image.clone());
        }
        if self.cache.len() >= CACHE_LIMIT {
            self.cache.clear();
        }
        let image = self.render(&key)?;
        self.cache.insert(key, image.clone());
        Some(image)
    }

    pub fn field_hit(&mut self, text: &str, font: &str, size: f32, width: f32, cursor: usize, x: f32) -> usize {
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let mut buffer = Buffer::new(fonts, Metrics::new(size, (size * 1.3).ceil()));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &Attrs::new().family(Family::Name(font)), Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        let cursor_x = buffer.cursor_position(&cosmic_text::Cursor::new(0, cursor)).map(|p| p.0).unwrap_or(0.0);
        let scroll = (cursor_x - width + 8.0).max(0.0).ceil();
        buffer.hit(x + scroll, size * 0.6).map(|c| c.index).unwrap_or(text.len())
    }

    fn render(&mut self, key: &Key) -> Option<TextImage> {
        // Loading the system fonts takes a moment, so it waits for the first text.
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let size = key.size_tenths as f32 / 10.0;
        let metrics = Metrics::new(size, (size * 1.3).ceil());
        let weight = if key.bold { Weight::SEMIBOLD } else { Weight::NORMAL };
        let attrs = Attrs::new().family(Family::Name(&key.font)).weight(weight);

        // Measure; if too wide, drop characters until it fits with "...".
        let mut shown = key.text.clone();
        let mut buffer = Buffer::new(fonts, metrics);
        buffer.set_wrap(Wrap::None);
        let width_of = |buffer: &mut Buffer, fonts: &mut FontSystem, s: &str| {
            buffer.set_text(s, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(fonts, false);
            buffer.layout_runs().map(|r| r.line_w).fold(0.0f32, f32::max)
        };
        let mut width = width_of(&mut buffer, fonts, &shown);
        if key.field.is_none() && width > key.max_width as f32 {
            let chars: Vec<char> = key.text.chars().collect();
            let (mut lo, mut hi) = (0usize, chars.len());
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let candidate: String = chars[..mid].iter().collect::<String>().trim_end().to_string() + "...";
                if width_of(&mut buffer, fonts, &candidate) <= key.max_width as f32 {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            shown = chars[..lo].iter().collect::<String>().trim_end().to_string() + "...";
            width = width_of(&mut buffer, fonts, &shown);
        }
        let cursor_x = |index| buffer.cursor_position(&cosmic_text::Cursor::new(0, index)).map(|p| p.0).unwrap_or(0.0);
        let caret = key.field.map(|field| cursor_x(field.cursor)).unwrap_or(0.0);
        let selection = key
            .field
            .and_then(|field| field.anchor.map(|anchor| (cursor_x(anchor).min(caret), cursor_x(anchor).max(caret))));
        let scroll = if key.field.is_some() { (caret - key.max_width as f32 + 8.0).max(0.0).ceil() } else { 0.0 };
        let w = if key.field.is_some() { key.max_width } else { width.ceil().max(1.0) as i32 };
        let h = metrics.line_height.ceil() as i32;
        buffer.set_size(Some(w as f32 + 1.0), Some(h as f32));

        let [r, g, b, a] = key.colour;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        if let (Some(field), Some((start, end))) = (key.field, selection) {
            let alpha = field.accent[3] / 4;
            let colour = [
                field.accent[0] as u32 * alpha as u32 / 255,
                field.accent[1] as u32 * alpha as u32 / 255,
                field.accent[2] as u32 * alpha as u32 / 255,
                alpha as u32,
            ]
            .map(|c| c as u8);
            for y in 0..h {
                for x in ((start - scroll).floor() as i32).max(0)..((end - scroll).ceil() as i32).min(w) {
                    let i = ((y * w + x) * 4) as usize;
                    pixels[i..i + 4].copy_from_slice(&colour);
                }
            }
        }
        buffer.draw(fonts, &mut self.swash, Color::rgba(r, g, b, a), |x, y, cw, ch, c| {
            for dy in 0..ch as i32 {
                for dx in 0..cw as i32 {
                    let (px, py) = (x + dx - scroll as i32, y + dy);
                    if px < 0 || py < 0 || px >= w || py >= h {
                        continue;
                    }
                    let i = ((py * w + px) * 4) as usize;
                    let alpha = c.a() as f32 / 255.0;
                    // Premultiplied "over" onto what is already there.
                    let src = [c.r() as f32 * alpha, c.g() as f32 * alpha, c.b() as f32 * alpha, c.a() as f32];
                    for k in 0..4 {
                        let dst = pixels[i + k] as f32;
                        pixels[i + k] = (src[k] + dst * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        });
        if let Some(field) = key.field.filter(|field| field.visible) {
            let x = ((caret - scroll).round() as i32).clamp(0, w - 1);
            let [r, g, b, a] = field.accent;
            let colour = [r as u32 * a as u32 / 255, g as u32 * a as u32 / 255, b as u32 * a as u32 / 255, a as u32]
                .map(|c| c as u8);
            for y in 1..h - 1 {
                let i = ((y * w + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&colour);
            }
        }
        let buffer = MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, (w, h), 1, Transform::Normal, None);
        Some(TextImage { buffer, width: w, height: h })
    }
}
