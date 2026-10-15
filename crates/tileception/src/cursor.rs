//! The pointer image drawn when Tileception owns the screen.

use std::io::Read;

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Logical, Point, Transform},
};
use xcursor::{parser::parse_xcursor, CursorTheme};

pub struct Cursor {
    pub buffer: MemoryRenderBuffer,
    pub hotspot: Point<i32, Logical>,
}

impl Cursor {
    /// Loads `left_ptr` from `XCURSOR_THEME` (or Adwaita), at `XCURSOR_SIZE`.
    pub fn load() -> Cursor {
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "Adwaita".into());
        let size: u32 = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
        match load_theme(&theme, size) {
            Some(cursor) => cursor,
            None => {
                tracing::warn!("cursor theme {theme} unavailable, drawing a plain arrow");
                fallback()
            }
        }
    }
}

fn load_theme(name: &str, size: u32) -> Option<Cursor> {
    let theme = CursorTheme::load(name);
    let path = ["left_ptr", "default"].iter().find_map(|n| theme.load_icon(n))?;
    let mut data = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut data).ok()?;
    let images = parse_xcursor(&data)?;
    let image = images.iter().min_by_key(|i| (i.size as i64 - size as i64).abs())?;
    let buffer = MemoryRenderBuffer::from_slice(
        &image.pixels_rgba,
        Fourcc::Abgr8888,
        (image.width as i32, image.height as i32),
        1,
        Transform::Normal,
        None,
    );
    Some(Cursor { buffer, hotspot: (image.xhot as i32, image.yhot as i32).into() })
}

/// A white arrow with a black outline, for systems without a cursor theme.
fn fallback() -> Cursor {
    const W: usize = 16;
    const H: usize = 24;
    let mut pixels = vec![0u8; W * H * 4];
    for y in 0..H {
        // The arrow widens by one pixel per row, up to the tail.
        let width = if y < 16 { y + 1 } else { 0 };
        for x in 0..width.min(W) {
            let edge = x == 0 || x + 1 == width || y == 15;
            let i = (y * W + x) * 4;
            let v = if edge { 0 } else { 255 };
            pixels[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let buffer =
        MemoryRenderBuffer::from_slice(&pixels, Fourcc::Abgr8888, (W as i32, H as i32), 1, Transform::Normal, None);
    Cursor { buffer, hotspot: (0, 0).into() }
}
