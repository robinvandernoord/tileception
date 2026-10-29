//! The background image, from `wallpaper` in the `[theme]` section. It is
//! decoded and scaled to the output once, then drawn behind everything. The
//! lock screen uses the same image.

use std::path::{Path, PathBuf};

use image::{DynamicImage, GenericImageView, RgbaImage, imageops::FilterType};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

/// How the image is made to fit the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scaling {
    /// Cover the whole screen, cropping what sticks out (keeps proportions).
    Fill,
    /// Show the whole image, with bars where the proportions differ.
    Fit,
    /// Stretch to the screen, ignoring proportions.
    Stretch,
    /// Original size, centred.
    Center,
}

impl Scaling {
    pub fn parse(value: &str) -> Result<Scaling, String> {
        match value {
            "fill" => Ok(Scaling::Fill),
            "fit" => Ok(Scaling::Fit),
            "stretch" => Ok(Scaling::Stretch),
            "center" => Ok(Scaling::Center),
            other => Err(format!("wallpaper_fit {other:?} is not fill, fit, stretch or center")),
        }
    }

    /// The matching swaylock `--scaling` value.
    pub fn swaylock(self) -> &'static str {
        match self {
            Scaling::Fill => "fill",
            Scaling::Fit => "fit",
            Scaling::Stretch => "stretch",
            Scaling::Center => "center",
        }
    }
}

/// A decoded wallpaper, scaled for one output size.
#[derive(Debug, Clone)]
pub struct Wallpaper {
    pub path: PathBuf,
    pub fit: Scaling,
    pub size: (u32, u32),
    pub buffer: MemoryRenderBuffer,
}

/// Expands a leading `~/` to the home directory.
pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Loads and scales `path` for a `width` x `height` screen, `background` filling any
/// bars (premultiplied RGBA).
pub fn load(path: &Path, fit: Scaling, width: u32, height: u32, background: [f32; 4]) -> Result<Wallpaper, String> {
    let img = image::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let canvas = compose(&img, fit, width, height, background);
    let buffer = MemoryRenderBuffer::from_slice(
        canvas.as_raw(),
        Fourcc::Abgr8888,
        (width as i32, height as i32),
        1,
        Transform::Normal,
        None,
    );
    Ok(Wallpaper { path: path.to_path_buf(), fit, size: (width, height), buffer })
}

fn compose(img: &DynamicImage, fit: Scaling, width: u32, height: u32, background: [f32; 4]) -> RgbaImage {
    let (image_width, image_height) = img.dimensions();
    let background_pixel = background.map(|channel| (channel * 255.0).round() as u8);
    let mut canvas = RgbaImage::from_pixel(width, height, image::Rgba(background_pixel));
    let place = |canvas: &mut RgbaImage, part: &RgbaImage| {
        let offset_x = (width as i64 - part.width() as i64) / 2;
        let offset_y = (height as i64 - part.height() as i64) / 2;
        image::imageops::overlay(canvas, part, offset_x, offset_y);
    };
    match fit {
        Scaling::Stretch => canvas = img.resize_exact(width, height, FilterType::Triangle).to_rgba8(),
        Scaling::Fill => canvas = img.resize_to_fill(width, height, FilterType::Triangle).to_rgba8(),
        Scaling::Fit => place(&mut canvas, &img.resize(width, height, FilterType::Triangle).to_rgba8()),
        Scaling::Center => {
            let part = img
                .crop_imm(
                    image_width.saturating_sub(width) / 2,
                    image_height.saturating_sub(height) / 2,
                    image_width.min(width),
                    image_height.min(height),
                )
                .to_rgba8();
            place(&mut canvas, &part);
        }
    }
    // Photos are opaque; premultiplying is a no-op for them, but keeps PNGs
    // with transparency right.
    for pixel in canvas.pixels_mut() {
        let alpha = pixel.0[3] as u32;
        for channel in &mut pixel.0[..3] {
            *channel = ((*channel as u32 * alpha + 127) / 255) as u8;
        }
    }
    canvas
}

impl crate::state::Tileception {
    /// The wallpaper scaled for the current output's pixels, loaded on first
    /// use and again when the theme or the output size changes.
    pub fn wallpaper_buffer(&mut self) -> Option<MemoryRenderBuffer> {
        let path = self.theme.wallpaper.clone()?;
        let (width, height) = self.output_pixels();
        let size = (width.max(0) as u32, height.max(0) as u32);
        let current = self.wallpaper.as_ref().is_some_and(|wallpaper| {
            wallpaper.path == path && wallpaper.fit == self.theme.wallpaper_fit && wallpaper.size == size
        });
        if !current {
            if self.wallpaper_failed.as_ref() == Some(&path) || size.0 == 0 || size.1 == 0 {
                return None;
            }
            match load(&path, self.theme.wallpaper_fit, size.0, size.1, self.theme.background.0) {
                Ok(wallpaper) => {
                    tracing::info!("wallpaper {} at {}x{}", path.display(), size.0, size.1);
                    self.wallpaper = Some(wallpaper);
                }
                Err(err) => {
                    tracing::warn!("wallpaper: {err}");
                    self.wallpaper_failed = Some(path);
                    return None;
                }
            }
        }
        self.wallpaper.as_ref().map(|wallpaper| wallpaper.buffer.clone())
    }

    /// A lock command line with `{wallpaper}` filled in, quoted.
    pub fn expand_wallpaper(&self, line: &str) -> String {
        let wallpaper = self.theme.wallpaper.as_ref().map(|path| path.display().to_string());
        line.replace("{wallpaper}", &shell_quote(wallpaper.as_deref().unwrap_or("")))
    }

    /// The shell command that locks the screen, showing the wallpaper.
    pub fn lock_command(&self) -> String {
        let wallpaper = self.theme.wallpaper.as_ref().map(|path| path.display().to_string());
        if let Some(custom) = &self.config.file.general.lock_command {
            return self.expand_wallpaper(custom);
        }
        let [red, green, blue, _] = self.theme.background.straight_rgba8();
        let [accent_red, accent_green, accent_blue, _] = self.theme.accent.straight_rgba8();
        let mut cmd = format!(
            "swaylock -f -c {red:02x}{green:02x}{blue:02x} --indicator-idle-visible --show-failed-attempts \
             --ring-color {accent_red:02x}{accent_green:02x}{accent_blue:02x} --key-hl-color {accent_red:02x}{accent_green:02x}{accent_blue:02x}"
        );
        if let Some(wallpaper) = wallpaper {
            cmd += &format!(" -i {} -s {}", shell_quote(&wallpaper), self.theme.wallpaper_fit.swaylock());
        }
        cmd
    }
}

/// Quotes a string for `sh`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_keep_the_requested_size() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(400, 100, image::Rgba([255, 0, 0, 255])));
        for fit in [Scaling::Fill, Scaling::Fit, Scaling::Stretch, Scaling::Center] {
            let canvas = compose(&img, fit, 200, 200, [0.0, 0.0, 0.0, 1.0]);
            assert_eq!(canvas.dimensions(), (200, 200), "{fit:?}");
        }
        // Fit leaves bars above and below a wide image.
        let canvas = compose(&img, Scaling::Fit, 200, 200, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(canvas.get_pixel(100, 10).0, [0, 0, 255, 255]);
        assert_eq!(canvas.get_pixel(100, 100).0, [255, 0, 0, 255]);
        assert_eq!(expand_home("/a/b"), PathBuf::from("/a/b"));
        assert!(Scaling::parse("zoom").is_err());
    }
}
