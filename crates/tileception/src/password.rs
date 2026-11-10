//! A typed secret, for the locker and the greeter: kept in a fixed buffer
//! that is wiped on every change, and never in command logs or text caches.

use smithay::input::keyboard::{keysyms, xkb};
use zeroize::{Zeroize, Zeroizing};

pub struct SecretField {
    bytes: Zeroizing<[u8; 4096]>,
    len: usize,
    /// The caret, as a byte offset at a character boundary.
    cursor: usize,
    /// Where a selection starts, the caret being its other end.
    anchor: Option<usize>,
    compose: Option<xkb::compose::State>,
}

impl Default for SecretField {
    fn default() -> Self {
        SecretField {
            bytes: Zeroizing::new([0; 4096]),
            len: 0,
            cursor: 0,
            anchor: None,
            compose: crate::ui::compose_state(),
        }
    }
}

impl SecretField {
    pub fn clear(&mut self) {
        self.bytes.zeroize();
        self.len = 0;
        self.cursor = 0;
        self.anchor = None;
        if let Some(compose) = &mut self.compose {
            compose.reset();
        }
    }

    pub fn select_all(&mut self) {
        if self.len > 0 {
            self.anchor = Some(0);
            self.cursor = self.len;
        }
    }

    pub fn is_selected(&self) -> bool {
        !self.selection().is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn selection(&self) -> std::ops::Range<usize> {
        let anchor = self.anchor.unwrap_or(self.cursor);
        anchor.min(self.cursor)..anchor.max(self.cursor)
    }

    /// Replaces `range` with `text` inside the fixed buffer: the rest moves
    /// in place and what it leaves behind is wiped, so no copy is made.
    fn replace(&mut self, range: std::ops::Range<usize>, text: &str) -> bool {
        let len = self.len - range.len() + text.len();
        if len > self.bytes.len() {
            return false;
        }
        self.bytes.copy_within(range.end..self.len, range.start + text.len());
        self.bytes[range.start..range.start + text.len()].copy_from_slice(text.as_bytes());
        if len < self.len {
            self.bytes[len..self.len].zeroize();
        }
        self.len = len;
        self.cursor = range.start + text.len();
        self.anchor = None;
        true
    }

    /// Paste directly into the wiped buffer, without putting a secret in a
    /// rendered text cache. Multiline/control input is refused.
    pub fn insert(&mut self, text: &str) -> bool {
        let text = text.trim_end_matches(['\r', '\n']);
        if text.chars().any(char::is_control) {
            return false;
        }
        self.replace(self.selection(), text)
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("typed UTF-8")
    }

    /// How many characters there are, for drawing a dot each.
    pub fn chars(&self) -> usize {
        self.as_str().chars().count()
    }

    /// The caret and the selection's anchor in characters, for drawing a
    /// mask of one character each.
    pub fn caret(&self) -> (usize, Option<usize>) {
        let count = |offset: usize| self.as_str()[..offset].chars().count();
        (count(self.cursor), self.anchor.filter(|_| self.is_selected()).map(count))
    }

    /// Takes back a half-typed compose sequence, else the selection or the
    /// character before the caret.
    pub fn backspace(&mut self) {
        self.delete(false, false);
    }

    /// Deletes the selection, else the character or word before (or with
    /// `forward`, after) the caret. Backwards, a half-typed compose
    /// sequence goes first.
    pub fn delete(&mut self, forward: bool, word: bool) {
        if !forward
            && let Some(compose) = &mut self.compose
            && compose.status() == xkb::compose::Status::Composing
        {
            compose.reset();
            return;
        }
        let mut range = self.selection();
        if range.is_empty() {
            range = if forward { self.cursor..self.next(word) } else { self.previous(word)..self.cursor };
        }
        self.replace(range, "");
    }

    /// Where the caret goes one character or word back.
    fn previous(&self, word: bool) -> usize {
        let before = &self.as_str()[..self.cursor];
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
        let after = &self.as_str()[self.cursor..];
        if word {
            let start = after.len() - after.trim_start_matches(|character: char| !character.is_alphanumeric()).len();
            self.cursor
                + after[start..]
                    .find(|character: char| !character.is_alphanumeric())
                    .map_or(after.len(), |index| start + index)
        } else {
            self.cursor + after.chars().next().map_or(0, char::len_utf8)
        }
    }

    pub fn move_to(&mut self, position: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = position.min(self.len);
    }

    pub fn move_horizontal(&mut self, right: bool, word: bool, extend: bool) {
        let range = self.selection();
        let position = if !extend && !range.is_empty() {
            if right { range.end } else { range.start }
        } else if right {
            self.next(word)
        } else {
            self.previous(word)
        };
        self.move_to(position, extend);
    }

    /// Adds what a key types at the caret, through compose sequences.
    /// Returns whether anything was added.
    pub fn type_key(&mut self, sym: u32) -> bool {
        let Some(encoded) = crate::ui::compose_text(&mut self.compose, sym) else { return false };
        self.replace(self.selection(), &encoded)
    }

    /// The editing keys every field has (see `text_input`), except copying
    /// and cutting, which a secret does not allow. Pasting is the caller's,
    /// through [`SecretField::insert`]. Returns whether the key was used.
    pub fn edit_key(&mut self, sym: u32, mods: crate::config::Mods) -> bool {
        let word = mods.ctrl || mods.alt;
        match sym {
            keysyms::KEY_a | keysyms::KEY_A if mods.ctrl => self.select_all(),
            keysyms::KEY_u | keysyms::KEY_U if mods.ctrl => self.clear(),
            keysyms::KEY_Left | keysyms::KEY_Right => self.move_horizontal(sym == keysyms::KEY_Right, word, mods.shift),
            keysyms::KEY_Home => self.move_to(0, mods.shift),
            keysyms::KEY_End => self.move_to(self.len, mods.shift),
            keysyms::KEY_BackSpace | keysyms::KEY_Delete => self.delete(sym == keysyms::KEY_Delete, word),
            _ if !mods.ctrl && !mods.alt && !mods.logo => return self.type_key(sym),
            _ => return false,
        }
        true
    }

    /// The text, leaving the field empty.
    pub fn take(&mut self) -> Zeroizing<String> {
        let text = Zeroizing::new(self.as_str().to_owned());
        self.clear();
        text
    }
}

#[cfg(test)]
mod tests {
    use smithay::input::keyboard::keysyms;

