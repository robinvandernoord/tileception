//! A person's look on the login screen, their "card". The greeter runs before
//! anyone logs in and cannot read home folders, so each Tileception session
//! publishes it:
//!
//! - the accent pair and colour scheme in AccountsService, as properties of
//!   the `org.tileception.Greeter` extension, which people may set for
//!   themselves without a password;
//! - a downscaled copy of the wallpaper in a drop folder (mode 1733: anyone may
//!   add a file, nobody may list or replace another's), as `<uid>.jpg`. The
//!   greeter uses a file only when it is a plain file owned by that uid.
//!
//! `[greeter] share = false` in the session's config takes both back.

use std::{
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::theme::{Colour, Theme};

const INTERFACE: &str = "org.tileception.Greeter";
const ACCOUNTS: &str = "org.freedesktop.Accounts";
/// The largest wallpaper copy the greeter reads.
const MAX_BYTES: u64 = 16 << 20;
/// The largest wallpaper copy a session writes, in pixels.
const MAX_SIZE: (u32, u32) = (3840, 2160);

/// `[greeter]` in the session's config.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShareConfig {
    /// Show this session's accent and wallpaper on the login screen.
    pub share: bool,
}

impl Default for ShareConfig {
    fn default() -> Self {
        ShareConfig { share: true }
    }
}

/// The drop folder, or `TILECEPTION_GREETER_CARDS` for tests.
pub fn folder() -> PathBuf {
    std::env::var_os("TILECEPTION_GREETER_CARDS")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/var/cache/tileception-greeter".into())
}

fn wallpaper_file(folder: &Path, uid: u32) -> PathBuf {
    folder.join(format!("{uid}.jpg"))
}

/// What a session publishes.
#[derive(Debug, Clone)]
pub struct Publish {
    pub uid: u32,
    pub share: bool,
    pub accent: String,
    pub accent_end: String,
    pub colour_scheme: String,
    pub wallpaper: Option<PathBuf>,
    /// The screen in pixels: the copy is no larger.
    pub screen: (u32, u32),
}

