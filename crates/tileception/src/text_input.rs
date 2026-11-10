//! Text fields: what every field Tileception draws itself has in common.
//!
//! [`TextInput`] holds a field's text, caret and selection, and
//! [`SecretField`] does so for a secret in a buffer that is wiped. Both take
//! their keys through one handler, `Buffer::edit`, so a key means the same in
//! every field. A field passes its keys to `key` and acts on what comes back
//! in [`Key`]; it handles only what is its own (Enter, Escape, Tab, the keys
//! of a list attached to it) and never matches an editing key itself. The
//! methods that move the caret and delete are private to this module to keep
//! it that way.

mod secret;

use std::ops::Range;

use smithay::input::keyboard::{keysyms, xkb};

pub use self::secret::SecretField;
use crate::config::Mods;

/// What a key did in a field.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Key {
    /// The key was the field's. Otherwise it is the caller's to use.
    pub used: bool,
    /// The text changed.
    pub changed: bool,
    /// What the clipboard is to do, which takes the compositor's state.
    pub clipboard: Option<Clipboard>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Clipboard {
    /// Text copied or cut from the field, for the clipboard.
    Copy(String),
    /// The field asks for the clipboard's text, to be given to its `paste`.
    Paste,
}

impl Key {
    const IGNORED: Key = Key { used: false, changed: false, clipboard: None };
    const MOVED: Key = Key { used: true, changed: false, clipboard: None };

    fn edited(changed: bool) -> Key {
        Key { used: true, changed, clipboard: None }
    }
}

/// How a field differs from a plain single line.
#[derive(Clone, Copy, Default)]
struct Rules {
    /// Enter starts a new line, and Home and End stay within the line.
    multiline: bool,
    /// The text is not shown: it is not copied or cut.
    concealed: bool,
    /// Only selecting all and copying work.
    read_only: bool,
    /// No caret is drawn, so there is none to move: keys add and delete at
    /// the end, and deleting a word takes everything.
    appending: bool,
}

/// The text of a field with its caret, edited by keys in `edit`.
trait Buffer {
    fn content(&self) -> &str;
    /// The caret and where a selection starts, as byte offsets.
    fn position(&self) -> (usize, Option<usize>);
    fn set_position(&mut self, cursor: usize, anchor: Option<usize>);
    /// Replaces `range` by `text`, leaving the caret behind it and nothing
    /// selected. False when there is no room, and nothing changed.
    fn splice(&mut self, range: Range<usize>, text: &str) -> bool;
    fn rules(&self) -> Rules;

    fn selected(&self) -> Range<usize> {
        let (cursor, anchor) = self.position();
        let anchor = anchor.unwrap_or(cursor);
        anchor.min(cursor)..anchor.max(cursor)
    }

    fn place(&mut self, position: usize, extend: bool) {
        let (cursor, anchor) = self.position();
        let anchor = if extend { anchor.or(Some(cursor)) } else { None };
        self.set_position(position.min(self.content().len()), anchor);
    }

    /// Where the caret goes one character or word back.
    fn previous(&self, word: bool) -> usize {
        let before = &self.content()[..self.position().0];
        if word {
            let end = before.trim_end_matches(|character: char| !character.is_alphanumeric()).len();
            before[..end]
                .char_indices()
                .rev()
                .find(|(_, character)| !character.is_alphanumeric())
                .map_or(0, |(index, character)| index + character.len_utf8())
        } else {
            before.char_indices().next_back().map_or(0, |(index, _)| index)
        }
    }

    /// Where the caret goes one character or word on.
    fn next(&self, word: bool) -> usize {
        let cursor = self.position().0;
        let after = &self.content()[cursor..];
        if word {
            let start = after.len() - after.trim_start_matches(|character: char| !character.is_alphanumeric()).len();
            cursor
                + after[start..]
                    .find(|character: char| !character.is_alphanumeric())
                    .map_or(after.len(), |index| start + index)
        } else {
            cursor + after.chars().next().map_or(0, char::len_utf8)
        }
    }

    fn step(&mut self, right: bool, word: bool, extend: bool) {
        let range = self.selected();
        let position = if !extend && !range.is_empty() {
            if right { range.end } else { range.start }
        } else if right {
            self.next(word)
        } else {
            self.previous(word)
        };
        self.place(position, extend);
    }

