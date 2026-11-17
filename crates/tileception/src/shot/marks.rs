//! What can be drawn on an area screenshot, and how it is drawn into
//! pixels. The picker shows the marks drawn this way over the screen, and
//! the copied or saved picture has them drawn the same way into it.

use resvg::tiny_skia::{
    FillRule, IntSize, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapPaint, PixmapRef, Stroke, Transform,
};
use tileception_core::{Point, Rect};

/// What a press inside the selection does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Moves the selection and resizes it by its edges.
    Select,
    Pen,
    Arrow,
    Rectangle,
    Highlighter,
    Text,
    /// Numbered circles, 1, 2, 3..., one for each click.
    Step,
    /// Pixelates a rectangle, in the picture itself.
    Redact,
}

impl Tool {
    pub const ALL: [Tool; 8] = [
        Tool::Select,
        Tool::Pen,
        Tool::Arrow,
        Tool::Rectangle,
        Tool::Highlighter,
        Tool::Text,
        Tool::Step,
        Tool::Redact,
    ];

    /// What the tool is called, with the key that picks it.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Move and resize  V",
            Tool::Pen => "Pen  P",
            Tool::Arrow => "Arrow  A",
            Tool::Rectangle => "Rectangle  R",
            Tool::Highlighter => "Highlighter  H",
            Tool::Text => "Text  T",
            Tool::Step => "Numbered steps  N",
            Tool::Redact => "Redact  B",
        }
    }

    /// The tool a letter picks, as `label` shows.
    pub fn for_key(letter: char) -> Option<Tool> {
        Some(match letter {
            'v' => Tool::Select,
            'p' => Tool::Pen,
            'a' => Tool::Arrow,
            'r' => Tool::Rectangle,
            'h' => Tool::Highlighter,
            't' => Tool::Text,
            'n' => Tool::Step,
            'b' => Tool::Redact,
            _ => return None,
        })
    }
}

/// The colours to draw with: red, amber, green, blue, white and black.
pub const PALETTE: [[u8; 3]; 6] = [
    [0xf0, 0x64, 0x6a],
    [0xf2, 0xb0, 0x4c],
    [0x2b, 0xd4, 0xa7],
    [0x5c, 0x8f, 0xf2],
    [0xf4, 0xf5, 0xf7],
    [0x14, 0x16, 0x1a],
];

/// Line widths in points: thin, medium and thick. Text and the numbered
/// circles grow with them.
pub const WIDTHS: [f64; 3] = [2.0, 4.0, 7.0];

/// How big a redacted rectangle's blocks are, in points.
const REDACT_BLOCK: f64 = 10.0;

/// One thing drawn on the screenshot, in points on the screen (the same
/// space as the selection).
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    Pen {
        points: Vec<Point>,
        colour: [u8; 3],
        width: f64,
    },
    /// A wide stroke that lets what is under it show through.
    Highlighter {
        points: Vec<Point>,
        colour: [u8; 3],
        width: f64,
    },
    Arrow {
        start: Point,
        end: Point,
        colour: [u8; 3],
        width: f64,
    },
    Rectangle {
        start: Point,
        end: Point,
        colour: [u8; 3],
        width: f64,
    },
    /// One line of text with its top left at `corner`.
    Text {
        corner: Point,
        text: String,
        colour: [u8; 3],
        width: f64,
    },
    Step {
        centre: Point,
        number: u32,
        colour: [u8; 3],
        width: f64,
    },
    Redact {
        start: Point,
        end: Point,
    },
}

impl Mark {
    /// Whether it is too small to keep, as a click that was meant as a drag.
    pub fn is_empty(&self) -> bool {
        let tiny = |start: &Point, end: &Point| (end.x - start.x).abs() < 3.0 && (end.y - start.y).abs() < 3.0;
        match self {
            Mark::Arrow { start, end, .. } => tiny(start, end),
            Mark::Rectangle { start, end, .. } | Mark::Redact { start, end } => {
                (end.x - start.x).abs() < 3.0 || (end.y - start.y).abs() < 3.0
            }
            Mark::Text { text, .. } => text.trim().is_empty(),
            Mark::Pen { .. } | Mark::Highlighter { .. } | Mark::Step { .. } => false,
        }
    }

