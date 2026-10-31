//! Tide, the look of Tileception: flat graphite surfaces, small radii and a
//! teal to blue accent with a soft glow. Colours can be overridden in the
//! `[theme]` section of the config.

use serde::{Deserialize, Serialize};

/// A colour as premultiplied RGBA, ready for the renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colour(pub [f32; 4]);

impl Colour {
    pub const TRANSPARENT: Colour = Colour([0.0; 4]);

    /// Parses `#rrggbb` or `#rrggbbaa`.
    pub fn parse(hex: &str) -> Result<Colour, String> {
        let digits = hex.trim().trim_start_matches('#');
        let byte = |index: usize| u8::from_str_radix(digits.get(index..index + 2).unwrap_or("zz"), 16);
        let (red, green, blue, alpha) = match digits.len() {
            6 => (byte(0), byte(2), byte(4), Ok(255)),
            8 => (byte(0), byte(2), byte(4), byte(6)),
            _ => return Err(format!("colour {hex:?} is not #rrggbb or #rrggbbaa")),
        };
        let (Ok(red), Ok(green), Ok(blue), Ok(alpha)) = (red, green, blue, alpha) else {
            return Err(format!("colour {hex:?} is not hexadecimal"));
        };
        let alpha = alpha as f32 / 255.0;
        Ok(Colour([red as f32 / 255.0 * alpha, green as f32 / 255.0 * alpha, blue as f32 / 255.0 * alpha, alpha]))
    }

    /// The same colour at a fraction of its opacity.
    pub fn alpha(self, factor: f32) -> Colour {
        Colour(self.0.map(|channel| channel * factor))
    }

    /// Partway from this colour to `other`; 0 is this one, 1 the other.
    pub fn mix(self, other: Colour, fraction: f32) -> Colour {
        let fraction = fraction.clamp(0.0, 1.0);
        Colour(std::array::from_fn(|index| self.0[index] + (other.0[index] - self.0[index]) * fraction))
    }

    /// Straight (not premultiplied) RGBA bytes, for rasterising text.
    pub fn straight_rgba8(self) -> [u8; 4] {
        let [red, green, blue, alpha] = self.0;
        let unpremultiply =
            |channel: f32| if alpha > 0.0 { (channel / alpha * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
        [unpremultiply(red), unpremultiply(green), unpremultiply(blue), (alpha * 255.0).round() as u8]
    }
}

/// The `[theme]` section as written in the config file.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeFile {
    pub background: String,
    pub surface: String,
    pub surface_raised: String,
    pub border: String,
    pub text: String,
    pub muted: String,
    pub accent: String,
    pub accent_end: String,
    pub on_accent: String,
    /// Errors, such as a wrong password or a critical notification.
    pub danger: String,
    /// Cautions, such as volume boosted past 100%.
    pub warning: String,
    /// The border of windows from another computer (handoff).
    pub guest: String,
    pub radius: f32,
    pub popup_radius: f32,
    /// How far the focus glow reaches, in pixels; 0 turns it off.
    pub glow: f32,
    pub font: String,
    pub font_size: f32,
    /// A background image (PNG, JPEG or WebP); `~/` is your home folder.
    pub wallpaper: Option<String>,
    /// fill, fit, stretch or center.
    pub wallpaper_fit: String,
    /// What applications are told to use: dark, light or default (their own
    /// choice).
    pub colour_scheme: String,
}

impl Default for ThemeFile {
    fn default() -> Self {
        ThemeFile {
            background: "#1c1f24".into(),
            surface: "#24272e".into(),
            surface_raised: "#2e323a".into(),
            border: "#343842".into(),
            text: "#e8eaee".into(),
            muted: "#8b919c".into(),
            accent: "#2bd4a7".into(),
            accent_end: "#5c8ff2".into(),
            on_accent: "#0d1117".into(),
            danger: "#f0646a".into(),
            warning: "#f2b04c".into(),
            guest: "#a78bfa".into(),
            radius: 6.0,
            popup_radius: 10.0,
            glow: 18.0,
            font: "Noto Sans".into(),
            font_size: 13.0,
            wallpaper: None,
            wallpaper_fit: "fill".into(),
            colour_scheme: "dark".into(),
        }
    }
}

/// The parsed theme the renderer uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub background: Colour,
    pub surface: Colour,
    pub surface_raised: Colour,
    pub border: Colour,
    pub text: Colour,
    pub muted: Colour,
    pub accent: Colour,
    pub accent_end: Colour,
    pub on_accent: Colour,
    pub danger: Colour,
    pub warning: Colour,
    pub guest: Colour,
    pub radius: f32,
    pub popup_radius: f32,
    pub glow: f32,
    pub font: String,
    pub font_size: f32,
    pub wallpaper: Option<std::path::PathBuf>,
    pub wallpaper_fit: crate::wallpaper::Scaling,
    pub colour_scheme: ColourScheme,
}

/// Whether applications should use their light or dark styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColourScheme {
    Dark,
    Light,
    /// No preference: applications decide.
    Default,
}

impl ColourScheme {
    pub fn parse(value: &str) -> Result<ColourScheme, String> {
        match value {
            "dark" => Ok(ColourScheme::Dark),
            "light" => Ok(ColourScheme::Light),
            "default" => Ok(ColourScheme::Default),
            other => Err(format!("colour_scheme {other:?} is not dark, light or default")),
        }
    }
}

impl Theme {
    pub fn from_file(file: &ThemeFile) -> Result<Theme, String> {
        Ok(Theme {
            background: Colour::parse(&file.background)?,
            surface: Colour::parse(&file.surface)?,
            surface_raised: Colour::parse(&file.surface_raised)?,
            border: Colour::parse(&file.border)?,
            text: Colour::parse(&file.text)?,
            muted: Colour::parse(&file.muted)?,
            accent: Colour::parse(&file.accent)?,
            accent_end: Colour::parse(&file.accent_end)?,
            on_accent: Colour::parse(&file.on_accent)?,
            danger: Colour::parse(&file.danger)?,
            warning: Colour::parse(&file.warning)?,
            guest: Colour::parse(&file.guest)?,
            radius: file.radius.max(0.0),
            popup_radius: file.popup_radius.max(0.0),
            glow: file.glow.max(0.0),
            font: file.font.clone(),
            font_size: file.font_size.clamp(6.0, 72.0),
            wallpaper: file.wallpaper.as_deref().filter(|path| !path.is_empty()).map(crate::wallpaper::expand_home),
            wallpaper_fit: crate::wallpaper::Scaling::parse(&file.wallpaper_fit)?,
            colour_scheme: ColourScheme::parse(&file.colour_scheme)?,
        })
    }
}

impl Theme {
    /// Popups (launcher, notifications, menus) sit a step above tiles.
    pub fn popup(&self) -> Colour {
        self.surface.mix(self.surface_raised, 0.5)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::from_file(&ThemeFile::default()).expect("built-in theme parses")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn parses_and_premultiplies() {
        assert_eq!(Colour::parse("#ff0000").unwrap(), Colour([1.0, 0.0, 0.0, 1.0]));
        let half = Colour::parse("#ffffff80").unwrap();
        assert!((half.0[0] - half.0[3]).abs() < 1e-6 && (half.0[3] - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(half.straight_rgba8(), [255, 255, 255, 128]);
        assert!(Colour::parse("red").is_err());
        assert!(Colour::parse("#12345").is_err());
        assert_eq!(Theme::default().colour_scheme, ColourScheme::Dark);
        assert!(ColourScheme::parse("dim").is_err());
    }
}
