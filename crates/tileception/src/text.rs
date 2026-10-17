//! Text for the compositor's own interface (tab titles, menus), rasterised
//! with cosmic-text into images that are cached and drawn as textures.

use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, UnderlineStyle, Weight, Wrap,
};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

use crate::{markdown::Span, theme::Colour};

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
    weight: u16,
    /// 0 for one line shortened with "...", otherwise wrapped to at most this many lines.
    lines: u8,
    field: Option<Field>,
}

/// Text as an image. The buffer has physical pixels (see [`Text::set_scale`]);
/// `width` and `height` are its size in logical pixels.
#[derive(Debug, Clone)]
pub struct TextImage {
    pub buffer: MemoryRenderBuffer,
    pub width: f64,
    pub height: f64,
}

/// How to draw styled spans with [`Text::rich`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RichStyle {
    pub size: f32,
    pub colour: Colour,
    /// For links.
    pub accent: Colour,
    pub weight: u16,
    /// Wrapped to at most this many lines, then ended in "...".
    pub lines: u8,
    /// All in the monospace font, as code.
    pub mono: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RichKey {
    spans: Vec<Span>,
    font: String,
    size_tenths: u32,
    colour: [u8; 4],
    accent: [u8; 4],
    max_width: i32,
    weight: u16,
    lines: u8,
    mono: bool,
}

/// Styled text as an image, with where its links are in it.
#[derive(Debug, Clone)]
pub struct RichImage {
    pub image: TextImage,
    /// x, y, width and height within the image (logical pixels), and the address.
    pub links: Vec<([f32; 4], String)>,
}

pub struct Text {
    fonts: Option<FontSystem>,
    swash: SwashCache,
    cache: HashMap<Key, TextImage>,
    rich: HashMap<RichKey, RichImage>,
    /// Physical pixels per logical pixel: text is laid out in logical
    /// pixels and rasterised this much larger, so it stays sharp.
    scale: f32,
}

impl Default for Text {
    fn default() -> Self {
        Text { fonts: None, swash: SwashCache::new(), cache: HashMap::new(), rich: HashMap::new(), scale: 1.0 }
    }
}

