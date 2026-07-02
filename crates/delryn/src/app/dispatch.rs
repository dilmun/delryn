//! Input dispatch: the central key router (`on_key`), the modal overlay key
//! handlers (images, notes, annotations, in-book search prompt), and the
//! `Action` dispatcher (`apply`). Routing only — the work lives in the concern
//! modules; child of `app`, so it calls their methods directly.

use crossterm::event::KeyModifiers;

use super::*;
use crate::config::{Config, ViewMode};
use crate::ui::TextInput;

/// The config knobs that change how a section wraps or how wide the reading
/// measure is. When any of them changes (a view-mode switch, a reading preset, a
/// width/spacing tweak), the section re-wraps, so the reader preserves its
/// reading position across the change via [`Reader::hold_reflow_position`].
fn reflow_key(c: &Config) -> (ViewMode, u16, u16, u8, u8, bool, bool, bool, bool) {
    (
        c.view_mode,
        c.side_padding,
        c.page_gap,
        c.line_spacing,
        c.paragraph_spacing,
        c.justify,
        c.tidy_spacing,
        c.code_wrap,
        c.table_wrap,
    )
}

impl App {
    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // A pending yes/no confirmation is modal: it answers before any popup.
        if self.pending_confirm.is_some() {
            self.confirm_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::Settings(_)) {
            self.settings_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::Prompt(_)) {
            self.prompt_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::MetaEdit(_)) {
            self.meta_edit_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::BulkRename(_)) {
            self.bulk_rename_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::CollEdit(_)) {
            self.lib_coll_edit_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::TagEdit(_)) {
            self.tag_edit_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::DupResolve(_)) {
            self.dup_resolve_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::IgnoredView(_)) {
            self.ignored_view_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::ShelfPicker(_)) {
            self.shelf_picker_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::ImageView(_)) {
            self.image_key(key);
            return;
        }
        if matches!(self.overlay, Overlay::Annot(_)) {
            self.annot_key(key);
            return;
        }
        // The stats overlay is read-only: any key dismisses it.
        if matches!(self.overlay, Overlay::Stats(_)) {
            self.overlay = Overlay::None;
            return;
        }
        if matches!(self.overlay, Overlay::Palette(_)) {
            self.palette_key(key);
            return;
        }
        // The in-book search prompt is a focused text input: it must capture
        // every key (including shortcut letters like 'i' / ';' / ':') before any
        // global shortcut below gets a chance to fire.
        if self.mode == Mode::Reader && self.reader.as_ref().is_some_and(|r| r.search.searching) {
            self.search_key(key);
            return;
        }
        // ':' opens the command palette in the library.
        if self.mode == Mode::Library && key.code == KeyCode::Char(':') {
            self.open_palette();
            return;
        }
        if self.mode == Mode::Reader && key.code == KeyCode::Char('i') {
            self.open_images();
            return;
        }
        if key.code == KeyCode::Char(';') {
            let scope = self.mode;
            self.overlay = Overlay::Settings(Settings {
                scope,
                tab: 0,
                row: first_setting_row(scope, 0),
            });
            return;
        }
        match self.mode {
            Mode::Reader => {
                // Clear any transient flash message on the next keypress.
                if let Some(r) = self.reader.as_mut() {
                    r.flash = None;
                }
                // While previewing a book from the duplicate resolver, Esc also
                // returns (in normal reading Esc clears the selection anchor).
                let action = if self.dup_preview.is_some() && key.code == KeyCode::Esc {
                    input::Action::Back
                } else {
                    input::map_key(key, &mut self.pending)
                };
                self.apply(action);
                // An activated external link asks for confirmation before opening.
                if let Some(url) = self.reader.as_mut().and_then(|r| r.take_pending_open()) {
                    let shown = crate::view::truncate(&url, 60);
                    self.ask_confirm(
                        &format!("Open in browser: {shown}?"),
                        super::confirm::ConfirmAction::OpenUrl(url),
                    );
                }
                // Returning to the library (Back) should reflect the latest state —
                // and restore the duplicate overlay if this was a preview.
                if self.mode == Mode::Library {
                    if let Some(dr) = self.dup_preview.take() {
                        self.overlay = Overlay::DupResolve(dr);
                    }
                    self.refresh_library();
                }
            }
            Mode::Library => {
                // Clear any transient flash (e.g. cover-embed result) on input.
                self.library.flash = None;
                self.library_key(key);
            }
        }
    }

    /// Open the image viewer on the current chapter's figures, selecting the one
    /// nearest the current reading position.
    fn open_images(&mut self) {
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

    fn image_key(&mut self, key: KeyEvent) {
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

    fn prompt_key(&mut self, key: KeyEvent) {
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

    fn annot_key(&mut self, key: KeyEvent) {
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

    fn search_key(&mut self, key: KeyEvent) {
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

    fn apply(&mut self, action: Action) {
        // Throttle PDF flips to the display rate (see `pdf_flip_ready`): a held
        // key advances one visible page per drawn frame rather than skipping.
        let flip_ready = self.pdf_flip_ready();
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let before = reader.section;
        let mut save = false;
        // Whole-page rasters (PDF) always navigate by page flip, never eased
        // line-scroll: easing re-renders the full-page image every frame, which
        // blanks/flickers it. So page-snap whenever paged mode is on *or* the
        // document is page-image-based, regardless of the continuous-scroll knob.
        let paged = self.config.paged || reader.is_paged_image();
        // Snapshot the wrap-affecting settings so we can preserve the reading
        // position if this action re-wraps the text (see below).
        let wrap_before = reflow_key(&self.config);
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back => {
                // Accumulate reading time for the session before leaving.
                if let (Some(start), Some(store)) = (self.session.started, &self.session.store) {
                    let secs = start.elapsed().as_secs() as i64;
                    if secs > 0 && !self.session.book_path.is_empty() {
                        store.add_read_time(&self.session.book_path, secs);
                    }
                }
                self.session.started = Some(Instant::now());
                self.mode = Mode::Library;
                save = true;
            }
            // Reader navigation (scroll / half- and full-page / top-bottom / goto).
            Action::Down(_)
            | Action::Up(_)
            | Action::HalfDown
            | Action::HalfUp
            | Action::PageDown
            | Action::PageUp
            | Action::Top
            | Action::Bottom
            | Action::Goto(_) => apply_nav(reader, action, paged, flip_ready),
            Action::ToggleStatus => self.config.show_status = !self.config.show_status,
            Action::CycleView => {
                self.config.view_mode = self.config.view_mode.next();
                save = true;
            }
            Action::CycleTheme => {
                // Theme is global: cycling it (here or in the library) recolours
                // every book and persists immediately to config — never per-book.
                self.config.theme = self.config.theme.next();
                self.config.save();
            }
            Action::CycleReadingMode => {
                let mode = self.config.reading_mode().next();
                self.config.apply_reading_mode(mode);
                reader.flash = Some(format!("mode: {}", mode.label()));
                save = true;
            }
            Action::ToggleFocus => self.config.focus_mode = !self.config.focus_mode,
            // `]` widens the text (less margin), `[` narrows it (more margin).
            Action::WidthUp => {
                self.config.side_padding = self.config.side_padding.saturating_sub(1);
            }
            Action::WidthDown => {
                self.config.side_padding =
                    (self.config.side_padding + 1).min(crate::config::MAX_SIDE_PADDING);
            }
            Action::LineSpacingDown => {
                self.config.line_spacing = self.config.line_spacing.saturating_sub(1);
            }
            Action::LineSpacingUp => {
                self.config.line_spacing =
                    (self.config.line_spacing + 1).min(crate::config::MAX_LINE_SPACING);
            }
            Action::ToggleSidebar => {
                self.config.show_sidebar = !self.config.show_sidebar;
                if !self.config.show_sidebar {
                    reader.focus = Focus::Content;
                }
            }
            Action::FocusToggle => {
                // Tab moves focus into the sidebar (showing it first if hidden),
                // then back to the content. Entering the sidebar, start the
                // cursor at the entry tracking the current reading position.
                if !self.config.show_sidebar {
                    self.config.show_sidebar = true;
                    reader.focus = Focus::Sidebar;
                    reader.sidebar_sel = reader.active_outline_row().unwrap_or(0);
                    reader.center_sidebar();
                } else if reader.focus == Focus::Content {
                    reader.focus = Focus::Sidebar;
                    reader.sidebar_sel = reader.active_outline_row().unwrap_or(0);
                    reader.center_sidebar();
                } else {
                    reader.focus = Focus::Content;
                }
            }
            Action::Activate => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_activate();
                } else {
                    // In the text pane, Enter follows the link cursor's anchor.
                    reader.activate_anchor();
                }
            }
            Action::NextAnchor => reader.next_anchor(),
            Action::PrevAnchor => reader.prev_anchor(),
            Action::ClearAnchor => {
                reader.clear_anchor();
            }
            Action::Expand => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_expand();
                } else if reader.is_paged_image() && reader.can_pan_horizontally() {
                    reader.pan_right(1); // l pans a zoomed page right
                }
            }
            Action::Collapse => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_collapse();
                } else if reader.is_paged_image() && reader.can_pan_horizontally() {
                    reader.pan_left(1); // h pans a zoomed page left
                }
            }
            Action::ZoomIn | Action::ZoomOut | Action::ZoomReset | Action::FitCycle => {
                // Zoom / pan is a single-page paged (PDF) feature.
                if reader.is_paged_image() && self.config.view_mode == ViewMode::Center {
                    match action {
                        Action::ZoomIn => reader.zoom_in(),
                        Action::ZoomOut => reader.zoom_out(),
                        Action::ZoomReset => reader.zoom_reset(),
                        Action::FitCycle => reader.cycle_fit(),
                        _ => {}
                    }
                    reader.flash = Some(reader.page_view.label());
                } else if reader.is_paged_image() {
                    reader.flash = Some("zoom needs single-page view (v)".into());
                }
            }
            Action::HistBack => reader.history_back(),
            Action::HistForward => reader.history_forward(),
            Action::Search => reader.start_search(),
            Action::SearchNext => reader.search_next(),
            Action::SearchPrev => reader.search_prev(),
            Action::AddBookmark => {
                if let Some(store) = &self.session.store
                    && !self.session.book_path.is_empty()
                {
                    store.add_bookmark(
                        &self.session.book_path,
                        reader.section,
                        &reader.current_quote(),
                    );
                    reader.flash = Some("bookmark added".into());
                    // `reader` is borrowed here, so push the refreshed set directly
                    // rather than via the `&mut self` helper.
                    let marks = store
                        .list_bookmarks(&self.session.book_path)
                        .into_iter()
                        .map(|a| (a.section, a.quote))
                        .collect();
                    reader.set_bookmarks(marks);
                }
            }
            Action::OpenAnnotations => {
                if let Some(store) = &self.session.store {
                    let items = store.list_bookmarks(&self.session.book_path);
                    self.overlay = Overlay::Annot(AnnotState { items, sel: 0 });
                }
            }
            Action::CopyCode => {
                reader.copy_visible_code();
            }
            Action::ToggleCodeWrap => {
                self.config.code_wrap = !self.config.code_wrap;
                reader.code_hscroll = 0;
                reader.flash = Some(
                    if self.config.code_wrap {
                        "code: wrap"
                    } else {
                        "code: no-wrap (< > to pan)"
                    }
                    .to_string(),
                );
                save = true;
            }
            // Horizontal panning only applies to non-wrapped code.
            Action::PanLeft => {
                reader.code_hscroll = reader.code_hscroll.saturating_sub(8);
            }
            Action::PanRight => {
                if !self.config.code_wrap {
                    reader.code_hscroll = (reader.code_hscroll + 8).min(400);
                }
            }
            Action::ToggleChapterLock => {
                self.config.chapter_lock = !self.config.chapter_lock;
                reader.flash = Some(
                    if self.config.chapter_lock {
                        "chapter lock: on"
                    } else {
                        "chapter lock: off"
                    }
                    .to_string(),
                );
                save = true;
            }
            Action::TogglePaged => {
                self.config.paged = !self.config.paged;
                if self.config.paged {
                    reader.snap_to_page(); // start on a clean page boundary
                }
                reader.flash = Some(
                    if self.config.paged {
                        "page mode: on"
                    } else {
                        "page mode: off (continuous)"
                    }
                    .to_string(),
                );
                save = true;
            }
            Action::NextChapter => reader.next_chapter(),
            Action::PrevChapter => reader.prev_chapter(),
            Action::NextElement => {
                reader.next_element();
            }
            Action::PrevElement => {
                reader.prev_element();
            }
            Action::None => {}
        }

        // A page flip while zoomed: start the new page at the top (a forward flip)
        // or bottom (a backward flip) so vertical panning reads continuously.
        if reader.is_paged_image() && reader.section != before && reader.page_zoomed() {
            reader.reset_pan_to(reader.section > before);
        }

        // If this action changed a wrap-affecting setting (view mode, width,
        // spacing, preset), the section re-wraps next frame — anchor the reading
        // position so it stays put instead of drifting to a stale line offset.
        if reflow_key(&self.config) != wrap_before {
            reader.hold_reflow_position();
        }

        // Persist on chapter change or a settings change (cheap).
        if (save || reader.section != before)
            && let Some(store) = &self.session.store
            && !self.session.book_path.is_empty()
        {
            let _ = store.save_progress(
                &self.session.book_path,
                reader.section,
                reader.within_frac(),
                self.config.view_mode,
                self.config.theme.name,
            );
        }
    }
}

