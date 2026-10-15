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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    font: String,
    size_tenths: u32,
    colour: [u8; 4],
    max_width: i32,
    bold: bool,
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
        if width > key.max_width as f32 {
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
        let w = width.ceil().max(1.0) as i32;
        let h = metrics.line_height.ceil() as i32;
        buffer.set_size(Some(w as f32 + 1.0), Some(h as f32));

        let [r, g, b, a] = key.colour;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        buffer.draw(fonts, &mut self.swash, Color::rgba(r, g, b, a), |x, y, cw, ch, c| {
            for dy in 0..ch as i32 {
                for dx in 0..cw as i32 {
                    let (px, py) = (x + dx, y + dy);
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
        let buffer = MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, (w, h), 1, Transform::Normal, None);
        Some(TextImage { buffer, width: w, height: h })
    }
}
