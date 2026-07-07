//! Modal overlay key handlers: the image viewer, the in-book search prompt, and
//! the notes/annotations overlay. Each consumes a `KeyEvent` while its overlay is
//! open; routing only — the heavy lifting lives in the concern modules.

use super::super::*;
use crate::ui::TextInput;
use crossterm::event::KeyModifiers;

impl App {
    /// Open the image viewer on the current chapter's figures, selecting the one
    /// nearest the current reading position.
    pub(super) fn open_images(&mut self) {
        let (Some(_picker), Some(reader)) = (self.picker.as_ref(), self.reader.as_mut()) else {
            return;
        };
        let current = reader.current_image_index();
        let figs = reader.figures(false);
        let mut viewer = ImageViewer::new(figs, false);
        if let (Some(v), Some(idx)) = (viewer.as_mut(), current) {
            v.select_image(idx);
        }
        self.overlay = match viewer {
            Some(v) => Overlay::ImageView(v),
            None => Overlay::None,
        };
    }

    /// Rebuild the viewer toggling between current-chapter and whole-book scope.
    fn toggle_image_scope(&mut self) {
        let Overlay::ImageView(v) = &self.overlay else {
            return;
        };
        let whole = !v.whole_book;
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let figs = reader.figures(whole);
        // Keep the viewer open even if the new scope is empty (shouldn't be).
        let Some(new_viewer) = ImageViewer::new(figs, whole) else {
            return;
        };
        // Free the outgoing viewer's terminal image before it's replaced.
        self.retire_image_viewer();
        self.overlay = Overlay::ImageView(new_viewer);
    }

    /// Free the open image viewer's last shown terminal image (before the viewer
    /// is dropped or replaced), queuing its id for the app's delete stream so the
    /// resident Kitty image isn't leaked. No-op when the viewer isn't open.
    fn retire_image_viewer(&mut self) {
        if let Overlay::ImageView(v) = &mut self.overlay {
            v.close();
            let deletes = v.take_deletes();
            self.overlay_image_deletes.extend(deletes);
        }
    }

    pub(super) fn image_key(&mut self, key: KeyEvent) {
        // Filter-typing mode captures every key.
        if matches!(&self.overlay, Overlay::ImageView(v) if v.filtering) {
            if let Overlay::ImageView(v) = &mut self.overlay {
                match key.code {
                    KeyCode::Esc => {
                        v.filtering = false;
                        v.set_filter(String::new());
                    }
                    KeyCode::Enter => v.filtering = false,
                    KeyCode::Backspace => {
                        let mut f = v.filter.clone();
                        f.pop();
                        v.set_filter(f);
                    }
                    KeyCode::Char(c) => {
                        let mut f = v.filter.clone();
                        f.push(c);
                        v.set_filter(f);
                    }
                    _ => {}
                }
            }
            return;
        }
        // Save-path editing mode captures every key.
        if matches!(&self.overlay, Overlay::ImageView(v) if v.saving) {
            if let Overlay::ImageView(v) = &mut self.overlay {
                match key.code {
                    KeyCode::Esc => v.saving = false,
                    KeyCode::Enter => {
                        let path = v.save_path.clone();
                        let msg = v.save_to(&path);
                        v.saving = false;
                        v.flash = Some(msg);
                    }
                    KeyCode::Backspace => {
                        v.save_path.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        v.save_path.clear();
                    }
                    KeyCode::Char(c) => v.save_path.push(c),
                    _ => {}
                }
            }
            return;
        }
        // Clear any transient flash (e.g. "saved …") on the next key.
        if let Overlay::ImageView(v) = &mut self.overlay {
            v.flash = None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') => {
                self.retire_image_viewer();
                self.overlay = Overlay::None;
            }
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Char('n') => {
                if let Overlay::ImageView(v) = &mut self.overlay {
                    v.move_sel(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up | KeyCode::Char('N') => {
                if let Overlay::ImageView(v) = &mut self.overlay {
                    v.move_sel(-1);
                }
            }
            KeyCode::Char('/') => {
                if let Overlay::ImageView(v) = &mut self.overlay {
                    v.filtering = true;
                }
            }
            // Save: open an editable path prompt prefilled with the default dir.
            KeyCode::Char('s') => {
                if let Overlay::ImageView(v) = &mut self.overlay {
                    v.save_path = v.default_save_path();
                    v.saving = true;
                }
            }
            // Copy the figure to the system clipboard.
            KeyCode::Char('c') => {
                let img = if let Overlay::ImageView(v) = &self.overlay {
                    v.current_rgba()
                } else {
                    None
                };
                if let Some(rgba) = img {
                    self.pending_clipboard_image = Some(rgba);
                    if let Overlay::ImageView(v) = &mut self.overlay {
                        v.flash = Some("copied to clipboard".into());
                    }
                }
            }
            KeyCode::Char('w') => self.toggle_image_scope(),
            // Cycle the image mode (faithful / invert / auto) — applies live and
            // persists (it's the global image preference).
            KeyCode::Char('m') => {
                self.config.image_mode = self.config.image_mode.next();
                self.config.save();
            }
            // Jump to the figure's place in the book, then close the viewer.
            KeyCode::Enter | KeyCode::Char('l') => {
                let target = if let Overlay::ImageView(v) = &self.overlay {
                    v.current().map(|fig| (fig.section, fig.image_index))
                } else {
                    None
                };
                if let Some((section, image_index)) = target {
                    if let Some(r) = self.reader.as_mut() {
                        r.jump_to_image(section, image_index);
                    }
                    self.retire_image_viewer();
                    self.overlay = Overlay::None;
                }
            }
            _ => {}
        }
    }

    pub(super) fn prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            // Cancelling an edit reopens the annotations overlay it was raised from
            // (keeping the cursor on the entry); a new-note prompt just returns to
            // the reader, dropping the un-saved note.
            KeyCode::Esc => {
                if let Overlay::Prompt(p) = &self.overlay {
                    match p.kind {
                        PromptKind::Name(id)
                        | PromptKind::Folder(id)
                        | PromptKind::EditNote(id) => self.refresh_bookmarks(id),
                        PromptKind::NewNote { .. } => self.overlay = Overlay::None,
                    }
                } else {
                    self.overlay = Overlay::None;
                }
            }
            KeyCode::Enter => self.prompt_commit(),
            _ => {
                if let Overlay::Prompt(p) = &mut self.overlay {
                    p.input.handle_key(key);
                }
            }
        }
    }

