//! Tag editing: a one-line prompt (opened with `T` in the library) to set a
//! book's free-form tags. On a multi-selection the typed tags are *added* to
//! every marked book; on a single book they *replace* its tags. Storage is
//! normalised (lowercase, trimmed, deduped) — see `delryn_model::tags`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Overlay, str_delete_before, str_insert};

/// Open tag-edit prompt: a text buffer plus which book paths it applies to.
pub struct TagInput {
    pub buf: String,
    pub cursor: usize,
    /// Books the committed tags apply to (the marked set, else the current book).
    pub targets: Vec<String>,
    /// Whether more than one book is targeted (typed tags are *added*, not
    /// replaced) — drives the prompt label and commit semantics.
    pub multi: bool,
}

impl App {
    /// Begin editing tags for the marked books (added to each) or, when nothing
    /// is marked, the selected book (replacing its tags — buffer pre-filled).
    pub(crate) fn open_tag_edit(&mut self) {
        let (targets, multi, buf) = if !self.library.marked.is_empty() {
            (
                self.library.marked.iter().cloned().collect(),
                true,
                String::new(),
            )
        } else if let Some(b) = self.library.books.get(self.library.sel) {
            (vec![b.path.clone()], false, b.tags.clone())
        } else {
            return;
        };
        let cursor = buf.chars().count();
        self.overlay = Overlay::TagEdit(TagInput {
            buf,
            cursor,
            targets,
            multi,
        });
    }

    /// Commit the typed tags: replace (single) or add to each (multi), then
    /// refresh so the change shows immediately.
    pub(crate) fn tag_edit_commit(&mut self) {
        let Overlay::TagEdit(input) = std::mem::replace(&mut self.overlay, Overlay::None) else {
            return;
        };
        if let Some(store) = &self.session.store {
            for path in &input.targets {
                let next = if input.multi {
                    // Union the typed tags with whatever the book already has.
                    let existing = self
                        .library
                        .books
                        .iter()
                        .find(|b| &b.path == path)
                        .map(|b| b.tags.clone())
                        .unwrap_or_default();
                    delryn_model::tags::normalize(&format!("{existing}, {}", input.buf))
                } else {
                    delryn_model::tags::normalize(&input.buf)
                };
                store.set_tags(path, &next);
            }
        }
        let n = input.targets.len();
        self.library.flash = Some(if n > 1 {
            format!("tagged {n} books")
        } else {
            "tags updated".into()
        });
        self.refresh_library();
    }

    /// Keys while the tag prompt is active (a simple single-line text input).
    pub(crate) fn tag_edit_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => self.tag_edit_commit(),
            KeyCode::Left => {
                if let Overlay::TagEdit(i) = &mut self.overlay {
                    i.cursor = i.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Overlay::TagEdit(i) = &mut self.overlay {
                    i.cursor = (i.cursor + 1).min(i.buf.chars().count());
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Overlay::TagEdit(i) = &mut self.overlay {
                    i.buf.clear();
                    i.cursor = 0;
                }
            }
            KeyCode::Backspace => {
                if let Overlay::TagEdit(i) = &mut self.overlay {
                    let c = i.cursor;
                    if str_delete_before(&mut i.buf, c) {
                        i.cursor -= 1;
                    }
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Overlay::TagEdit(i) = &mut self.overlay {
                    let cur = i.cursor;
                    str_insert(&mut i.buf, cur, c);
                    i.cursor += 1;
                }
            }
            _ => {}
        }
    }
}
