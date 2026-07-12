//! Modal overlay key handlers: the image viewer, the in-book search prompt, and
//! the notes/annotations overlay. Each consumes a `KeyEvent` while its overlay is
//! open; routing only — the heavy lifting lives in the concern modules.

use super::super::*;
use crate::HighlightColor;
use crate::ui::TextInput;
use crossterm::event::KeyModifiers;

impl App {
    /// `I` opens the figure pick-mode (the image analogue of `F` for folds): with
    /// one figure in view it opens the viewer on it, with several it badges them and
    /// awaits a digit (see [`App::hint_key`]).
    pub(super) fn open_images(&mut self) {
        if self.picker.is_none() {
            return;
        }
        let outcome = match self.reader.as_mut() {
            Some(r) => r.hint_start(HintKind::Image),
            None => return,
        };
        match outcome {
            HintStart::None => self.set_reader_flash("no image in view"),
            HintStart::Single(idx) => self.open_image_at(idx),
            HintStart::Entered(n) => self.set_reader_flash(&format!("image: press 1–{n} · Esc")),
        }
    }

    /// Open the image viewer on the current chapter's figures, selecting figure
    /// `image_index`. Shared by the single-figure fast path and a badge pick.
    fn open_image_at(&mut self, image_index: usize) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let figs = reader.figures(false);
        let mut viewer = ImageViewer::new(figs, false);
        if let Some(v) = viewer.as_mut() {
            v.select_image(image_index);
        }
        self.overlay = match viewer {
            Some(v) => Overlay::ImageView(v),
            None => Overlay::None,
        };
    }

    /// Number-badge pick-mode key (`F`/`I`): a `1..=9` digit acts on that element —
    /// toggles a fold or opens the figure — anything else cancels. Digits are
    /// consumed here so they never feed the vim count prefix. See [`Reader::hint`].
    pub(super) fn hint_key(&mut self, key: KeyEvent) {
        let picked = match key.code {
            KeyCode::Char(c @ '1'..='9') => {
                let n = c.to_digit(10).unwrap() as usize;
                self.reader.as_mut().and_then(|r| r.hint_pick(n))
            }
            _ => {
                // Esc or any other key cancels the pick.
                if let Some(r) = self.reader.as_mut() {
                    r.hint_cancel();
                    r.flash = None;
                }
                return;
            }
        };
        match picked {
            Some((HintKind::Code, idx)) => {
                if let Some(r) = self.reader.as_mut() {
                    let msg = r.toggle_fold_at(idx);
                    r.flash = Some(msg);
                }
            }
            Some((HintKind::Image, idx)) => self.open_image_at(idx),
            None => {} // out-of-range digit: stay in the mode
        }
    }

    /// Set the reader's transient flash (no-op outside the reader).
    fn set_reader_flash(&mut self, msg: &str) {
        if let Some(r) = self.reader.as_mut() {
            r.flash = Some(msg.to_string());
        }
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

    /// Close the image viewer and return to the reader (retire its terminal image,
    /// then drop the overlay).
    fn close_image_viewer(&mut self) {
        self.retire_image_viewer();
        self.overlay = Overlay::None;
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
        // Standard vim list navigation (j/k · Ctrl-d/u · PgUp/Dn · g/G) over the
        // figures — shared with every other list.
        if let Overlay::ImageView(v) = &mut self.overlay
            && let Some(ns) = crate::input::list_nav(key, v.sel, v.position().1, 10)
        {
            v.sel = ns;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') => self.close_image_viewer(),
            KeyCode::Char('n') => {
                if let Overlay::ImageView(v) = &mut self.overlay {
                    v.move_sel(1);
                }
            }
            KeyCode::Char('N') => {
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
            KeyCode::Enter | KeyCode::Char('l') => self.image_go_selected(),
            _ => {}
        }
    }

    /// Jump to the selected figure's place in the book and close the viewer
    /// (Enter / `l` / double-click).
    pub(crate) fn image_go_selected(&mut self) {
        let target = if let Overlay::ImageView(v) = &self.overlay {
            v.current().map(|fig| (fig.section, fig.image_index))
        } else {
            None
        };
        if let Some((section, image_index)) = target {
            if let Some(r) = self.reader.as_mut() {
                r.jump_to_image(section, image_index);
                // The viewer's full-screen figure can evict the destination section's
                // inline images from the terminal; force them to rebuild with fresh
                // ids so they re-transmit on arrival instead of showing blank until a
                // stray keypress churns the cache.
                r.restage_visible_images();
            }
            self.close_image_viewer();
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
            .map(AnnotTab::of)
            .unwrap_or(AnnotTab::Bookmarks);
        let sel = items
            .iter()
            .filter(|i| AnnotTab::of(i) == tab)
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
        // Standard vim list navigation over the annotations.
        if let Some(ns) = crate::input::list_nav(key, sel, len, 10) {
            if let Overlay::Annot(a) = &mut self.overlay {
                a.sel = ns;
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('\'') | KeyCode::Char('q') => self.overlay = Overlay::None,
            // Cycle the Bookmarks / Notes / Highlights tabs (⇥ / → forward, ⇤ / ←
            // back).
            KeyCode::Tab | KeyCode::Right => {
                if let Overlay::Annot(a) = &mut self.overlay {
                    a.tab = a.tab.next();
                    a.sel = 0;
                    a.filter.clear();
                }
            }
            KeyCode::BackTab | KeyCode::Left => {
                if let Overlay::Annot(a) = &mut self.overlay {
                    a.tab = a.tab.prev();
                    a.sel = 0;
                    a.filter.clear();
                }
            }
            KeyCode::Char('/') => {
                if let Overlay::Annot(a) = &mut self.overlay {
                    a.filtering = true;
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
            r.set_annotations(store.list_annotations(&self.session.book_path));
        }
    }

    /// Handle a key in cursor/selection mode. Motions move the caret (either page
    /// of a spread); `v`/Space drops or lifts the selection anchor. Annotations act
    /// at the caret — `m` bookmark, `H` highlight the caret line — and, once
    /// selecting, on the range: `y` copy, `1`-`5`/`H` highlight, `a` note.
    /// `Esc`/`V` leave the mode.
    pub(super) fn visual_key(&mut self, key: KeyEvent) {
        let selecting = self
            .reader
            .as_ref()
            .is_some_and(Reader::selection_selecting);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('V') => {
                if let Some(r) = self.reader.as_mut() {
                    r.cancel_selection();
                }
            }
            // Half-page caret navigation (extends the range while selecting).
            KeyCode::Char('d') if ctrl => self.selection_motion(Reader::selection_half_down),
            KeyCode::Char('u') if ctrl => self.selection_motion(Reader::selection_half_up),
            // Start selecting from the caret (or lift the anchor to move freely).
            KeyCode::Char('v') | KeyCode::Char(' ') => {
                if let Some(r) = self.reader.as_mut() {
                    r.toggle_selection_anchor();
                }
            }
            KeyCode::Char('h') | KeyCode::Left => self.selection_motion(Reader::selection_left),
            KeyCode::Char('l') | KeyCode::Right => self.selection_motion(Reader::selection_right),
            KeyCode::Char('j') | KeyCode::Down => self.selection_motion(Reader::selection_down),
            KeyCode::Char('k') | KeyCode::Up => self.selection_motion(Reader::selection_up),
            KeyCode::Char('w') => self.selection_motion(Reader::selection_word_forward),
            KeyCode::Char('b') => self.selection_motion(Reader::selection_word_back),
            KeyCode::Char('0') | KeyCode::Home => {
                self.selection_motion(Reader::selection_line_start)
            }
            KeyCode::Char('$') | KeyCode::End => self.selection_motion(Reader::selection_line_end),
            // Bookmark the caret's line (the cursor-aware `current_quote` anchors
            // there, so it works on either page of a spread). Stays in cursor mode.
            KeyCode::Char('m') => self.apply(Action::AddBookmark),
            KeyCode::Char('y') if selecting => {
                if let Some(r) = self.reader.as_mut() {
                    r.copy_selection();
                }
            }
            // Highlight the selected range, else cycle the caret line's highlight.
            KeyCode::Char('H') => {
                if selecting {
                    self.highlight_selection(HighlightColor::ALL[0]);
                } else {
                    self.apply(Action::AddHighlight);
                }
            }
            KeyCode::Char(c) if selecting && ('1'..='5').contains(&c) => {
                self.highlight_selection(HighlightColor::ALL[c as usize - '1' as usize]);
            }
            // Look up the selected phrase, else the word under the caret (vim `K`),
            // in the dictionary + Wikipedia panel. Leaves the selection intact.
            KeyCode::Char('K') => {
                let term = self.reader.as_ref().map(|r| {
                    if selecting {
                        r.selection_text()
                    } else {
                        r.word_at_caret()
                    }
                });
                if let Some(term) = term {
                    self.open_word_lookup(term);
                }
            }
            // Note on the selection, else on the caret's line (then leave the mode,
            // handing over to the commentary prompt).
            KeyCode::Char('a') => {
                if selecting {
                    self.note_on_selection();
                } else {
                    self.apply(Action::AddNote);
                    if let Some(r) = self.reader.as_mut() {
                        r.cancel_selection();
                    }
                }
            }
            _ => {}
        }
    }

    /// Run a caret motion on the open reader (no-op if none is open).
    fn selection_motion(&mut self, motion: fn(&mut Reader)) {
        if let Some(r) = self.reader.as_mut() {
            motion(r);
        }
    }

    /// Highlight the current selection in `color`, then leave visual mode.
    fn highlight_selection(&mut self, color: HighlightColor) {
        let Some((section, quote)) = self
            .reader
            .as_ref()
            .map(|r| (r.section, r.selection_text()))
        else {
            return;
        };
        if let Some(r) = self.reader.as_mut() {
            r.cancel_selection();
        }
        if quote.is_empty() || self.session.book_path.is_empty() {
            return;
        }
        if let Some(store) = &self.session.store {
            store.add_highlight(&self.session.book_path, section, &quote, color.index());
        }
        if let Some(r) = self.reader.as_mut() {
            r.flash = Some(format!("highlight: {}", color.label()));
        }
        self.sync_reader_bookmarks();
    }

    /// Open the note prompt anchored to the current selection, then leave visual
    /// mode (the note is saved on commit — see [`Self::prompt_commit`]).
    fn note_on_selection(&mut self) {
        let Some((section, quote)) = self
            .reader
            .as_ref()
            .map(|r| (r.section, r.selection_text()))
        else {
            return;
        };
        if let Some(r) = self.reader.as_mut() {
            r.cancel_selection();
        }
        if quote.is_empty() || self.session.book_path.is_empty() {
            return;
        }
        self.overlay = Overlay::Prompt(Prompt {
            kind: PromptKind::NewNote { section, quote },
            input: TextInput::from(String::new()),
        });
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
