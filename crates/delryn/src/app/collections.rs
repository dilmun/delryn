//! Collections & shelves: the inline sidebar editor (create/rename/delete a
//! collection) and the add-to-collection picker (file one or many books onto
//! existing or new collections).

use crossterm::event::{KeyCode, KeyEvent};

use super::confirm::ConfirmAction;
use super::{App, LibView, Overlay};
use crate::ui::TextInput;

/// Add-to-collection picker: toggle the focused book's membership in existing
/// collections, or type a new collection name. The last row is "new".
pub struct ShelfPicker {
    /// Books being filed — one (the current book) or many (a multi-selection).
    pub targets: Vec<String>,
    /// Title, for the popup header ("Title" or "N books").
    pub title: String,
    /// (collection name, whether *all* targets are currently on it).
    pub shelves: Vec<(String, bool)>,
    /// Focused row; `shelves.len()` selects the "＋ New collection" row.
    pub sel: usize,
    /// Buffer while typing a new collection name (`None` when not creating).
    pub new_name: Option<String>,
}

impl ShelfPicker {
    /// The "new collection" row index (one past the existing shelves).
    pub fn new_row(&self) -> usize {
        self.shelves.len()
    }
}

/// Inline collection editing in the sidebar: typing a name to create a new
/// collection, or renaming an existing one (clearing the name deletes it).
pub struct CollInput {
    pub input: TextInput,
    /// `Some(old)` while renaming that collection; `None` while creating one.
    pub rename_from: Option<String>,
}

impl App {
    /// For each existing collection, whether *all* the targets belong to it
    /// (used to pre-tick rows in the picker).
    fn shelf_membership(&self, targets: &[String]) -> Vec<(String, bool)> {
        let Some(store) = &self.session.store else {
            return Vec::new();
        };
        store
            .all_shelves()
            .into_iter()
            .map(|(name, _)| {
                let all = !targets.is_empty()
                    && targets
                        .iter()
                        .all(|p| store.shelves_for(p).iter().any(|s| s == &name));
                (name, all)
            })
            .collect()
    }

    fn lib_coll_input_mut(&mut self) -> Option<&mut CollInput> {
        match &mut self.overlay {
            Overlay::CollEdit(i) => Some(i),
            _ => None,
        }
    }

    /// Begin creating a new collection (inline at the "＋ New" sidebar row).
    pub(crate) fn lib_coll_begin_new(&mut self) {
        self.overlay = Overlay::CollEdit(CollInput {
            input: TextInput::new(),
            rename_from: None,
        });
    }

    /// Begin renaming the focused sidebar collection in place.
    pub(crate) fn lib_coll_begin_rename(&mut self) {
        if let LibView::Shelf(name) = &self.library.view {
            let name = name.clone();
            self.overlay = Overlay::CollEdit(CollInput {
                input: TextInput::from(name.clone()),
                rename_from: Some(name),
            });
        }
    }

    /// Commit the inline edit: rename, create, or (on an emptied name) delete.
    pub(crate) fn lib_coll_commit(&mut self) {
        let Overlay::CollEdit(ce) = std::mem::replace(&mut self.overlay, Overlay::None) else {
            return;
        };
        let name = ce.input.text().trim().to_string();
        if let Some(store) = &self.session.store {
            match (&ce.rename_from, name.is_empty()) {
                (Some(old), true) => store.delete_shelf(old), // cleared name ⇒ delete
                (Some(old), false) => store.rename_shelf(old, &name),
                (None, false) => store.create_collection(&name),
                (None, true) => {} // empty new ⇒ no-op
            }
        }
        // Follow the result: view the created/renamed collection.
        if !name.is_empty() {
            self.library.side_new = false;
            self.library.view = LibView::Shelf(name);
            self.library.sel = 0;
        }
        self.refresh_library();
    }

