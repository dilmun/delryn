//! Input dispatch: the central key router (`on_key`), the modal overlay key
//! handlers (images, notes, annotations, in-book search prompt), and the
//! `Action` dispatcher (`apply`). Routing only — the work lives in the concern
//! modules; child of `app`, so it calls their methods directly.

use crossterm::event::KeyModifiers;

use super::*;

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
        if self.settings.is_some() {
            self.settings_key(key);
            return;
        }
        if self.prompt.is_some() {
            self.prompt_key(key);
            return;
        }
        if self.meta_edit.is_some() {
            self.meta_edit_key(key);
            return;
        }
        if self.bulk_rename.is_some() {
            self.bulk_rename_key(key);
            return;
        }
        if self.lib_coll_edit.is_some() {
            self.lib_coll_edit_key(key);
            return;
        }
        if self.shelf_picker.is_some() {
            self.shelf_picker_key(key);
            return;
        }
        if self.image_view.is_some() {
            self.image_key(key);
            return;
        }
        if self.annot.is_some() {
            self.annot_key(key);
            return;
        }
        // The stats overlay is read-only: any key dismisses it.
        if self.stats.is_some() {
            self.stats = None;
            return;
        }
        if self.palette.is_some() {
            self.palette_key(key);
            return;
        }
        // The in-book search prompt is a focused text input: it must capture
        // every key (including shortcut letters like 'i' / ';' / ':') before any
        // global shortcut below gets a chance to fire.
        if self.mode == Mode::Reader && self.reader.as_ref().is_some_and(|r| r.searching) {
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
            self.settings = Some(Settings {
                scope,
                row: first_setting_row(scope),
            });
            return;
        }
        match self.mode {
            Mode::Reader => {
                // Clear any transient flash message on the next keypress.
                if let Some(r) = self.reader.as_mut() {
                    r.flash = None;
                }
                let action = input::map_key(key, &mut self.pending);
                self.apply(action);
                // An activated external link asks for confirmation before opening.
                if let Some(url) = self.reader.as_mut().and_then(|r| r.take_pending_open()) {
                    let shown = crate::view::truncate(&url, 60);
                    self.ask_confirm(
                        &format!("Open in browser: {shown}?"),
                        super::confirm::ConfirmAction::OpenUrl(url),
                    );
                }
                // Returning to the library (Back) should reflect the latest state.
                if self.mode == Mode::Library {
                    self.refresh_library();
                }
            }
            Mode::Library => {
                // Clear any transient flash (e.g. cover-embed result) on input.
                self.lib_flash = None;
                self.library_key(key);
            }
        }
    }

    /// Open the image viewer on the current section's images.
    fn open_images(&mut self) {
        let policy = crate::media::RenderPolicy {
            tint: crate::view::theme_ink(self.config.theme),
            mode: self.config.image_mode,
        };
        let (Some(picker), Some(reader)) = (self.picker.as_ref(), self.reader.as_mut()) else {
            return;
        };
        let images = reader.doc.section_images(reader.section);
        self.image_view = ImageView::new(picker, &images, policy);
    }

    fn image_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') => self.image_view = None,
            KeyCode::Char('n') | KeyCode::Char('l') | KeyCode::Right | KeyCode::Char('j') => {
                if let Some(v) = self.image_view.as_mut() {
                    v.next();
                }
            }
            KeyCode::Char('N') | KeyCode::Char('h') | KeyCode::Left | KeyCode::Char('k') => {
                if let Some(v) = self.image_view.as_mut() {
                    v.prev();
                }
            }
            _ => {}
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => self.prompt_commit(),
            KeyCode::Backspace => {
                if let Some(p) = self.prompt.as_mut() {
                    p.buffer.pop();
                }
            }
            // Ctrl-U clears the line (handy for a prefilled rename/folder field).
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(p) = self.prompt.as_mut() {
                    p.buffer.clear();
                }
            }
            KeyCode::Char(c) => {
                if let Some(p) = self.prompt.as_mut() {
                    p.buffer.push(c);
                }
            }
            _ => {}
        }
    }

    /// Apply the committed prompt text to its bookmark (rename / file), then
    /// dismiss the prompt and refresh the open overlay in place.
    fn prompt_commit(&mut self) {
        let Some(p) = self.prompt.take() else {
            return;
        };
        let text = p.buffer.trim().to_string();
        let id = match p.kind {
            PromptKind::Name(id) => {
                if let Some(store) = &self.store {
                    store.set_annotation_name(id, &text);
                }
                id
            }
            PromptKind::Folder(id) => {
                if let Some(store) = &self.store {
                    store.set_annotation_folder(id, &text);
                }
                id
            }
        };
        self.refresh_bookmarks(id);
    }

    /// Reload the open bookmarks overlay, keeping the cursor on bookmark `keep_id`
    /// (whose position may have shifted when its folder changed).
    fn refresh_bookmarks(&mut self, keep_id: i64) {
        if let (Some(store), Some(a)) = (&self.store, self.annot.as_mut()) {
            a.items = store.list_bookmarks(&self.book_path);
            if let Some(pos) = a.items.iter().position(|i| i.id == keep_id) {
                a.sel = pos;
            } else if a.sel >= a.items.len() {
                a.sel = a.items.len().saturating_sub(1);
            }
        }
        self.sync_reader_bookmarks();
    }

    fn annot_key(&mut self, key: KeyEvent) {
        let Some(a) = self.annot.as_ref() else {
            return;
        };
        let (len, sel) = (a.items.len(), a.sel);
        match key.code {
            KeyCode::Esc | KeyCode::Char('\'') | KeyCode::Char('q') => self.annot = None,
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(a) = self.annot.as_mut()
                    && len > 0
                {
                    a.sel = (sel + 1).min(len - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(a) = self.annot.as_mut() {
                    a.sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Enter | KeyCode::Char('l') => {
                let target = self
                    .annot
                    .as_ref()
                    .and_then(|a| a.items.get(a.sel))
                    .map(|i| (i.section, i.quote.clone()));
                if let Some((section, quote)) = target {
                    if let Some(r) = self.reader.as_mut() {
                        r.jump_to(section, Some(&quote));
                    }
                    self.annot = None;
                }
            }
            // Name (or rename) the selected entry; prefilled with its current name.
            KeyCode::Char('r') => {
                if let Some(i) = self.annot.as_ref().and_then(|a| a.items.get(a.sel)) {
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Name(i.id),
                        buffer: i.name.clone(),
                    });
                }
            }
            // File the selected entry into a folder; prefilled with its current one.
            KeyCode::Char('f') => {
                if let Some(i) = self.annot.as_ref().and_then(|a| a.items.get(a.sel)) {
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Folder(i.id),
                        buffer: i.folder.clone(),
                    });
                }
            }
            KeyCode::Char('d') => {
                let id = self
                    .annot
                    .as_ref()
                    .and_then(|a| a.items.get(a.sel))
                    .map(|i| i.id);
                if let (Some(id), Some(store)) = (id, &self.store) {
                    store.delete_annotation(id);
                    let items = store.list_bookmarks(&self.book_path);
                    if let Some(a) = self.annot.as_mut() {
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
        if let (Some(store), Some(r)) = (&self.store, self.reader.as_mut()) {
            let marks = store
                .list_bookmarks(&self.book_path)
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
                reader.searching = false;
                reader.search_input.clear();
            }
            KeyCode::Enter => reader.run_search(),
            KeyCode::Tab => reader.cycle_search_mode(),
            KeyCode::Up => reader.search_history_recall(-1),
            KeyCode::Down => reader.search_history_recall(1),
            KeyCode::Backspace => {
                reader.history_pos = None;
                reader.search_input.pop();
            }
            KeyCode::Char(c) => {
                reader.history_pos = None;
                reader.search_input.push(c);
            }
            _ => {}
        }
    }

    fn apply(&mut self, action: Action) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let before = reader.section;
        let mut save = false;
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back => {
                // Accumulate reading time for the session before leaving.
                if let (Some(start), Some(store)) = (self.session_start, &self.store) {
                    let secs = start.elapsed().as_secs() as i64;
                    if secs > 0 && !self.book_path.is_empty() {
                        store.add_read_time(&self.book_path, secs);
                    }
                }
                self.session_start = Some(Instant::now());
                self.mode = Mode::Library;
                save = true;
            }
            // In paged mode, vertical content navigation flips whole pages.
            Action::Down(n) => match reader.focus {
                Focus::Content if self.config.paged => reader.page_forward(),
                Focus::Content => reader.queue_scroll(n as isize),
                Focus::Sidebar => reader.sidebar_move(n as isize),
            },
            Action::Up(n) => match reader.focus {
                Focus::Content if self.config.paged => reader.page_backward(),
                Focus::Content => reader.queue_scroll(-(n as isize)),
                Focus::Sidebar => reader.sidebar_move(-(n as isize)),
            },
            Action::HalfDown if self.config.paged => reader.page_forward(),
            Action::HalfUp if self.config.paged => reader.page_backward(),
            Action::HalfDown => reader.scroll_down(reader.page_lines.max(2) / 2),
            Action::HalfUp => reader.scroll_up(reader.page_lines.max(2) / 2),
            Action::PageDown if self.config.paged => reader.page_forward(),
            Action::PageUp if self.config.paged => reader.page_backward(),
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
            Action::ToggleStatus => self.config.show_status = !self.config.show_status,
            Action::CycleView => {
                self.config.view_mode = self.config.view_mode.next();
                save = true;
            }
            Action::CycleTheme => {
                self.config.theme = self.config.theme.next();
                save = true;
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
                }
            }
            Action::Collapse => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_collapse();
                }
            }
            Action::HistBack => reader.history_back(),
            Action::HistForward => reader.history_forward(),
            Action::Search => reader.start_search(),
            Action::SearchNext => reader.search_next(),
            Action::SearchPrev => reader.search_prev(),
            Action::AddBookmark => {
                if let Some(store) = &self.store
                    && !self.book_path.is_empty()
                {
                    store.add_bookmark(&self.book_path, reader.section, &reader.current_quote());
                    reader.flash = Some("bookmark added".into());
                    // `reader` is borrowed here, so push the refreshed set directly
                    // rather than via the `&mut self` helper.
                    let marks = store
                        .list_bookmarks(&self.book_path)
                        .into_iter()
                        .map(|a| (a.section, a.quote))
                        .collect();
                    reader.set_bookmarks(marks);
                }
            }
            Action::OpenAnnotations => {
                if let Some(store) = &self.store {
                    let items = store.list_bookmarks(&self.book_path);
                    self.annot = Some(AnnotState { items, sel: 0 });
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

        // Persist on chapter change or a settings change (cheap).
        if (save || reader.section != before)
            && let Some(store) = &self.store
            && !self.book_path.is_empty()
        {
            let _ = store.save_progress(
                &self.book_path,
                reader.section,
                reader.within_frac(),
                self.config.view_mode,
                self.config.theme.name,
            );
        }
    }
}
