//! Tag editing: a one-line prompt (opened with `T` in the library) to set a
//! book's free-form tags. On a multi-selection the typed tags are *added* to
//! every marked book; on a single book they *replace* its tags. Storage is
//! normalised (lowercase, trimmed, deduped) — see `delryn_model::tags`.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Overlay};
use crate::ui::TextInput;

/// Open tag-edit prompt: a text buffer plus which book paths it applies to.
pub struct TagInput {
    pub input: TextInput,
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
        self.overlay = Overlay::TagEdit(TagInput {
            input: TextInput::from(buf),
            targets,
            multi,
        });
    }

    /// Commit the typed tags: replace (single) or add to each (multi), then
    /// refresh so the change shows immediately.
    pub(crate) fn tag_edit_commit(&mut self) {
        let Overlay::TagEdit(te) = std::mem::replace(&mut self.overlay, Overlay::None) else {
            return;
        };
        let typed = te.input.text();
        if let Some(store) = &self.session.store {
            for path in &te.targets {
                let next = if te.multi {
                    // Union the typed tags with whatever the book already has.
                    let existing = self
                        .library
                        .books
                        .iter()
                        .find(|b| &b.path == path)
                        .map(|b| b.tags.clone())
                        .unwrap_or_default();
                    delryn_model::tags::normalize(&format!("{existing}, {typed}"))
                } else {
                    delryn_model::tags::normalize(typed)
                };
                store.set_tags(path, &next);
            }
        }
        let n = te.targets.len();
        self.library.flash = Some(if n > 1 {
            format!("tagged {n} books")
        } else {
            "tags updated".into()
        });
        self.refresh_library();
    }

    /// Keys while the tag prompt is active (a simple single-line text input).
    pub(crate) fn tag_edit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => self.tag_edit_commit(),
            _ => {
                if let Overlay::TagEdit(i) = &mut self.overlay {
                    i.input.handle_key(key);
                }
            }
        }
    }
}