    /// Keys while the inline collection editor is active.
    pub(crate) fn lib_coll_edit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => {
                // Creating a new collection commits at once (nothing to undo);
                // renaming or deleting an existing one asks for confirmation.
                match &self.overlay {
                    Overlay::CollEdit(i) if i.rename_from.is_none() => self.lib_coll_commit(),
                    Overlay::CollEdit(i) => {
                        let typed = i.input.text().trim();
                        let q = match &i.rename_from {
                            Some(old) if typed.is_empty() => format!("Delete “{old}”?"),
                            _ => format!("Rename to “{typed}”?"),
                        };
                        self.ask_confirm(&q, ConfirmAction::Collection);
                    }
                    _ => {}
                }
            }
            _ => {
                if let Some(i) = self.lib_coll_input_mut() {
                    i.input.handle_key(key);
                }
            }
        }
    }

    /// Open the add-to-collection picker, pre-ticking the collections the
    /// target(s) already belong to. Files the multi-selection when present.
    pub(crate) fn open_shelf_picker(&mut self) {
        // Operate on the multi-selection when present, else the current book.
        let (targets, title) = if !self.library.marked.is_empty() {
            let targets: Vec<String> = self
                .library
                .books
                .iter()
                .filter(|b| self.library.marked.contains(&b.path))
                .map(|b| b.path.clone())
                .collect();
            let n = targets.len();
            (
                targets,
                format!("{n} book{}", if n == 1 { "" } else { "s" }),
            )
        } else {
            match self.library.books.get(self.library.sel) {
                Some(b) => (vec![b.path.clone()], b.title.clone()),
                None => return,
            }
        };
        if targets.is_empty() {
            return;
        }
        let shelves = self.shelf_membership(&targets);
        self.overlay = Overlay::ShelfPicker(ShelfPicker {
            targets,
            title,
            shelves,
            sel: 0,
            new_name: None,
        });
    }

    /// In a collection view, drop the selected book from that collection.
    pub(crate) fn remove_from_current_shelf(&mut self) {
        let LibView::Shelf(name) = &self.library.view else {
            return;
        };
        let name = name.clone();
        if let (Some(store), Some(book)) = (
            &self.session.store,
            self.library.books.get(self.library.sel),
        ) {
            store.remove_from_shelf(&book.path, &name);
        }
        self.refresh_library();
    }

    pub(crate) fn shelf_picker_key(&mut self, key: KeyEvent) {
        let Overlay::ShelfPicker(p) = &mut self.overlay else {
            return;
        };
        // Creating a new collection: the row is a text input.
        if let Some(buf) = p.new_name.as_mut() {
            match key.code {
                KeyCode::Esc => p.new_name = None,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Enter => {
                    let name = buf.trim().to_string();
                    if !name.is_empty() {
                        if let Some(store) = &self.session.store {
                            for path in &p.targets {
                                store.add_to_shelf(path, &name);
                            }
                        }
                        self.refresh_shelf_picker();
                    } else {
                        p.new_name = None;
                    }
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // After a bulk filing, the selection has been consumed.
                let bulk = p.targets.len() > 1;
                self.overlay = Overlay::None;
                if bulk {
                    self.lib_exit_visual();
                }
                self.refresh_library();
            }
            KeyCode::Up | KeyCode::Char('k') => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.sel = (p.sel + 1).min(p.new_row()),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if p.sel == p.new_row() {
                    p.new_name = Some(String::new());
                } else {
                    self.toggle_picked_shelf();
                }
            }
            _ => {}
        }
    }

    /// Toggle every target book's membership in the selected collection. A
    /// ticked row (all members) removes all; otherwise it adds all.
    fn toggle_picked_shelf(&mut self) {
        let Overlay::ShelfPicker(p) = &mut self.overlay else {
            return;
        };
        let Some((name, member)) = p.shelves.get_mut(p.sel) else {
            return;
        };
        let add = !*member;
        if let Some(store) = &self.session.store {
            for path in &p.targets {
                if add {
                    store.add_to_shelf(path, name);
                } else {
                    store.remove_from_shelf(path, name);
                }
            }
        }
        *member = add;
    }

    /// Rebuild the picker's shelf list after a new collection is created, then
    /// leave creating mode with the cursor on the new entry.
    fn refresh_shelf_picker(&mut self) {
        let targets = match &self.overlay {
            Overlay::ShelfPicker(p) => p.targets.clone(),
            _ => return,
        };
        let shelves = self.shelf_membership(&targets);
        if let Overlay::ShelfPicker(p) = &mut self.overlay {
            p.shelves = shelves;
            p.new_name = None;
            p.sel = p.sel.min(p.new_row());
        }
    }
}