/// Paints a glyph's coverage `c` at (`x`, `y`), premultiplied "over" what is
/// already in the `w` x `h` pixels.
fn blend(pixels: &mut [u8], w: i32, h: i32, (x, y): (i32, i32), c: Color) {
    if x < 0 || y < 0 || x >= w || y >= h {
        return;
    }
    let i = ((y * w + x) * 4) as usize;
    let alpha = c.a() as f32 / 255.0;
    let src = [c.r() as f32 * alpha, c.g() as f32 * alpha, c.b() as f32 * alpha, c.a() as f32];
    for k in 0..4 {
        let dst = pixels[i + k] as f32;
        pixels[i + k] = (src[k] + dst * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8;
    }
}

/// The first `chars` characters of `spans`, ended in "...".
fn cut_spans(spans: &[Span], mut chars: usize) -> Vec<Span> {
    let mut out = Vec::new();
    for span in spans {
        let n = span.text.chars().count();
        if n < chars {
            out.push(span.clone());
            chars -= n;
            continue;
        }
        let text: String = span.text.chars().take(chars).collect();
        out.push(Span { text: text.trim_end().to_string() + "...", ..span.clone() });
        return out;
    }
    out
}

impl Text {
    /// Drops cached images, for example after the theme changed.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.rich.clear();
    }

    /// Rasterises at a new output scale from now on.
    pub fn set_scale(&mut self, scale: f32) {
        if self.scale != scale {
            self.scale = scale;
            self.clear();
        }
    }

    /// Spans of bold, italic, code and links, wrapped at word boundaries
    /// to `max_width`.
    pub fn rich(&mut self, spans: &[Span], font: &str, style: RichStyle, max_width: f32) -> Option<RichImage> {
        if spans.iter().all(|s| s.text.is_empty()) || max_width < 4.0 {
            return None;
        }
        let key = RichKey {
            spans: spans.to_vec(),
            font: font.to_string(),
            size_tenths: (style.size * 10.0).round() as u32,
            colour: style.colour.straight_rgba8(),
            accent: style.accent.straight_rgba8(),
            max_width: max_width.floor() as i32,
            weight: style.weight,
            lines: style.lines.max(1),
            mono: style.mono,
        };
        if let Some(hit) = self.rich.get(&key) {
            return Some(hit.clone());
        }
        if self.rich.len() >= CACHE_LIMIT {
            self.rich.clear();
        }
        let image = self.render_rich(&key)?;
        self.rich.insert(key, image.clone());
        Some(image)
    }

    fn render_rich(&mut self, key: &RichKey) -> Option<RichImage> {
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let scale = self.scale;
        let size = key.size_tenths as f32 / 10.0 * scale;
        let max_width = key.max_width as f32 * scale;
        let metrics = Metrics::new(size, (size * 1.4).ceil());
        let family = if key.mono { Family::Monospace } else { Family::Name(&key.font) };
        let base = Attrs::new().family(family).weight(Weight(key.weight));
        let [ar, ag, ab, aa] = key.accent;
        let accent = Color::rgba(ar, ag, ab, aa);
        let [r, g, b, a] = key.colour;
        // Links and code by their place in the spans, counted from 1 (0 is
        // neither), to find them in the layout.
        let attrs = |span: &Span, index: usize| {
            let mut attrs = base.clone();
            if span.marks.bold {
                attrs = attrs.weight(Weight(key.weight.max(Weight::SEMIBOLD.0) + 100));
            }
            if span.marks.italic {
                attrs = attrs.style(cosmic_text::Style::Italic);
            }
            if span.marks.code {
                attrs = attrs.family(Family::Monospace).color(Color::rgba(r, g, b, (a as u32 * 85 / 100) as u8));
            }
            if span.marks.strike {
                attrs = attrs.strikethrough();
            }
            if span.link.is_some() {
                attrs = attrs.color(accent).underline(UnderlineStyle::Single);
            }
            if span.link.is_some() || span.marks.code {
                attrs = attrs.metadata(index + 1);
            }
            attrs
        };
        let mut buffer = Buffer::new(fonts, metrics);
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_size(Some(max_width), None);
        let layout = |buffer: &mut Buffer, fonts: &mut FontSystem, spans: &[Span]| {
            let list: Vec<(&str, Attrs)> =
                spans.iter().enumerate().map(|(i, s)| (s.text.as_str(), attrs(s, i))).collect();
            buffer.set_rich_text(list, &base, Shaping::Advanced, None);
            buffer.shape_until_scroll(fonts, false);
            buffer.layout_runs().count()
        };
        let mut spans = key.spans.clone();
        if layout(&mut buffer, fonts, &spans) > key.lines as usize {
            let total: usize = key.spans.iter().map(|s| s.text.chars().count()).sum();
            let (mut lo, mut hi) = (0usize, total);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                if layout(&mut buffer, fonts, &cut_spans(&key.spans, mid)) <= key.lines as usize {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            spans = cut_spans(&key.spans, lo);
            layout(&mut buffer, fonts, &spans);
        }
        let width = buffer.layout_runs().map(|r| r.line_w).fold(0.0f32, f32::max);
        let rows = buffer.layout_runs().count().max(1);
        // Room for the shading behind code at the end of a line.
        let w = width.ceil().max(1.0) as i32 + (3.0 * scale).ceil() as i32;
        let h = (metrics.line_height * rows as f32).ceil() as i32;
        buffer.set_size(Some(max_width), Some(h as f32));

        // Where the links and code are: their glyphs, a rectangle per line.
        let mut marked: Vec<([f32; 4], &Span)> = Vec::new();
        for run in buffer.layout_runs() {
            let mut current: Option<(usize, f32, f32)> = None;
            let mut close = |current: &mut Option<(usize, f32, f32)>| {
                if let Some((index, x0, x1)) = current.take()
                    && let Some(span) = spans.get(index - 1)
                {
                    marked.push(([x0, run.line_top, x1 - x0, run.line_height], span));
                }
            };
            for glyph in run.glyphs {
                match current {
                    Some((index, _, ref mut x1)) if index == glyph.metadata => *x1 = glyph.x + glyph.w,
                    _ => {
                        close(&mut current);
                        if glyph.metadata > 0 {
                            current = Some((glyph.metadata, glyph.x, glyph.x + glyph.w));
                        }
                    }
                }
            }
            close(&mut current);
        }

        let mut pixels = vec![0u8; (w * h * 4) as usize];
        // Code is shaded, faintly in the text's own colour.
        let shade = [r, g, b].map(|c| (c as u32 * 28 / 255) as u8);
        for ([x, y, cw, ch], _) in marked.iter().filter(|(_, s)| s.marks.code && s.link.is_none()) {
            let pad = 2.0 * scale;
            let (x0, x1) = (((x - pad).floor() as i32).max(0), ((x + cw + pad).ceil() as i32).min(w));
            let (y0, y1) = (((y + ch * 0.12).floor() as i32).max(0), ((y + ch * 0.92).ceil() as i32).min(h));
            for py in y0..y1 {
                for px in x0..x1 {
                    let i = ((py * w + px) * 4) as usize;
                    pixels[i..i + 4].copy_from_slice(&[shade[0], shade[1], shade[2], 28]);
                }
            }
        }
        buffer.draw(fonts, &mut self.swash, Color::rgba(r, g, b, a), |x, y, cw, ch, c| {
            for dy in 0..ch as i32 {
                for dx in 0..cw as i32 {
                    blend(&mut pixels, w, h, (x + dx, y + dy), c);
                }
            }
        });
        let links =
            marked.into_iter().filter_map(|(rect, s)| Some((rect.map(|v| v / scale), s.link.clone()?))).collect();
        let buffer = MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, (w, h), 1, Transform::Normal, None);
        let (width, height) = (f64::from(w) / f64::from(scale), f64::from(h) / f64::from(scale));
        Some(RichImage { image: TextImage { buffer, width, height }, links })
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
        let weight = if bold { Weight::SEMIBOLD.0 } else { Weight::NORMAL.0 };
        self.styled(text, font, size, colour, max_width, weight, 0)
    }

    /// Text at a given weight (100 to 900). With `lines` above 0 it wraps
    /// at word boundaries to `max_width` and ends in "..." after that many lines.
    #[allow(clippy::too_many_arguments, reason = "plain drawing parameters, all needed for the cache key")]
    pub fn styled(
        &mut self,
        text: &str,
        font: &str,
        size: f32,
        colour: Colour,
        max_width: f32,
        weight: u16,
        lines: u8,
    ) -> Option<TextImage> {
        if text.is_empty() || max_width < 4.0 {
            return None;
        }
        let key = Key {
            text: text.to_string(),
            font: font.to_string(),
            size_tenths: (size * 10.0).round() as u32,
            colour: colour.straight_rgba8(),
            // Up, not down: a width measured from this text must fit it again,
            // also once multiplied by a fractional scale.
            max_width: max_width.ceil() as i32,
            weight,
            lines,
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
            weight: Weight::NORMAL.0,
            lines: 0,
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
        // In physical pixels, as the field was drawn.
        let (size, width, x) = (size * self.scale, width * self.scale, x * self.scale);
        let mut buffer = Buffer::new(fonts, Metrics::new(size, (size * 1.3).ceil()));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &Attrs::new().family(Family::Name(font)), Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        let cursor_x = buffer.cursor_position(&cosmic_text::Cursor::new(0, cursor)).map(|p| p.0).unwrap_or(0.0);
        let scroll = (cursor_x - width + 8.0 * self.scale).max(0.0).ceil();
        buffer.hit(x + scroll, size * 0.6).map(|c| c.index).unwrap_or(text.len())
    }

    fn render(&mut self, key: &Key) -> Option<TextImage> {
        // Loading the system fonts takes a moment, so it waits for the first text.
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let scale = self.scale;
        let size = key.size_tenths as f32 / 10.0 * scale;
        // The widest the image may be, in physical pixels.
        let max_width = (key.max_width as f32 * scale).floor() as i32;
        let metrics = Metrics::new(size, (size * 1.3).ceil());
        let attrs = Attrs::new().family(Family::Name(&key.font)).weight(Weight(key.weight));

        // Measure; if too wide (or too many lines), drop characters until it fits with "...".
        let mut shown = key.text.clone();
        let mut buffer = Buffer::new(fonts, metrics);
        let wrapped = key.lines > 0 && key.field.is_none();
        if wrapped {
            buffer.set_wrap(Wrap::WordOrGlyph);
            buffer.set_size(Some(max_width as f32), None);
        } else {
            buffer.set_wrap(Wrap::None);
        }
        let max_lines = key.lines as usize;
        let width_of = |buffer: &mut Buffer, fonts: &mut FontSystem, s: &str| {
            buffer.set_text(s, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(fonts, false);
            buffer.layout_runs().map(|r| r.line_w).fold(0.0f32, f32::max)
        };
        // Too long: wider than allowed on one line, or more lines than allowed when wrapping.
        let too_long = |buffer: &mut Buffer, fonts: &mut FontSystem, s: &str| {
            let width = width_of(buffer, fonts, s);
            if wrapped { buffer.layout_runs().count() > max_lines } else { width > max_width as f32 }
        };
        let mut width = width_of(&mut buffer, fonts, &shown);
        if key.field.is_none() && too_long(&mut buffer, fonts, &shown) {
            let chars: Vec<char> = key.text.chars().collect();
            let (mut lo, mut hi) = (0usize, chars.len());
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let candidate: String = chars[..mid].iter().collect::<String>().trim_end().to_string() + "...";
                if !too_long(&mut buffer, fonts, &candidate) {
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
        let scroll = if key.field.is_some() { (caret - max_width as f32 + 8.0 * scale).max(0.0).ceil() } else { 0.0 };
        let w = if key.field.is_some() { max_width.max(1) } else { width.ceil().max(1.0) as i32 };
        let rows = if wrapped { buffer.layout_runs().count().max(1) } else { 1 };
        let h = (metrics.line_height * rows as f32).ceil() as i32;
        if wrapped {
            buffer.set_size(Some(max_width as f32), Some(h as f32));
        } else {
            buffer.set_size(Some(w as f32 + 1.0), Some(h as f32));
        }

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
                    blend(&mut pixels, w, h, (x + dx - scroll as i32, y + dy), c);
                }
            }
        });
        if let Some(field) = key.field.filter(|field| field.visible) {
            let x = ((caret - scroll).round() as i32).clamp(0, w - 1);
            let [r, g, b, a] = field.accent;
            let colour = [r as u32 * a as u32 / 255, g as u32 * a as u32 / 255, b as u32 * a as u32 / 255, a as u32]
                .map(|c| c as u8);
            // One logical pixel wide.
            let caret_w = scale.round().max(1.0) as i32;
            for y in 1..h - 1 {
                for x in x..(x + caret_w).min(w) {
                    let i = ((y * w + x) * 4) as usize;
                    pixels[i..i + 4].copy_from_slice(&colour);
                }
            }
        }
        let buffer = MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, (w, h), 1, Transform::Normal, None);
        let (width, height) = (f64::from(w) / f64::from(scale), f64::from(h) / f64::from(scale));
        Some(TextImage { buffer, width, height })
    }
}