fn hex(colour: Colour) -> String {
    let [r, g, b, _] = colour.straight_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

impl Publish {
    pub fn of(theme: &Theme, share: bool, screen: (u32, u32)) -> Publish {
        let colour_scheme = match theme.colour_scheme {
            crate::theme::ColourScheme::Dark => "dark",
            crate::theme::ColourScheme::Light => "light",
            crate::theme::ColourScheme::Default => "default",
        };
        Publish {
            uid: current_uid(),
            share,
            accent: hex(theme.accent),
            accent_end: hex(theme.accent_end),
            colour_scheme: colour_scheme.into(),
            wallpaper: theme.wallpaper.clone(),
            screen,
        }
    }

    /// Publishes (or with `share` off, withdraws) the card. Blocking: run it
    /// off the event loop.
    pub fn run(&self, folder: &Path) {
        let (accent, accent_end, scheme) = if self.share {
            (self.accent.as_str(), self.accent_end.as_str(), self.colour_scheme.as_str())
        } else {
            ("", "", "")
        };
        if let Err(error) = set_colours(self.uid, accent, accent_end, scheme) {
            tracing::info!("greeter card: colours not shared ({error}); is org.tileception.Greeter installed?");
        }
        let file = wallpaper_file(folder, self.uid);
        let result = match (&self.wallpaper, self.share) {
            (Some(source), true) => write_wallpaper(source, &file, self.screen),
            _ => match std::fs::remove_file(&file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            },
        };
        if let Err(error) = result {
            tracing::info!("greeter card: wallpaper not shared: {error}");
        }
    }
}

#[allow(unsafe_code, reason = "the real user id comes from libc's getuid")]
fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// AccountsService's object for a user, looked up by `method` with `key`.
fn user_path<K: serde::Serialize + zbus::zvariant::Type>(
    bus: &zbus::blocking::Connection,
    method: &str,
    key: K,
) -> zbus::Result<OwnedObjectPath> {
    let reply = bus.call_method(Some(ACCOUNTS), "/org/freedesktop/Accounts", Some(ACCOUNTS), method, &(key,))?;
    reply.body().deserialize::<OwnedObjectPath>()
}

fn set_colours(uid: u32, accent: &str, accent_end: &str, scheme: &str) -> zbus::Result<()> {
    let bus = zbus::blocking::Connection::system()?;
    let path = user_path(&bus, "FindUserById", i64::from(uid))?;
    for (name, value) in [("Accent", accent), ("AccentEnd", accent_end), ("ColourScheme", scheme)] {
        bus.call_method(
            Some(ACCOUNTS),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Set",
            &(INTERFACE, name, Value::from(value)),
        )?;
    }
    Ok(())
}

/// The wallpaper, no larger than the screen, as a JPEG at `file`. Written
/// next to it first, so the greeter never reads half a file.
fn write_wallpaper(source: &Path, file: &Path, screen: (u32, u32)) -> Result<(), String> {
    let image = image::open(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let (w, h) = (screen.0.clamp(1, MAX_SIZE.0), screen.1.clamp(1, MAX_SIZE.1));
    let image = if image.width() > w || image.height() > h {
        image.resize_to_fill(w, h, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&image.to_rgb8())
        .map_err(|e| e.to_string())?;
    let temporary = file.with_extension(format!("jpg.{}.new", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)
        .and_then(|mut f| f.write_all(&jpeg))
        .and_then(|()| std::fs::rename(&temporary, file));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("{}: {error}", file.display()));
    }
    Ok(())
}

/// What the greeter knows of someone's look.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Card {
    pub accent: Option<(Colour, Colour)>,
    /// A copy of their wallpaper that is safe to read.
    pub wallpaper: Option<PathBuf>,
}

/// Someone's card, from AccountsService and the drop folder. Blocking.
pub fn read(login: &str, uid: u32, folder: &Path) -> Card {
    let accent = read_colours(login).unwrap_or_else(|error| {
        tracing::debug!("greeter card for {login}: {error}");
        None
    });
    Card { accent, wallpaper: trusted_wallpaper(folder, uid) }
}

fn read_colours(login: &str) -> zbus::Result<Option<(Colour, Colour)>> {
    let bus = zbus::blocking::Connection::system()?;
    let path = user_path(&bus, "FindUserByName", login)?;
    let get = |name: &str| -> zbus::Result<String> {
        let reply = bus.call_method(
            Some(ACCOUNTS),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(INTERFACE, name),
        )?;
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize()?;
        Ok(String::try_from(value).unwrap_or_default())
    };
    Ok(parse_accent(&get("Accent")?, &get("AccentEnd")?))
}

/// Both colours, or neither: half a pair would clash with the fallback.
fn parse_accent(accent: &str, accent_end: &str) -> Option<(Colour, Colour)> {
    let colour = |s: &str| Colour::parse(s.trim()).ok().filter(|c| c.0[3] > 0.99);
    Some((colour(accent)?, colour(accent_end)?))
}

/// `<uid>.jpg` when it is a plain file of reasonable size owned by `uid`.
/// Anyone may drop files in the folder, so the owner is what counts.
fn trusted_wallpaper(folder: &Path, uid: u32) -> Option<PathBuf> {
    let file = wallpaper_file(folder, uid);
    let meta = std::fs::symlink_metadata(&file).ok()?;
    (meta.file_type().is_file() && meta.uid() == uid && meta.len() > 0 && meta.len() <= MAX_BYTES).then_some(file)
}

/// A wallpaper filling `w` x `h` pixels, premultiplied RGBA.
pub fn decode(file: &Path, w: u32, h: u32) -> Option<Vec<u8>> {
    let reader = image::ImageReader::open(file).ok()?.with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    let mut reader = reader;
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let filled = image.resize_to_fill(w.max(1), h.max(1), image::imageops::FilterType::Triangle).to_rgba8();
    // Photos are opaque, so premultiplying changes nothing.
    Some(filled.into_raw())
}

/// An accent pair from a wallpaper (RGBA bytes), for someone who shared a
/// wallpaper but no colours: its most common vivid hue and the next one at
/// least 60 degrees round, brightened until dark text reads on them. None for
/// a grey or dull picture, which then gets the colours made from the login.
pub fn accent_from(rgba: &[u8]) -> Option<(Colour, Colour)> {
    const BINS: usize = 12;
    let mut weight = [0.0_f32; BINS];
    let mut sum = [[0.0_f32; 3]; BINS];
    for pixel in rgba.as_chunks::<4>().0 {
        let rgb = [pixel[0], pixel[1], pixel[2]].map(|c| f32::from(c) / 255.0);
        let (hue, saturation, value) = hsv(rgb);
        if saturation < 0.35 || value < 0.3 {
            continue;
        }
        let bin = ((hue / 360.0 * BINS as f32) as usize).min(BINS - 1);
        let w = saturation * value;
        weight[bin] += w;
        for (total, channel) in sum[bin].iter_mut().zip(rgb) {
            *total += channel * w;
        }
    }
    let pixels = (rgba.len() / 4).max(1) as f32;
    let by_weight = |&a: &usize, &b: &usize| weight[a].total_cmp(&weight[b]);
    let first = (0..BINS).max_by(by_weight).filter(|&b| weight[b] / pixels > 0.02)?;
    // Two bins (60 degrees) or more away on the colour wheel.
    let apart = |b: usize| (b + BINS - first) % BINS >= 2 && (first + BINS - b) % BINS >= 2;
    let second = (0..BINS).filter(|&b| apart(b)).max_by(by_weight).filter(|&b| weight[b] / pixels > 0.01);
    let colour = |bin: usize| readable(sum[bin].map(|c| c / weight[bin]));
    let start = colour(first)?;
    let end = match second {
        Some(bin) => colour(bin)?,
        // One hue only: the same, turned 40 degrees.
        None => {
            let (hue, saturation, value) = hsv(sum[first].map(|c| c / weight[first]));
            readable(rgb_of((hue + 40.0) % 360.0, saturation, value))?
        }
    };
    Some((start, end))
}

/// `rgb` made vivid and light enough for dark text on it (4.5:1 against the
/// theme's near-black), or None if even lightened it is not.
fn readable(rgb: [f32; 3]) -> Option<Colour> {
    let (hue, saturation, value) = hsv(rgb);
    let mut rgb = rgb_of(hue, saturation.clamp(0.45, 0.85), value.max(0.85));
    for _ in 0..5 {
        if (luminance(rgb) + 0.05) / 0.056 >= 4.5 {
            return Some(Colour([rgb[0], rgb[1], rgb[2], 1.0]));
        }
        rgb = rgb.map(|c| c + (1.0 - c) * 0.15);
    }
    None
}

/// WCAG relative luminance of sRGB.
fn luminance(rgb: [f32; 3]) -> f32 {
    let linear = rgb.map(|c| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) });
    0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]
}

