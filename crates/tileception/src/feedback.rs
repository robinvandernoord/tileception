//! "Send feedback": a pre-filled new-issue page on GitHub, opened in the
//! browser. The issue is filed under the user's own GitHub account; nothing
//! is sent from here.

use std::fmt::Write as _;

use crate::Tileception;

const NEW_ISSUE: &str = "https://github.com/robinvandernoord/tileception/issues/new";

/// The new-issue page with `title` (may be empty) and a body holding the
/// version. The `feedback` label comes from the issue template, which GitHub
/// applies for anyone; labels in the query only stick for people who may
/// label issues.
pub fn issue_url(title: &str) -> String {
    let body = format!(
        "<!-- What happened, or what would you like Tileception to do? -->\n\n\n\n---\nTileception {}\n",
        env!("CARGO_PKG_VERSION")
    );
    format!("{NEW_ISSUE}?template=feedback.md&labels=feedback&title={}&body={}", encode(title), encode(&body))
}

/// Percent-encodes everything but URL-safe characters, for a query value.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

impl Tileception {
    /// Opens the feedback page in the browser.
    pub fn send_feedback(&mut self, title: &str) {
        self.spawn("xdg-open", &[issue_url(title)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(encode("plain-text_1.0~"), "plain-text_1.0~");
    }

    #[test]
    fn fills_in_the_issue() {
        let url = issue_url("Zoom is slow");
        assert!(
            url.starts_with(&format!("{NEW_ISSUE}?template=feedback.md&labels=feedback&title=Zoom%20is%20slow&body="))
        );
        assert!(url.contains(&encode(env!("CARGO_PKG_VERSION"))));
        assert!(!url.contains(' ') && !url.contains('\n'));
    }
}
