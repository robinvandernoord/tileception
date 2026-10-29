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
        let h = hex.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("zz"), 16);
        let (r, g, b, a) = match h.len() {
            6 => (byte(0), byte(2), byte(4), Ok(255)),
            8 => (byte(0), byte(2), byte(4), byte(6)),
            _ => return Err(format!("colour {hex:?} is not #rrggbb or #rrggbbaa")),
        };
        let (Ok(r), Ok(g), Ok(b), Ok(a)) = (r, g, b, a) else {
            return Err(format!("colour {hex:?} is not hexadecimal"));
        };
        let a = a as f32 / 255.0;
        Ok(Colour([r as f32 / 255.0 * a, g as f32 / 255.0 * a, b as f32 / 255.0 * a, a]))
    }

    /// The same colour at a fraction of its opacity.
    pub fn alpha(self, factor: f32) -> Colour {
        Colour(self.0.map(|c| c * factor))
    }

    /// Partway from this colour to `other`; 0 is this one, 1 the other.
    pub fn mix(self, other: Colour, t: f32) -> Colour {
        let t = t.clamp(0.0, 1.0);
        Colour(std::array::from_fn(|i| self.0[i] + (other.0[i] - self.0[i]) * t))
    }

    /// Straight (not premultiplied) RGBA bytes, for rasterising text.
    pub fn straight_rgba8(self) -> [u8; 4] {
        let [r, g, b, a] = self.0;
        let un = |c: f32| if a > 0.0 { (c / a * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
        [un(r), un(g), un(b), (a * 255.0).round() as u8]
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
    pub fn parse(s: &str) -> Result<ColourScheme, String> {
        match s {
            "dark" => Ok(ColourScheme::Dark),
            "light" => Ok(ColourScheme::Light),
            "default" => Ok(ColourScheme::Default),
            other => Err(format!("colour_scheme {other:?} is not dark, light or default")),
        }
    }
}

impl Theme {
    pub fn from_file(f: &ThemeFile) -> Result<Theme, String> {
        Ok(Theme {
            background: Colour::parse(&f.background)?,
            surface: Colour::parse(&f.surface)?,
            surface_raised: Colour::parse(&f.surface_raised)?,
            border: Colour::parse(&f.border)?,
            text: Colour::parse(&f.text)?,
            muted: Colour::parse(&f.muted)?,
            accent: Colour::parse(&f.accent)?,
            accent_end: Colour::parse(&f.accent_end)?,
            on_accent: Colour::parse(&f.on_accent)?,
            danger: Colour::parse(&f.danger)?,
            warning: Colour::parse(&f.warning)?,
            radius: f.radius.max(0.0),
            popup_radius: f.popup_radius.max(0.0),
            glow: f.glow.max(0.0),
            font: f.font.clone(),
            font_size: f.font_size.clamp(6.0, 72.0),
            wallpaper: f.wallpaper.as_deref().filter(|w| !w.is_empty()).map(crate::wallpaper::expand_home),
            wallpaper_fit: crate::wallpaper::Scaling::parse(&f.wallpaper_fit)?,
            colour_scheme: ColourScheme::parse(&f.colour_scheme)?,
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
