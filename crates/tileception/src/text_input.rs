//! Text fields: what every field Tileception draws itself has in common.
//!
//! [`TextInput`] holds a field's text, caret and selection, and
//! [`SecretField`] does so for a secret in a buffer that is wiped. Both take
//! their keys through one handler, `Buffer::edit`, and the pointer through
//! `Buffer::point`, so a key or a click means the same in every field. A
//! field passes its keys to `key` and acts on what comes back in [`Key`]; it
//! handles only what is its own (Enter, Escape, Tab, the keys of a list
//! attached to it) and never matches an editing key itself. The methods that
//! move the caret and delete are private to this module to keep it that way.

mod secret;

use std::{
    ops::Range,
    time::{Duration, Instant},
};

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
    /// What the clipboard is to do, which takes the compositor's state:
    /// given to `Tileception::field_clipboard`.
    pub clipboard: Clipboard,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Clipboard {
    /// Text copied or cut from the field, for the clipboard.
    pub copy: Option<String>,
    /// The field asks for the clipboard's text, to be given to its `paste`.
    pub paste: bool,
    /// What the key left selected, for the primary selection, which a
    /// middle click pastes.
    pub selected: Option<String>,
}

impl Key {
    fn ignored() -> Key {
        Key::default()
    }

    fn moved() -> Key {
        Key::edited(false)
    }

    fn edited(changed: bool) -> Key {
        Key { used: true, changed, clipboard: Clipboard::default() }
    }
}

/// A keypad key with NumLock off, as the key it stands for.
pub fn plain(sym: u32) -> u32 {
    match sym {
        keysyms::KEY_KP_Left => keysyms::KEY_Left,
        keysyms::KEY_KP_Right => keysyms::KEY_Right,
        keysyms::KEY_KP_Up => keysyms::KEY_Up,
        keysyms::KEY_KP_Down => keysyms::KEY_Down,
        keysyms::KEY_KP_Home => keysyms::KEY_Home,
        keysyms::KEY_KP_End => keysyms::KEY_End,
        keysyms::KEY_KP_Page_Up => keysyms::KEY_Page_Up,
        keysyms::KEY_KP_Page_Down => keysyms::KEY_Page_Down,
        keysyms::KEY_KP_Insert => keysyms::KEY_Insert,
        keysyms::KEY_KP_Delete => keysyms::KEY_Delete,
        _ => sym,
    }
}

/// How a field differs from a plain single line.
#[derive(Clone, Copy, Default)]
struct Rules {
    /// Enter starts a new line, and Home and End stay within the line.
    multiline: bool,
    /// Lines are indented: Tab indents, Shift+Tab takes an indent away and
    /// Enter keeps the line's.
    code: bool,
    /// The text is not shown: it is not copied or cut, and has no words.
    concealed: bool,
    /// Only selecting and copying work.
    read_only: bool,
    /// No caret is drawn, so there is none to move: keys add and delete at
    /// the end, and deleting a word takes everything.
    appending: bool,
}

/// What an edit is, for undoing a run of them in one step.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Change {
    #[default]
    Other,
    /// A typed character.
    Type,
    /// One character deleted.
    Erase,
}

/// Presses of the pointer, for telling a double and a triple click.
#[derive(Default)]
struct Clicks {
    /// When and where the last press was.
    last: Option<(Instant, usize)>,
    count: u8,
    /// The word or line a double or triple click selected, which a drag
    /// from there keeps selected.
    pivot: Option<Range<usize>>,
}

/// How soon a press at the same place counts as one more click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
const INDENT: &str = "    ";

/// The text of a field with its caret, edited by keys in `edit` and by the
/// pointer in `point`.
trait Buffer {
    fn content(&self) -> &str;
    /// The caret and where a selection starts, as byte offsets.
    fn position(&self) -> (usize, Option<usize>);
    fn set_position(&mut self, cursor: usize, anchor: Option<usize>);
    /// Replaces `range` by `text`, leaving the caret behind it and nothing
    /// selected. False when there is no room, and nothing changed.
    fn splice(&mut self, range: Range<usize>, text: &str) -> bool;
    fn rules(&self) -> Rules;
    fn clicks(&mut self) -> &mut Clicks;
    /// Says what the next `splice` is, for buffers that can undo.
    fn mark(&mut self, _change: Change) {}
    /// Undoes the last step, or does it again. Returns whether there was one.
    fn revert(&mut self, _redo: bool) -> bool {
        false
    }

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

    /// The line around a place, without its newline: everything in a field
    /// of one line.
    fn line(&self, position: usize) -> Range<usize> {
        let text = self.content();
        if !self.rules().multiline {
            return 0..text.len();
        }
        let start = text[..position].rfind('\n').map_or(0, |offset| offset + 1);
        start..position + text[position..].find('\n').unwrap_or(text.len() - position)
    }

