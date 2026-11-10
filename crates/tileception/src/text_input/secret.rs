//! A typed secret, for the locker and the greeter: kept in a fixed buffer
//! that is wiped on every change, and never in command logs or text caches.

use std::ops::Range;

use smithay::input::keyboard::xkb;
use zeroize::{Zeroize, Zeroizing};

use super::{Buffer, Key, Rules};
use crate::config::Mods;

pub struct SecretField {
    bytes: Zeroizing<[u8; 4096]>,
    len: usize,
    /// The caret, as a byte offset at a character boundary.
    cursor: usize,
    /// Where a selection starts, the caret being its other end.
    anchor: Option<usize>,
    /// Drawn without a caret, so keys only add and delete at the end.
    appending: bool,
    compose: Option<xkb::compose::State>,
}

impl Default for SecretField {
    fn default() -> Self {
        SecretField {
            bytes: Zeroizing::new([0; 4096]),
            len: 0,
            cursor: 0,
            anchor: None,
            appending: false,
            compose: crate::ui::compose_state(),
        }
    }
}

impl Buffer for SecretField {
    fn content(&self) -> &str {
        self.as_str()
    }

    fn position(&self) -> (usize, Option<usize>) {
        (self.cursor, self.anchor)
    }

    fn set_position(&mut self, cursor: usize, anchor: Option<usize>) {
        self.cursor = cursor;
        self.anchor = anchor;
    }

    /// Replaces inside the fixed buffer: the rest moves in place and what
    /// it leaves behind is wiped, so no copy is made.
    fn splice(&mut self, range: Range<usize>, text: &str) -> bool {
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

    fn rules(&self) -> Rules {
        Rules { concealed: true, appending: self.appending, ..Default::default() }
    }
}

impl SecretField {
    /// A field drawn as a row of dots without a caret, as the greeter's:
    /// its keys add and delete at the end only.
    pub fn appending() -> SecretField {
        SecretField { appending: true, ..Default::default() }
    }

    pub fn clear(&mut self) {
        self.bytes.zeroize();
        self.len = 0;
        self.cursor = 0;
        self.anchor = None;
        if let Some(compose) = &mut self.compose {
            compose.reset();
        }
    }

    pub fn is_selected(&self) -> bool {
        !self.selected().is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Paste directly into the wiped buffer, without putting a secret in a
    /// rendered text cache. Multiline/control input is refused.
    pub fn insert(&mut self, text: &str) -> bool {
        let text = text.trim_end_matches(['\r', '\n']);
        if text.chars().any(char::is_control) {
            return false;
        }
        self.splice(self.selected(), text)
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

    /// Gives the field a key: the keys every field has (see `text_input`),
    /// except copying and cutting, which a secret does not allow. A paste it
    /// asks for goes to [`SecretField::insert`].
    pub fn key(&mut self, sym: u32, mods: Mods) -> Key {
        let mut compose = self.compose.take();
        let key = self.edit(&mut compose, sym, mods);
        self.compose = compose;
        key
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
    use crate::{config::Mods, text_input::Clipboard};

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
        secret.key(keysyms::KEY_Home, plain);
        secret.key(keysyms::KEY_Delete, plain);
        assert_eq!(secret.as_str(), "orrect horse");
        // Ctrl+Right skips a word; typing goes in at the caret.
        secret.key(keysyms::KEY_Right, ctrl);
        assert!(secret.insert("ed"));
        assert_eq!(secret.as_str(), "orrected horse");
        // Ctrl+Backspace takes the word before the caret.
        secret.key(keysyms::KEY_BackSpace, ctrl);
        assert_eq!(secret.as_str(), " horse");
        // Shift+End selects to the end; typing replaces the selection.
        secret.key(keysyms::KEY_Right, plain);
        secret.key(keysyms::KEY_End, shift);
        assert_eq!(secret.caret(), (6, Some(1)));
        assert!(secret.insert("é"));
        assert_eq!(secret.as_str(), " é");
        // Left over a two-byte character, then Ctrl+Delete to the end.
        secret.key(keysyms::KEY_Left, plain);
        assert_eq!(secret.caret(), (1, None));
        secret.key(keysyms::KEY_Delete, ctrl);
        assert_eq!(secret.as_str(), " ");
        // Copying and cutting are not among its keys; pasting is asked for.
        secret.key(keysyms::KEY_a, ctrl);
        assert!(!secret.key(keysyms::KEY_c, ctrl).used && !secret.key(keysyms::KEY_x, ctrl).used);
        assert_eq!(secret.as_str(), " ");
        assert_eq!(secret.key(keysyms::KEY_v, ctrl).clipboard, Some(Clipboard::Paste));
    }

    #[test]
    fn edits_leave_nothing_behind_the_text() {
        let mut secret = field("hunter2");
        let (plain, ctrl) = (Mods::default(), Mods { ctrl: true, ..Default::default() });
        secret.key(keysyms::KEY_Home, plain);
        secret.key(keysyms::KEY_Right, plain);
        secret.key(keysyms::KEY_Right, plain);
        secret.key(keysyms::KEY_Delete, ctrl);
        assert_eq!(secret.as_str(), "hu");
        assert!(secret.bytes[secret.len..].iter().all(|byte| *byte == 0));
        let text = secret.take();
        assert_eq!(text.as_str(), "hu");
        assert!(secret.is_empty() && secret.bytes.iter().all(|byte| *byte == 0));
        assert!(!secret.insert("two\nlines"));
    }

    #[test]
    fn select_all_is_replaced_by_the_next_key() {
        let (plain, ctrl) = (Mods::default(), Mods { ctrl: true, ..Default::default() });
        let mut field = SecretField::default();
        field.key(keysyms::KEY_a, ctrl);
        assert!(!field.is_selected(), "nothing to select");
        assert!(field.key(keysyms::KEY_a, plain).changed && field.key(keysyms::KEY_b, plain).changed);
        field.key(keysyms::KEY_a, ctrl);
        assert!(field.key(keysyms::KEY_c, plain).changed);
        assert_eq!(field.as_str(), "c");
        assert!(!field.is_selected());
        field.key(keysyms::KEY_a, ctrl);
        field.key(keysyms::KEY_BackSpace, plain);
        assert!(field.is_empty());
    }

    #[test]
    fn without_a_caret_keys_work_at_the_end() {
        let (plain, ctrl) = (Mods::default(), Mods { ctrl: true, ..Default::default() });
        let mut field = SecretField::appending();
        assert!(field.insert("two words"));
        for sym in [keysyms::KEY_Left, keysyms::KEY_Home] {
            assert!(!field.key(sym, plain).used);
        }
        // Delete takes from the end too, there being nothing after it.
        field.key(keysyms::KEY_Delete, plain);
        field.key(keysyms::KEY_BackSpace, plain);
        assert_eq!(field.as_str(), "two wor");
        // Words do not show in a row of dots: all of it goes.
        field.key(keysyms::KEY_BackSpace, ctrl);
        assert!(field.is_empty());
    }
}