/// Reader navigation — scroll, half/full-page motion, top/bottom, and `Ng` jump.
/// In paged mode (or for page-image documents) vertical motion flips whole pages;
/// a held PDF flip is throttled to the drawn frame via `flip_ready`. Split out of
/// [`App::apply`] so its action dispatch stays a flat router.
fn apply_nav(reader: &mut Reader, action: Action, paged: bool, flip_ready: bool) {
    let page_forward = |r: &mut Reader| {
        if flip_ready {
            r.page_forward();
        }
    };
    let page_backward = |r: &mut Reader| {
        if flip_ready {
            r.page_backward();
        }
    };
    match action {
        // A count prefix (`10j`) jumps that many pages; a bare/held key flips one.
        // When the page is zoomed, pan down first and only flip at the bottom
        // edge (the new page starts at the top — reset centrally in `apply`).
        Action::Down(n) => match reader.focus {
            Focus::Content if paged => {
                if !reader.try_pan_down(n) {
                    if n > 1 {
                        reader.page_jump(n as isize);
                    } else {
                        page_forward(reader);
                    }
                }
            }
            Focus::Content => reader.queue_scroll(n as isize),
            Focus::Sidebar => reader.sidebar_move(n as isize),
        },
        Action::Up(n) => match reader.focus {
            Focus::Content if paged => {
                if !reader.try_pan_up(n) {
                    if n > 1 {
                        reader.page_jump(-(n as isize));
                    } else {
                        page_backward(reader);
                    }
                }
            }
            Focus::Content => reader.queue_scroll(-(n as isize)),
            Focus::Sidebar => reader.sidebar_move(-(n as isize)),
        },
        Action::HalfDown if paged => page_forward(reader),
        Action::HalfUp if paged => page_backward(reader),
        Action::HalfDown => reader.scroll_down(reader.page_lines.max(2) / 2),
        Action::HalfUp => reader.scroll_up(reader.page_lines.max(2) / 2),
        Action::PageDown if paged => page_forward(reader),
        Action::PageUp if paged => page_backward(reader),
        Action::PageDown => reader.scroll_down(reader.page_lines.max(1)),
        Action::PageUp => reader.scroll_up(reader.page_lines.max(1)),
        Action::Top => {
            if reader.focus == Focus::Sidebar {
                reader.sidebar_sel = 0;
            } else {
                reader.scroll = 0;
            }
        }
        Action::Bottom => {
            if reader.focus == Focus::Sidebar {
                reader.sidebar_sel = reader.outline.len().saturating_sub(1);
            } else {
                reader.scroll = reader.max_scroll();
            }
        }
        // `Ng`: jump to page/section N (1-based), clamped. Records history.
        Action::Goto(n) => {
            let last = reader.section_count().saturating_sub(1);
            reader.jump_to(n.saturating_sub(1).min(last), None);
        }
        _ => {}
    }
}