    use super::SecretField;
    use crate::config::Mods;

    fn field(text: &str) -> SecretField {
        let mut field = SecretField::default();
        assert!(field.insert(text));
        field
    }

    #[test]
    fn edits_at_the_caret_like_other_fields() {
        let plain = Mods::default();
        let ctrl = Mods { ctrl: true, ..Default::default() };
        let shift = Mods { shift: true, ..Default::default() };
        let mut secret = field("correct horse");
        assert_eq!(secret.caret(), (13, None));
        // Home, then Delete removes forwards.
        secret.edit_key(keysyms::KEY_Home, plain);
        secret.edit_key(keysyms::KEY_Delete, plain);
        assert_eq!(secret.as_str(), "orrect horse");
        // Ctrl+Right skips a word; typing goes in at the caret.
        secret.edit_key(keysyms::KEY_Right, ctrl);
        assert!(secret.insert("ed"));
        assert_eq!(secret.as_str(), "orrected horse");
        // Ctrl+Backspace takes the word before the caret.
        secret.edit_key(keysyms::KEY_BackSpace, ctrl);
        assert_eq!(secret.as_str(), " horse");
        // Shift+End selects to the end; typing replaces the selection.
        secret.edit_key(keysyms::KEY_Right, plain);
        secret.edit_key(keysyms::KEY_End, shift);
        assert_eq!(secret.caret(), (6, Some(1)));
        assert!(secret.insert("é"));
        assert_eq!(secret.as_str(), " é");
        // Left over a two-byte character, then Ctrl+Delete to the end.
        secret.edit_key(keysyms::KEY_Left, plain);
        assert_eq!(secret.caret(), (1, None));
        secret.edit_key(keysyms::KEY_Delete, ctrl);
        assert_eq!(secret.as_str(), " ");
        // Copying is not one of its keys.
        assert!(!secret.edit_key(keysyms::KEY_c, ctrl));
    }

    #[test]
    fn edits_leave_nothing_behind_the_text() {
        let mut secret = field("hunter2");
        secret.move_to(2, false);
        secret.delete(true, true);
        assert_eq!(secret.as_str(), "hu");
        assert!(secret.bytes[secret.len..].iter().all(|byte| *byte == 0));
        let text = secret.take();
        assert_eq!(text.as_str(), "hu");
        assert!(secret.is_empty() && secret.bytes.iter().all(|byte| *byte == 0));
        assert!(!secret.insert("two\nlines"));
    }

    #[test]
    fn select_all_is_replaced_by_the_next_key() {
        let mut field = SecretField::default();
        field.select_all();
        assert!(!field.is_selected(), "nothing to select");
        assert!(field.type_key(keysyms::KEY_a) && field.type_key(keysyms::KEY_b));
        field.select_all();
        assert!(field.type_key(keysyms::KEY_c));
        assert_eq!(field.as_str(), "c");
        assert!(!field.is_selected());
        field.select_all();
        field.backspace();
        assert!(field.is_empty());
    }
}