    /// Takes the drag on to `point`: the far corner or end, or the next
    /// point of a stroke.
    pub fn drag_to(&mut self, point: Point) {
        match self {
            Mark::Pen { points, .. } | Mark::Highlighter { points, .. } => {
                let far = points.last().is_none_or(|last| (last.x - point.x).hypot(last.y - point.y) >= 1.0);
                if far {
                    points.push(point);
                }
            }
            Mark::Arrow { end, .. } | Mark::Rectangle { end, .. } | Mark::Redact { end, .. } => *end = point,
            Mark::Text { .. } | Mark::Step { .. } => {}
        }
    }
}

/// The text size for a line width, in points.
pub fn text_size(width: f64) -> f64 {
    12.0 + width * 2.6
}

/// The radius of a numbered circle for a line width, in points.
pub fn step_radius(width: f64) -> f64 {
    9.0 + width * 1.4
}

/// The screen's pixels that redacting hides: all of them, `scale` pixels
/// to a point, RGBA from the top left.
pub struct Source<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// Draws text, `size` pixels high, as premultiplied RGBA: the pixels and
/// their width and height. [`crate::text::Text::pixels`] does it.
pub type DrawText<'a> = dyn FnMut(&str, f32, [u8; 4]) -> Option<(Vec<u8>, u32, u32)> + 'a;

/// Draws `marks` into `pixmap`, which holds the screen from `origin` (in
/// pixels) at `scale` pixels to a point.
pub fn draw(
    pixmap: &mut Pixmap,
    origin: (i32, i32),
    scale: f64,
    marks: &[Mark],
    source: Option<&Source<'_>>,
    text: &mut DrawText<'_>,
) {
    let to_pixels = Transform::from_row(scale as f32, 0.0, 0.0, scale as f32, -origin.0 as f32, -origin.1 as f32);
    for mark in marks {
        match mark {
            Mark::Pen { points, colour, width } => stroke_points(pixmap, points, *colour, 255, *width, to_pixels),
            Mark::Highlighter { points, colour, width } => {
                stroke_points(pixmap, points, *colour, 110, highlighter_width(*width), to_pixels)
            }
            Mark::Arrow { start, end, colour, width } => arrow(pixmap, *start, *end, *colour, *width, to_pixels),
            Mark::Rectangle { start, end, colour, width } => {
                let rect = corners(*start, *end);
                let Some(path) =
                    resvg::tiny_skia::Rect::from_xywh(rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32)
                        .map(PathBuilder::from_rect)
                else {
                    continue;
                };
                pixmap.stroke_path(&path, &paint(*colour, 255), &stroke(*width), to_pixels, None);
            }
            Mark::Text { corner, text: words, colour, width } => {
                let size = (text_size(*width) * scale) as f32;
                let left = (corner.x * scale).round() as i32 - origin.0;
                let top = (corner.y * scale).round() as i32 - origin.1;
                // A soft shadow keeps it readable on any background.
                let shadow = (scale.round() as i32).max(1);
                if let Some(pixels) = text(words, size, [0, 0, 0, 150]) {
                    blit(pixmap, &pixels, (left + shadow, top + shadow));
                }
                if let Some(pixels) = text(words, size, [colour[0], colour[1], colour[2], 255]) {
                    blit(pixmap, &pixels, (left, top));
                }
            }
            Mark::Step { centre, number, colour, width } => {
                let radius = step_radius(*width);
                if let Some(path) = PathBuilder::from_circle(centre.x as f32, centre.y as f32, radius as f32) {
                    pixmap.fill_path(&path, &paint(*colour, 255), FillRule::Winding, to_pixels, None);
                    let edge = Stroke { width: 1.5, ..Stroke::default() };
                    pixmap.stroke_path(&path, &paint([255, 255, 255], 200), &edge, to_pixels, None);
                }
                let ink = if luminance(*colour) > 0.6 { [20, 22, 26, 255] } else { [255, 255, 255, 255] };
                let size = (radius * 1.25 * scale) as f32;
                if let Some(pixels) = text_bold(text, &number.to_string(), size, ink) {
                    let left = (centre.x * scale).round() as i32 - origin.0 - pixels.1 as i32 / 2;
                    let top = (centre.y * scale).round() as i32 - origin.1 - pixels.2 as i32 / 2;
                    blit(pixmap, &pixels, (left, top));
                }
            }
            Mark::Redact { start, end } => redact(pixmap, origin, scale, corners(*start, *end), source),
        }
    }
}

