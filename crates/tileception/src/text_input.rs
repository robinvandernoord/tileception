//! Single-line editing with a UTF-8 caret and an optional selection anchor.

#[derive(Default)]
pub struct TextInput {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
}

impl TextInput {
    pub fn selection(&self) -> std::ops::Range<usize> {
        let anchor = self.anchor.unwrap_or(self.cursor);
        anchor.min(self.cursor)..anchor.max(self.cursor)
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    pub fn insert(&mut self, value: &str) {
        let range = self.selection();
        if self.text.len() - range.len() + value.len() > 4096 {
            return;
        }
        self.cursor = range.start + value.len();
        self.text.replace_range(range, value);
        self.anchor = None;
    }

    pub fn move_to(&mut self, position: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = position.min(self.text.len());
    }

    fn previous(&self, word: bool) -> usize {
        if word {
            let before = &self.text[..self.cursor];
            let end = before.trim_end_matches(|c: char| !c.is_alphanumeric()).len();
            before[..end]
                .rfind(|c: char| !c.is_alphanumeric())
                .map(|i| {
                    i + before[i..].chars().next().expect("character index points to an existing character").len_utf8()
                })
                .unwrap_or(0)
        } else {
            self.text[..self.cursor].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
        }
    }

    fn next(&self, word: bool) -> usize {
        let after = &self.text[self.cursor..];
        if word {
            let start = after.len() - after.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
            self.cursor + after[start..].find(|c: char| !c.is_alphanumeric()).map(|i| start + i).unwrap_or(after.len())
        } else {
            self.cursor + after.chars().next().map(char::len_utf8).unwrap_or(0)
        }
    }

    pub fn move_horizontal(&mut self, right: bool, word: bool, extend: bool) {
        let range = self.selection();
        let position = if !extend && !range.is_empty() {
            if right {
                range.end
            } else {
                range.start
            }
        } else if right {
            self.next(word)
        } else {
            self.previous(word)
        };
        self.move_to(position, extend);
    }

    pub fn delete(&mut self, forward: bool, word: bool) {
        if self.selection().is_empty() {
            let position = if forward { self.next(word) } else { self.previous(word) };
            self.anchor = Some(position);
        }
        self.insert("");
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }
}