    /// The word at a place, or the run of spaces and punctuation there.
    /// Text that is not shown has no words to see: all of it.
    fn word(&self, position: usize) -> Range<usize> {
        let text = self.content();
        if self.rules().concealed {
            return 0..text.len();
        }
        let line = self.line(position);
        let (before, after) = (&text[line.start..position], &text[position..line.end]);
        let Some(kind) = after.chars().next().or_else(|| before.chars().next_back()).map(char::is_alphanumeric) else {
            return position..position;
        };
        let other = |character: char| character.is_alphanumeric() != kind;
        let start = before.rfind(other).map_or(line.start, |offset| {
            line.start + offset + before[offset..].chars().next().map_or(0, char::len_utf8)
        });
        start..position + after.find(other).unwrap_or(after.len())
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
            if !word {
                self.mark(Change::Erase);
            }
        }
        !range.is_empty() && self.splice(range, "")
    }

    /// Replaces the selection by `text`. Returns whether the text changed.
    fn put(&mut self, text: &str) -> bool {
        let range = self.selected();
        (!range.is_empty() || !text.is_empty()) && self.splice(range, text)
    }

    /// Indents the lines the selection touches, or takes an indent away
    /// from them, keeping them selected. Returns whether the text changed.
    fn shift_lines(&mut self, out: bool) -> bool {
        let range = self.selected();
        let (cursor, anchor) = self.position();
        let lines = self.line(range.start).start..self.line(range.end).end;
        let several = self.content()[lines.clone()].contains('\n');
        let mut shifted = String::new();
        // Where the selection's ends go: each moves with its own line, and
        // one inside an indent that is taken away goes to the line's start.
        let (mut start, mut end) = (range.start, range.end);
        let (mut offset, mut changed) = (lines.start, false);
        for (index, line) in self.content()[lines.clone()].split('\n').enumerate() {
            if index > 0 {
                shifted.push('\n');
            }
            let begins = lines.start + shifted.len();
            let (taken, added) = if out {
                let indent = if line.starts_with('\t') {
                    1
                } else {
                    line.bytes().take(INDENT.len()).take_while(|byte| *byte == b' ').count()
                };
                (indent, 0)
            } else if several && line.is_empty() {
                (0, 0)
            } else {
                shifted.push_str(INDENT);
                (0, INDENT.len())
            };
            shifted.push_str(&line[taken..]);
            changed |= taken + added > 0;
            let within = offset..=offset + line.len();
            let moved = |place: usize| begins + added + (place - offset).saturating_sub(taken);
            if within.contains(&range.start) {
                start = moved(range.start);
            }
            if within.contains(&range.end) {
                end = moved(range.end);
            }
            offset += line.len() + 1;
        }
        if !changed || !self.splice(lines, &shifted) {
            return false;
        }
        match anchor.filter(|_| !range.is_empty()) {
            Some(anchor) if anchor > cursor => self.set_position(start, Some(end)),
            Some(_) => self.set_position(end, Some(start)),
            None => self.set_position(start, None),
        }
        true
    }

    /// The one place a key is given its meaning in a field.
    fn edit(&mut self, compose: &mut Option<xkb::compose::State>, sym: u32, mods: Mods) -> Key {
        let before = self.selected();
        let mut key = self.press_key(compose, sym, mods);
        let after = self.selected();
        if key.used && !after.is_empty() && after != before && !self.rules().concealed {
            key.clipboard.selected = Some(self.content()[after].to_owned());
        }
        key
    }

    fn press_key(&mut self, compose: &mut Option<xkb::compose::State>, sym: u32, mods: Mods) -> Key {
        let sym = plain(sym);
        let rules = self.rules();
        self.clicks().pivot = None;
        let end = self.content().len();
        let ctrl = |lower: u32, upper: u32| mods.ctrl && (sym == lower || sym == upper);
        let typing = !mods.ctrl && !mods.alt && !mods.logo;
        if ctrl(keysyms::KEY_a, keysyms::KEY_A) {
            self.set_position(end, (end > 0).then_some(0));
            return Key::moved();
        }
        // Ctrl+C and Ctrl+X, or as of old Ctrl+Insert and Shift+Delete.
        let range = self.selected();
        let copy = ctrl(keysyms::KEY_c, keysyms::KEY_C) || (sym == keysyms::KEY_Insert && mods.ctrl && !mods.shift);
        let cut = ctrl(keysyms::KEY_x, keysyms::KEY_X);
        let old_cut = sym == keysyms::KEY_Delete && mods.shift && typing;
        if copy || cut || (old_cut && !range.is_empty() && !rules.concealed && !rules.read_only) {
            if rules.concealed || range.is_empty() || (!copy && rules.read_only) {
                return Key::ignored();
            }
            let text = self.content()[range.clone()].to_owned();
            let changed = !copy && self.splice(range, "");
            return Key { used: true, changed, clipboard: Clipboard { copy: Some(text), ..Default::default() } };
        }
        if rules.read_only {
            return Key::ignored();
        }
        // Ctrl+V, or as of old Shift+Insert.
        if ctrl(keysyms::KEY_v, keysyms::KEY_V) || (sym == keysyms::KEY_Insert && mods.shift && !mods.ctrl) {
            return Key { used: true, changed: false, clipboard: Clipboard { paste: true, ..Default::default() } };
        }
        // Ctrl+Z undoes; Ctrl+Shift+Z and Ctrl+Y do it again.
        let again = ctrl(keysyms::KEY_y, keysyms::KEY_Y);
        if again || ctrl(keysyms::KEY_z, keysyms::KEY_Z) {
            return Key::edited(self.revert(again || mods.shift));
        }
        let cursor = self.position().0;
        if ctrl(keysyms::KEY_u, keysyms::KEY_U) || ctrl(keysyms::KEY_k, keysyms::KEY_K) {
            if let Some(compose) = compose {
                compose.reset();
            }
            // Ctrl+U clears the line, Ctrl+K what is left of it.
            let line = self.line(cursor);
            let range =
                if sym == keysyms::KEY_u || sym == keysyms::KEY_U || rules.appending { line } else { cursor..line.end };
            return Key::edited(!range.is_empty() && self.splice(range, ""));
        }
        // Ctrl+W takes the word before the caret, as Ctrl+Backspace does.
        let (sym, word) = if ctrl(keysyms::KEY_w, keysyms::KEY_W) {
            (keysyms::KEY_BackSpace, true)
        } else {
            (sym, mods.ctrl || mods.alt)
        };
        match sym {
            keysyms::KEY_Left | keysyms::KEY_Right if !rules.appending => {
                self.step(sym == keysyms::KEY_Right, word, mods.shift);
                Key::moved()
            }
            keysyms::KEY_Home | keysyms::KEY_End if !rules.appending => {
                let range = if mods.ctrl { 0..end } else { self.line(cursor) };
                self.place(if sym == keysyms::KEY_Home { range.start } else { range.end }, mods.shift);
                Key::moved()
            }
            keysyms::KEY_BackSpace | keysyms::KEY_Delete => {
                // Backspace first takes back a half-typed compose sequence.
                if sym == keysyms::KEY_BackSpace
                    && let Some(compose) = compose
                    && compose.status() == xkb::compose::Status::Composing
                {
                    compose.reset();
                    return Key::moved();
                }
                Key::edited(if !rules.appending {
                    self.remove(sym == keysyms::KEY_Delete, word)
                } else if word {
                    end > 0 && self.splice(0..end, "")
                } else {
                    self.remove(false, false)
                })
            }
            keysyms::KEY_Return | keysyms::KEY_KP_Enter if rules.multiline && typing => {
                let mut text = String::from("\n");
                if rules.code {
                    let line = &self.content()[self.line(range.start).start..range.start];
                    text.push_str(&line[..line.len() - line.trim_start_matches([' ', '\t']).len()]);
                }
                Key::edited(self.put(&text))
            }
            keysyms::KEY_ISO_Left_Tab if rules.code && typing => Key::edited(self.shift_lines(true)),
            keysyms::KEY_Tab if rules.code && typing && (mods.shift || !range.is_empty()) => {
                Key::edited(self.shift_lines(mods.shift))
            }
            keysyms::KEY_Tab if rules.code && typing => Key::edited(self.put(INDENT)),
            _ if typing => match crate::ui::compose_text(compose, sym) {
                Some(text) => {
                    self.mark(Change::Type);
                    Key::edited(self.put(&text))
                }
                None if compose.as_ref().is_some_and(|compose| compose.status() == xkb::compose::Status::Composing) => {
                    Key::moved()
                }
                None => Key::ignored(),
            },
            _ => Key::ignored(),
        }
    }

    /// The one place a press of the pointer is given its meaning: it places
    /// the caret, a second one there selects the word and a third the line
    /// (in a field of one line, everything). With `extend`, for a drag or a
    /// press with Shift, it selects up to there instead.
    fn point(&mut self, position: usize, extend: bool, now: Instant) {
        let position = position.min(self.content().len());
        if extend {
            match self.clicks().pivot.clone() {
                // From a double or triple click, the word or line stays in.
                Some(pivot) if position < pivot.start => self.set_position(position, Some(pivot.end)),
                Some(pivot) => self.set_position(position.max(pivot.end), Some(pivot.start)),
                None => self.place(position, true),
            }
            return;
        }
        let clicks = self.clicks();
        let again = clicks
            .last
            .is_some_and(|(when, place)| place == position && now.saturating_duration_since(when) < MULTI_CLICK);
        clicks.count = if again { clicks.count % 3 + 1 } else { 1 };
        clicks.last = Some((now, position));
        let range = match clicks.count {
            2 => Some(self.word(position)),
            3 => Some(self.line(position)),
            _ => None,
        }
        .filter(|range| !range.is_empty());
        match &range {
            Some(range) => self.set_position(range.end, Some(range.start)),
            None => self.place(position, false),
        }
        self.clicks().pivot = range;
    }

    /// What is selected, to offer as the primary selection: nothing of a
    /// text that is not shown.
    fn offered(&self) -> Option<String> {
        let range = self.selected();
        (!range.is_empty() && !self.rules().concealed).then(|| self.content()[range].to_owned())
    }
}