/// The highlighter's stroke for a line width: much wider than the pen's.
fn highlighter_width(width: f64) -> f64 {
    width * 3.0 + 8.0
}

/// Numbers are drawn as text; the text callback has no weight, so the
/// number is drawn twice, a pixel apart, to make it bolder.
fn text_bold(text: &mut DrawText<'_>, words: &str, size: f32, ink: [u8; 4]) -> Option<(Vec<u8>, u32, u32)> {
    let (mut pixels, width, height) = text(words, size, ink)?;
    let row = width as usize * 4;
    let copy = pixels.clone();
    for line in 0..height as usize {
        for column in (1..width as usize).rev() {
            let offset = line * row + column * 4;
            let before = offset - 4;
            for channel in 0..4 {
                pixels[offset + channel] = pixels[offset + channel].max(copy[before + channel]);
            }
        }
    }
    Some((pixels, width, height))
}

/// The rectangle between two corners, whichever way it was dragged.
pub fn corners(start: Point, end: Point) -> Rect {
    Rect::new(start.x.min(end.x), start.y.min(end.y), (end.x - start.x).abs(), (end.y - start.y).abs())
}

fn luminance([red, green, blue]: [u8; 3]) -> f64 {
    (0.299 * f64::from(red) + 0.587 * f64::from(green) + 0.114 * f64::from(blue)) / 255.0
}

fn paint(colour: [u8; 3], alpha: u8) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(colour[0], colour[1], colour[2], alpha);
    paint.anti_alias = true;
    paint
}

fn stroke(width: f64) -> Stroke {
    Stroke { width: width as f32, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() }
}

fn stroke_points(pixmap: &mut Pixmap, points: &[Point], colour: [u8; 3], alpha: u8, width: f64, transform: Transform) {
    let Some(first) = points.first() else { return };
    let mut builder = PathBuilder::new();
    builder.move_to(first.x as f32, first.y as f32);
    if points.len() == 1 {
        // A click draws a dot.
        builder.line_to(first.x as f32 + 0.01, first.y as f32);
    }
    for point in &points[1..] {
        builder.line_to(point.x as f32, point.y as f32);
    }
    if let Some(path) = builder.finish() {
        pixmap.stroke_path(&path, &paint(colour, alpha), &stroke(width), transform, None);
    }
}

/// A line with a filled head at `to`.
fn arrow(pixmap: &mut Pixmap, start: Point, end: Point, colour: [u8; 3], width: f64, transform: Transform) {
    let (run, rise) = (end.x - start.x, end.y - start.y);
    let length = run.hypot(rise);
    if length < 0.5 {
        return;
    }
    let (along_x, along_y) = (run / length, rise / length);
    let head = (width * 3.5 + 6.0).min(length);
    let half = head * 0.55;
    let base = Point::new(end.x - along_x * head, end.y - along_y * head);
    // The shaft stops inside the head, so its round end does not show past the tip.
    let shaft_end = Point::new(end.x - along_x * head * 0.6, end.y - along_y * head * 0.6);
    let mut line = PathBuilder::new();
    line.move_to(start.x as f32, start.y as f32);
    line.line_to(shaft_end.x as f32, shaft_end.y as f32);
    if let Some(path) = line.finish() {
        pixmap.stroke_path(&path, &paint(colour, 255), &stroke(width), transform, None);
    }
    let mut tip = PathBuilder::new();
    tip.move_to(end.x as f32, end.y as f32);
    tip.line_to((base.x - along_y * half) as f32, (base.y + along_x * half) as f32);
    tip.line_to((base.x + along_y * half) as f32, (base.y - along_x * half) as f32);
    tip.close();
    if let Some(path) = tip.finish() {
        pixmap.fill_path(&path, &paint(colour, 255), FillRule::Winding, transform, None);
        let join = Stroke { width: (width * 0.5) as f32, line_join: LineJoin::Round, ..Stroke::default() };
        pixmap.stroke_path(&path, &paint(colour, 255), &join, transform, None);
    }
}

