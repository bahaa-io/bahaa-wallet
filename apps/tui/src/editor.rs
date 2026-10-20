use zeroize::{Zeroize, Zeroizing};

const CAPACITY: usize = 64;

#[derive(Default)]
pub(crate) struct Editor {
    text: Zeroizing<String>,
    cursor: usize,
}

impl Editor {
    pub(crate) fn new(text: &str) -> Self {
        let mut editor = Self::default();
        editor.insert(text);
        editor
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn insert(&mut self, text: &str) -> bool {
        let clean: Zeroizing<String> = Zeroizing::new(
            text.chars()
                .map(|c| if c.is_whitespace() { ' ' } else { c })
                .filter(|c| !c.is_control())
                .collect(),
        );
        if clean.is_empty() {
            return false;
        }
        self.reserve(clean.len());
        let at = self.byte(self.cursor);
        self.text.insert_str(at, &clean);
        self.cursor += clean.chars().count();
        true
    }

    pub(crate) fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        let at = self.byte(self.cursor);
        self.text.remove(at);
        true
    }

    pub(crate) fn delete(&mut self) -> bool {
        if self.cursor >= self.len() {
            return false;
        }
        let at = self.byte(self.cursor);
        self.text.remove(at);
        true
    }

    pub(crate) fn delete_word(&mut self) -> bool {
        let start = self.word_start(true);
        if start == self.cursor {
            return false;
        }
        let range = self.byte(start)..self.byte(self.cursor);
        self.text.replace_range(range, "");
        self.cursor = start;
        true
    }

    pub(crate) fn clear(&mut self) -> bool {
        if self.text.is_empty() {
            return false;
        }
        self.text.zeroize();
        self.cursor = 0;
        true
    }

    pub(crate) fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(crate) fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    pub(crate) fn home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn end(&mut self) {
        self.cursor = self.len();
    }

    pub(crate) fn word(&self) -> Option<&str> {
        let next = self.text.chars().nth(self.cursor);
        if next.is_some_and(|c| c != ' ') {
            return None;
        }
        let word = &self.text[self.byte(self.word_start(false))..self.byte(self.cursor)];
        (!word.is_empty()).then_some(word)
    }

    pub(crate) fn complete(&mut self, word: &str, space: bool) {
        let start = self.word_start(false);
        let range = self.byte(start)..self.byte(self.cursor);
        let mut completed = Zeroizing::new(String::from(word));
        if space && self.text.chars().nth(self.cursor) != Some(' ') {
            completed.push(' ');
        }
        self.reserve(completed.len());
        self.text.replace_range(range, &completed);
        self.cursor = start + completed.chars().count();
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte(&self, position: usize) -> usize {
        self.text
            .char_indices()
            .nth(position)
            .map_or(self.text.len(), |(index, _)| index)
    }

    fn word_start(&self, skip_spaces: bool) -> usize {
        let before: Vec<char> = self.text.chars().take(self.cursor).collect();
        let mut start = before.len();
        if skip_spaces {
            while start > 0 && before[start - 1] == ' ' {
                start -= 1;
            }
        }
        while start > 0 && before[start - 1] != ' ' {
            start -= 1;
        }
        start
    }

    fn reserve(&mut self, extra: usize) {
        let needed = self.text.len() + extra;
        if needed > self.text.capacity() {
            let mut grown = Zeroizing::new(String::with_capacity(
                needed.max(CAPACITY).next_power_of_two(),
            ));
            grown.push_str(&self.text);
            self.text = grown;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;

    #[test]
    fn edits_follow_the_cursor() {
        let mut editor = Editor::new("abandon abi");
        assert_eq!(editor.word(), Some("abi"));
        editor.complete("ability", true);
        assert_eq!(editor.text(), "abandon ability ");
        editor.left();
        editor.left();
        assert_eq!(editor.word(), None);
        editor.end();
        assert!(editor.delete_word());
        assert_eq!(editor.text(), "abandon ");
        editor.home();
        assert!(editor.insert("x\ty"));
        assert_eq!(editor.text(), "x yabandon ");
        assert!(editor.delete());
        assert!(editor.backspace());
        assert_eq!(editor.text(), "x bandon ");
        assert!(editor.clear());
        assert!(!editor.backspace());
    }

    #[test]
    fn growing_keeps_the_text() {
        let mut editor = Editor::default();
        for _ in 0..100 {
            editor.insert("é");
        }
        assert_eq!(editor.text().chars().count(), 100);
        assert_eq!(editor.cursor(), 100);
    }
}
