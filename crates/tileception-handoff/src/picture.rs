//! Window pictures over the wire: the changed parts of a picture are cut
//! into tiles, compressed and packed into messages that fit one encrypted
//! frame, and put back together on the other side.

use crate::wire::{PICTURE_BUDGET, Picture, Tile};

/// Tiles are at most this big, so a tile always fits a message even when it
/// does not compress at all.
const TILE_WIDTH: u32 = 128;
const TILE_HEIGHT: u32 = 64;

/// The largest picture accepted, in pixels on a side.
const LARGEST: u32 = 16384;

/// A rectangle of a picture, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// What a picture message is about besides its tiles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    pub window: u64,
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

/// The messages that carry the `changed` areas of an RGBA picture. There is
/// always at least one, the last marked so; it may carry no tiles.
pub fn pack(header: Header, pixels: &[u8], changed: &[Area]) -> Vec<Picture> {
    let message = |tiles: Vec<Tile>, last| Picture {
        window: header.window,
        serial: header.serial,
        width: header.width,
        height: header.height,
        scale: header.scale,
        tiles,
        last,
    };
    let mut messages = Vec::new();
    let mut tiles = Vec::new();
    let mut size = 0;
    for area in changed {
        for tile in cut(*area, header.width, header.height) {
            let compressed = compress(pixels, header.width, tile);
            if size + compressed.len() > PICTURE_BUDGET && !tiles.is_empty() {
                messages.push(message(std::mem::take(&mut tiles), false));
                size = 0;
            }
            size += compressed.len();
            tiles.push(Tile {
                left: tile.left,
                top: tile.top,
                width: tile.width,
                height: tile.height,
                pixels: compressed,
            });
        }
    }
    messages.push(message(tiles, true));
    messages
}

/// An area clipped to the picture and cut into tiles.
fn cut(area: Area, width: u32, height: u32) -> Vec<Area> {
    let right = area.left.saturating_add(area.width).min(width);
    let bottom = area.top.saturating_add(area.height).min(height);
    let mut tiles = Vec::new();
    let mut top = area.top;
    while top < bottom {
        let mut left = area.left;
        while left < right {
            tiles.push(Area { left, top, width: TILE_WIDTH.min(right - left), height: TILE_HEIGHT.min(bottom - top) });
            left += TILE_WIDTH;
        }
        top += TILE_HEIGHT;
    }
    tiles
}

fn compress(pixels: &[u8], width: u32, tile: Area) -> Vec<u8> {
    let stride = width as usize * 4;
    let row = tile.width as usize * 4;
    let mut rows = Vec::with_capacity(row * tile.height as usize);
    for line in tile.top..tile.top + tile.height {
        let start = line as usize * stride + tile.left as usize * 4;
        rows.extend_from_slice(pixels.get(start..start + row).unwrap_or(&[]));
    }
    lz4_flex::compress_prepend_size(&rows)
}

/// A picture put back together from its messages.
#[derive(Debug, Default)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    /// RGBA rows, top to bottom.
    pub pixels: Vec<u8>,
}

impl Canvas {
    /// Paints one message's tiles, starting afresh when the size changed.
    /// False when the message does not make sense; nothing is painted then.
    pub fn paint(&mut self, picture: &Picture) -> bool {
        if picture.width == 0 || picture.height == 0 || picture.width > LARGEST || picture.height > LARGEST {
            return false;
        }
        if (picture.width, picture.height) != (self.width, self.height) {
            self.width = picture.width;
            self.height = picture.height;
            self.pixels = vec![0; picture.width as usize * picture.height as usize * 4];
        }
        let fits = |tile: &Tile| {
            tile.left.checked_add(tile.width).is_some_and(|right| right <= self.width)
                && tile.top.checked_add(tile.height).is_some_and(|bottom| bottom <= self.height)
        };
        let mut rows = Vec::with_capacity(picture.tiles.len());
        for tile in &picture.tiles {
            let Ok(decompressed) = lz4_flex::decompress_size_prepended(&tile.pixels) else { return false };
            if !fits(tile) || decompressed.len() != tile.width as usize * tile.height as usize * 4 {
                return false;
            }
            rows.push(decompressed);
        }
        let stride = self.width as usize * 4;
        for (tile, decompressed) in picture.tiles.iter().zip(rows) {
            let row = tile.width as usize * 4;
            for (line, source) in decompressed.chunks_exact(row).enumerate() {
                let start = (tile.top as usize + line) * stride + tile.left as usize * 4;
                self.pixels[start..start + row].copy_from_slice(source);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let mut state = u32::from(seed) | 1;
        (0..width * height * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect()
    }

    fn header(width: u32, height: u32) -> Header {
        Header { window: 4, serial: 9, width, height, scale: 1.25 }
    }

    #[test]
    fn a_whole_picture_comes_back_the_same() {
        let (width, height) = (300, 170);
        let pixels = noise(width, height, 3);
        let messages = pack(header(width, height), &pixels, &[Area { left: 0, top: 0, width, height }]);
        assert!(messages.len() > 1, "incompressible pixels need several messages");
        assert!(messages.iter().rev().skip(1).all(|message| !message.last));
        assert!(messages.last().is_some_and(|message| message.last));
        let mut canvas = Canvas::default();
        for message in &messages {
            assert!(canvas.paint(message));
        }
        assert_eq!(canvas.pixels, pixels);
    }

    #[test]
    fn only_changed_areas_travel() {
        let (width, height) = (400, 300);
        let before = noise(width, height, 5);
        let mut canvas = Canvas::default();
        for message in pack(header(width, height), &before, &[Area { left: 0, top: 0, width, height }]) {
            canvas.paint(&message);
        }
        let mut after = before.clone();
        let changed = Area { left: 390, top: 290, width: 50, height: 50 };
        for line in 290..300 {
            for column in 390..400 {
                let start = (line * width + column) as usize * 4;
                after[start..start + 4].copy_from_slice(&[1, 2, 3, 4]);
            }
        }
        let messages = pack(header(width, height), &after, &[changed]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tiles.len(), 1, "clipped to the picture");
        assert!(canvas.paint(&messages[0]));
        assert_eq!(canvas.pixels, after);
    }

    #[test]
    fn nothing_changed_still_ends_the_picture() {
        let messages = pack(header(10, 10), &noise(10, 10, 1), &[]);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].last && messages[0].tiles.is_empty());
    }

    #[test]
    fn tiles_outside_the_picture_are_refused() {
        let mut canvas = Canvas::default();
        let pixels = lz4_flex::compress_prepend_size(&[0; 16]);
        let picture = Picture {
            window: 1,
            serial: 1,
            width: 4,
            height: 4,
            scale: 1.0,
            tiles: vec![Tile { left: 3, top: 3, width: 2, height: 2, pixels }],
            last: true,
        };
        assert!(!canvas.paint(&picture));
    }
}