    /// Deletes the selection, else the character or word before (or with
    /// `forward`, after) the caret. Returns whether anything went.
    fn remove(&mut self, forward: bool, word: bool) -> bool {
        let mut range = self.selected();
        if range.is_empty() {
            let cursor = self.position().0;
            range = if forward { cursor..self.next(word) } else { self.previous(word)..cursor };
        }
        !range.is_empty() && self.splice(range, "")
    }

    /// Replaces the selection by `text`. Returns whether the text changed.
    fn put(&mut self, text: &str) -> bool {
        let range = self.selected();
        (!range.is_empty() || !text.is_empty()) && self.splice(range, text)
    }

    /// The one place a key is given its meaning in a field.
    fn edit(&mut self, compose: &mut Option<xkb::compose::State>, sym: u32, mods: Mods) -> Key {
        let rules = self.rules();
        let word = mods.ctrl || mods.alt;
        let end = self.content().len();
        let ctrl = |lower: u32, upper: u32| mods.ctrl && (sym == lower || sym == upper);
        if ctrl(keysyms::KEY_a, keysyms::KEY_A) {
            self.set_position(end, (end > 0).then_some(0));
            return Key::MOVED;
        }
        let cut = ctrl(keysyms::KEY_x, keysyms::KEY_X);
        if cut || ctrl(keysyms::KEY_c, keysyms::KEY_C) {
            let range = self.selected();
            if rules.concealed || range.is_empty() || (cut && rules.read_only) {
                return Key::IGNORED;
            }
            let text = self.content()[range.clone()].to_owned();
            let changed = cut && self.splice(range, "");
            return Key { used: true, changed, clipboard: Some(Clipboard::Copy(text)) };
        }
        if rules.read_only {
            return Key::IGNORED;
        }
        if ctrl(keysyms::KEY_v, keysyms::KEY_V) {
            return Key { used: true, changed: false, clipboard: Some(Clipboard::Paste) };
        }
        if ctrl(keysyms::KEY_u, keysyms::KEY_U) {
            if let Some(compose) = compose {
                compose.reset();
            }
            return Key::edited(end > 0 && self.splice(0..end, ""));
        }
        let typing = !mods.ctrl && !mods.alt && !mods.logo;
        match sym {
            keysyms::KEY_Left | keysyms::KEY_Right if !rules.appending => {
                self.step(sym == keysyms::KEY_Right, word, mods.shift);
                Key::MOVED
            }
            keysyms::KEY_Home | keysyms::KEY_End if !rules.appending => {
                let cursor = self.position().0;
                let text = self.content();
                let position = match (sym == keysyms::KEY_Home, rules.multiline && !mods.ctrl) {
                    (true, false) => 0,
                    (false, false) => end,
                    (true, true) => text[..cursor].rfind('\n').map_or(0, |offset| offset + 1),
                    (false, true) => cursor + text[cursor..].find('\n').unwrap_or(end - cursor),
                };
                self.place(position, mods.shift);
                Key::MOVED
            }
            keysyms::KEY_BackSpace | keysyms::KEY_Delete => {
                // Backspace first takes back a half-typed compose sequence.
                if sym == keysyms::KEY_BackSpace
                    && let Some(compose) = compose
                    && compose.status() == xkb::compose::Status::Composing
                {
                    compose.reset();
                    return Key::MOVED;
                }
                Key::edited(if !rules.appending {
                    self.remove(sym == keysyms::KEY_Delete, word)
                } else if word {
                    end > 0 && self.splice(0..end, "")
                } else {
                    self.remove(false, false)
                })
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if rules.multiline && typing => Key::edited(self.put("\n")),
            _ if typing => match crate::ui::compose_text(compose, sym) {
                Some(text) => Key::edited(self.put(&text)),
                None if compose.as_ref().is_some_and(|compose| compose.status() == xkb::compose::Status::Composing) => {
                    Key::MOVED
                }
                None => Key::IGNORED,
            },
            _ => Key::IGNORED,
        }
    }
}

/// A field's text, with a UTF-8 caret and an optional selection anchor.
pub struct TextInput {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    /// The text is drawn masked, as a password is until it is revealed:
    /// copying and cutting are off meanwhile.
    pub hidden: bool,
    multiline: bool,
    read_only: bool,
    limit: usize,
}

impl Default for TextInput {
    fn default() -> Self {
        TextInput {
            text: String::new(),
            cursor: 0,
            anchor: None,
            hidden: false,
            multiline: false,
            read_only: false,
            limit: 4096,
        }
    }
}

impl Buffer for TextInput {
    fn content(&self) -> &str {
        &self.text
    }

