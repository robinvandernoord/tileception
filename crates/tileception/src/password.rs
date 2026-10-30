//! A typed secret, for the locker and the greeter: kept in a fixed buffer
//! that is wiped on every change, and never in command logs or text caches.

use smithay::input::keyboard::xkb;
use zeroize::{Zeroize, Zeroizing};

pub struct SecretField {
    bytes: Zeroizing<[u8; 4096]>,
    len: usize,
    /// Ctrl+A: the next key replaces everything.
    selected: bool,
    compose: Option<xkb::compose::State>,
}

impl Default for SecretField {
    fn default() -> Self {
        SecretField { bytes: Zeroizing::new([0; 4096]), len: 0, selected: false, compose: crate::ui::compose_state() }
    }
}

impl SecretField {
    pub fn clear(&mut self) {
        self.bytes.zeroize();
        self.len = 0;
        self.selected = false;
        if let Some(compose) = &mut self.compose {
            compose.reset();
        }
    }

    pub fn select_all(&mut self) {
        self.selected = self.len > 0;
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("typed UTF-8")
    }

    /// How many characters there are, for drawing a dot each.
    pub fn chars(&self) -> usize {
        self.as_str().chars().count()
    }

    /// Takes back a half-typed compose sequence, else the selection or the
    /// last character.
    pub fn backspace(&mut self) {
        if self.selected {
            self.clear();
            return;
        }
        if let Some(compose) = &mut self.compose
            && compose.status() == xkb::compose::Status::Composing
        {
            compose.reset();
            return;
        }
        if self.len > 0 {
            let last = self.as_str().char_indices().last().map(|(index, _)| index).unwrap_or(0);
            self.bytes[last..self.len].zeroize();
            self.len = last;
        }
    }

    /// Adds what a key types, through compose sequences. Returns whether
    /// anything was added.
    pub fn type_key(&mut self, sym: u32) -> bool {
        let Some(encoded) = crate::ui::compose_text(&mut self.compose, sym) else { return false };
        if self.selected {
            self.clear();
        }
        if self.len + encoded.len() > self.bytes.len() {
            return false;
        }
        self.bytes[self.len..self.len + encoded.len()].copy_from_slice(encoded.as_bytes());
        self.len += encoded.len();
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
