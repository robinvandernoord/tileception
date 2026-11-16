//! The formats and sizes an area screenshot can be copied or saved in,
//! from the menus beside Copy and Save. PNG at full size stays the default.

use image::{ExtendedColorType, ImageEncoder};
use serde::{Deserialize, Serialize};

/// A file format, with JPG's quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Png,
    /// Lossless: the same pixels as PNG, usually in much less room.
    Webp,
    Jpg(u8),
}

impl Format {
    pub fn label(self) -> String {
        match self {
            Format::Png => "PNG".into(),
            Format::Webp => "WebP".into(),
            Format::Jpg(quality) => format!("JPG {quality}"),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Webp => "webp",
            Format::Jpg(_) => "jpg",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Webp => "image/webp",
            Format::Jpg(_) => "image/jpeg",
        }
    }
}

/// How big the picture comes out: a share of the area's pixels, or a
/// thumbnail at most this many pixels on its longest side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Size {
    Percent(u32),
    Longest(u32),
}

impl Size {
    pub const FULL: Size = Size::Percent(100);

    /// `"50%"`, or `"400px"` for a thumbnail.
    pub fn parse(text: &str) -> Result<Size, String> {
        let text = text.trim();
        let number = |digits: &str| digits.trim().parse::<u32>().ok().filter(|number| *number > 0);
        if let Some(percent) = text.strip_suffix('%').and_then(number) {
            return Ok(Size::Percent(percent.min(100)));
        }
        if let Some(pixels) = text.strip_suffix("px").and_then(number) {
            return Ok(Size::Longest(pixels));
        }
        Err(format!("{text:?} is not a size; use a percentage (\"50%\") or pixels (\"400px\")"))
    }

    pub fn label(self) -> String {
        match self {
            Size::Percent(percent) => format!("{percent}%"),
            Size::Longest(pixels) => format!("Thumb {pixels}"),
        }
    }

    /// The width and height for a picture `width` by `height`, never larger.
    pub fn fit(self, width: u32, height: u32) -> (u32, u32) {
        let ratio = match self {
            Size::Percent(percent) => f64::from(percent) / 100.0,
            Size::Longest(pixels) => f64::from(pixels) / f64::from(width.max(height).max(1)),
        }
        .min(1.0);
        let scaled = |side: u32| ((f64::from(side) * ratio).round() as u32).max(1);
        (scaled(width), scaled(height))
    }
}

/// The `[capture]` section of the config.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// The JPG qualities to choose from (1 to 100); the first is chosen at first.
    pub jpg_qualities: Vec<u8>,
    /// The sizes to choose from: percentages ("50%") or thumbnails ("400px").
    pub sizes: Vec<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self { jpg_qualities: vec![100, 95, 85], sizes: vec!["100%".into(), "50%".into(), "400px".into()] }
    }
}

impl CaptureConfig {
    /// The qualities that are valid, or 85 alone when none is.
    pub fn qualities(&self) -> Vec<u8> {
        let mut qualities: Vec<u8> = Vec::new();
        for quality in self.jpg_qualities.iter().copied().filter(|quality| (1..=100).contains(quality)) {
            if !qualities.contains(&quality) {
                qualities.push(quality);
            }
        }
        if qualities.is_empty() { vec![85] } else { qualities }
    }

    /// The sizes that parse, or full size alone when none does.
    pub fn sizes(&self) -> Vec<Size> {
        let mut sizes: Vec<Size> = Vec::new();
        for text in &self.sizes {
            match Size::parse(text) {
                Ok(size) if !sizes.contains(&size) => sizes.push(size),
                Ok(_) => {}
                Err(error) => tracing::warn!("[capture] sizes: {error}"),
            }
        }
        if sizes.is_empty() { vec![Size::FULL] } else { sizes }
    }
}

/// The picture at `size`: RGBA pixels, width and height.
pub fn resize(rgba: &[u8], width: u32, height: u32, size: Size) -> Option<(Vec<u8>, u32, u32)> {
    let (new_width, new_height) = size.fit(width, height);
    if (new_width, new_height) == (width, height) {
        return Some((rgba.to_vec(), width, height));
    }
    let picture = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let smaller = image::imageops::resize(&picture, new_width, new_height, image::imageops::FilterType::Lanczos3);
    Some((smaller.into_raw(), new_width, new_height))
}