    /// Apply the committed prompt text: rename / file / edit-note reopen the
    /// annotations overlay; a new note is saved and control returns to the reader.
    fn prompt_commit(&mut self) {
        let Overlay::Prompt(p) = std::mem::replace(&mut self.overlay, Overlay::None) else {
            return;
        };
        let text = p.input.text().trim().to_string();
        match p.kind {
            PromptKind::Name(id) => {
                if let Some(store) = &self.session.store {
                    store.set_annotation_name(id, &text);
                }
                self.refresh_bookmarks(id);
            }
            PromptKind::Folder(id) => {
                if let Some(store) = &self.session.store {
                    store.set_annotation_folder(id, &text);
                }
                self.refresh_bookmarks(id);
            }
            PromptKind::EditNote(id) => {
                if let Some(store) = &self.session.store {
                    store.set_annotation_note(id, &text);
                }
                self.refresh_bookmarks(id);
            }
            PromptKind::NewNote { section, quote } => {
                // An empty body cancels — no blank notes.
                if !text.is_empty() {
                    if let Some(store) = &self.session.store
                        && !self.session.book_path.is_empty()
                    {
                        store.add_note(&self.session.book_path, section, &quote, &text);
                    }
                    self.sync_reader_bookmarks();
                    if let Some(r) = self.reader.as_mut() {
                        r.flash = Some("note added".into());
                    }
                }
                self.overlay = Overlay::None;
            }
        }
    }

    /// (Re)open the bookmarks overlay from the store, keeping the cursor on
    /// bookmark `keep_id` (whose position may have shifted when its folder
    /// changed). Used after a prompt commits/cancels to restore the list it was
    /// raised from.
    fn refresh_bookmarks(&mut self, keep_id: i64) {
        let Some(store) = &self.session.store else {
            self.overlay = Overlay::None;
            return;
        };
        let items = store.list_annotations(&self.session.book_path);
        // Reopen on the tab the edited annotation lives on, cursor kept on it.
        let tab = items
            .iter()
            .find(|i| i.id == keep_id)
            .map(|i| {
                if i.is_note() {
                    AnnotTab::Notes
                } else {
                    AnnotTab::Bookmarks
                }
            })
            .unwrap_or(AnnotTab::Bookmarks);
        let sel = items
            .iter()
            .filter(|i| i.is_note() == matches!(tab, AnnotTab::Notes))
            .position(|i| i.id == keep_id)
            .unwrap_or(0);
        self.overlay = Overlay::Annot(AnnotState {
            items,
            tab,
            sel,
            filter: String::new(),
            filtering: false,
        });
        self.sync_reader_bookmarks();
    }

    /// Jump to the currently selected annotation and close the overlay — shared by
    /// the Enter/`l` key and a mouse double-click on a row.
    pub(crate) fn annot_jump_selected(&mut self) {
        let target = if let Overlay::Annot(a) = &self.overlay {
            a.selected().map(|i| (i.section, i.quote))
        } else {
            None
        };
        if let Some((section, quote)) = target {
            if let Some(r) = self.reader.as_mut() {
                r.jump_to(section, Some(&quote));
            }
            self.overlay = Overlay::None;
        }
    }

