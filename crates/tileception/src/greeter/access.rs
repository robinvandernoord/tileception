//! Accessibility on the login screen: larger text and high contrast, switched
//! from the accessibility panel or the launcher and remembered for the next boot.

use crate::{
    theme::{Colour, Theme},
    ui::glyph,
};

/// How much larger the text is with larger text on.
const LARGER: f32 = 1.3;

/// One switch of the accessibility panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aid {
    LargerText,
    HighContrast,
}

impl Aid {
    /// In the panel's order.
    pub const ALL: [Aid; 2] = [Aid::LargerText, Aid::HighContrast];

    pub fn label(self) -> &'static str {
        match self {
            Aid::LargerText => "Larger text",
            Aid::HighContrast => "High contrast",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Aid::LargerText => glyph::ZOOM_IN,
            Aid::HighContrast => glyph::CONTRAST,
        }
    }

    /// The launcher's row: what choosing it does.
    pub fn action(self, enabled: bool) -> String {
        let label = self.label().to_lowercase();
        if enabled { format!("Turn off {label}") } else { format!("Turn on {label}") }
    }

    /// More words that find it in the launcher.
    pub fn aliases(self) -> &'static str {
        match self {
            Aid::LargerText => "accessibility bigger font size zoom",
            Aid::HighContrast => "accessibility colours colors",
        }
    }
}

/// `base` with the switches applied: the text scaled up, and for high
/// contrast white on black with a yellow accent, no frost and no glow.
pub fn apply(base: &Theme, larger_text: bool, high_contrast: bool) -> Theme {
    let mut theme = base.clone();
    if larger_text {
        theme.font_size = (base.font_size * LARGER).clamp(6.0, 72.0);
    }
    if high_contrast {
        let black = Colour([0.0, 0.0, 0.0, 1.0]);
        let white = Colour([1.0; 4]);
        let yellow = Colour([1.0, 0.84, 0.0, 1.0]);
        theme.background = black;
        theme.surface = black;
        theme.surface_raised = Colour([0.16, 0.16, 0.16, 1.0]);
        theme.border = white;
        theme.text = white;
        theme.muted = Colour([0.88, 0.88, 0.88, 1.0]);
        theme.accent = yellow;
        theme.accent_end = yellow;
        theme.on_accent = black;
        theme.danger = Colour([1.0, 0.45, 0.45, 1.0]);
        theme.warning = Colour([1.0, 0.62, 0.1, 1.0]);
        theme.glow = 0.0;
        theme.frost = false;
    }
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_change_only_what_they_say() {
        let base = Theme::default();
        let plain = apply(&base, false, false);
        assert_eq!((plain.font_size, plain.text, plain.accent), (base.font_size, base.text, base.accent));
        let larger = apply(&base, true, false);
        assert!(larger.font_size > base.font_size);
        assert_eq!(larger.background, base.background);
        let contrast = apply(&base, false, true);
        assert_eq!(contrast.font_size, base.font_size);
        assert_eq!(contrast.text, Colour([1.0; 4]));
        assert_eq!(contrast.background, Colour([0.0, 0.0, 0.0, 1.0]));
        assert!(!contrast.frost);
    }

    #[test]
    fn launcher_rows_say_what_they_do() {
        assert_eq!(Aid::LargerText.action(false), "Turn on larger text");
        assert_eq!(Aid::HighContrast.action(true), "Turn off high contrast");
    }
}
