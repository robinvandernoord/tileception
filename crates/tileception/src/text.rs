//! Text for the compositor's own interface (tab titles, menus), rasterised
//! with cosmic-text into images that are cached and drawn as textures.

use std::{collections::HashMap, ops::Range, sync::Arc};

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
/// Sizes measured without drawing are small: many more are kept.
const SIZES_LIMIT: usize = 8192;

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
    /// The text as drawn, shortened or not.
    pub text: Arc<str>,
    /// Where each character is, for selecting text.
    pub glyphs: Arc<[GlyphBox]>,
    /// What the text covers within the image, if it drew anything.
    pub ink: Option<Ink>,
}

/// The part of a [`TextImage`] its glyphs cover, in logical pixels from the
/// image's top left. The line box also holds room for accents and
/// descenders, so a label without them sits off its centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ink {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Ink {
    pub fn centre(&self) -> (f64, f64) {
        ((self.left + self.right) / 2.0, (self.top + self.bottom) / 2.0)
    }
}

/// The box around the pixels of `pixels` (`width` wide, premultiplied
/// RGBA) at least half as opaque as the most opaque one, so the faint
/// edges of anti-aliasing don't widen it to whole pixels on either side.
fn ink(pixels: &[u8], width: i32, scale: f32) -> Option<Ink> {
    let peak = pixels.as_chunks::<4>().0.iter().map(|pixel| pixel[3]).max().filter(|&alpha| alpha > 0)?;
    let width = width.max(1) as usize;
    let (mut left, mut top, mut right, mut bottom) = (usize::MAX, usize::MAX, 0, 0);
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if u16::from(pixel[3]) * 2 >= u16::from(peak) {
            let (x_pos, y_pos) = (index % width, index / width);
            left = left.min(x_pos);
            top = top.min(y_pos);
            right = right.max(x_pos + 1);
            bottom = bottom.max(y_pos + 1);
        }
    }
    let scale = f64::from(scale);
    Some(Ink {
        left: left as f64 / scale,
        top: top as f64 / scale,
        right: right as f64 / scale,
        bottom: bottom as f64 / scale,
    })
}

/// A drawn character: its place in the image in logical pixels (the whole
/// line's height), and its bytes in [`TextImage::text`].
#[derive(Debug, Clone, Copy)]
pub struct GlyphBox {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub start: usize,
    pub end: usize,
}

