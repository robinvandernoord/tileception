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

#[cfg(test)]
mod tests {
    use super::*;

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