    fn position(&self) -> (usize, Option<usize>) {
        (self.cursor, self.anchor)
    }

    fn set_position(&mut self, cursor: usize, anchor: Option<usize>) {
        self.cursor = cursor;
        self.anchor = anchor;
    }

    fn splice(&mut self, range: Range<usize>, text: &str) -> bool {
        // Text already over the limit can still shrink.
        if text.len() > range.len() && self.text.len() - range.len() + text.len() > self.limit {
            return false;
        }
        self.cursor = range.start + text.len();
        self.text.replace_range(range, text);
        self.anchor = None;
        true
    }

    fn rules(&self) -> Rules {
        Rules { multiline: self.multiline, concealed: self.hidden, read_only: self.read_only, appending: false }
    }
}

impl TextInput {
    /// A single line holding `text`, with the caret at its end.
    pub fn new(text: impl Into<String>) -> TextInput {
        let text = text.into();
        TextInput { cursor: text.len(), text, ..Default::default() }
    }

    /// Makes it a text of several lines, of at most `limit` bytes.
    pub fn multiline(mut self, limit: usize) -> TextInput {
        self.multiline = true;
        self.limit = limit;
        self
    }

    /// Makes it a text to read and copy from, not to change.
    pub fn read_only(mut self, read_only: bool) -> TextInput {
        self.read_only = read_only;
        self
    }

    pub fn selection(&self) -> Range<usize> {
        self.selected()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    /// Puts `value` in place of the selection, as typing it would. Returns
    /// whether there was room.
    pub fn insert(&mut self, value: &str) -> bool {
        self.splice(self.selected(), value)
    }

    /// Inserts text from the clipboard: as one line, unless this field has
    /// several. Returns whether there was room.
    pub fn paste(&mut self, text: &str) -> bool {
        if self.read_only {
            return false;
        }
        let text = if self.multiline { text } else { text.trim_end_matches(['\r', '\n']) };
        let text: String = text
            .chars()
            .filter(|character| *character != '\r')
            .filter_map(|character| match character {
                '\n' | '\t' if self.multiline => Some(character),
                '\n' | '\t' => Some(' '),
                _ => (!character.is_control()).then_some(character),
            })
            .collect();
        self.insert(&text)
    }

    /// Places the caret at a byte offset, as a press of the pointer does;
    /// `extend` selects up to there from where the caret was.
    pub fn move_to(&mut self, position: usize, extend: bool) {
        self.place(position, extend);
    }

    /// Places the caret where a single line was pressed: `x_pos` from the
    /// left of the text, drawn `width` wide by `text::Text::field`.
    pub fn click(
        &mut self,
        renderer: &mut crate::text::Text,
        font: &str,
        size: f32,
        width: f32,
        x_pos: f32,
        extend: bool,
    ) {
        let position = renderer.field_hit(&self.text, font, size, width, self.cursor, x_pos);
        self.place(position, extend);
    }

    /// Gives the field a key. `compose` is the state of dead keys and
    /// compose sequences where this field is.
    pub fn key(&mut self, compose: &mut Option<xkb::compose::State>, sym: u32, mods: Mods) -> Key {
        self.edit(compose, sym, mods)
    }
}

#[cfg(test)]
mod tests {
    use smithay::input::keyboard::keysyms;

    use super::{Clipboard, Key, TextInput};
    use crate::config::Mods;

    const PLAIN: Mods = Mods { logo: false, alt: false, ctrl: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, ..PLAIN };
    const ALT: Mods = Mods { alt: true, ..PLAIN };
    const SHIFT: Mods = Mods { shift: true, ..PLAIN };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, ..PLAIN };

    fn press(input: &mut TextInput, sym: u32, mods: Mods) -> Key {
        input.key(&mut None, sym, mods)
    }