/// A text as it was, to go back to.
struct Past {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
}

/// The steps a text took, for undoing them.
#[derive(Default)]
struct History {
    undone: Vec<Past>,
    redone: Vec<Past>,
    /// What the next change is, as `Buffer::mark` said.
    next: Change,
    /// The last change, where it left the caret and whether it typed a
    /// space: one of the same kind right there joins its step.
    last: Option<(Change, usize, bool)>,
}

/// How many steps are kept, and of how much text together.
const STEPS: usize = 200;
const STEP_BYTES: usize = 4 * 1024 * 1024;

/// A field's text, with a UTF-8 caret and an optional selection anchor.
pub struct TextInput {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    /// The text is drawn masked, as a password is until it is revealed:
    /// copying and cutting are off meanwhile.
    pub hidden: bool,
    multiline: bool,
    code: bool,
    read_only: bool,
    limit: usize,
    history: History,
    clicks: Clicks,
}

impl Default for TextInput {
    fn default() -> Self {
        TextInput {
            text: String::new(),
            cursor: 0,
            anchor: None,
            hidden: false,
            multiline: false,
            code: false,
            read_only: false,
            limit: 4096,
            history: History::default(),
            clicks: Clicks::default(),
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
        if range.is_empty() && text.is_empty() {
            return true;
        }
        // Typing on, or deleting on, where the last such change ended is
        // part of its step, so a word is undone at once; a space after a
        // word starts the next step.
        let change = std::mem::take(&mut self.history.next);
        let space = text.chars().all(char::is_whitespace);
        let joins = change != Change::Other
            && self.anchor.is_none_or(|anchor| anchor == self.cursor)
            && self.history.last.is_some_and(|(last, place, spaced)| {
                last == change && place == self.cursor && !(change == Change::Type && space && !spaced)
            });
        if !joins {
            let past = Past { text: self.text.clone(), cursor: self.cursor, anchor: self.anchor };
            self.history.undone.push(past);
            while self.history.undone.len() > 1
                && (self.history.undone.len() > STEPS
                    || self.history.undone.iter().map(|past| past.text.len()).sum::<usize>() > STEP_BYTES)
            {
                self.history.undone.remove(0);
            }
        }
        self.history.redone.clear();
        self.cursor = range.start + text.len();
        self.text.replace_range(range, text);
        self.anchor = None;
        self.history.last = Some((change, self.cursor, space));
        true
    }

    fn rules(&self) -> Rules {
        Rules {
            multiline: self.multiline,
            code: self.code,
            concealed: self.hidden,
            read_only: self.read_only,
            appending: false,
        }
    }

    fn clicks(&mut self) -> &mut Clicks {
        &mut self.clicks
    }

    fn mark(&mut self, change: Change) {
        self.history.next = change;
    }

    fn revert(&mut self, redo: bool) -> bool {
        let History { undone, redone, last, .. } = &mut self.history;
        let (source, target) = if redo { (redone, undone) } else { (undone, redone) };
        let Some(past) = source.pop() else { return false };
        target.push(Past {
            text: std::mem::replace(&mut self.text, past.text),
            cursor: self.cursor,
            anchor: self.anchor,
        });
        self.cursor = past.cursor;
        self.anchor = past.anchor;
        *last = None;
        true
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

    /// Makes it a text of indented lines: Tab indents, Shift+Tab takes an
    /// indent away, and Enter keeps the line's.
    pub fn code(mut self) -> TextInput {
        self.code = true;
        self
    }

    /// Makes it a text to read and copy from, not to change.
    pub fn read_only(mut self, read_only: bool) -> TextInput {
        self.read_only = read_only;
        self
    }

    /// Puts the caret before the text instead of behind it.
    pub fn at_start(mut self) -> TextInput {
        self.cursor = 0;
        self
    }

    pub fn selection(&self) -> Range<usize> {
        self.selected()
    }

    /// What is selected, to offer as the primary selection once the pointer
    /// lets go: `Tileception::offer_primary` takes it.
    pub fn selected_text(&self) -> Option<String> {
        self.offered()
    }

    /// Empties the field for its next use: there is nothing to undo then.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
        self.history = History::default();
        self.clicks = Clicks::default();
    }

    /// Puts `value` in place of the selection, as typing it would. Returns
    /// whether there was room.
    pub fn insert(&mut self, value: &str) -> bool {
        self.history.next = Change::Other;
        self.splice(self.selected(), value)
    }

    /// Puts `value` in place of all the text, as one step to undo. Returns
    /// whether there was room.
    pub fn set(&mut self, value: &str) -> bool {
        self.history.next = Change::Other;
        self.splice(0..self.text.len(), value)
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

    /// Places the caret at a byte offset, for the keys a field handles
    /// itself, such as moving between its rows; `extend` selects up to
    /// there from where the caret was. The pointer goes to `press`.
    pub fn move_to(&mut self, position: usize, extend: bool) {
        self.clicks.pivot = None;
        self.place(position, extend);
    }

    /// The pointer pressed at a byte offset: see `Buffer::point`. `extend`
    /// is for a drag, or a press with Shift.
    pub fn press(&mut self, position: usize, extend: bool) {
        self.point(position, extend, Instant::now());
    }

    /// The pointer pressed on a single line: `x_pos` from the left of the
    /// text, drawn `width` wide by `text::Text::field`.
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
        self.press(position, extend);
    }

    /// As `click`, for a field drawn as one `mask` for each character
    /// while it is `hidden`.
    #[allow(clippy::too_many_arguments, reason = "a field's text geometry, as `click` takes it")]
    pub fn click_masked(
        &mut self,
        renderer: &mut crate::text::Text,
        mask: &str,
        font: &str,
        size: f32,
        width: f32,
        x_pos: f32,
        extend: bool,
    ) {
        if !self.hidden {
            return self.click(renderer, font, size, width, x_pos, extend);
        }
        let shown = mask.repeat(self.text.chars().count());
        let cursor = self.text[..self.cursor].chars().count() * mask.len();
        let character = renderer.field_hit(&shown, font, size, width, cursor, x_pos) / mask.len().max(1);
        let position = self.text.char_indices().nth(character).map_or(self.text.len(), |(offset, _)| offset);
        self.press(position, extend);
    }

    /// Gives the field a key. `compose` is the state of dead keys and
    /// compose sequences where this field is.
    pub fn key(&mut self, compose: &mut Option<xkb::compose::State>, sym: u32, mods: Mods) -> Key {
        let key = self.edit(compose, sym, mods);
        self.history.next = Change::Other;
        key
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use smithay::input::keyboard::keysyms;

    use super::{Buffer, Key, TextInput};
    use crate::config::Mods;

    const PLAIN: Mods = Mods { logo: false, alt: false, ctrl: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, ..PLAIN };
    const ALT: Mods = Mods { alt: true, ..PLAIN };
    const SHIFT: Mods = Mods { shift: true, ..PLAIN };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, ..PLAIN };

    fn press(input: &mut TextInput, sym: u32, mods: Mods) -> Key {
        input.key(&mut None, sym, mods)
    }

    fn presses(input: &mut TextInput, keys: &[(u32, Mods)]) -> Key {
        keys.iter().fold(Key::default(), |_, (sym, mods)| press(input, *sym, *mods))
    }

    /// Types text, a key for each character.
    fn type_text(input: &mut TextInput, text: &str) {
        for character in text.chars() {
            press(input, xkb_sym(character), PLAIN);
        }
    }

    fn xkb_sym(character: char) -> u32 {
        smithay::input::keyboard::xkb::utf32_to_keysym(character as u32).raw()
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

    /// What a key did.
    #[derive(Debug, PartialEq)]
    enum Did {
        Ignored,
        Moved,
        Changed,
        Copied(&'static str),
        Cut(&'static str),
        AskedPaste,
    }

    fn did(key: &Key) -> Did {
        match (&key.clipboard.copy, key.clipboard.paste, key.used, key.changed) {
            (Some(text), _, _, changed) => {
                let text: &'static str = Box::leak(text.clone().into_boxed_str());
                if changed { Did::Cut(text) } else { Did::Copied(text) }
            }
            (None, true, ..) => Did::AskedPaste,
            (None, false, false, _) => Did::Ignored,
            (None, false, true, false) => Did::Moved,
            (None, false, true, true) => Did::Changed,
        }
    }

    /// Every key a single line has, and what it does to `one two, thr\u{e9}e`
    /// with the caret behind `two`.
    #[test]
    fn single_line_keys() {
        use Did::*;
        use keysyms::*;
        type Presses = &'static [(u32, Mods)];
        let cases: &[(Presses, &str, Did)] = &[
            (&[(KEY_Left, PLAIN)], "one tw|o, thr\u{e9}e", Moved),
            (&[(KEY_Right, PLAIN)], "one two,| thr\u{e9}e", Moved),
            (&[(KEY_Left, CTRL)], "one |two, thr\u{e9}e", Moved),
            (&[(KEY_Left, ALT)], "one |two, thr\u{e9}e", Moved),
            (&[(KEY_Right, CTRL)], "one two, thr\u{e9}e|", Moved),
            (&[(KEY_Left, SHIFT)], "one tw|o^, thr\u{e9}e", Moved),
            (&[(KEY_Right, CTRL_SHIFT)], "one two^, thr\u{e9}e|", Moved),
            (&[(KEY_Left, SHIFT), (KEY_Left, SHIFT), (KEY_Right, PLAIN)], "one two|, thr\u{e9}e", Moved),
            (&[(KEY_Left, SHIFT), (KEY_Left, SHIFT), (KEY_Left, PLAIN)], "one t|wo, thr\u{e9}e", Moved),
            (&[(KEY_Home, PLAIN)], "|one two, thr\u{e9}e", Moved),
            (&[(KEY_End, PLAIN)], "one two, thr\u{e9}e|", Moved),
            (&[(KEY_Home, SHIFT)], "|one two^, thr\u{e9}e", Moved),
            (&[(KEY_End, SHIFT)], "one two^, thr\u{e9}e|", Moved),
            (&[(KEY_BackSpace, PLAIN)], "one tw|, thr\u{e9}e", Changed),
            (&[(KEY_Delete, PLAIN)], "one two| thr\u{e9}e", Changed),
            (&[(KEY_BackSpace, CTRL)], "one |, thr\u{e9}e", Changed),
            (&[(KEY_BackSpace, ALT)], "one |, thr\u{e9}e", Changed),
            (&[(KEY_w, CTRL)], "one |, thr\u{e9}e", Changed),
            (&[(KEY_Delete, CTRL)], "one two|", Changed),
            (&[(KEY_k, CTRL)], "one two|", Changed),
            (&[(KEY_End, PLAIN), (KEY_BackSpace, PLAIN), (KEY_BackSpace, PLAIN)], "one two, thr|", Changed),
            (&[(KEY_Home, SHIFT), (KEY_BackSpace, PLAIN)], "|, thr\u{e9}e", Changed),
            (&[(KEY_a, CTRL)], "^one two, thr\u{e9}e|", Moved),
            (&[(KEY_A, CTRL_SHIFT)], "^one two, thr\u{e9}e|", Moved),
            (&[(KEY_a, CTRL), (KEY_x, PLAIN)], "x|", Changed),
            (&[(KEY_u, CTRL)], "|", Changed),
            (&[(KEY_Left, CTRL_SHIFT), (KEY_c, CTRL)], "one |two^, thr\u{e9}e", Copied("two")),
            (&[(KEY_Left, CTRL_SHIFT), (KEY_Insert, CTRL)], "one |two^, thr\u{e9}e", Copied("two")),
            (&[(KEY_Left, CTRL_SHIFT), (KEY_x, CTRL)], "one |, thr\u{e9}e", Cut("two")),
            (&[(KEY_Left, CTRL_SHIFT), (KEY_Delete, SHIFT)], "one |, thr\u{e9}e", Cut("two")),
            (&[(KEY_v, CTRL)], "one two|, thr\u{e9}e", AskedPaste),
            (&[(KEY_Insert, SHIFT)], "one two|, thr\u{e9}e", AskedPaste),
            (&[(KEY_exclam, SHIFT)], "one two!|, thr\u{e9}e", Changed),
            // The keypad with NumLock off.
            (&[(KEY_KP_Left, PLAIN)], "one tw|o, thr\u{e9}e", Moved),
            (&[(KEY_KP_Home, SHIFT)], "|one two^, thr\u{e9}e", Moved),
            (&[(KEY_KP_End, PLAIN)], "one two, thr\u{e9}e|", Moved),
            (&[(KEY_KP_Delete, PLAIN)], "one two| thr\u{e9}e", Changed),
            (&[(KEY_KP_Insert, SHIFT)], "one two|, thr\u{e9}e", AskedPaste),
            // Undone and done again.
            (&[(KEY_BackSpace, CTRL), (KEY_z, CTRL)], "one two|, thr\u{e9}e", Changed),
            (&[(KEY_BackSpace, CTRL), (KEY_z, CTRL), (KEY_Z, CTRL_SHIFT)], "one |, thr\u{e9}e", Changed),
            (&[(KEY_BackSpace, CTRL), (KEY_z, CTRL), (KEY_y, CTRL)], "one |, thr\u{e9}e", Changed),
            (&[(KEY_z, CTRL)], "one two|, thr\u{e9}e", Moved),
            (&[(KEY_y, CTRL)], "one two|, thr\u{e9}e", Moved),
            // Not the field's: left to whoever owns it.
            (&[(KEY_c, CTRL)], "one two|, thr\u{e9}e", Ignored),
            (&[(KEY_x, CTRL)], "one two|, thr\u{e9}e", Ignored),
            (&[(KEY_Return, PLAIN)], "one two|, thr\u{e9}e", Ignored),
            (&[(KEY_Tab, PLAIN)], "one two|, thr\u{e9}e", Ignored),
            (&[(KEY_Escape, PLAIN)], "one two|, thr\u{e9}e", Ignored),
            (&[(KEY_Up, PLAIN)], "one two|, thr\u{e9}e", Ignored),
            (&[(KEY_f, CTRL)], "one two|, thr\u{e9}e", Ignored),
            (&[(KEY_f, ALT)], "one two|, thr\u{e9}e", Ignored),
        ];
        for (keys, expected, last) in cases {
            let mut input = TextInput::new("one two, thr\u{e9}e");
            input.move_to(7, false);
            let outcome = presses(&mut input, keys);
            assert_eq!(state(&input), *expected, "after {keys:?}");
            assert_eq!(did(&outcome), *last, "after {keys:?}");
        }
    }

    #[test]
    fn undoes_a_word_at_a_time() {
        let undo = |input: &mut TextInput| press(input, keysyms::KEY_z, CTRL).changed;
        let mut input = TextInput::default();
        type_text(&mut input, "one two  three");
        for expected in ["one two|", "one|", "|"] {
            assert!(undo(&mut input));
            assert_eq!(state(&input), expected);
        }
        assert!(!undo(&mut input), "nothing left to undo");
        // Done again in the same steps; a new edit then ends that.
        assert!(press(&mut input, keysyms::KEY_y, CTRL).changed);
        assert_eq!(state(&input), "one|");
        type_text(&mut input, "s");
        assert!(!press(&mut input, keysyms::KEY_y, CTRL).changed);
        assert_eq!(state(&input), "ones|");
        // Characters deleted one after another come back together, but
        // not those deleted elsewhere after moving.
        presses(&mut input, &[(keysyms::KEY_BackSpace, PLAIN), (keysyms::KEY_BackSpace, PLAIN)]);
        presses(&mut input, &[(keysyms::KEY_Home, PLAIN), (keysyms::KEY_Delete, PLAIN)]);
        assert_eq!(state(&input), "|n");
        assert!(undo(&mut input));
        assert_eq!(state(&input), "|on");
        assert!(undo(&mut input));
        assert_eq!(state(&input), "ones|");
        // A paste, a cut and a replaced selection are each one step, and
        // the selection comes back with the text.
        input.paste(" pasted text");
        presses(&mut input, &[(keysyms::KEY_Left, CTRL_SHIFT), (keysyms::KEY_x, CTRL)]);
        presses(&mut input, &[(keysyms::KEY_a, CTRL)]);
        type_text(&mut input, "x");
        for expected in ["^ones pasted |", "ones pasted |text^", "ones|"] {
            assert!(undo(&mut input));
            assert_eq!(state(&input), expected);
        }
        // A field emptied for its next use has nothing to undo.
        input.clear();
        assert!(!undo(&mut input));
        // Replacing all of it is one step.
        type_text(&mut input, "ab");
        input.set("whole");
        assert!(undo(&mut input));
        assert_eq!(state(&input), "ab|");
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
        assert!(!press(&mut input, keysyms::KEY_Tab, PLAIN).used);
        assert!(input.paste("a\tb\r\nc\u{7}\n"));
        assert_eq!(input.text, "one\na\tb\nc\n\ntwo");
        // No room: nothing changes, though deleting still works.
        assert!(!input.paste(&"x".repeat(64)));
        assert!(press(&mut input, keysyms::KEY_BackSpace, PLAIN).changed);
        // Ctrl+U clears the line and Ctrl+K what is left of it, not all.
        input.move_to(5, false);
        press(&mut input, keysyms::KEY_k, CTRL);
        assert_eq!(state(&input), "one\na|\nc\ntwo");
        press(&mut input, keysyms::KEY_u, CTRL);
        assert_eq!(state(&input), "one\n|\nc\ntwo");
    }

    #[test]
    fn indents_code() {
        let mut input = TextInput::new("[a]\n  b = 1\n\nc = 2").multiline(256).code();
        // Enter keeps the line's indentation.
        input.move_to(11, false);
        press(&mut input, keysyms::KEY_Return, PLAIN);
        assert_eq!(state(&input), "[a]\n  b = 1\n  |\n\nc = 2");
        // Tab indents at the caret; Shift+Tab takes the line's indent away.
        press(&mut input, keysyms::KEY_Tab, PLAIN);
        assert_eq!(state(&input), "[a]\n  b = 1\n      |\n\nc = 2");
        press(&mut input, keysyms::KEY_ISO_Left_Tab, SHIFT);
        assert_eq!(state(&input), "[a]\n  b = 1\n  |\n\nc = 2");
        press(&mut input, keysyms::KEY_Tab, SHIFT);
        assert_eq!(state(&input), "[a]\n  b = 1\n|\n\nc = 2");
        assert!(!press(&mut input, keysyms::KEY_Tab, SHIFT).changed, "no indent left");
        // With lines selected, Tab indents them all but the empty ones,
        // and they stay selected.
        input.move_to(5, false);
        input.move_to(input.text.len() - 2, true);
        press(&mut input, keysyms::KEY_Tab, PLAIN);
        assert_eq!(state(&input), "[a]\n     ^ b = 1\n\n\n    c =| 2");
        presses(&mut input, &[(keysyms::KEY_Tab, SHIFT), (keysyms::KEY_Tab, SHIFT)]);
        assert_eq!(state(&input), "[a]\n^b = 1\n\n\nc =| 2");
        // Each of those is one step to undo.
        press(&mut input, keysyms::KEY_z, CTRL);
        assert_eq!(input.text, "[a]\n  b = 1\n\n\nc = 2");
        // A selection ending inside an indent that is taken away ends at
        // that line's start, not in the line before it.
        let mut input = TextInput::new("a\u{e9}\n    x").multiline(64).code();
        input.move_to(0, false);
        input.move_to(6, true);
        assert_eq!(state(&input), "^a\u{e9}\n  |  x");
        assert!(press(&mut input, keysyms::KEY_Tab, SHIFT).changed);
        assert_eq!(state(&input), "^a\u{e9}\n|x");
        // The same for where it starts, and for a caret alone.
        let mut input = TextInput::new("    a\n\tb").multiline(64).code();
        input.move_to(8, false);
        input.move_to(2, true);
        press(&mut input, keysyms::KEY_Tab, SHIFT);
        assert_eq!(state(&input), "|a\nb^");
        let mut input = TextInput::new("      a").multiline(64).code();
        input.move_to(2, false);
        press(&mut input, keysyms::KEY_Tab, SHIFT);
        assert_eq!(state(&input), "|  a");
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
        let all = press(&mut input, keysyms::KEY_a, CTRL);
        assert_eq!(all.clipboard.selected, None, "not offered to a middle click either");
        for (sym, mods) in [(keysyms::KEY_c, CTRL), (keysyms::KEY_x, CTRL), (keysyms::KEY_Insert, CTRL)] {
            assert_eq!(did(&press(&mut input, sym, mods)), Did::Ignored);
        }
        // Shift+Delete then only deletes.
        input.hidden = false;
        assert_eq!(did(&press(&mut input, keysyms::KEY_c, CTRL)), Did::Copied("secret"));
        input.hidden = true;
        assert_eq!(did(&press(&mut input, keysyms::KEY_Delete, SHIFT)), Did::Changed);
        assert_eq!(input.text, "");
    }

    #[test]
    fn read_only_copies() {
        let mut input = TextInput::new("text").read_only(true);
        assert!(press(&mut input, keysyms::KEY_a, CTRL).used);
        for (sym, mods) in [(keysyms::KEY_x, CTRL), (keysyms::KEY_Delete, SHIFT), (keysyms::KEY_v, CTRL)] {
            assert_eq!(did(&press(&mut input, sym, mods)), Did::Ignored);
        }
        assert_eq!(did(&press(&mut input, keysyms::KEY_c, CTRL)), Did::Copied("text"));
        for sym in [keysyms::KEY_BackSpace, keysyms::KEY_Left, keysyms::KEY_x, keysyms::KEY_Down] {
            assert_eq!(did(&press(&mut input, sym, PLAIN)), Did::Ignored);
        }
        assert_eq!(did(&press(&mut input, keysyms::KEY_z, CTRL)), Did::Ignored);
        assert!(!input.paste("more"));
        assert_eq!(input.text, "text");
    }

    #[test]
    fn offers_what_a_key_selected() {
        let mut input = TextInput::new("one two");
        assert_eq!(press(&mut input, keysyms::KEY_Left, CTRL_SHIFT).clipboard.selected.as_deref(), Some("two"));
        assert_eq!(press(&mut input, keysyms::KEY_a, CTRL).clipboard.selected.as_deref(), Some("one two"));
        // Nothing new is selected by these.
        assert_eq!(press(&mut input, keysyms::KEY_c, CTRL).clipboard.selected, None);
        assert_eq!(press(&mut input, keysyms::KEY_Left, PLAIN).clipboard.selected, None);
        assert_eq!(input.selected_text(), None);
    }

    #[test]
    fn clicks_select_words_and_lines() {
        let start = Instant::now();
        let soon = |clicks: u64| start + Duration::from_millis(clicks * 100);
        let mut input = TextInput::new("one two, three\nfour").multiline(64);
        input.point(5, false, soon(0));
        assert_eq!(state(&input), "one t|wo, three\nfour");
        input.point(5, false, soon(1));
        assert_eq!(state(&input), "one ^two|, three\nfour");
        // Dragging on from a double click keeps the word, either way.
        input.point(11, true, soon(1));
        assert_eq!(state(&input), "one ^two, th|ree\nfour");
        input.point(1, true, soon(1));
        assert_eq!(state(&input), "o|ne two^, three\nfour");
        input.point(5, true, soon(1));
        assert_eq!(state(&input), "one ^two|, three\nfour");
        input.point(5, false, soon(2));
        assert_eq!(state(&input), "^one two, three|\nfour");
        // A fourth press starts over, and so does a slow or a moved one.
        input.point(5, false, soon(3));
        assert_eq!(state(&input), "one t|wo, three\nfour");
        input.point(5, false, soon(9));
        assert_eq!(state(&input), "one t|wo, three\nfour");
        input.point(6, false, soon(10));
        assert_eq!(state(&input), "one tw|o, three\nfour");
        // Between words, the run of spaces and punctuation.
        input.point(7, false, soon(20));
        input.point(7, false, soon(21));
        assert_eq!(state(&input), "one two^, |three\nfour");
        // A press with Shift selects from the caret.
        input.point(2, false, soon(30));
        input.point(6, true, soon(31));
        assert_eq!(state(&input), "on^e tw|o, three\nfour");
        // A key between two presses is no double click's drag.
        input.point(16, false, soon(40));
        input.point(16, false, soon(41));
        assert_eq!(state(&input), "one two, three\n^four|");
        press(&mut input, keysyms::KEY_Left, PLAIN);
        input.point(17, true, soon(42));
        assert_eq!(state(&input), "one two, three\n^fo|ur");
        // Masked text has no words to see: all of it.
        let mut hidden = TextInput::new("pass word");
        hidden.hidden = true;
        hidden.point(2, false, soon(50));
        hidden.point(2, false, soon(51));
        assert_eq!(state(&hidden), "^pass word|");
        assert_eq!(hidden.selected_text(), None);
    }
}
