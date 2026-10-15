//! The background image, from `wallpaper` in the `[theme]` section. It is
//! decoded and scaled to the output once, then drawn behind everything. The
//! lock screen uses the same image.

use std::path::{Path, PathBuf};

use image::{imageops::FilterType, DynamicImage, GenericImageView, RgbaImage};
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
    pub fn parse(s: &str) -> Result<Scaling, String> {
        match s {
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

/// Loads and scales `path` for a `w` x `h` screen, `background` filling any
/// bars (premultiplied RGBA).
pub fn load(path: &Path, fit: Scaling, w: u32, h: u32, background: [f32; 4]) -> Result<Wallpaper, String> {
    let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let canvas = compose(&img, fit, w, h, background);
    let buffer = MemoryRenderBuffer::from_slice(
        canvas.as_raw(),
        Fourcc::Abgr8888,
        (w as i32, h as i32),
        1,
        Transform::Normal,
        None,
    );
    Ok(Wallpaper { path: path.to_path_buf(), fit, size: (w, h), buffer })
}

fn compose(img: &DynamicImage, fit: Scaling, w: u32, h: u32, background: [f32; 4]) -> RgbaImage {
    let (iw, ih) = img.dimensions();
    let bg = background.map(|c| (c * 255.0).round() as u8);
    let mut canvas = RgbaImage::from_pixel(w, h, image::Rgba(bg));
    let place = |canvas: &mut RgbaImage, part: &RgbaImage| {
        let x = (w as i64 - part.width() as i64) / 2;
        let y = (h as i64 - part.height() as i64) / 2;
        image::imageops::overlay(canvas, part, x, y);
    };
    match fit {
        Scaling::Stretch => canvas = img.resize_exact(w, h, FilterType::Triangle).to_rgba8(),
        Scaling::Fill => canvas = img.resize_to_fill(w, h, FilterType::Triangle).to_rgba8(),
        Scaling::Fit => place(&mut canvas, &img.resize(w, h, FilterType::Triangle).to_rgba8()),
        Scaling::Center => {
            let part =
                img.crop_imm(iw.saturating_sub(w) / 2, ih.saturating_sub(h) / 2, iw.min(w), ih.min(h)).to_rgba8();
            place(&mut canvas, &part);
        }
    }
    // Photos are opaque; premultiplying is a no-op for them, but keeps PNGs
    // with transparency right.
    for p in canvas.pixels_mut() {
        let a = p.0[3] as u32;
        for c in &mut p.0[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    canvas
}

impl crate::state::Tileception {
    /// The wallpaper scaled for the current output, loaded on first use and
    /// again when the theme or the output size changes.
    pub fn wallpaper_buffer(&mut self) -> Option<MemoryRenderBuffer> {
        let path = self.theme.wallpaper.clone()?;
        let out = self.output_rect();
        let size = (out.w as u32, out.h as u32);
        let current = self
            .wallpaper
            .as_ref()
            .is_some_and(|w| w.path == path && w.fit == self.theme.wallpaper_fit && w.size == size);
        if !current {
            if self.wallpaper_failed.as_ref() == Some(&path) || size.0 == 0 || size.1 == 0 {
                return None;
            }
            match load(&path, self.theme.wallpaper_fit, size.0, size.1, self.theme.background.0) {
                Ok(w) => {
                    tracing::info!("wallpaper {} at {}x{}", path.display(), size.0, size.1);
                    self.wallpaper = Some(w);
                }
                Err(err) => {
                    tracing::warn!("wallpaper: {err}");
                    self.wallpaper_failed = Some(path);
                    return None;
                }
            }
        }
        self.wallpaper.as_ref().map(|w| w.buffer.clone())
    }

    /// The shell command that locks the screen, showing the wallpaper.
    pub fn lock_command(&self) -> String {
        let wallpaper = self.theme.wallpaper.as_ref().map(|p| p.display().to_string());
        if let Some(custom) = &self.config.file.general.lock_command {
            return custom.replace("{wallpaper}", &shell_quote(wallpaper.as_deref().unwrap_or("")));
        }
        let [r, g, b, _] = self.theme.background.straight_rgba8();
        let [ar, ag, ab, _] = self.theme.accent.straight_rgba8();
        let mut cmd = format!(
            "swaylock -f -c {r:02x}{g:02x}{b:02x} --indicator-idle-visible --show-failed-attempts \
             --ring-color {ar:02x}{ag:02x}{ab:02x} --key-hl-color {ar:02x}{ag:02x}{ab:02x}"
        );
        if let Some(w) = wallpaper {
            cmd += &format!(" -i {} -s {}", shell_quote(&w), self.theme.wallpaper_fit.swaylock());
        }
        cmd
    }
}

/// Quotes a string for `sh`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_keep_the_requested_size() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(400, 100, image::Rgba([255, 0, 0, 255])));
        for fit in [Scaling::Fill, Scaling::Fit, Scaling::Stretch, Scaling::Center] {
            let c = compose(&img, fit, 200, 200, [0.0, 0.0, 0.0, 1.0]);
            assert_eq!(c.dimensions(), (200, 200), "{fit:?}");
        }
        // Fit leaves bars above and below a wide image.
        let c = compose(&img, Scaling::Fit, 200, 200, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(c.get_pixel(100, 10).0, [0, 0, 255, 255]);
        assert_eq!(c.get_pixel(100, 100).0, [255, 0, 0, 255]);
        assert_eq!(expand_home("/a/b"), PathBuf::from("/a/b"));
        assert!(Scaling::parse("zoom").is_err());
    }
}