    /// The text with the caret as `|` and the selection's other end as `^`.
    fn state(input: &TextInput) -> String {
        let mut text = input.text.clone();
        let mut marks = vec![(input.cursor, '|')];
        marks.extend(input.anchor.filter(|anchor| *anchor != input.cursor).map(|anchor| (anchor, '^')));
        marks.sort_unstable_by_key(|mark| std::cmp::Reverse(mark.0));
        for (offset, mark) in marks {
            text.insert(offset, mark);
        }
        text
    }

    /// Every key a single line has, and what it does to `one two, thr\u{e9}e`
    /// with the caret behind `two`.
    #[test]
    fn single_line_keys() {
        use keysyms::*;
        const MOVED: Key = Key { used: true, changed: false, clipboard: None };
        const CHANGED: Key = Key { used: true, changed: true, clipboard: None };
        let copy = |text: &str, changed| Key { used: true, changed, clipboard: Some(Clipboard::Copy(text.into())) };
        type Presses = &'static [(u32, Mods)];
        let cases: &[(Presses, &str, Key)] = &[
            (&[(KEY_Left, PLAIN)], "one tw|o, thr\u{e9}e", MOVED),
            (&[(KEY_Right, PLAIN)], "one two,| thr\u{e9}e", MOVED),
            (&[(KEY_Left, CTRL)], "one |two, thr\u{e9}e", MOVED),
            (&[(KEY_Left, ALT)], "one |two, thr\u{e9}e", MOVED),
            (&[(KEY_Right, CTRL)], "one two, thr\u{e9}e|", MOVED),
            (&[(KEY_Left, SHIFT)], "one tw|o^, thr\u{e9}e", MOVED),
            (&[(KEY_Right, CTRL_SHIFT)], "one two^, thr\u{e9}e|", MOVED),
            (&[(KEY_Left, SHIFT), (KEY_Left, SHIFT), (KEY_Right, PLAIN)], "one two|, thr\u{e9}e", MOVED),
            (&[(KEY_Left, SHIFT), (KEY_Left, SHIFT), (KEY_Left, PLAIN)], "one t|wo, thr\u{e9}e", MOVED),
            (&[(KEY_Home, PLAIN)], "|one two, thr\u{e9}e", MOVED),
            (&[(KEY_End, PLAIN)], "one two, thr\u{e9}e|", MOVED),
            (&[(KEY_Home, SHIFT)], "|one two^, thr\u{e9}e", MOVED),
            (&[(KEY_End, SHIFT)], "one two^, thr\u{e9}e|", MOVED),
            (&[(KEY_BackSpace, PLAIN)], "one tw|, thr\u{e9}e", CHANGED),
            (&[(KEY_Delete, PLAIN)], "one two| thr\u{e9}e", CHANGED),
            (&[(KEY_BackSpace, CTRL)], "one |, thr\u{e9}e", CHANGED),
            (&[(KEY_BackSpace, ALT)], "one |, thr\u{e9}e", CHANGED),
            (&[(KEY_Delete, CTRL)], "one two|", CHANGED),
            (&[(KEY_End, PLAIN), (KEY_BackSpace, PLAIN), (KEY_BackSpace, PLAIN)], "one two, thr|", CHANGED),
            (&[(KEY_Home, SHIFT), (KEY_BackSpace, PLAIN)], "|, thr\u{e9}e", CHANGED),
            (&[(KEY_a, CTRL)], "^one two, thr\u{e9}e|", MOVED),
            (&[(KEY_A, CTRL_SHIFT)], "^one two, thr\u{e9}e|", MOVED),
            (&[(KEY_a, CTRL), (KEY_x, PLAIN)], "x|", CHANGED),
            (&[(KEY_u, CTRL)], "|", CHANGED),
            (&[(KEY_Left, CTRL_SHIFT), (KEY_c, CTRL)], "one |two^, thr\u{e9}e", copy("two", false)),
            (&[(KEY_Left, CTRL_SHIFT), (KEY_x, CTRL)], "one |, thr\u{e9}e", copy("two", true)),
            (
                &[(KEY_v, CTRL)],
                "one two|, thr\u{e9}e",
                Key { used: true, changed: false, clipboard: Some(Clipboard::Paste) },
            ),
            (&[(KEY_exclam, SHIFT)], "one two!|, thr\u{e9}e", CHANGED),
            // Not the field's: left to whoever owns it.
            (&[(KEY_c, CTRL)], "one two|, thr\u{e9}e", Key::IGNORED),
            (&[(KEY_x, CTRL)], "one two|, thr\u{e9}e", Key::IGNORED),
            (&[(KEY_Return, PLAIN)], "one two|, thr\u{e9}e", Key::IGNORED),
            (&[(KEY_Tab, PLAIN)], "one two|, thr\u{e9}e", Key::IGNORED),
            (&[(KEY_Escape, PLAIN)], "one two|, thr\u{e9}e", Key::IGNORED),
            (&[(KEY_Up, PLAIN)], "one two|, thr\u{e9}e", Key::IGNORED),
            (&[(KEY_f, CTRL)], "one two|, thr\u{e9}e", Key::IGNORED),
            (&[(KEY_f, ALT)], "one two|, thr\u{e9}e", Key::IGNORED),
        ];
        for (keys, expected, last) in cases {
            let mut input = TextInput::new("one two, thr\u{e9}e");
            input.move_to(7, false);
            let mut outcome = Key::IGNORED;
            for (sym, mods) in *keys {
                outcome = press(&mut input, *sym, *mods);
            }
            assert_eq!(state(&input), *expected, "after {keys:?}");
            assert_eq!(outcome, *last, "after {keys:?}");
        }
    }