/// Opaque RGBA pixels as a file in `format`.
pub fn encode(rgba: &[u8], width: u32, height: u32, format: Format) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match format {
        Format::Png => {
            let size = resvg::tiny_skia::IntSize::from_wh(width, height).ok_or("the picture is empty")?;
            let pixmap = resvg::tiny_skia::Pixmap::from_vec(rgba.to_vec(), size).ok_or("the picture is cut short")?;
            return super::marks::png(&pixmap);
        }
        Format::Webp => image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .write_image(rgba, width, height, ExtendedColorType::Rgba8)
            .map_err(|error| error.to_string())?,
        Format::Jpg(quality) => {
            // JPG has no alpha; the picture is opaque anyway.
            let rgb: Vec<u8> =
                rgba.as_chunks::<4>().0.iter().flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
                .write_image(&rgb, width, height, ExtendedColorType::Rgb8)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(out)
}

/// A file size for people: "412 KB", "1.4 MB".
pub fn file_size(bytes: usize) -> String {
    let kilobytes = bytes as f64 / 1024.0;
    if kilobytes < 1000.0 { format!("{} KB", kilobytes.ceil() as u64) } else { format!("{:.1} MB", kilobytes / 1024.0) }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_percentages_or_thumbnails() {
        assert_eq!(Size::parse("50%"), Ok(Size::Percent(50)));
        assert_eq!(Size::parse(" 400px "), Ok(Size::Longest(400)));
        assert_eq!(Size::parse("150%"), Ok(Size::Percent(100)));
        assert!(Size::parse("0%").is_err());
        assert!(Size::parse("big").is_err());
        assert_eq!(Size::Longest(400).label(), "Thumb 400");
    }

    #[test]
    fn sizes_never_grow_a_picture() {
        assert_eq!(Size::Percent(50).fit(1001, 600), (501, 300));
        assert_eq!(Size::Longest(400).fit(1600, 800), (400, 200));
        assert_eq!(Size::Longest(400).fit(300, 100), (300, 100));
        assert_eq!(Size::Percent(1).fit(10, 10), (1, 1));
    }

    #[test]
    fn the_config_keeps_valid_unique_choices() {
        let config = CaptureConfig { jpg_qualities: vec![0, 90, 90, 101, 70], sizes: vec!["50%".into(), "x".into()] };
        assert_eq!(config.qualities(), vec![90, 70]);
        assert_eq!(config.sizes(), vec![Size::Percent(50)]);
        let empty = CaptureConfig { jpg_qualities: vec![], sizes: vec![] };
        assert_eq!((empty.qualities(), empty.sizes()), (vec![85], vec![Size::FULL]));
    }

    #[test]
    fn every_format_writes_its_own_kind_of_file() {
        let rgba: Vec<u8> = (0..16u32 * 16).flat_map(|index| [(index % 256) as u8, 40, 200, 255]).collect();
        let png = encode(&rgba, 16, 16, Format::Png).unwrap();
        let webp = encode(&rgba, 16, 16, Format::Webp).unwrap();
        let jpg = encode(&rgba, 16, 16, Format::Jpg(85)).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!((&webp[0..4], &webp[8..12]), (&b"RIFF"[..], &b"WEBP"[..]));
        assert_eq!(&jpg[0..2], &[0xff, 0xd8]);
        // Lossless WebP gives back the very same pixels.
        let decoded = image::load_from_memory(&webp).unwrap().to_rgba8();
        assert_eq!(decoded.into_raw(), rgba);
    }

    #[test]
    fn resizing_keeps_the_pixels_opaque_and_the_shape() {
        let rgba = vec![200; 8 * 4 * 4];
        let (smaller, width, height) = resize(&rgba, 8, 4, Size::Percent(50)).unwrap();
        assert_eq!((width, height, smaller.len()), (4, 2, 4 * 2 * 4));
    }

    #[test]
    fn file_sizes_read_well() {
        assert_eq!(file_size(1), "1 KB");
        assert_eq!(file_size(421_000), "412 KB");
        assert_eq!(file_size(1_468_006), "1.4 MB");
    }
}
