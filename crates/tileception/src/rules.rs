//! Window rules: `[[rules]]` entries that decide how matching windows open.

use serde::{Deserialize, Serialize};

/// One `[[rules]]` entry. Patterns are case-insensitive, with `*` for any
/// run of characters and `?` for one; a rule without patterns matches nothing.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WindowRule {
    /// The Wayland app id or the X11 class.
    pub app_id: Option<String>,
    pub title: Option<String>,
    /// Float above the layout (true) or tile (false), whatever the window asks.
    pub float: Option<bool>,
    /// Width and height for a floating window.
    pub size: Option<[u32; 2]>,
    pub fullscreen: Option<bool>,
    /// Open minimized, in the strip at the bottom edge.
    pub minimized: Option<bool>,
}

/// What the matching rules decide together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Decision {
    pub float: Option<bool>,
    pub size: Option<[u32; 2]>,
    pub fullscreen: bool,
    pub minimized: bool,
}

impl WindowRule {
    pub fn matches(&self, app_id: &str, title: &str) -> bool {
        if self.app_id.is_none() && self.title.is_none() {
            return false;
        }
        self.app_id.as_deref().is_none_or(|pattern| glob(pattern, app_id))
            && self.title.as_deref().is_none_or(|pattern| glob(pattern, title))
    }
}

/// Applies every matching rule in order; later ones override earlier ones.
pub fn decide(rules: &[WindowRule], app_id: &str, title: &str) -> Decision {
    let mut decision = Decision::default();
    for rule in rules.iter().filter(|rule| rule.matches(app_id, title)) {
        decision.float = rule.float.or(decision.float);
        decision.size = rule.size.or(decision.size);
        decision.fullscreen = rule.fullscreen.unwrap_or(decision.fullscreen);
        decision.minimized = rule.minimized.unwrap_or(decision.minimized);
    }
    decision
}

pub fn validate(rules: &[WindowRule]) -> Result<(), String> {
    for (index, rule) in rules.iter().enumerate() {
        if rule.app_id.is_none() && rule.title.is_none() {
            return Err(format!("[[rules]] entry {} needs app_id or title", index + 1));
        }
        if rule.size.is_some_and(|[width, height]| width == 0 || height == 0) {
            return Err(format!("[[rules]] entry {}: size needs a width and height above 0", index + 1));
        }
    }
    Ok(())
}

/// Case-insensitive glob with `*` and `?`.
fn glob(pattern: &str, text: &str) -> bool {
    let pattern_chars: Vec<char> = pattern.to_lowercase().chars().collect();
    let text_chars: Vec<char> = text.to_lowercase().chars().collect();
    // Classic two-pointer match, backtracking to the last star.
    let (mut pattern_index, mut text_index) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while text_index < text_chars.len() {
        if pattern_index < pattern_chars.len()
            && (pattern_chars[pattern_index] == '?' || pattern_chars[pattern_index] == text_chars[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern_chars.len() && pattern_chars[pattern_index] == '*' {
            star = Some((pattern_index, text_index));
            pattern_index += 1;
        } else if let Some((star_pattern_index, star_text_index)) = star {
            pattern_index = star_pattern_index + 1;
            text_index = star_text_index + 1;
            star = Some((star_pattern_index, star_text_index + 1));
        } else {
            return false;
        }
    }
    pattern_chars[pattern_index..].iter().all(|character| *character == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match() {
        assert!(glob("pavucontrol", "PavuControl"));
        assert!(glob("*picture-in-picture*", "Picture-in-Picture"));
        assert!(glob("org.gnome.*", "org.gnome.Calculator"));
        assert!(glob("fire?ox", "firefox"));
        assert!(glob("*", ""));
        assert!(!glob("fire", "firefox"));
        assert!(!glob("*.desktop", "firefox"));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXbYY"));
    }

    #[test]
    fn later_rules_win() {
        let rules = vec![
            WindowRule { app_id: Some("firefox".into()), float: Some(false), ..Default::default() },
            WindowRule {
                app_id: Some("firefox".into()),
                title: Some("Picture-in-Picture".into()),
                float: Some(true),
                size: Some([480, 270]),
                ..Default::default()
            },
            WindowRule { title: Some("*secret*".into()), minimized: Some(true), ..Default::default() },
        ];
        assert_eq!(decide(&rules, "firefox", "Mozilla Firefox").float, Some(false));
        let pip = decide(&rules, "firefox", "Picture-in-Picture");
        assert_eq!((pip.float, pip.size), (Some(true), Some([480, 270])));
        assert!(decide(&rules, "kitty", "my secret notes").minimized);
        assert_eq!(decide(&rules, "kitty", "bash"), Decision::default());
        assert!(validate(&rules).is_ok());
        assert!(validate(&[WindowRule::default()]).is_err());
    }
}
