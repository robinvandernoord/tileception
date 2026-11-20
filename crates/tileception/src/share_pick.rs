//! Choosing what to share: xdg-desktop-portal-wlr runs `tileception
//! pick-source` as its chooser (`chooser_type=dmenu`), which hands the
//! choices to the compositor and prints the one picked in the launcher.
//!
//! The portal writes one choice per line and reads back the line chosen,
//! unchanged. From 0.8 on the lines are `Monitor: <name> <description>` and
//! `Window: <title> (<identifier>)`; 0.7 writes bare screen names. Nothing
//! printed means the choice was cancelled.
//!
//! A program that comes back with restore data from an earlier share (see
//! [`crate::screen_cast`]) gets the same screen or window without the
//! launcher, when it is still there.

use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tileception_core::WindowId;

use crate::Tileception;

/// How long the portal waits for a choice before giving up.
const CHOOSE_WAIT: Duration = Duration::from_secs(120);

/// How long restore data waits for the chooser it is for: the portal runs
/// the chooser while the program's request is passed on to it.
const RESTORE_WAIT: Duration = Duration::from_secs(10);

/// One thing the portal offers to share.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// The portal's line, given back when this is chosen.
    pub line: String,
    pub what: Shared,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shared {
    /// A screen, by output name, with the portal's description of it.
    Screen { name: String, description: String },
    /// A window, by the identifier the window list gave it, when it is still
    /// open here, with its title as the portal listed it.
    Window { identifier: String, window: Option<WindowId>, title: String },
}

/// A screen or window as restore data remembers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// By output name.
    Screen(String),
    /// By its identifier in the window list.
    Window(String),
}

impl Choice {
    /// What restore data remembers this choice by.
    pub fn target(&self) -> Target {
        match &self.what {
            Shared::Screen { name, .. } => Target::Screen(name.clone()),
            Shared::Window { identifier, .. } => Target::Window(identifier.clone()),
        }
    }
}

/// A choice the launcher waits on, answering the deferred `pick-source`
/// command with this request number.
pub struct Pick {
    pub request: u64,
    pub choices: Vec<Choice>,
}

/// What the screen cast portal and the chooser tell each other.
#[derive(Default)]
pub struct Sharing {
    /// What the next chooser is to give without asking, from restore data.
    pub restore: Option<(Target, Instant)>,
    /// What the last chooser gave, for the portal to remember.
    pub chosen: Arc<Mutex<Option<Target>>>,
}

impl Sharing {
    fn record(&self, target: Option<Target>) {
        *self.chosen.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = target;
    }
}

/// `tileception pick-source`: the portal's chooser. Reads the choices from
/// standard input and prints the one picked.
pub fn run() -> Result<(), String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).map_err(|err| format!("cannot read the choices: {err}"))?;
    let reply = crate::ipc::request(&["pick-source".into(), encode(&input)])
        .map_err(|err| format!("cannot reach Tileception: {err}"))?;
    if let Some(error) = reply.strip_prefix("error: ") {
        return Err(error.trim().to_string());
    }
    if !reply.trim().is_empty() {
        println!("{}", reply.trim_end_matches('\n'));
    }
    Ok(())
}

/// The choices as one word of a command: commands are a line split at
/// whitespace, so whitespace, `%` and anything not ASCII go as `%XX`.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'%' => "%25".to_string(),
            _ if byte.is_ascii_graphic() => char::from(byte).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// What [`encode`] made, back as the original text.
pub fn decode(word: &str) -> Result<String, String> {
    let bytes = word.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = word.get(index + 1..index + 3).ok_or("a choice ends early")?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| "a choice is garbled")?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "a choice is not text".into())
}

