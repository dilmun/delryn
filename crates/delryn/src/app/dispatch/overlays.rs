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
        if let Some(reader) = self.reader.as_mut() {
            let figs = reader.figures(whole);
            // Keep the viewer open even if the new scope is empty (shouldn't be).
            if let Some(v) = ImageViewer::new(figs, whole) {
                self.overlay = Overlay::ImageView(v);
            }
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') => self.overlay = Overlay::None,
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
                    self.overlay = Overlay::None;
                }
            }
            _ => {}
        }
    }

    pub(super) fn prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            // Cancelling reopens the bookmarks overlay the prompt was raised from
            // (keeping the cursor on the entry being edited), leaving it unchanged.
            KeyCode::Esc => {
                if let Overlay::Prompt(p) = &self.overlay {
                    let id = match p.kind {
                        PromptKind::Name(id) | PromptKind::Folder(id) => id,
                    };
                    self.refresh_bookmarks(id);
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

    /// Apply the committed prompt text to its bookmark (rename / file), then
    /// reopen the bookmarks overlay (the prompt is always raised from it).
    fn prompt_commit(&mut self) {
        let Overlay::Prompt(p) = std::mem::replace(&mut self.overlay, Overlay::None) else {
            return;
        };
        let text = p.input.text().trim().to_string();
        let id = match p.kind {
            PromptKind::Name(id) => {
                if let Some(store) = &self.session.store {
                    store.set_annotation_name(id, &text);
                }
                id
            }
            PromptKind::Folder(id) => {
                if let Some(store) = &self.session.store {
                    store.set_annotation_folder(id, &text);
                }
                id
            }
        };
        self.refresh_bookmarks(id);
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
        let items = store.list_bookmarks(&self.session.book_path);
        let sel = items
            .iter()
            .position(|i| i.id == keep_id)
            .unwrap_or_else(|| items.len().saturating_sub(1));
        self.overlay = Overlay::Annot(AnnotState { items, sel });
        self.sync_reader_bookmarks();
    }

    pub(super) fn annot_key(&mut self, key: KeyEvent) {
        let Overlay::Annot(a) = &self.overlay else {
            return;
        };
        let (len, sel) = (a.items.len(), a.sel);
        match key.code {
            KeyCode::Esc | KeyCode::Char('\'') | KeyCode::Char('q') => self.overlay = Overlay::None,
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
            KeyCode::Enter | KeyCode::Char('l') => {
                let target = if let Overlay::Annot(a) = &self.overlay {
                    a.items.get(a.sel).map(|i| (i.section, i.quote.clone()))
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
            // Name (or rename) the selected entry; prefilled with its current name.
            KeyCode::Char('r') => {
                let target = if let Overlay::Annot(a) = &self.overlay {
                    a.items.get(a.sel).map(|i| (i.id, i.name.clone()))
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
            KeyCode::Char('f') => {
                let target = if let Overlay::Annot(a) = &self.overlay {
                    a.items.get(a.sel).map(|i| (i.id, i.folder.clone()))
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
            KeyCode::Char('d') => {
                let id = if let Overlay::Annot(a) = &self.overlay {
                    a.items.get(a.sel).map(|i| i.id)
                } else {
                    None
                };
                if let (Some(id), Some(store)) = (id, &self.session.store) {
                    store.delete_annotation(id);
                    let items = store.list_bookmarks(&self.session.book_path);
                    if let Overlay::Annot(a) = &mut self.overlay {
                        a.items = items;
                        if a.sel >= a.items.len() {
                            a.sel = a.items.len().saturating_sub(1);
                        }
                    }
                    self.sync_reader_bookmarks();
                }
            }
            _ => {}
        }
    }

    /// Push the open book's bookmarks down to the reader so it can mark their
    /// lines in the gutter. Cheap; call after any add/delete/move and on open.
    pub(crate) fn sync_reader_bookmarks(&mut self) {
        if let (Some(store), Some(r)) = (&self.session.store, self.reader.as_mut()) {
            let marks = store
                .list_bookmarks(&self.session.book_path)
                .into_iter()
                .map(|a| (a.section, a.quote))
                .collect();
            r.set_bookmarks(marks);
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
