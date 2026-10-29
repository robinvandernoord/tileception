//! Keyboard layouts to pick from on the greeter: every layout and variant
//! xkb describes in its rules list, by the names people know them by.

use std::path::Path;

/// One layout, or one variant of a layout, as xkb knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    pub layout: String,
    /// Empty for the layout itself.
    pub variant: String,
    /// Such as "Dutch" or "English (US, intl., with AltGr dead keys)".
    pub description: String,
}

/// Every layout and variant, from xkb's evdev rules list (else the base
/// one). Empty when xkb's data is not installed.
pub fn catalogue() -> Vec<Known> {
    ["/usr/share/X11/xkb/rules/evdev.lst", "/usr/share/X11/xkb/rules/base.lst"]
        .iter()
        .find_map(|path| std::fs::read_to_string(Path::new(path)).ok())
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// The `! layout` and `! variant` sections of a rules list. Variant lines
/// name their layout before the description: `altgr-intl  us: English (...)`.
fn parse(text: &str) -> Vec<Known> {
    let mut section = "";
    let mut known = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix('!') {
            section = name.trim();
            continue;
        }
        let Some((name, rest)) = line.trim().split_once(char::is_whitespace) else { continue };
        let rest = rest.trim();
        match section {
            "layout" => known.push(Known { layout: name.into(), variant: String::new(), description: rest.into() }),
            "variant" => {
                if let Some((layout, description)) = rest.split_once(": ") {
                    known.push(Known { layout: layout.into(), variant: name.into(), description: description.into() });
                }
            }
            _ => {}
        }
    }
    known
}

/// A short name, as in the bar: "nl", or "us altgr-intl".
pub fn short(layout: &str, variant: &str) -> String {
    if variant.is_empty() { layout.to_string() } else { format!("{layout} {variant}") }
}

/// How a layout is remembered: xkb's own `us(altgr-intl)`, or `nl`.
pub fn remembered(layout: &str, variant: &str) -> String {
    if variant.is_empty() { layout.to_string() } else { format!("{layout}({variant})") }
}

/// A remembered layout back as layout and variant.
pub fn recall(text: &str) -> (String, String) {
    match text.strip_suffix(')').and_then(|t| t.split_once('(')) {
        Some((layout, variant)) => (layout.to_string(), variant.to_string()),
        None => (text.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_layouts_and_variants() {
        let text = "! model\n  pc105           Generic 105-key PC\n\n! layout\n  us              English (US)\n  nl              Dutch\n\n! variant\n  altgr-intl      us: English (US, intl., with AltGr dead keys)\n\n! option\n  grp                  Switching to another layout\n";
        let known = parse(text);
        assert_eq!(known.len(), 3);
        assert_eq!(known[1], Known { layout: "nl".into(), variant: String::new(), description: "Dutch".into() });
        assert_eq!(known[2].layout, "us");
        assert_eq!(known[2].variant, "altgr-intl");
        assert_eq!(known[2].description, "English (US, intl., with AltGr dead keys)");
    }

    #[test]
    fn remembered_layouts_come_back() {
        assert_eq!(recall(&remembered("us", "altgr-intl")), ("us".into(), "altgr-intl".into()));
        assert_eq!(recall(&remembered("nl", "")), ("nl".into(), String::new()));
    }
}