/// Hue in degrees, saturation and value, each of the latter 0 to 1.
fn hsv([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    (hue, if max <= f32::EPSILON { 0.0 } else { delta / max }, max)
}

fn rgb_of(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    let c = value * saturation;
    let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r, g, b].map(|channel| channel + value - c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(colours: &[([u8; 3], usize)]) -> Vec<u8> {
        colours.iter().flat_map(|&(rgb, n)| std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], n)).flatten().collect()
    }

    #[test]
    fn accents_come_from_the_wallpapers_strongest_hues() {
        // Mostly sunset orange, some purple, and a lot of dark sky.
        let rgba = picture(&[([230, 120, 40], 500), ([130, 70, 200], 300), ([20, 22, 30], 1200)]);
        let (start, end) = accent_from(&rgba).expect("an accent");
        let (start_hue, _, _) = hsv([start.0[0], start.0[1], start.0[2]]);
        let (end_hue, _, _) = hsv([end.0[0], end.0[1], end.0[2]]);
        assert!((15.0..45.0).contains(&start_hue), "orange first, got {start_hue}");
        assert!((250.0..290.0).contains(&end_hue), "purple second, got {end_hue}");
        for colour in [start, end] {
            assert!((luminance([colour.0[0], colour.0[1], colour.0[2]]) + 0.05) / 0.056 >= 4.5);
        }
    }

    #[test]
    fn grey_wallpapers_give_no_accent() {
        assert_eq!(accent_from(&picture(&[([128, 128, 128], 1000), ([10, 10, 10], 1000)])), None);
    }

    #[test]
    fn one_hue_is_turned_for_the_second_colour() {
        let (start, end) = accent_from(&picture(&[([40, 160, 90], 1000)])).expect("an accent");
        assert_ne!(start, end);
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tileception-cards-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn accents_come_in_pairs() {
        assert!(parse_accent("#2bd4a7", "#5c8ff2").is_some());
        assert_eq!(parse_accent("#2bd4a7", ""), None);
        assert_eq!(parse_accent("", ""), None);
        assert_eq!(parse_accent("green", "#5c8ff2"), None);
        // Transparent accents would vanish against the tiles.
        assert_eq!(parse_accent("#2bd4a700", "#5c8ff2"), None);
    }

    #[test]
    fn wallpapers_round_trip_through_the_folder() {
        let dir = folder("round-trip");
        let source = dir.join("source.png");
        image::RgbImage::from_pixel(400, 300, image::Rgb([200, 100, 50])).save(&source).expect("saves");
        let uid = current_uid();
        let file = wallpaper_file(&dir, uid);
        write_wallpaper(&source, &file, (200, 100)).expect("writes");
        let written = image::open(&file).expect("a JPEG");
        assert_eq!((written.width(), written.height()), (200, 100));
        assert_eq!(trusted_wallpaper(&dir, uid), Some(file.clone()));
        let pixels = decode(&file, 20, 10).expect("decodes");
        assert_eq!(pixels.len(), 20 * 10 * 4);
        // Small wallpapers are not enlarged.
        write_wallpaper(&source, &file, (1920, 1080)).expect("writes");
        assert_eq!(image::open(&file).expect("a JPEG").width(), 400);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_owners_file_counts() {
        let dir = folder("owner");
        let uid = current_uid();
        std::fs::write(wallpaper_file(&dir, uid + 1), b"not mine").expect("writes");
        // A file named for someone else, but owned by us, is not theirs.
        assert_eq!(trusted_wallpaper(&dir, uid + 1), None);
        // Nor is a link, even to a file we own.
        std::fs::write(dir.join("real.jpg"), b"x").expect("writes");
        std::os::unix::fs::symlink(dir.join("real.jpg"), wallpaper_file(&dir, uid)).expect("links");
        assert_eq!(trusted_wallpaper(&dir, uid), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn share_defaults_on() {
        assert!(ShareConfig::default().share);
        let config: ShareConfig = toml::from_str("share = false").expect("parses");
        assert!(!config.share);
    }
}