/// The text `buffer` holds and where its characters were laid out, scrolled
/// left by `scroll` physical pixels.
fn glyph_boxes(buffer: &Buffer, scale: f32, scroll: f32) -> (Arc<str>, Arc<[GlyphBox]>) {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(buffer.lines.len());
    for line in &buffer.lines {
        starts.push(text.len());
        text.push_str(line.text());
        text.push_str(line.ending().as_str());
    }
    let scale = f64::from(scale);
    let glyphs = buffer
        .layout_runs()
        .flat_map(|run| {
            let base = starts.get(run.line_i).copied().unwrap_or(0);
            run.glyphs.iter().map(move |glyph| GlyphBox {
                left: f64::from(glyph.x - scroll) / scale,
                top: f64::from(run.line_top) / scale,
                width: f64::from(glyph.w) / scale,
                height: f64::from(run.line_height) / scale,
                start: base + glyph.start,
                end: base + glyph.end,
            })
        })
        .collect();
    (text.into(), glyphs)
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RowsKey {
    text: String,
    font: String,
    size_tenths: u32,
    width: i32,
}

pub struct Text {
    fonts: Option<FontSystem>,
    swash: SwashCache,
    cache: HashMap<Key, TextImage>,
    rich: HashMap<RichKey, RichImage>,
    /// Wrapped rows of multi-line fields, see [`Text::rows`].
    rows: HashMap<RowsKey, Vec<Range<usize>>>,
    /// Sizes measured without drawing, see [`Text::styled_size`].
    sizes: HashMap<Key, (f64, f64)>,
    rich_sizes: HashMap<RichKey, (f64, f64)>,
    /// Physical pixels per logical pixel: text is laid out in logical
    /// pixels and rasterised this much larger, so it stays sharp.
    scale: f32,
}

impl Default for Text {
    fn default() -> Self {
        Text {
            fonts: None,
            swash: SwashCache::new(),
            cache: HashMap::new(),
            rich: HashMap::new(),
            rows: HashMap::new(),
            sizes: HashMap::new(),
            rich_sizes: HashMap::new(),
            scale: 1.0,
        }
    }
}

/// Plain text laid out for drawing, cut short with "..." where it must be.
struct PlainShaped {
    buffer: Buffer,
    metrics: Metrics,
    /// The widest it may be, in physical pixels.
    max_width: i32,
    /// Wrapped to lines rather than kept on one.
    wrapped: bool,
    /// Its widest line, in physical pixels.
    width: f32,
}

/// Lays out plain text as [`Text::styled`] draws it, without drawing it.
fn shape_plain(fonts: &mut FontSystem, key: &Key, scale: f32) -> PlainShaped {
    let size = key.size_tenths as f32 / 10.0 * scale;
    let max_width = (key.max_width as f32 * scale).floor() as i32;
    let metrics = Metrics::new(size, (size * 1.3).ceil());
    let attrs = Attrs::new()
        .family(if key.font == "monospace" { Family::Monospace } else { Family::Name(&key.font) })
        .weight(Weight(key.weight));

    // Measure; if too wide (or too many lines), drop characters until it fits with "...".
    let mut buffer = Buffer::new(fonts, metrics);
    let wrapped = key.lines > 0 && key.field.is_none();
    if wrapped {
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_size(Some(max_width as f32), None);
    } else {
        buffer.set_wrap(Wrap::None);
    }
    let max_lines = key.lines as usize;
    let width_of = |buffer: &mut Buffer, fonts: &mut FontSystem, sample: &str| {
        buffer.set_text(sample, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        buffer.layout_runs().map(|run| run.line_w).fold(0.0f32, f32::max)
    };
    // Too long: wider than allowed on one line, or more lines than allowed when wrapping.
    let exceeds = |buffer: &Buffer, width: f32| {
        if wrapped { buffer.layout_runs().count() > max_lines } else { width > max_width as f32 }
    };
    let mut width = width_of(&mut buffer, fonts, &key.text);
    if key.field.is_none() && exceeds(&buffer, width) {
        let chars: Vec<char> = key.text.chars().collect();
        let (mut low, mut high) = (0usize, chars.len());
        while low < high {
            let mid = (low + high).div_ceil(2);
            let candidate: String = chars[..mid].iter().collect::<String>().trim_end().to_string() + "...";
            let candidate_width = width_of(&mut buffer, fonts, &candidate);
            if !exceeds(&buffer, candidate_width) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        let shown = chars[..low].iter().collect::<String>().trim_end().to_string() + "...";
        width = width_of(&mut buffer, fonts, &shown);
    }
    PlainShaped { buffer, metrics, max_width, wrapped, width }
}

/// The size of plain text in physical pixels, as [`Text::styled`] draws it.
fn plain_pixels(shaped: &PlainShaped, field: bool) -> (i32, i32) {
    let pixel_width = if field { shaped.max_width.max(1) } else { shaped.width.ceil().max(1.0) as i32 };
    let rows = if shaped.wrapped { shaped.buffer.layout_runs().count().max(1) } else { 1 };
    (pixel_width, (shaped.metrics.line_height * rows as f32).ceil() as i32)
}

/// Rich text laid out for drawing: the buffer at its final size, the spans
/// shown (cut short with "..." past its lines) and its size in pixels.
struct RichShaped {
    buffer: Buffer,
    spans: Vec<Span>,
    pixel_width: i32,
    pixel_height: i32,
}

/// The cache key for rich text, or `None` for nothing to show.
fn rich_key(spans: &[Span], font: &str, style: RichStyle, max_width: f32) -> Option<RichKey> {
    if spans.iter().all(|span| span.text.is_empty()) || max_width < 4.0 {
        return None;
    }
    Some(RichKey {
        spans: spans.to_vec(),
        font: font.to_string(),
        size_tenths: (style.size * 10.0).round() as u32,
        colour: style.colour.straight_rgba8(),
        accent: style.accent.straight_rgba8(),
        max_width: max_width.floor() as i32,
        weight: style.weight,
        lines: style.lines.max(1),
        mono: style.mono,
    })
}

/// Lays out rich text as [`Text::rich`] draws it, without drawing it.
fn shape_rich(fonts: &mut FontSystem, key: &RichKey, scale: f32) -> RichShaped {
    let size = key.size_tenths as f32 / 10.0 * scale;
    let max_width = key.max_width as f32 * scale;
    let metrics = Metrics::new(size, (size * 1.4).ceil());
    let family = if key.mono || key.font == "monospace" { Family::Monospace } else { Family::Name(&key.font) };
    let base = Attrs::new().family(family).weight(Weight(key.weight));
    let [accent_red, accent_green, accent_blue, accent_alpha] = key.accent;
    let accent = Color::rgba(accent_red, accent_green, accent_blue, accent_alpha);
    let [red, green, blue, alpha] = key.colour;
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
            attrs =
                attrs.family(Family::Monospace).color(Color::rgba(red, green, blue, (alpha as u32 * 85 / 100) as u8));
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
            spans.iter().enumerate().map(|(index, span)| (span.text.as_str(), attrs(span, index))).collect();
        buffer.set_rich_text(list, &base, Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        buffer.layout_runs().count()
    };
    let mut spans = key.spans.clone();
    if layout(&mut buffer, fonts, &spans) > key.lines as usize {
        let total: usize = key.spans.iter().map(|span| span.text.chars().count()).sum();
        let (mut low, mut high) = (0usize, total);
        while low < high {
            let mid = (low + high).div_ceil(2);
            if layout(&mut buffer, fonts, &cut_spans(&key.spans, mid)) <= key.lines as usize {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        spans = cut_spans(&key.spans, low);
        layout(&mut buffer, fonts, &spans);
    }
    let width = buffer.layout_runs().map(|run| run.line_w).fold(0.0f32, f32::max);
    let rows = buffer.layout_runs().count().max(1);
    // Room for the shading behind code at the end of a line.
    let pixel_width = width.ceil().max(1.0) as i32 + (3.0 * scale).ceil() as i32;
    let pixel_height = (metrics.line_height * rows as f32).ceil() as i32;
    buffer.set_size(Some(max_width), Some(pixel_height as f32));
    RichShaped { buffer, spans, pixel_width, pixel_height }
}

/// Paints a glyph's coverage at (`x_pos`, `y_pos`), premultiplied "over" what
/// is already in the `width` x `height` pixels.
fn blend(pixels: &mut [u8], width: i32, height: i32, (x_pos, y_pos): (i32, i32), colour: Color) {
    let src_alpha = colour.a();
    if src_alpha == 0 || x_pos < 0 || y_pos < 0 || x_pos >= width || y_pos >= height {
        return;
    }
    let offset = ((y_pos * width + x_pos) * 4) as usize;
    let alpha_u32 = u32::from(src_alpha);
    if src_alpha == 255 || pixels[offset + 3] == 0 {
        pixels[offset] = ((u32::from(colour.r()) * alpha_u32 + 127) / 255) as u8;
        pixels[offset + 1] = ((u32::from(colour.g()) * alpha_u32 + 127) / 255) as u8;
        pixels[offset + 2] = ((u32::from(colour.b()) * alpha_u32 + 127) / 255) as u8;
        pixels[offset + 3] = src_alpha;
        return;
    }
    let inv_alpha = 255 - alpha_u32;
    let src_rgb = [u32::from(colour.r()), u32::from(colour.g()), u32::from(colour.b())];
    for channel in 0..3 {
        let blended = (src_rgb[channel] * alpha_u32 + u32::from(pixels[offset + channel]) * inv_alpha + 127) / 255;
        pixels[offset + channel] = blended.min(255) as u8;
    }
    let blended_alpha = alpha_u32 + (u32::from(pixels[offset + 3]) * inv_alpha + 127) / 255;
    pixels[offset + 3] = blended_alpha.min(255) as u8;
}

/// The first `chars` characters of `spans`, ended in "...".
fn cut_spans(spans: &[Span], mut chars: usize) -> Vec<Span> {
    let mut out = Vec::new();
    for span in spans {
        let count = span.text.chars().count();
        if count < chars {
            out.push(span.clone());
            chars -= count;
            continue;
        }
        let text: String = span.text.chars().take(chars).collect();
        out.push(Span { text: text.trim_end().to_string() + "...", ..span.clone() });
        return out;
    }
    out
}

impl Text {
    /// Entries and texture bytes of the plain and rich text caches, for
    /// `tileception msg caches`.
    pub fn stats(&self) -> ((usize, usize), (usize, usize)) {
        let pixels = f64::from(self.scale) * f64::from(self.scale);
        let bytes = |image: &TextImage| (image.width * image.height * pixels * 4.0) as usize;
        let sum = |images: &mut dyn Iterator<Item = &TextImage>| {
            images.fold((0, 0), |(count, total), image| (count + 1, total + bytes(image)))
        };
        (sum(&mut self.cache.values()), sum(&mut self.rich.values().map(|rich| &rich.image)))
    }

    /// Drops cached images, for example after the theme changed.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.rich.clear();
        self.rows.clear();
        self.sizes.clear();
        self.rich_sizes.clear();
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
    /// The size [`Text::rich`] gives these spans, in logical pixels, laid out
    /// but not drawn: for measuring many texts of which few are shown.
    pub fn rich_size(&mut self, spans: &[Span], font: &str, style: RichStyle, max_width: f32) -> Option<(f64, f64)> {
        let key = rich_key(spans, font, style, max_width)?;
        if let Some(hit) = self.rich.get(&key) {
            return Some((hit.image.width, hit.image.height));
        }
        if let Some(size) = self.rich_sizes.get(&key) {
            return Some(*size);
        }
        let scale = self.scale;
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let shaped = shape_rich(fonts, &key, scale);
        let size =
            (f64::from(shaped.pixel_width) / f64::from(scale), f64::from(shaped.pixel_height) / f64::from(scale));
        if self.rich_sizes.len() >= SIZES_LIMIT {
            self.rich_sizes.clear();
        }
        self.rich_sizes.insert(key, size);
        Some(size)
    }

    pub fn rich(&mut self, spans: &[Span], font: &str, style: RichStyle, max_width: f32) -> Option<RichImage> {
        let key = rich_key(spans, font, style, max_width)?;
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
        let scale = self.scale;
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let RichShaped { buffer, spans, pixel_width, pixel_height } = shape_rich(fonts, key, scale);
        let [red, green, blue, alpha] = key.colour;
        let mut buffer = buffer;

        // Where the links and code are: their glyphs, a rectangle per line.
        let mut marked: Vec<([f32; 4], &Span)> = Vec::new();
        for run in buffer.layout_runs() {
            let mut current: Option<(usize, f32, f32)> = None;
            let mut close = |current: &mut Option<(usize, f32, f32)>| {
                if let Some((index, start_x, end_x)) = current.take()
                    && let Some(span) = spans.get(index - 1)
                {
                    marked.push(([start_x, run.line_top, end_x - start_x, run.line_height], span));
                }
            };
            for glyph in run.glyphs {
                match current {
                    Some((index, _, ref mut end_x)) if index == glyph.metadata => *end_x = glyph.x + glyph.w,
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

        let mut pixels = vec![0u8; (pixel_width * pixel_height * 4) as usize];
        // Code is shaded, faintly in the text's own colour.
        let shade = [red, green, blue].map(|channel| (channel as u32 * 28 / 255) as u8);
        for ([span_x, span_y, span_width, span_height], _) in
            marked.iter().filter(|(_, span)| span.marks.code && span.link.is_none())
        {
            let pad = 2.0 * scale;
            let (left, right) =
                (((span_x - pad).floor() as i32).max(0), ((span_x + span_width + pad).ceil() as i32).min(pixel_width));
            let (top, bottom) = (
                ((span_y + span_height * 0.12).floor() as i32).max(0),
                ((span_y + span_height * 0.92).ceil() as i32).min(pixel_height),
            );
            for pixel_y in top..bottom {
                for pixel_x in left..right {
                    let offset = ((pixel_y * pixel_width + pixel_x) * 4) as usize;
                    pixels[offset..offset + 4].copy_from_slice(&[shade[0], shade[1], shade[2], 28]);
                }
            }
        }
        buffer.draw(
            fonts,
            &mut self.swash,
            Color::rgba(red, green, blue, alpha),
            |glyph_x, glyph_y, glyph_width, glyph_height, colour| {
                for delta_y in 0..glyph_height as i32 {
                    for delta_x in 0..glyph_width as i32 {
                        blend(&mut pixels, pixel_width, pixel_height, (glyph_x + delta_x, glyph_y + delta_y), colour);
                    }
                }
            },
        );
        let links = marked
            .into_iter()
            .filter_map(|(rect, span)| Some((rect.map(|coord| coord / scale), span.link.clone()?)))
            .collect();
        let (text, glyphs) = glyph_boxes(&buffer, scale, 0.0);
        let ink = ink(&pixels, pixel_width, scale);
        let buffer = MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Abgr8888,
            (pixel_width, pixel_height),
            1,
            Transform::Normal,
            None,
        );
        let (width, height) = (f64::from(pixel_width) / f64::from(scale), f64::from(pixel_height) / f64::from(scale));
        Some(RichImage { image: TextImage { buffer, width, height, text, glyphs, ink }, links })
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

    /// The size [`Text::styled`] gives this text, in logical pixels, laid
    /// out but not drawn: for measuring many texts of which few are shown.
    #[allow(clippy::too_many_arguments, reason = "the same parameters as `styled`, all in the cache key")]
    pub fn styled_size(
        &mut self,
        text: &str,
        font: &str,
        size: f32,
        colour: Colour,
        max_width: f32,
        weight: u16,
        lines: u8,
    ) -> Option<(f64, f64)> {
        if text.is_empty() || max_width < 4.0 {
            return None;
        }
        let key = Key {
            text: text.to_string(),
            font: font.to_string(),
            size_tenths: (size * 10.0).round() as u32,
            colour: colour.straight_rgba8(),
            max_width: max_width.ceil() as i32,
            weight,
            lines,
            field: None,
        };
        if let Some(hit) = self.cache.get(&key) {
            return Some((hit.width, hit.height));
        }
        if let Some(size) = self.sizes.get(&key) {
            return Some(*size);
        }
        let scale = self.scale;
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let (pixel_width, pixel_height) = plain_pixels(&shape_plain(fonts, &key, scale), false);
        let size = (f64::from(pixel_width) / f64::from(scale), f64::from(pixel_height) / f64::from(scale));
        if self.sizes.len() >= SIZES_LIMIT {
            self.sizes.clear();
        }
        self.sizes.insert(key, size);
        Some(size)
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

    /// Splits text into rows that fit `width`, wrapping between words, or
    /// inside a word too long for a row, as wrapped labels do. Each newline
    /// starts a row. Rows are byte ranges that together cover the text apart
    /// from its newlines, so every caret position falls in a row.
    pub fn rows(&mut self, text: &str, font: &str, size: f32, width: f32) -> Vec<Range<usize>> {
        let key = RowsKey {
            text: text.to_owned(),
            font: font.to_owned(),
            size_tenths: (size * 10.0).round() as u32,
            width: width.floor().max(1.0) as i32,
        };
        if let Some(rows) = self.rows.get(&key) {
            return rows.clone();
        }
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        // In physical pixels, as the rows are drawn.
        let size = size * self.scale;
        let mut buffer = Buffer::new(fonts, Metrics::new(size, (size * 1.3).ceil()));
        buffer.set_wrap(Wrap::WordOrGlyph);
        buffer.set_size(Some(key.width as f32 * self.scale), None);
        let family = if font == "monospace" { Family::Monospace } else { Family::Name(font) };
        let mut rows = Vec::new();
        let mut start = 0;
        for line in text.split('\n') {
            buffer.set_text(line, &Attrs::new().family(family), Shaping::Advanced, None);
            buffer.shape_until_scroll(fonts, false);
            // Where each visual row begins within the line; the first row
            // always begins at the line's start.
            let mut breaks: Vec<usize> = buffer
                .layout_runs()
                .skip(1)
                .filter_map(|run| run.glyphs.iter().map(|glyph| glyph.start).min())
                .filter(|&offset| offset > 0 && offset < line.len())
                .collect();
            breaks.sort_unstable();
            breaks.dedup();
            let mut row_start = 0;
            for offset in breaks.into_iter().chain([line.len()]) {
                rows.push(start + row_start..start + offset);
                row_start = offset;
            }
            start += line.len() + 1;
        }
        if self.rows.len() >= CACHE_LIMIT {
            self.rows.clear();
        }
        self.rows.insert(key, rows.clone());
        rows
    }

    pub fn field_hit(&mut self, text: &str, font: &str, size: f32, width: f32, cursor: usize, x_pos: f32) -> usize {
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        // In physical pixels, as the field was drawn.
        let (size, width, x_pos) = (size * self.scale, width * self.scale, x_pos * self.scale);
        let mut buffer = Buffer::new(fonts, Metrics::new(size, (size * 1.3).ceil()));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(
            text,
            &Attrs::new().family(if font == "monospace" { Family::Monospace } else { Family::Name(font) }),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(fonts, false);
        let cursor_x =
            buffer.cursor_position(&cosmic_text::Cursor::new(0, cursor)).map(|position| position.0).unwrap_or(0.0);
        let scroll = (cursor_x - width + 8.0 * self.scale).max(0.0).ceil();
        buffer.hit(x_pos + scroll, size * 0.6).map(|hit| hit.index).unwrap_or(text.len())
    }

    fn render(&mut self, key: &Key) -> Option<TextImage> {
        let scale = self.scale;
        // Loading the system fonts takes a moment, so it waits for the first text.
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let shaped = shape_plain(fonts, key, scale);
        let (pixel_width, pixel_height) = plain_pixels(&shaped, key.field.is_some());
        let PlainShaped { mut buffer, max_width, wrapped, .. } = shaped;
        let cursor_x = |index| {
            buffer.cursor_position(&cosmic_text::Cursor::new(0, index)).map(|position| position.0).unwrap_or(0.0)
        };
        let caret = key.field.map(|field| cursor_x(field.cursor)).unwrap_or(0.0);
        let selection = key
            .field
            .and_then(|field| field.anchor.map(|anchor| (cursor_x(anchor).min(caret), cursor_x(anchor).max(caret))));
        let scroll = if key.field.is_some() { (caret - max_width as f32 + 8.0 * scale).max(0.0).ceil() } else { 0.0 };
        if wrapped {
            buffer.set_size(Some(max_width as f32), Some(pixel_height as f32));
        } else {
            buffer.set_size(Some(pixel_width as f32 + 1.0), Some(pixel_height as f32));
        }

        let [red, green, blue, alpha] = key.colour;
        let mut pixels = vec![0u8; (pixel_width * pixel_height * 4) as usize];
        if let (Some(field), Some((start, end))) = (key.field, selection) {
            let alpha = field.accent[3] / 4;
            let colour = [
                field.accent[0] as u32 * alpha as u32 / 255,
                field.accent[1] as u32 * alpha as u32 / 255,
                field.accent[2] as u32 * alpha as u32 / 255,
                alpha as u32,
            ]
            .map(|channel| channel as u8);
            for pixel_y in 0..pixel_height {
                for pixel_x in ((start - scroll).floor() as i32).max(0)..((end - scroll).ceil() as i32).min(pixel_width)
                {
                    let offset = ((pixel_y * pixel_width + pixel_x) * 4) as usize;
                    pixels[offset..offset + 4].copy_from_slice(&colour);
                }
            }
        }
        buffer.draw(
            fonts,
            &mut self.swash,
            Color::rgba(red, green, blue, alpha),
            |glyph_x, glyph_y, glyph_width, glyph_height, colour| {
                for delta_y in 0..glyph_height as i32 {
                    for delta_x in 0..glyph_width as i32 {
                        blend(
                            &mut pixels,
                            pixel_width,
                            pixel_height,
                            (glyph_x + delta_x - scroll as i32, glyph_y + delta_y),
                            colour,
                        );
                    }
                }
            },
        );
        if let Some(field) = key.field.filter(|field| field.visible) {
            let caret_x = ((caret - scroll).round() as i32).clamp(0, pixel_width - 1);
            let [accent_red, accent_green, accent_blue, accent_alpha] = field.accent;
            let colour = [
                accent_red as u32 * accent_alpha as u32 / 255,
                accent_green as u32 * accent_alpha as u32 / 255,
                accent_blue as u32 * accent_alpha as u32 / 255,
                accent_alpha as u32,
            ]
            .map(|channel| channel as u8);
            // One logical pixel wide.
            let caret_w = scale.round().max(1.0) as i32;
            for pixel_y in 1..pixel_height - 1 {
                for pixel_x in caret_x..(caret_x + caret_w).min(pixel_width) {
                    let offset = ((pixel_y * pixel_width + pixel_x) * 4) as usize;
                    pixels[offset..offset + 4].copy_from_slice(&colour);
                }
            }
        }
        let (text, glyphs) = glyph_boxes(&buffer, scale, scroll);
        let ink = ink(&pixels, pixel_width, scale);
        let buffer = MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Abgr8888,
            (pixel_width, pixel_height),
            1,
            Transform::Normal,
            None,
        );
        let (width, height) = (f64::from(pixel_width) / f64::from(scale), f64::from(pixel_height) / f64::from(scale));
        Some(TextImage { buffer, width, height, text, glyphs, ink })
    }

    /// One line of text as premultiplied RGBA pixels, `size` pixels high
    /// whatever the output's scale, for drawing into a picture (the marks
    /// on an area screenshot). Returns the pixels, their width and height.
    pub fn pixels(
        &mut self,
        text: &str,
        font: &str,
        size: f32,
        colour: [u8; 4],
        bold: bool,
    ) -> Option<(Vec<u8>, u32, u32)> {
        if text.is_empty() || size < 1.0 {
            return None;
        }
        let fonts = self.fonts.get_or_insert_with(FontSystem::new);
        let metrics = Metrics::new(size, (size * 1.3).ceil());
        let weight = if bold { Weight::BOLD } else { Weight::NORMAL };
        let attrs = Attrs::new()
            .family(if font == "monospace" { Family::Monospace } else { Family::Name(font) })
            .weight(weight);
        let mut buffer = Buffer::new(fonts, metrics);
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);
        let width = buffer.layout_runs().map(|run| run.line_w).fold(0.0f32, f32::max).ceil().max(1.0) as i32;
        let height = metrics.line_height.ceil() as i32;
        buffer.set_size(Some(width as f32 + 1.0), Some(height as f32));
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let [red, green, blue, alpha] = colour;
        buffer.draw(
            fonts,
            &mut self.swash,
            Color::rgba(red, green, blue, alpha),
            |left, top, glyph_w, glyph_h, colour| {
                for delta_y in 0..glyph_h as i32 {
                    for delta_x in 0..glyph_w as i32 {
                        blend(&mut pixels, width, height, (left + delta_x, top + delta_y), colour);
                    }
                }
            },
        );
        Some((pixels, width as u32, height as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4×3 image with `opaque` pixels fully covered and `faint` ones at a
    /// quarter.
    fn pixels(opaque: &[(usize, usize)], faint: &[(usize, usize)]) -> Vec<u8> {
        let mut pixels = vec![0; 4 * 3 * 4];
        for (&(x_pos, y_pos), alpha) in
            opaque.iter().map(|place| (place, 255)).chain(faint.iter().map(|place| (place, 64)))
        {
            pixels[(y_pos * 4 + x_pos) * 4 + 3] = alpha;
        }
        pixels
    }

    #[test]
    fn ink_covers_the_drawn_pixels_without_their_faint_edges() {
        let ink = ink(&pixels(&[(1, 1), (2, 1)], &[(0, 0), (3, 2)]), 4, 1.0).expect("pixels were drawn");
        assert_eq!(ink, Ink { left: 1.0, top: 1.0, right: 3.0, bottom: 2.0 });
        assert_eq!(ink.centre(), (2.0, 1.5));
    }

    #[test]
    fn ink_is_in_logical_pixels() {
        let ink = ink(&pixels(&[(1, 1), (2, 2)], &[]), 4, 2.0).expect("pixels were drawn");
        assert_eq!(ink, Ink { left: 0.5, top: 0.5, right: 1.5, bottom: 1.5 });
    }

    #[test]
    fn nothing_drawn_has_no_ink() {
        assert_eq!(ink(&pixels(&[], &[]), 4, 1.0), None);
    }

    #[test]
    fn blend_premultiplies_and_composites_over() {
        let mut buf = vec![0u8; 4];
        // Transparent source leaves destination untouched.
        blend(&mut buf, 1, 1, (0, 0), Color::rgba(200, 100, 50, 0));
        assert_eq!(buf, [0, 0, 0, 0]);
        // Onto empty destination: premultiplied straight write.
        blend(&mut buf, 1, 1, (0, 0), Color::rgba(200, 100, 50, 128));
        assert_eq!(buf, [100, 50, 25, 128]);
        // Translucent over existing pixel.
        blend(&mut buf, 1, 1, (0, 0), Color::rgba(0, 200, 100, 128));
        assert_eq!(buf, [50, 125, 63, 192]);
        // Opaque source replaces destination.
        blend(&mut buf, 1, 1, (0, 0), Color::rgba(10, 20, 30, 255));
        assert_eq!(buf, [10, 20, 30, 255]);
    }
}