/// Premultiplied RGBA pixels over the pixmap, their top left at `at`.
fn blit(pixmap: &mut Pixmap, (pixels, width, height): &(Vec<u8>, u32, u32), offset: (i32, i32)) {
    if let Some(image) = PixmapRef::from_bytes(pixels, *width, *height) {
        pixmap.draw_pixmap(offset.0, offset.1, image, &PixmapPaint::default(), Transform::identity(), None);
    }
}

/// Blocks of the average colour of the screen under them, so what was
/// there cannot be read or restored. Without the screen's pixels, plain
/// grey blocks.
fn redact(pixmap: &mut Pixmap, origin: (i32, i32), scale: f64, rect: Rect, source: Option<&Source<'_>>) {
    let block = (REDACT_BLOCK * scale).round().max(2.0) as i32;
    let left = (rect.x * scale).round() as i32;
    let top = (rect.y * scale).round() as i32;
    let right = ((rect.x + rect.w) * scale).round() as i32;
    let bottom = ((rect.y + rect.h) * scale).round() as i32;
    let mut row = top;
    while row < bottom {
        let mut column = left;
        let row_end = (row + block).min(bottom);
        while column < right {
            let column_end = (column + block).min(right);
            let colour = source
                .and_then(|source| average(source, (column, row), (column_end, row_end)))
                .unwrap_or([0x4a, 0x4f, 0x59]);
            if let Some(cell) = resvg::tiny_skia::Rect::from_ltrb(
                (column - origin.0) as f32,
                (row - origin.1) as f32,
                (column_end - origin.0) as f32,
                (row_end - origin.1) as f32,
            ) {
                let mut solid = paint(colour, 255);
                solid.anti_alias = false;
                pixmap.fill_rect(cell, &solid, Transform::identity(), None);
            }
            column = column_end;
        }
        row = row_end;
    }
}

/// The average colour of the source's pixels from `start` up to `end`.
fn average(source: &Source<'_>, start: (i32, i32), end: (i32, i32)) -> Option<[u8; 3]> {
    let (width, height) = (source.width as i32, source.height as i32);
    let (left, top) = (start.0.clamp(0, width), start.1.clamp(0, height));
    let (right, bottom) = (end.0.clamp(0, width), end.1.clamp(0, height));
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    for row in top..bottom {
        for column in left..right {
            let offset = (row as usize * source.width as usize + column as usize) * 4;
            let pixel = source.rgba.get(offset..offset + 3)?;
            for channel in 0..3 {
                sum[channel] += u64::from(pixel[channel]);
            }
            count += 1;
        }
    }
    (count > 0).then(|| sum.map(|total| (total / count) as u8))
}

/// A pixmap holding the screen's pixels from `left`, `top`, `width` wide
/// and `height` high, opaque, for drawing the marks into.
pub fn crop(source: &Source<'_>, (left, top, width, height): (u32, u32, u32, u32)) -> Option<Pixmap> {
    let size = IntSize::from_wh(width, height)?;
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for row in top..top + height {
        let start = (row as usize * source.width as usize + left as usize) * 4;
        let line = source.rgba.get(start..start + width as usize * 4)?;
        data.extend(line.as_chunks::<4>().0.iter().flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255]));
    }
    Pixmap::from_vec(data, size)
}