    pub(super) fn annot_key(&mut self, key: KeyEvent) {
        let Overlay::Annot(a) = &self.overlay else {
            return;
        };

        // While typing a filter, printable keys edit it; arrows still navigate.
        if a.filtering {
            if let Overlay::Annot(a) = &mut self.overlay {
                match key.code {
                    KeyCode::Esc => {
                        a.filter.clear();
                        a.filtering = false;
                        a.sel = 0;
                    }
                    KeyCode::Enter => a.filtering = false,
                    KeyCode::Backspace => {
                        a.filter.pop();
                        a.sel = 0;
                    }
                    KeyCode::Char(c) => {
                        a.filter.push(c);
                        a.sel = 0;
                    }
                    KeyCode::Up => a.sel = a.sel.saturating_sub(1),
                    KeyCode::Down => {
                        let n = a.filtered().len();
                        if n > 0 {
                            a.sel = (a.sel + 1).min(n - 1);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        let (len, sel) = (a.filtered().len(), a.sel);
        match key.code {
            KeyCode::Esc | KeyCode::Char('\'') | KeyCode::Char('q') => self.overlay = Overlay::None,
            // Switch between the Bookmarks and Notes tabs (Tab or ← / →).
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                if let Overlay::Annot(a) = &mut self.overlay {
                    a.tab = a.tab.toggled();
                    a.sel = 0;
                    a.filter.clear();
                }
            }
            KeyCode::Char('/') => {
                if let Overlay::Annot(a) = &mut self.overlay {
                    a.filtering = true;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if let Overlay::Annot(a) = &mut self.overlay
                    && len > 0
                {
                    a.sel = (sel + 1).min(len - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Overlay::Annot(a) = &mut self.overlay {
                    a.sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Enter | KeyCode::Char('l') => self.annot_jump_selected(),
            // Name (or rename) the selected entry; prefilled with its current name.
            KeyCode::Char('r') => {
                let target = if let Overlay::Annot(a) = &self.overlay {
                    a.selected().map(|i| (i.id, i.name))
                } else {
                    None
                };
                if let Some((id, name)) = target {
                    self.overlay = Overlay::Prompt(Prompt {
                        kind: PromptKind::Name(id),
                        input: TextInput::from(name),
                    });
                }
            }
            // File the selected entry into a folder; prefilled with its current one.
            // (`f` is the window-resize key, so folder-filing is `F`.)
            KeyCode::Char('F') => {
                let target = if let Overlay::Annot(a) = &self.overlay {
                    a.selected().map(|i| (i.id, i.folder))
                } else {
                    None
                };
                if let Some((id, folder)) = target {
                    self.overlay = Overlay::Prompt(Prompt {
                        kind: PromptKind::Folder(id),
                        input: TextInput::from(folder),
                    });
                }
            }
            // Edit a note's commentary; prefilled with the current body. Bookmarks
            // carry no commentary, so `e` on one is a no-op.
            KeyCode::Char('e') => {
                let target = if let Overlay::Annot(a) = &self.overlay {
                    a.selected().filter(|i| i.is_note()).map(|i| (i.id, i.note))
                } else {
                    None
                };
                if let Some((id, note)) = target {
                    self.overlay = Overlay::Prompt(Prompt {
                        kind: PromptKind::EditNote(id),
                        input: TextInput::from(note),
                    });
                }
            }
            KeyCode::Char('d') => {
                let id = if let Overlay::Annot(a) = &self.overlay {
                    a.selected().map(|i| i.id)
                } else {
                    None
                };
                if let (Some(id), Some(store)) = (id, &self.session.store) {
                    store.delete_annotation(id);
                    let items = store.list_annotations(&self.session.book_path);
                    if let Overlay::Annot(a) = &mut self.overlay {
                        a.items = items;
                        let m = a.filtered().len();
                        if a.sel >= m {
                            a.sel = m.saturating_sub(1);
                        }
                    }
                    self.sync_reader_bookmarks();
                }
            }
            _ => {}
        }
    }

    /// Push the open book's annotations down to the reader so it can mark their
    /// lines in the gutter. Cheap; call after any add/delete/move and on open.
    pub(crate) fn sync_reader_bookmarks(&mut self) {
        if let (Some(store), Some(r)) = (&self.session.store, self.reader.as_mut()) {
            let marks = store
                .list_annotations(&self.session.book_path)
                .into_iter()
                .map(|a| {
                    let is_note = a.is_note();
                    (a.section, a.quote, is_note)
                })
                .collect();
            r.set_annotations(marks);
        }
    }

    pub(super) fn search_key(&mut self, key: KeyEvent) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                reader.search.searching = false;
                reader.search.input.clear();
            }
            KeyCode::Enter => reader.run_search(),
            KeyCode::Tab => reader.cycle_search_mode(),
            KeyCode::Up => reader.search_history_recall(-1),
            KeyCode::Down => reader.search_history_recall(1),
            KeyCode::Backspace => {
                reader.search.history_pos = None;
                reader.search.input.pop();
            }
            KeyCode::Char(c) => {
                reader.search.history_pos = None;
                reader.search.input.push(c);
            }
            _ => {}
        }
    }
}
