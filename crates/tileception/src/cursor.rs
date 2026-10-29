//! The pointer image drawn when Tileception owns the screen.

use std::io::Read;

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Logical, Point, Transform},
};
use xcursor::{CursorTheme, parser::parse_xcursor};

pub struct Cursor {
    /// In physical pixels.
    pub buffer: MemoryRenderBuffer,
    /// In the buffer's pixels.
    pub hotspot: Point<i32, Logical>,
    /// The output scale it was loaded for.
    pub scale: f64,
}

impl Cursor {
    /// Loads `left_ptr` from `XCURSOR_THEME` (or Adwaita), at `XCURSOR_SIZE`
    /// times the output's scale.
    pub fn load(scale: f64) -> Cursor {
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "Adwaita".into());
        let size: u32 = std::env::var("XCURSOR_SIZE").ok().and_then(|value| value.parse().ok()).unwrap_or(24);
        let size = (f64::from(size) * scale).round() as u32;
        let cursor = load_theme(&theme, size).unwrap_or_else(|| {
            tracing::warn!("cursor theme {theme} unavailable, drawing a plain arrow");
            fallback()
        });
        Cursor { scale, ..cursor }
    }
}

fn load_theme(name: &str, size: u32) -> Option<Cursor> {
    let theme = CursorTheme::load(name);
    let path = ["left_ptr", "default"].iter().find_map(|name| theme.load_icon(name))?;
    let mut data = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut data).ok()?;
    let images = parse_xcursor(&data)?;
    let image = images.iter().min_by_key(|image| (image.size as i64 - size as i64).abs())?;
    let buffer = MemoryRenderBuffer::from_slice(
        &image.pixels_rgba,
        Fourcc::Abgr8888,
        (image.width as i32, image.height as i32),
        1,
        Transform::Normal,
        None,
    );
    Some(Cursor { buffer, hotspot: (image.xhot as i32, image.yhot as i32).into(), scale: 1.0 })
}

/// A white arrow with a black outline, for systems without a cursor theme.
fn fallback() -> Cursor {
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
    let buffer = MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Abgr8888,
        (WIDTH as i32, HEIGHT as i32),
        1,
        Transform::Normal,
        None,
    );
    Cursor { buffer, hotspot: (0, 0).into(), scale: 1.0 }
}