    #[test]
    fn several_lines() {
        let mut input = TextInput::new("one\ntwo").multiline(64);
        press(&mut input, keysyms::KEY_Home, PLAIN);
        assert_eq!(state(&input), "one\n|two");
        press(&mut input, keysyms::KEY_End, SHIFT);
        assert_eq!(state(&input), "one\n^two|");
        press(&mut input, keysyms::KEY_Home, CTRL);
        assert_eq!(state(&input), "|one\ntwo");
        press(&mut input, keysyms::KEY_End, PLAIN);
        assert!(press(&mut input, keysyms::KEY_Return, PLAIN).changed);
        assert_eq!(state(&input), "one\n|\ntwo");
        // Its own to decide, as moving between rows takes their layout.
        assert!(!press(&mut input, keysyms::KEY_Up, PLAIN).used);
        assert!(!press(&mut input, keysyms::KEY_Return, CTRL).used);
        assert!(input.paste("a\tb\r\nc\u{7}\n"));
        assert_eq!(input.text, "one\na\tb\nc\n\ntwo");
        // No room: nothing changes, though deleting still works.
        assert!(!input.paste(&"x".repeat(64)));
        assert!(press(&mut input, keysyms::KEY_BackSpace, PLAIN).changed);
    }

    #[test]
    fn pastes_one_line() {
        let mut input = TextInput::new("ab");
        input.move_to(1, false);
        assert!(input.paste("one\r\ntwo\tthree\u{7}\n"));
        assert_eq!(state(&input), "aone two three|b");
    }

    #[test]
    fn hidden_text_stays() {
        let mut input = TextInput::new("secret");
        input.hidden = true;
        press(&mut input, keysyms::KEY_a, CTRL);
        assert_eq!(press(&mut input, keysyms::KEY_c, CTRL), Key::IGNORED);
        assert_eq!(press(&mut input, keysyms::KEY_x, CTRL), Key::IGNORED);
        assert_eq!(input.text, "secret");
        input.hidden = false;
        assert_eq!(press(&mut input, keysyms::KEY_c, CTRL).clipboard, Some(Clipboard::Copy("secret".into())));
    }

    #[test]
    fn read_only_copies() {
        let mut input = TextInput::new("text").read_only(true);
        assert!(press(&mut input, keysyms::KEY_a, CTRL).used);
        assert_eq!(press(&mut input, keysyms::KEY_x, CTRL), Key::IGNORED);
        assert_eq!(press(&mut input, keysyms::KEY_c, CTRL).clipboard, Some(Clipboard::Copy("text".into())));
        for sym in [keysyms::KEY_BackSpace, keysyms::KEY_Left, keysyms::KEY_x, keysyms::KEY_Down] {
            assert_eq!(press(&mut input, sym, PLAIN), Key::IGNORED);
        }
        assert_eq!(press(&mut input, keysyms::KEY_v, CTRL), Key::IGNORED);
        assert!(!input.paste("more"));
        assert_eq!(input.text, "text");
    }
}
