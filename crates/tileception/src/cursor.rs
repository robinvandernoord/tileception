//! The pointer images drawn when Tileception owns the screen: the theme's
//! arrow, and the shapes programs ask for by name (cursor-shape-v1), so every
//! window shows the same theme at the same size.

use std::{
    collections::HashMap,
    io::Read,
    sync::Arc,
    time::{Duration, Instant},
};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::CursorIcon,
    utils::{Logical, Point, Transform},
};
use xcursor::{CursorTheme, parser::parse_xcursor};

/// One pointer picture, in physical pixels.
#[derive(Debug, Clone)]
pub struct Frame {
    pub buffer: MemoryRenderBuffer,
    /// In the picture's pixels.
    pub hotspot: Point<i32, Logical>,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA rows, as in `buffer`, for sending elsewhere.
    pub pixels: Arc<[u8]>,
    /// Its place in its shape's animation; 0 for a still shape.
    pub index: usize,
}

impl Frame {
    fn new(pixels: &[u8], width: u32, height: u32, hotspot: (u32, u32), index: usize) -> Frame {
        let buffer = MemoryRenderBuffer::from_slice(
            pixels,
            Fourcc::Abgr8888,
            (width as i32, height as i32),
            1,
            Transform::Normal,
            None,
        );
        let hotspot = (hotspot.0 as i32, hotspot.1 as i32).into();
        Frame { buffer, hotspot, width, height, pixels: pixels.into(), index }
    }
}

/// A shape's frames, each with how long it shows in milliseconds; one frame
/// with no delay for a still shape.
type Frames = Vec<(Frame, u32)>;

pub struct Cursor {
    /// The theme's arrow.
    arrow: Frame,
    /// The output scale it was loaded for.
    pub scale: f64,
    theme: String,
    /// In physical pixels.
    size: u32,
    /// Named shapes loaded so far; `None` when the theme has no such shape.
    shapes: HashMap<CursorIcon, Option<Frames>>,
    /// When animations started, so every animated shape runs on one clock.
    started: Instant,
}

impl Cursor {
    /// Loads `left_ptr` from `XCURSOR_THEME` (or Adwaita), at `XCURSOR_SIZE`
    /// times the output's scale.
    pub fn load(scale: f64) -> Cursor {
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "Adwaita".into());
        let size: u32 = std::env::var("XCURSOR_SIZE").ok().and_then(|value| value.parse().ok()).unwrap_or(24);
        let size = (f64::from(size) * scale).round() as u32;
        let arrow = load_theme(&theme, size, &["left_ptr", "default"])
            .and_then(|frames| frames.into_iter().next())
            .map_or_else(
                || {
                    tracing::warn!("cursor theme {theme} unavailable, drawing a plain arrow");
                    fallback()
                },
                |(frame, _)| frame,
            );
        Cursor { arrow, scale, theme, size, shapes: HashMap::new(), started: Instant::now() }
    }

    /// The picture for a shape a program asked for, loaded on first use; the
    /// arrow when the theme lacks it. For an animated shape, the frame for
    /// now and how long until the next one.
    pub fn shape(&mut self, icon: CursorIcon) -> (Frame, Option<Duration>) {
        if icon == CursorIcon::Default {
            return (self.arrow.clone(), None);
        }
        let (theme, size) = (&self.theme, self.size);
        let loaded = self.shapes.entry(icon).or_insert_with(|| {
            let names: Vec<&str> = std::iter::once(icon.name()).chain(icon.alt_names().iter().copied()).collect();
            load_theme(theme, size, &names)
        });
        let Some(frames) = loaded.as_ref() else { return (self.arrow.clone(), None) };
        frame_at(frames, self.started.elapsed())
    }
}

/// The frame showing `elapsed` into a looping animation, and how long it
/// still shows.
fn frame_at(frames: &Frames, elapsed: Duration) -> (Frame, Option<Duration>) {
    let total: u64 = frames.iter().map(|(_, delay)| u64::from(*delay)).sum();
    let (Some(first), true) = (frames.first(), frames.len() > 1 && total > 0) else {
        return (frames.first().map(|(frame, _)| frame.clone()).unwrap_or_else(fallback), None);
    };
    let mut into = (elapsed.as_millis() % u128::from(total)) as u64;
    for (frame, delay) in frames {
        let delay = u64::from(*delay);
        if into < delay {
            return (frame.clone(), Some(Duration::from_millis(delay - into)));
        }
        into -= delay;
    }
    (first.0.clone(), Some(Duration::from_millis(u64::from(first.1))))
}

/// The first of `names` the theme has: every frame at the image size closest
/// to `size`.
fn load_theme(name: &str, size: u32, names: &[&str]) -> Option<Frames> {
    let theme = CursorTheme::load(name);
    let path = names.iter().find_map(|name| theme.load_icon(name))?;
    let mut data = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut data).ok()?;
    let images = parse_xcursor(&data)?;
    let nominal = images.iter().min_by_key(|image| (image.size as i64 - size as i64).abs())?.size;
    let frames: Frames = images
        .iter()
        .filter(|image| image.size == nominal)
        .enumerate()
        .map(|(index, image)| {
            let frame = Frame::new(&image.pixels_rgba, image.width, image.height, (image.xhot, image.yhot), index);
            (frame, image.delay)
        })
        .collect();
    (!frames.is_empty()).then_some(frames)
}

/// A white arrow with a black outline, for systems without a cursor theme.
fn fallback() -> Frame {
    const WIDTH: usize = 16;
    const HEIGHT: usize = 24;
    let mut pixels = vec![0u8; WIDTH * HEIGHT * 4];
    for row in 0..HEIGHT {
        // The arrow widens by one pixel per row, up to the tail.
        let width = if row < 16 { row + 1 } else { 0 };
        for column in 0..width.min(WIDTH) {
            let edge = column == 0 || column + 1 == width || row == 15;
            let index = (row * WIDTH + column) * 4;
            let shade = if edge { 0 } else { 255 };
            pixels[index..index + 4].copy_from_slice(&[shade, shade, shade, 255]);
        }
    }
    Frame::new(&pixels, WIDTH as u32, HEIGHT as u32, (0, 0), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(delays: &[u32]) -> Frames {
        delays.iter().enumerate().map(|(index, delay)| (Frame { index, ..fallback() }, *delay)).collect()
    }

    #[test]
    fn animated_shapes_loop_through_their_frames() {
        let animated = frames(&[30, 50, 20]);
        let shown = |millis: u64| {
            let (frame, next) = frame_at(&animated, Duration::from_millis(millis));
            (frame.index, next.map(|wait| wait.as_millis()))
        };
        assert_eq!(shown(0), (0, Some(30)));
        assert_eq!(shown(29), (0, Some(1)));
        assert_eq!(shown(30), (1, Some(50)));
        assert_eq!(shown(95), (2, Some(5)));
        // Round again after 100 ms.
        assert_eq!(shown(130), (1, Some(50)));
        // A still shape asks for no further frames.
        assert_eq!(frame_at(&frames(&[0]), Duration::from_secs(5)).1, None);
    }
}