/// Reads one of the portal's lines; `window` finds an open window by the
/// identifier the window list gave it.
pub fn parse(line: &str, window: impl Fn(&str) -> Option<WindowId>) -> Choice {
    let what = if let Some(rest) = line.strip_prefix("Window: ") {
        // The identifier is the last parenthesised part; titles may hold
        // parentheses of their own.
        let (title, identifier) =
            rest.strip_suffix(')').and_then(|rest| rest.rsplit_once(" (")).unwrap_or((rest, rest));
        Shared::Window { identifier: identifier.to_string(), window: window(identifier), title: title.to_string() }
    } else {
        let rest = line.strip_prefix("Monitor: ").unwrap_or(line);
        let (name, description) = rest.split_once(' ').unwrap_or((rest, ""));
        Shared::Screen { name: name.to_string(), description: description.trim().to_string() }
    };
    Choice { line: line.to_string(), what }
}

impl Tileception {
    /// `pick-source`: shows the portal's choices in the launcher and answers
    /// once one is picked, or with nothing when the launcher is closed.
    pub(crate) fn pick_source(&mut self, encoded: &str) -> Result<String, String> {
        let text = decode(encoded)?;
        let choices: Vec<Choice> = text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .map(|line| parse(line, |identifier| self.window_with_identifier(identifier)))
            .collect();
        if choices.is_empty() {
            return Err("there is nothing to share".into());
        }
        if self.is_locked() {
            return Err("the screen is locked".into());
        }
        // Restore data from the program's earlier share: the same screen or
        // window again, when it is still there.
        if let Some((target, asked)) = self.sharing.restore.take()
            && asked.elapsed() < RESTORE_WAIT
            && let Some(choice) = choices.iter().find(|choice| choice.target() == target)
        {
            tracing::debug!("share: {:?} again, from restore data", choice.line);
            self.sharing.record(Some(target));
            return Ok(format!("{}\n", choice.line));
        }
        tracing::debug!("share: asking which of {} to share", choices.len());
        let request = self.ipc.next_request();
        self.defer_reply_for(request, CHOOSE_WAIT, "nothing was chosen in time");
        self.open_source_pick(Pick { request, choices });
        Ok(String::new())
    }

    /// Answers the portal with what was picked, or that nothing was.
    pub(crate) fn source_picked(&mut self, pick: Pick, chosen: Option<usize>) {
        let choice = chosen.and_then(|index| pick.choices.get(index));
        match choice {
            Some(choice) => tracing::debug!("share: {:?} chosen", choice.line),
            None => tracing::debug!("share: nothing chosen"),
        }
        self.sharing.record(choice.map(Choice::target));
        let line = choice.map(|choice| format!("{}\n", choice.line));
        self.reply_later(pick.request, Ok(line.unwrap_or_default()));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn choices_travel_as_one_word() {
        let text = "Monitor: HDMI-A-1 Dell Inc. U2720Q\nWindow: 100% done — café (a1b2)\n";
        let word = encode(text);
        assert!(!word.contains([' ', '\n']));
        assert_eq!(decode(&word).unwrap(), text);
        assert!(decode("%4").is_err());
    }

    #[test]
    fn portal_lines_name_screens_and_windows() {
        let found = |identifier: &str| (identifier == "a1b2").then_some(WindowId(7));
        assert_eq!(
            parse("Monitor: HDMI-A-1 Dell Inc. U2720Q", found).what,
            Shared::Screen { name: "HDMI-A-1".into(), description: "Dell Inc. U2720Q".into() }
        );
        // xdg-desktop-portal-wlr 0.7 gives only the name.
        assert_eq!(parse("eDP-1", found).target(), Target::Screen("eDP-1".into()));
        let window = parse("Window: notes (draft) (a1b2)", found);
        assert_eq!(
            window.what,
            Shared::Window { identifier: "a1b2".into(), window: Some(WindowId(7)), title: "notes (draft)".into() }
        );
        assert_eq!(window.target(), Target::Window("a1b2".into()));
        assert_eq!(
            parse("Window: gone (zz)", found).what,
            Shared::Window { identifier: "zz".into(), window: None, title: "gone".into() }
        );
    }
}
