//! A single-line text editor with a char cursor.
//!
//! Delryn reimplemented single-line editing ~13 times (the library/image filters,
//! the bookmark prompt, in-book search, the tag/collection editors, the metadata
//! lookup form, bulk-rename), each with its own buffer and inconsistent cursor
//! handling. `TextInput` is the one widget they all share, so every text field
//! edits identically. It is pure state + key handling — no rendering, no business
//! logic; a view reads [`TextInput::text`] and draws the caret at
//! [`TextInput::cursor`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single line of editable text and a cursor position (a **char** index into
/// the text, `0..=chars`). Clone is cheap and used to stash/restore drafts.
#[derive(Default, Clone)]
pub struct TextInput {
    buf: String,
    /// Caret position as a char index in `0..=buf.chars().count()`.
    cursor: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed with existing text, caret at the end (e.g. editing a current value).
    pub fn from(text: impl Into<String>) -> Self {
        let buf = text.into();
        let cursor = buf.chars().count();
        Self { buf, cursor }
    }

    pub fn text(&self) -> &str {
        &self.buf
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Caret position, as a char index — for rendering the cursor.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the text, caret to the end (recall a history entry, reset a field).
    pub fn set(&mut self, text: impl Into<String>) {
        self.buf = text.into();
        self.cursor = self.buf.chars().count();
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cursor = 0;
    }

    /// Take the text out, leaving the input empty (on submit).
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.buf)
    }

    /// Byte offset of the caret, for splicing into `buf`.
    fn byte_at(&self, cursor: usize) -> usize {
        self.buf
            .char_indices()
            .nth(cursor)
            .map_or(self.buf.len(), |(b, _)| b)
    }

    pub fn insert(&mut self, ch: char) {
        let at = self.byte_at(self.cursor);
        self.buf.insert(at, ch);
        self.cursor += 1;
    }

    /// Delete the char before the caret (Backspace).
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_at(self.cursor - 1);
        let end = self.byte_at(self.cursor);
        self.buf.replace_range(start..end, "");
        self.cursor -= 1;
    }

    /// Delete the char at the caret (Delete / forward-delete).
    pub fn delete(&mut self) {
        let len = self.buf.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.buf.replace_range(start..end, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.buf.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buf.chars().count();
    }

    /// Handle an editing key, returning `true` if it was consumed. Covers the
    /// universal set — char insert, Backspace, Delete, Left/Right, Home/End, and
    /// Ctrl+U (clear). Non-editing keys (Enter, Esc, Tab, …) return `false` so the
    /// caller decides submit/cancel/navigation.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if ctrl => self.clear(),
            // Plain (or shifted) printable char — ignore other modifier combos so
            // shortcuts like Ctrl+S aren't swallowed as text.
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(c)
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }

    #[test]
    fn insert_and_text() {
        let mut t = TextInput::new();
        for c in "abc".chars() {
            t.insert(c);
        }
        assert_eq!(t.text(), "abc");
        assert_eq!(t.cursor(), 3);
    }

    #[test]
    fn cursor_move_and_mid_insert() {
        let mut t = TextInput::from("ac");
        t.left(); // between a and c
        t.insert('b');
        assert_eq!(t.text(), "abc");
        assert_eq!(t.cursor(), 2);
    }

    #[test]
    fn backspace_and_delete() {
        let mut t = TextInput::from("abc");
        t.backspace();
        assert_eq!(t.text(), "ab");
        t.home();
        t.delete();
        assert_eq!(t.text(), "b");
        t.backspace(); // at home: no-op
        assert_eq!(t.text(), "b");
    }

    #[test]
    fn unicode_is_char_indexed() {
        let mut t = TextInput::from("áé");
        t.left();
        t.insert('x'); // between á and é
        assert_eq!(t.text(), "áxé");
        t.end();
        t.backspace();
        assert_eq!(t.text(), "áx");
    }

    #[test]
    fn handle_key_consumes_editing_keys_only() {
        let mut t = TextInput::new();
        assert!(t.handle_key(key('h')));
        assert!(t.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
        assert_eq!(t.text(), ""); // Ctrl+U cleared
        t.set("z");
        assert!(t.handle_key(code(KeyCode::Backspace)));
        assert!(!t.handle_key(code(KeyCode::Enter))); // not consumed
        assert!(!t.handle_key(code(KeyCode::Esc)));
    }
}