/// The pixmap as a PNG file. Its pixels are opaque, so premultiplied and
/// straight alpha are the same.
pub fn png(pixmap: &Pixmap) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, pixmap.width(), pixmap.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    writer.write_image_data(pixmap.data()).map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())?;
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    fn no_text(_: &str, _: f32, _: [u8; 4]) -> Option<(Vec<u8>, u32, u32)> {
        None
    }

    fn pixel(pixmap: &Pixmap, column: u32, row: u32) -> [u8; 4] {
        let offset = ((row * pixmap.width() + column) * 4) as usize;
        pixmap.data()[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn tools_have_their_own_keys() {
        for tool in Tool::ALL {
            let key = tool.label().chars().last().unwrap().to_ascii_lowercase();
            assert_eq!(Tool::for_key(key), Some(tool), "{tool:?}");
        }
    }

    #[test]
    fn rectangles_are_drawn_where_they_are_on_screen() {
        // The pixmap holds the screen from (10, 10) pixels, at two pixels a point.
        let mut pixmap = Pixmap::new(40, 40).unwrap();
        let mark = Mark::Rectangle {
            start: Point::new(15.0, 15.0),
            end: Point::new(10.0, 10.0),
            colour: [255, 0, 0],
            width: 2.0,
        };
        draw(&mut pixmap, (10, 10), 2.0, &[mark], None, &mut no_text);
        // Its edge at x = 10 points is pixel 20, which is 10 into the pixmap.
        assert_eq!(pixel(&pixmap, 10, 15), [255, 0, 0, 255]);
        // Inside it stays empty.
        assert_eq!(pixel(&pixmap, 15, 15), [0, 0, 0, 0]);
    }

    #[test]
    fn redacting_averages_the_screen_under_each_block() {
        // Ten by ten pixels: the left half black, the right half white.
        let rgba: Vec<u8> =
            (0..100).flat_map(|offset| if offset % 10 < 5 { [0, 0, 0, 255] } else { [255; 4] }).collect();
        let source = Source { rgba: &rgba, width: 10, height: 10 };
        let mut pixmap = crop(&source, (0, 0, 10, 10)).unwrap();
        // One block of ten points across the middle averages to grey.
        let mark = Mark::Redact { start: Point::new(0.0, 0.0), end: Point::new(10.0, 10.0) };
        draw(&mut pixmap, (0, 0), 1.0, &[mark], Some(&source), &mut no_text);
        assert_eq!(pixel(&pixmap, 0, 0), [127, 127, 127, 255]);
        assert_eq!(pixel(&pixmap, 9, 9), [127, 127, 127, 255]);
    }

    #[test]
    fn crops_are_opaque_and_in_place() {
        let rgba: Vec<u8> = (0..16u8).flat_map(|offset| [offset, offset, offset, 0]).collect();
        let source = Source { rgba: &rgba, width: 4, height: 4 };
        let pixmap = crop(&source, (1, 2, 2, 2)).unwrap();
        assert_eq!((pixmap.width(), pixmap.height()), (2, 2));
        assert_eq!(pixel(&pixmap, 0, 0), [9, 9, 9, 255]);
        assert_eq!(pixel(&pixmap, 1, 1), [14, 14, 14, 255]);
        assert!(crop(&source, (3, 3, 2, 2)).is_none());
    }

    #[test]
    fn clicks_and_short_drags_are_not_kept() {
        let dot = Point::new(5.0, 5.0);
        assert!(Mark::Arrow { start: dot, end: Point::new(6.0, 6.0), colour: [0; 3], width: 2.0 }.is_empty());
        assert!(Mark::Redact { start: dot, end: Point::new(50.0, 6.0) }.is_empty());
        assert!(!Mark::Arrow { start: dot, end: Point::new(50.0, 6.0), colour: [0; 3], width: 2.0 }.is_empty());
        assert!(!Mark::Pen { points: vec![dot], colour: [0; 3], width: 2.0 }.is_empty());
    }

    #[test]
    fn pngs_decode_to_the_same_pixels() {
        let rgba: Vec<u8> = (0..4u8).flat_map(|offset| [offset * 60, 0, 0, 255]).collect();
        let source = Source { rgba: &rgba, width: 2, height: 2 };
        let pixmap = crop(&source, (0, 0, 2, 2)).unwrap();
        let bytes = png(&pixmap).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let mut reader = decoder.read_info().unwrap();
        let mut out = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut out).unwrap();
        assert_eq!(out, pixmap.data());
    }
}
