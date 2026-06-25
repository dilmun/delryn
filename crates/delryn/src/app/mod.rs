//! Application state and event dispatch.
//!
//! Two top-level modes behaving like tabs (Library | Reader). For now the
//! Library is a stub; the Reader is the working EPUB vertical slice. See
//! `DESIGN.md` §4, §6.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::config::Config;
use crate::document::epub::{self, EpubDocument};
use crate::document::epub_write;
use crate::input::{self, Action, Pending};
use crate::media::{self, ImageBuilder};
use crate::online;
use crate::store::{Annotation, BookRow, LibrarySection, Store};
use crate::theme;
use ratatui_image::picker::Picker;

mod confirm;
pub use confirm::PendingConfirm;

mod settings;
pub use settings::{SettingItem, SettingRow, Settings, first_setting_row, settings_rows};

mod mouse;
pub use mouse::{LayoutRects, MouseHits};

mod rename;
pub use rename::{BulkRename, BulkTarget};

mod select;

mod collections;
pub use collections::{CollInput, ShelfPicker};

mod editor;
pub use editor::{
    EditMode, EditTab, LOOKUP_FIELDS, LookupForm, META_FIELDS, MetaEdit, ONLINE_LIMIT, OnlineMsg,
    Search,
};

mod reader;
pub use reader::Reader;

mod image_view;
pub use image_view::{Figure, ImageViewer};

mod library;
pub use library::{LibPane, LibView, SortKey};

mod dispatch;

mod palette;
pub use palette::{Command, Palette, PaletteItem};

/// How long the library selection must hold still before the detail-pane cover
/// is (re)built, so holding j/k stays smooth.
const COVER_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(110);

/// Number of decoded sections kept in memory (current ± neighbours).
const CACHE_CAP: usize = 9;
/// Number of built image protocols kept in memory / GPU-resident in the
/// terminal. Reused across section revisits; LRU-evicted (and deleted from the
/// terminal) beyond this.
const IMAGE_CACHE_CAP: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Library,
    Reader,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Content,
    Sidebar,
}

/// Open bookmarks overlay state (the folder-grouped list + cursor).
pub struct AnnotState {
    pub items: Vec<Annotation>,
    pub sel: usize,
}

/// What a one-line text prompt's typed text becomes when committed. Both target
/// a bookmark; notes are a Phase 4 concern with their own flow.
pub enum PromptKind {
    /// The custom name of bookmark `id` (empty clears it back to the quote).
    Name(i64),
    /// The folder bookmark `id` belongs to (empty = ungrouped).
    Folder(i64),
}

/// A one-line text prompt shown at the bottom of the reader (rename a bookmark /
/// file it into a folder).
pub struct Prompt {
    pub kind: PromptKind,
    pub buffer: String,
}

pub struct App {
    pub mode: Mode,
    pub config: Config,
    pub reader: Option<Reader>,
    pub last_layout: LayoutRects,
    /// Clickable regions from the last render (mouse hit-testing).
    pub mouse: MouseHits,
    pub pending: Pending,
    pub should_quit: bool,
    /// Open settings popup, if any.
    pub settings: Option<Settings>,
    /// Open annotations overlay, if any.
    pub annot: Option<AnnotState>,
    /// Open library-statistics overlay, if any.
    pub stats: Option<crate::library::stats::LibraryStats>,
    /// Open command palette, if any.
    pub palette: Option<Palette>,
    /// Active bottom-row text prompt (note / rename bookmark / file in folder).
    pub prompt: Option<Prompt>,
    /// Open metadata-edit form (library), if any.
    pub meta_edit: Option<MetaEdit>,
    /// Open bulk-rename popup (template applied to the marked books), if any.
    pub bulk_rename: Option<BulkRename>,
    /// Inline sidebar collection editor (create / rename), if active.
    pub lib_coll_edit: Option<CollInput>,
    /// A destructive action awaiting a yes/no confirmation, if any. Intercepts
    /// input ahead of every popup and is answered with y/⏎ or n/Esc.
    pub pending_confirm: Option<PendingConfirm>,
    /// Remaining book paths to edit after the current one, when editing a
    /// multi-selection one book at a time (`^S` saves+advances, `Esc` skips).
    pub edit_queue: Vec<String>,
    /// Total books in the current edit queue (for the `N/total` header).
    pub edit_total: usize,
    /// Open image viewer overlay, if any.
    pub image_view: Option<ImageViewer>,
    /// An image queued for the system clipboard (`(w, h, RGBA)`), set by the
    /// viewer's copy action and drained by the main loop.
    pub pending_clipboard_image: Option<(u32, u32, Vec<u8>)>,
    /// Detected terminal image protocol (None if unsupported / headless).
    pub picker: Option<Picker>,
    /// Background builder for inline-image protocols.
    pub image_builder: Option<ImageBuilder>,
    /// Start of the current reading session, for time tracking.
    session_start: Option<Instant>,
    store: Option<Store>,
    /// Canonical path of the open book; key for persistence.
    book_path: String,
    // Library view state.
    pub lib_view: LibView,
    /// Which pane has the keyboard (Sidebar / List / Detail). Tab cycles it.
    pub lib_pane: LibPane,
    /// Show the sections/collections sidebar.
    pub lib_show_sidebar: bool,
    /// Sidebar / detail pane widths (resizable with `[` `]`).
    pub lib_sidebar_w: u16,
    pub lib_detail_w: u16,
    /// Cached (collection name, book count), refreshed with the book list.
    pub lib_shelves: Vec<(String, usize)>,
    pub lib_books: Vec<BookRow>,
    pub lib_sel: usize,
    /// Effective multi-selection for bulk actions, keyed by book path. The union
    /// of the individually-toggled `lib_marked_base` and the live visual range.
    pub lib_marked: HashSet<String>,
    /// Books toggled individually with Space (non-contiguous), kept separate so a
    /// live visual range can be layered on top without clobbering them.
    pub lib_marked_base: HashSet<String>,
    /// Visual-select anchor (book index) while in visual mode; `None` otherwise.
    /// The selection is the contiguous range between the anchor and `lib_sel`.
    pub lib_visual: Option<usize>,
    /// Sidebar cursor parked on the trailing "＋ New collection" row.
    pub lib_side_new: bool,
    /// Active sort key and direction for the book list.
    pub lib_sort: SortKey,
    pub lib_sort_desc: bool,
    pub lib_filter: String,
    pub lib_filtering: bool,
    /// Transient message shown in the library status bar (e.g. cover embedded);
    /// cleared on the next keypress.
    pub lib_flash: Option<String>,
    /// Open add-to-collection picker, if any.
    pub shelf_picker: Option<ShelfPicker>,
    /// Receiver for async Open Library results (search / cover), if a request
    /// from the editor's Online tab is in flight.
    pub online_rx: Option<Receiver<OnlineMsg>>,
    /// Show the right-hand detail pane (cover + metadata).
    pub lib_detail: bool,
    /// Cover image protocol for the detail pane, rebuilt when the selection
    /// settles (debounced so holding j/k stays smooth).
    pub lib_cover: Option<media::CoverImage>,
    /// Book path the current `lib_cover` was built for (avoids rebuilds).
    pub lib_cover_path: String,
    /// Path the cover wants to settle on, and when it last changed (debounce).
    lib_cover_target: String,
    lib_cover_at: Instant,
    /// Grid view: number of columns from the last render (for 2D navigation).
    pub lib_grid_cols: usize,
    /// Grid view: lazily-built cover protocols, keyed by book path
    /// (`None` = no cover / failed, so we don't retry every frame).
    pub lib_grid_covers: HashMap<String, Option<media::CoverImage>>,
    /// Grid view: visible covers still waiting to be built (keeps redrawing).
    pub lib_grid_pending: bool,
    /// Cover-tab preview image protocol + the URL it was built for, plus the
    /// debounce target/timer for fetching the highlighted result's cover.
    pub edit_cover: Option<media::CoverImage>,
    pub edit_cover_url: String,
    edit_cover_target: String,
    edit_cover_at: Instant,
}

/// Series index without a trailing `.0` (`2.0` → "2", `2.5` → "2.5"), for
/// prefilling the edit form.
fn fmt_series_index(i: f32) -> String {
    if i.fract().abs() < f32::EPSILON {
        format!("{}", i as i64)
    } else {
        format!("{i}")
    }
}

/// Cover image bytes for a book: a fetched cover from the cache if present,
/// otherwise the EPUB's own cover. `None` if neither exists.
fn load_cover_bytes(path: &str) -> Option<Vec<u8>> {
    if path.is_empty() {
        return None;
    }
    match std::fs::read(online::cover_cache_path(path)) {
        Ok(bytes) if !bytes.is_empty() => return Some(bytes),
        _ => {}
    }
    // Declared cover, else the first content image (converted files declare none).
    epub::extract_cover(path).map(|(b, _)| b)
}

// Title/author/ISBN heuristics + filename templating now live in
// `delryn_model::naming`; re-exported so existing `app::{fill_template, …}` and
// the bare in-module calls keep working.
pub use delryn_model::naming::{
    filename_title, fill_template, first_author, looks_like_id, main_title, sanitize_filename,
};

/// Write a staged cover into the book file itself (EPUB only), returning a status
/// line for the library flash. Non-EPUB files are left untouched — the cover is
/// still cached for delryn's own display by the caller.
fn embed_cover_into_file(path: &str, bytes: &[u8]) -> String {
    let is_epub = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("epub"));
    if !is_epub {
        return "cover saved (file is not an EPUB — not embedded)".into();
    }
    match epub_write::embed_cover(std::path::Path::new(path), bytes) {
        Ok(_) => "✓ cover embedded into EPUB".into(),
        Err(e) => format!("cover cached, but embed failed: {e}"),
    }
}

/// Insert `ch` at char index `cursor` in `s` (clamped to the end).
fn str_insert(s: &mut String, cursor: usize, ch: char) {
    let byte = s.char_indices().nth(cursor).map_or(s.len(), |(b, _)| b);
    s.insert(byte, ch);
}

/// Remove the char before `cursor`; returns whether one was removed.
fn str_delete_before(s: &mut String, cursor: usize) -> bool {
    if cursor == 0 {
        return false;
    }
    if let Some((byte, _)) = s.char_indices().nth(cursor - 1) {
        s.remove(byte);
        return true;
    }
    false
}

/// Remove the char at `cursor` (no-op past the end).
fn str_delete_at(s: &mut String, cursor: usize) {
    if let Some((byte, _)) = s.char_indices().nth(cursor) {
        s.remove(byte);
    }
}

/// Build a reader for `path`, applying global config and any saved per-book
/// overrides (theme, view mode, resume position).
fn build_reader(path: &str, store: &Option<Store>) -> Result<(Reader, Config, String)> {
    // Dispatch by file format. Only EPUB has a `Document` backend today; other
    // recognized formats report cleanly here instead of failing deep in the EPUB
    // parser with a cryptic error. See the Phase 5 plan in `TODO.md`.
    let fmt = crate::document::BookFormat::from_path(path);
    if !fmt.is_readable() {
        anyhow::bail!(
            "{} files aren't readable yet — only EPUB opens for now",
            fmt.label()
        );
    }
    let doc = EpubDocument::open(path)?;
    let mut reader = Reader::new(Box::new(doc))?;
    let mut config = Config::load();
    let book_path = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    if let Some(store) = store
        && let Some(p) = store.load_progress(&book_path)
    {
        config.view_mode = p.view_mode;
        if let Some(t) = theme::by_name(&p.theme) {
            config.theme = t;
        }
        reader.load(p.section);
        reader.pending_frac = Some(p.frac);
    }
    // Seed the gutter with this book's bookmarks (independent of saved progress).
    if let Some(store) = store {
        let marks = store
            .list_bookmarks(&book_path)
            .into_iter()
            .map(|a| (a.section, a.quote))
            .collect();
        reader.set_bookmarks(marks);
    }
    Ok((reader, config, book_path))
}

impl App {
    pub fn open_book(path: &str) -> Result<Self> {
        let store = Store::open_default().ok();
        let (reader, config, book_path) = build_reader(path, &store)?;
        if let Some(s) = &store {
            s.mark_opened(&book_path);
        }
        Ok(Self {
            mode: Mode::Reader,
            config,
            reader: Some(reader),
            last_layout: LayoutRects::default(),
            mouse: MouseHits::default(),
            pending: Pending::default(),
            should_quit: false,
            settings: None,
            annot: None,
            stats: None,
            palette: None,
            prompt: None,
            meta_edit: None,
            bulk_rename: None,
            lib_coll_edit: None,
            pending_confirm: None,
            edit_queue: Vec::new(),
            edit_total: 0,
            image_view: None,
            pending_clipboard_image: None,
            picker: None,
            image_builder: None,
            session_start: Some(Instant::now()),
            store,
            book_path,
            lib_view: LibView::Section(LibrarySection::All),
            lib_pane: LibPane::List,
            lib_show_sidebar: true,
            lib_sidebar_w: 24,
            lib_detail_w: 36,
            lib_shelves: Vec::new(),
            lib_books: Vec::new(),
            lib_sel: 0,
            lib_marked: HashSet::new(),
            lib_marked_base: HashSet::new(),
            lib_visual: None,
            lib_side_new: false,
            lib_sort: SortKey::Default,
            lib_sort_desc: false,
            lib_filter: String::new(),
            lib_filtering: false,
            lib_flash: None,
            shelf_picker: None,
            online_rx: None,
            lib_detail: true,
            lib_cover: None,
            lib_cover_path: String::new(),
            lib_cover_target: String::new(),
            lib_cover_at: Instant::now(),
            lib_grid_cols: 1,
            lib_grid_covers: HashMap::new(),
            lib_grid_pending: false,
            edit_cover: None,
            edit_cover_url: String::new(),
            edit_cover_target: String::new(),
            edit_cover_at: Instant::now(),
        })
    }

    pub fn library() -> Self {
        let config = Config::load();
        let store = Store::open_default().ok();
        if let Some(s) = &store {
            crate::library::scan(&config.library_paths, s);
        }
        let mut app = Self {
            mode: Mode::Library,
            config,
            reader: None,
            last_layout: LayoutRects::default(),
            mouse: MouseHits::default(),
            pending: Pending::default(),
            should_quit: false,
            settings: None,
            annot: None,
            stats: None,
            palette: None,
            prompt: None,
            meta_edit: None,
            bulk_rename: None,
            lib_coll_edit: None,
            pending_confirm: None,
            edit_queue: Vec::new(),
            edit_total: 0,
            image_view: None,
            pending_clipboard_image: None,
            picker: None,
            image_builder: None,
            session_start: None,
            store,
            book_path: String::new(),
            lib_view: LibView::Section(LibrarySection::All),
            lib_pane: LibPane::List,
            lib_show_sidebar: true,
            lib_sidebar_w: 24,
            lib_detail_w: 36,
            lib_shelves: Vec::new(),
            lib_books: Vec::new(),
            lib_sel: 0,
            lib_marked: HashSet::new(),
            lib_marked_base: HashSet::new(),
            lib_visual: None,
            lib_side_new: false,
            lib_sort: SortKey::Default,
            lib_sort_desc: false,
            lib_filter: String::new(),
            lib_filtering: false,
            lib_flash: None,
            shelf_picker: None,
            online_rx: None,
            lib_detail: true,
            lib_cover: None,
            lib_cover_path: String::new(),
            lib_cover_target: String::new(),
            lib_cover_at: Instant::now(),
            lib_grid_cols: 1,
            lib_grid_covers: HashMap::new(),
            lib_grid_pending: false,
            edit_cover: None,
            edit_cover_url: String::new(),
            edit_cover_target: String::new(),
            edit_cover_at: Instant::now(),
        };
        app.refresh_library();
        app
    }

    fn open_selected(&mut self) {
        let Some(path) = self.lib_books.get(self.lib_sel).map(|b| b.path.clone()) else {
            return;
        };
        self.flush_reading_time();
        match build_reader(&path, &self.store) {
            Ok((reader, config, book_path)) => {
                self.reader = Some(reader);
                self.config = config;
                self.book_path = book_path;
                self.mode = Mode::Reader;
                self.session_start = Some(Instant::now());
                if let Some(s) = &self.store {
                    s.mark_opened(&self.book_path);
                }
            }
            // Surface the reason (e.g. an unsupported format) on the status row
            // rather than silently doing nothing on Enter.
            Err(e) => self.lib_flash = Some(e.to_string()),
        }
    }

    /// Persist the current reading position (best-effort).
    pub fn save_progress(&self) {
        if let (Some(store), Some(reader)) = (&self.store, &self.reader)
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

    /// Accumulate elapsed reading time into the open book and reset the clock.
    fn flush_reading_time(&mut self) {
        if let (Some(start), Some(store)) = (self.session_start, &self.store) {
            let secs = start.elapsed().as_secs() as i64;
            if secs > 0 && !self.book_path.is_empty() {
                store.add_read_time(&self.book_path, secs);
            }
        }
        if self.session_start.is_some() {
            self.session_start = Some(Instant::now());
        }
    }

    /// Save progress + reading time on quit.
    pub fn on_exit(&mut self) {
        self.flush_reading_time();
        self.save_progress();
    }

    pub fn total_read_seconds(&self) -> i64 {
        self.store
            .as_ref()
            .map(|s| s.total_read_seconds())
            .unwrap_or(0)
    }

    /// Terminal image ids to delete (evicted from the reader's cache).
    pub fn take_image_deletes(&mut self) -> Vec<u32> {
        self.reader
            .as_mut()
            .map(|r| r.take_image_deletes())
            .unwrap_or_default()
    }

    /// Text queued for the system clipboard (OSC 52), if any.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.reader.as_mut().and_then(|r| r.take_clipboard())
    }

    /// An image queued for the system clipboard (`(w, h, RGBA)`), if any.
    pub fn take_clipboard_image(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        self.pending_clipboard_image.take()
    }

    /// Whether any blocking overlay/popup is currently open. The main loop forces
    /// a full repaint when this toggles, so a closed popup's cells (which may sit
    /// over an inline image) don't leave a ghost the cell-diff misses.
    pub fn any_overlay_open(&self) -> bool {
        self.settings.is_some()
            || self.annot.is_some()
            || self.stats.is_some()
            || self.palette.is_some()
            || self.meta_edit.is_some()
            || self.bulk_rename.is_some()
            || self.shelf_picker.is_some()
            || self.image_view.is_some()
    }

    /// Is a smooth scroll in progress, or are inline images still building (so
    /// the loop should keep drawing until things settle)?
    pub fn animating(&self) -> bool {
        let Some(r) = self.reader.as_ref() else {
            return false;
        };
        r.is_scrolling() || (self.mode == Mode::Reader && r.images_pending())
    }

    /// Advance one frame of smooth scrolling; returns whether anything moved.
    pub fn step_scroll(&mut self) -> bool {
        if self.mode != Mode::Reader {
            return false;
        }
        self.reader.as_mut().is_some_and(|r| r.step_scroll())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LibLayout;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
    use ratatui::layout::Rect;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    // Proves the library key bindings reach their handlers (regression guard for
    // the new e/c/v actions).
    #[test]
    fn library_keys_dispatch() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_keys_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/k.epub", "K", "Auth", None, 1, 1, 1, "", None, "", "", "", "",
                )
                .unwrap();
        }

        let mut app = App::library();
        assert_eq!(app.lib_books.len(), 1, "seeded book loads into the list");

        // v cycles the layout: List → Compact → Grid → List.
        assert_eq!(app.config.library_layout, LibLayout::List);
        app.on_key(key('v'));
        assert_eq!(app.config.library_layout, LibLayout::Compact);
        app.on_key(key('v'));
        assert_eq!(app.config.library_layout, LibLayout::Grid);
        app.on_key(key('v'));
        assert_eq!(
            app.config.library_layout,
            LibLayout::List,
            "v wraps back to list"
        );

        // e opens the metadata editor; Esc closes it.
        app.on_key(key('e'));
        assert!(app.meta_edit.is_some(), "e opens the metadata editor");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.meta_edit.is_none(), "Esc closes the editor");

        // c opens the add-to-collection picker.
        app.on_key(key('c'));
        assert!(app.shelf_picker.is_some(), "c opens the collection picker");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Tab focuses the sidebar; j/k then navigate sections (not the book list).
    #[test]
    fn library_sidebar_focus_nav() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_focus_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/n.epub", "N", "Auth", None, 1, 1, 1, "", None, "", "", "", "",
                )
                .unwrap();
        }

        let mut app = App::library();
        assert_eq!(app.lib_pane, LibPane::List, "starts in the list");
        assert_eq!(app.lib_view, LibView::Section(LibrarySection::All));

        // h moves the keyboard left into the sidebar.
        app.on_key(key('h'));
        assert_eq!(app.lib_pane, LibPane::Sidebar);

        // j/k now walk the sections (All → Favorites → All), not the book list.
        app.on_key(key('j'));
        assert_eq!(app.lib_view, LibView::Section(LibrarySection::Favorites));
        app.on_key(key('k'));
        assert_eq!(app.lib_view, LibView::Section(LibrarySection::All));

        // g jumps to the first section; k there is clamped (no wrap).
        app.on_key(key('g'));
        assert_eq!(app.lib_view, LibView::Section(LibrarySection::Recent));
        app.on_key(key('k'));
        assert_eq!(
            app.lib_view,
            LibView::Section(LibrarySection::Recent),
            "clamped at top"
        );

        // Enter steps into the list.
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.lib_pane, LibPane::List);

        // b hides the sidebar; focus falls back to the list (it can't stay there).
        app.on_key(key('h')); // focus sidebar
        assert_eq!(app.lib_pane, LibPane::Sidebar);
        app.on_key(key('b')); // hide sidebar
        assert!(!app.lib_show_sidebar);
        assert_eq!(app.lib_pane, LibPane::List, "focus leaves the hidden pane");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    // Tab cycles the three panes; [ ] resize the focused side pane (clamped).
    #[test]
    fn pane_cycle_and_resize() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_panes_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let mut app = App::library();

        // List → Detail → Sidebar → List.
        assert_eq!(app.lib_pane, LibPane::List);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.lib_pane, LibPane::Detail);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.lib_pane, LibPane::Sidebar);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.lib_pane, LibPane::List);

        // Resize the sidebar (focus it first).
        app.on_key(key('h'));
        assert_eq!(app.lib_pane, LibPane::Sidebar);
        let w0 = app.lib_sidebar_w;
        app.on_key(key('>'));
        assert_eq!(app.lib_sidebar_w, w0 + 2);
        app.on_key(key('<'));
        assert_eq!(app.lib_sidebar_w, w0);
        for _ in 0..40 {
            app.on_key(key('<'));
        }
        assert_eq!(
            app.lib_sidebar_w,
            library::SIDEBAR_W_MIN,
            "clamped at the minimum"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Two-mode editing: j/k navigate fields, Enter enters edit mode, ^S saves;
    // numeric validation blocks the save.
    #[test]
    fn meta_editor_modes_and_validation() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_edit2_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/k.epub",
                    "K",
                    "Auth",
                    Some(1999),
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e'));
        assert_eq!(
            app.meta_edit.as_ref().unwrap().mode,
            EditMode::Nav,
            "opens in nav mode"
        );

        // In nav mode, 'j' moves fields (does NOT type); Enter enters edit mode.
        app.on_key(key('j')); // → Author
        assert_eq!(app.meta_edit.as_ref().unwrap().row, 1);
        app.on_key(key('k')); // back to Title
        app.on_key(code(KeyCode::Enter)); // edit Title
        assert_eq!(app.meta_edit.as_ref().unwrap().mode, EditMode::Edit);
        // Mid-string insert: cursor at end of "K"; Left then 'X' → "XK".
        app.on_key(code(KeyCode::Left));
        app.on_key(key('X'));
        assert_eq!(app.meta_edit.as_ref().unwrap().values[0], "XK");
        app.on_key(code(KeyCode::Esc)); // back to nav (not closed)
        assert!(app.meta_edit.is_some());
        assert_eq!(app.meta_edit.as_ref().unwrap().mode, EditMode::Nav);

        // Navigate to Year, edit it to garbage → invalid.
        app.on_key(key('j')); // Author
        app.on_key(key('j')); // Year
        app.on_key(code(KeyCode::Enter));
        app.on_key(ctrl('u'));
        app.on_key(key('a'));
        app.on_key(code(KeyCode::Esc));
        assert!(
            app.meta_edit.as_ref().unwrap().has_invalid(),
            "non-numeric year invalid"
        );

        // ^S must NOT even prompt to save while invalid.
        app.on_key(ctrl('s'));
        assert!(
            app.pending_confirm.is_none(),
            "no save prompt while invalid"
        );
        assert!(app.meta_edit.is_some(), "save blocked while invalid");

        // Fix the year, then ^S → confirm → save and persist.
        app.on_key(code(KeyCode::Enter));
        app.on_key(ctrl('u'));
        for c in "2001".chars() {
            app.on_key(key(c));
        }
        app.on_key(code(KeyCode::Esc));
        // ^S asks for confirmation; the editor stays open until answered.
        app.on_key(ctrl('s'));
        assert!(app.pending_confirm.is_some(), "^S asks to confirm");
        assert!(app.meta_edit.is_some(), "editor open while confirming");
        app.on_key(key('y')); // confirm
        assert!(app.pending_confirm.is_none(), "prompt dismissed");
        assert!(app.meta_edit.is_none(), "valid edit saves & closes");
        let b = &app.lib_books[0];
        assert_eq!(b.title, "XK");
        assert_eq!(b.year, Some(2001));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ^S raises a yes/no prompt; n (or Esc) cancels without saving, leaving the
    // editor open. Unrelated keys are ignored while the prompt is up.
    #[test]
    fn save_confirmation_can_be_cancelled() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_cfm_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/k.epub",
                    "K",
                    "Auth",
                    Some(1999),
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e')); // open editor
        // Change the title so there's something to save.
        app.on_key(code(KeyCode::Enter));
        app.on_key(key('X'));
        app.on_key(code(KeyCode::Esc)); // back to nav

        // ^S raises the prompt; nothing is saved yet and the editor stays open.
        app.on_key(ctrl('s'));
        assert!(app.pending_confirm.is_some(), "prompt up");
        // An unrelated key is ignored — the prompt is modal.
        app.on_key(code(KeyCode::Tab));
        assert!(
            app.pending_confirm.is_some(),
            "stray key ignored, prompt stays"
        );
        // n cancels, the editor remains open, nothing persisted.
        app.on_key(key('n'));
        assert!(app.pending_confirm.is_none(), "n dismisses the prompt");
        assert!(app.meta_edit.is_some(), "editor stays open after cancel");
        assert_eq!(app.lib_books[0].title, "K", "nothing persisted on cancel");

        // Esc also cancels the prompt (and keeps the editor).
        app.on_key(ctrl('s'));
        app.on_key(code(KeyCode::Esc));
        assert!(
            app.pending_confirm.is_none() && app.meta_edit.is_some(),
            "Esc cancels"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // 's' cycles the sort key; 'S' flips direction; the list reorders.
    #[test]
    fn library_sort_keys() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_sort_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/a.epub",
                    "A",
                    "x",
                    Some(2010),
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
            store
                .upsert_book(
                    "/b.epub",
                    "B",
                    "x",
                    Some(1999),
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
            store
                .upsert_book(
                    "/c.epub",
                    "C",
                    "x",
                    Some(2001),
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
        }

        let mut app = App::library();
        let titles = |a: &App| {
            a.lib_books
                .iter()
                .map(|b| b.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(&app), ["A", "B", "C"], "All section sorts by title");

        // Default → Title → Author → Year.
        app.on_key(key('s'));
        app.on_key(key('s'));
        app.on_key(key('s'));
        assert_eq!(app.lib_sort, SortKey::Year);
        assert_eq!(titles(&app), ["B", "C", "A"], "year ascending");

        app.on_key(key('S'));
        assert!(app.lib_sort_desc);
        assert_eq!(titles(&app), ["A", "C", "B"], "year descending");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Grid view: h/l move ±1, j/k move ±cols (clamped).
    #[test]
    fn grid_2d_navigation() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_grid_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            for t in ["A", "B", "C", "D", "E", "F"] {
                store
                    .upsert_book(
                        &format!("/{t}.epub"),
                        t,
                        "x",
                        None,
                        1,
                        1,
                        1,
                        "",
                        None,
                        "",
                        "",
                        "",
                        "",
                    )
                    .unwrap();
            }
        }

        let mut app = App::library();
        app.config.library_layout = LibLayout::Grid;
        app.lib_grid_cols = 3; // normally set by the renderer
        assert_eq!(app.lib_sel, 0);

        app.on_key(key('l')); // → 1
        app.on_key(key('l')); // → 2
        assert_eq!(app.lib_sel, 2);
        app.on_key(key('j')); // down a row: 2 + 3 = 5
        assert_eq!(app.lib_sel, 5);
        app.on_key(key('k')); // up a row: 5 - 3 = 2
        assert_eq!(app.lib_sel, 2);
        app.on_key(key('h')); // ← 1
        assert_eq!(app.lib_sel, 1);
        app.on_key(key('j')); // 1 + 3 = 4
        app.on_key(key('j')); // 4 + 3 = 7 → clamped to 5
        assert_eq!(app.lib_sel, 5, "clamped to last");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Renaming a book moves the file and repoints the DB; the list reflects it.
    #[test]
    fn rename_book_moves_file_and_updates_db() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_rn_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let books = tmp.join("books");
        std::fs::create_dir_all(&books).unwrap();
        let old = books.join("messy old name.epub");
        std::fs::write(&old, b"not really an epub").unwrap();
        let old_str = old.to_string_lossy().into_owned();
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    &old_str,
                    "Clean Title",
                    "Auth",
                    Some(2001),
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
            store.set_favorite(&old_str, true);
        }

        let mut app = App::library();
        assert_eq!(app.lib_books.len(), 1);
        // `r` renames the current book (no need to mark it) via the popup.
        app.on_key(key('r')); // rename popup (default "%T.%E")
        assert!(app.bulk_rename.is_some());
        app.on_key(ctrl('s')); // ^S asks to confirm
        assert!(app.pending_confirm.is_some(), "rename asks to confirm");
        app.on_key(key('y')); // confirm + apply

        let new = books.join("Clean Title.epub");
        assert!(new.exists(), "renamed file exists");
        assert!(!old.exists(), "old file is gone");
        assert_eq!(
            app.lib_books[0].path,
            new.to_string_lossy(),
            "DB path repointed and reloaded"
        );
        assert!(
            app.lib_books[0].favorite,
            "favorite preserved across rename"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Multi-select + bulk rename: mark all, open the popup, apply the template.
    #[test]
    fn bulk_rename_renames_marked_books() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_bulk_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let books = tmp.join("books");
        std::fs::create_dir_all(&books).unwrap();
        for (file, title) in [("a old.epub", "Alpha"), ("b old.epub", "Beta")] {
            let p = books.join(file);
            std::fs::write(&p, b"x").unwrap();
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    &p.to_string_lossy(),
                    title,
                    "Auth",
                    None,
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
        }

        let mut app = App::library();
        assert_eq!(app.lib_books.len(), 2);
        app.on_key(key('V')); // visual select from book 0
        app.on_key(key('j')); // extend to book 1
        assert_eq!(app.lib_marked.len(), 2);
        app.on_key(key('r')); // rename the selection (not the editor)
        assert!(app.bulk_rename.is_some());
        assert!(app.meta_edit.is_none());
        app.on_key(ctrl('s')); // ^S asks to confirm
        app.on_key(key('y')); // confirm + apply default "%T.%E"

        assert!(books.join("Alpha.epub").exists(), "Alpha renamed");
        assert!(books.join("Beta.epub").exists(), "Beta renamed");
        assert!(!books.join("a old.epub").exists(), "old file gone");
        assert!(app.bulk_rename.is_none(), "popup closed after apply");
        assert!(app.lib_marked.is_empty(), "selection cleared after apply");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // A left-click on a book's hit rect selects it (mouse dispatch).
    #[test]
    fn library_click_selects_book() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_click_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            for (p, t) in [("/a.epub", "A"), ("/b.epub", "B")] {
                store
                    .upsert_book(p, t, "Auth", None, 1, 1, 1, "", None, "", "", "", "")
                    .unwrap();
            }
        }
        let mut app = App::library();
        // Stand in for the render that normally fills these rects.
        app.mouse.books = vec![(0, Rect::new(0, 0, 20, 1)), (1, Rect::new(0, 1, 20, 1))];
        app.on_mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.lib_sel, 1, "click on the second row selects it");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Inline sidebar collection editing: create → rename → delete (clear name).
    #[test]
    fn collections_inline_create_rename_delete() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_cm_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/k.epub", "K", "Auth", None, 1, 1, 1, "", None, "", "", "", "",
                )
                .unwrap();
        }
        let names = |a: &App| {
            a.store
                .as_ref()
                .unwrap()
                .all_shelves()
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        };

        let mut app = App::library();
        // Create (the "＋ New collection" inline field).
        app.lib_coll_begin_new();
        for c in "Sci".chars() {
            app.on_key(key(c)); // routed to the inline editor
        }
        app.on_key(code(KeyCode::Enter));
        assert_eq!(names(&app), vec!["Sci"]);
        assert!(matches!(app.lib_view, LibView::Shelf(ref n) if n == "Sci"));

        // Rename in place: Sci → SciFi. ⏎ asks to confirm; y commits.
        app.lib_coll_begin_rename();
        for c in "Fi".chars() {
            app.on_key(key(c));
        }
        app.on_key(code(KeyCode::Enter));
        assert!(app.pending_confirm.is_some(), "rename asks to confirm");
        app.on_key(key('y'));
        assert_eq!(names(&app), vec!["SciFi"]);

        // Delete by clearing the name and confirming.
        app.lib_coll_begin_rename();
        app.on_key(ctrl('u'));
        app.on_key(code(KeyCode::Enter));
        assert!(app.pending_confirm.is_some(), "delete asks to confirm");
        app.on_key(key('y'));
        assert!(names(&app).is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Space toggles individual (non-contiguous) books into the selection.
    #[test]
    fn space_selects_individual_books() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_space_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            for (p, t) in [("/a.epub", "A"), ("/b.epub", "B"), ("/c.epub", "C")] {
                store
                    .upsert_book(p, t, "Auth", None, 1, 1, 1, "", None, "", "", "", "")
                    .unwrap();
            }
        }

        let mut app = App::library();
        app.on_key(key(' ')); // pick A, advance to B
        app.on_key(code(KeyCode::Down)); // skip B → C
        app.on_key(key(' ')); // pick C
        assert!(app.lib_marked.contains("/a.epub"));
        assert!(app.lib_marked.contains("/c.epub"));
        assert!(
            !app.lib_marked.contains("/b.epub"),
            "B was skipped — non-contiguous"
        );
        assert!(app.lib_visual.is_none(), "Space doesn't enter visual mode");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // `c` with a multi-selection files every selected book into the collection.
    #[test]
    fn bulk_shelf_assign_files_all_selected() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_bsa_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            for (p, t) in [("/a.epub", "A"), ("/b.epub", "B")] {
                store
                    .upsert_book(p, t, "Auth", None, 1, 1, 1, "", None, "", "", "", "")
                    .unwrap();
            }
        }

        let mut app = App::library();
        app.on_key(key('V')); // visual select from book 0
        app.on_key(key('j')); // extend to book 1
        assert_eq!(app.lib_marked.len(), 2);
        app.on_key(key('c')); // bulk shelf picker
        assert_eq!(app.shelf_picker.as_ref().unwrap().targets.len(), 2);
        // Select the "＋ New collection" row, create "Unread", filing both books.
        app.on_key(code(KeyCode::Enter)); // onto +New row → start typing
        for c in "Unread".chars() {
            app.on_key(key(c));
        }
        app.on_key(code(KeyCode::Enter)); // create + file all
        app.on_key(key('q')); // close (clears the selection)

        let store = app.store.as_ref().unwrap();
        assert!(store.shelves_for("/a.epub").contains(&"Unread".to_string()));
        assert!(store.shelves_for("/b.epub").contains(&"Unread".to_string()));
        assert!(
            app.lib_marked.is_empty(),
            "selection cleared after bulk file"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The settings cursor lands only on items, never on section headers.
    #[test]
    fn settings_nav_skips_section_headers() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_set_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };

        let mut app = App::library();
        app.on_key(key(';')); // opens settings scoped to the library
        assert_eq!(app.settings.as_ref().unwrap().scope, Mode::Library);

        let rows = settings_rows(Mode::Library);
        assert!(
            matches!(rows[0], SettingRow::Section(_)),
            "first row is a header"
        );
        for _ in 0..25 {
            let row = app.settings.as_ref().unwrap().row;
            assert!(
                matches!(rows[row], SettingRow::Item(_)),
                "cursor never rests on a section header (row {row})"
            );
            app.on_key(key('j'));
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Lookup tab: seed fields are pre-filled, j/k moves focus, typing edits the
    // focused field, and the read-only query is composed from them.
    #[test]
    fn lookup_form_seeds_and_edits() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_srch_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/k.epub",
                    "K",
                    "Auth",
                    Some(2010),
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e'));
        // Details → Cover → Lookup.
        for _ in 0..2 {
            app.on_key(code(KeyCode::Tab));
        }
        let ed = app.meta_edit.as_ref().unwrap();
        assert_eq!(ed.tab, EditTab::Online);
        // Seeded from metadata: name=title, author=first author (year excluded).
        assert_eq!(ed.lookup.name, "K");
        assert_eq!(ed.lookup.author, "Auth");
        assert_eq!(ed.lookup.query(), "K Auth");
        assert_eq!(ed.lookup.focus, 0); // Title focused

        // Typing edits the focused field (Title), entering edit mode.
        app.on_key(key('!'));
        let ed = app.meta_edit.as_ref().unwrap();
        assert!(ed.lookup.editing);
        assert_eq!(ed.lookup.name, "K!");
        app.on_key(code(KeyCode::Esc));
        assert!(!app.meta_edit.as_ref().unwrap().lookup.editing);

        // j moves focus to Author; editing it changes only that field.
        app.on_key(key('j'));
        assert_eq!(app.meta_edit.as_ref().unwrap().lookup.focus, 1);
        app.on_key(code(KeyCode::Enter)); // edit Author
        app.on_key(key('x'));
        let ed = app.meta_edit.as_ref().unwrap();
        assert_eq!(ed.lookup.author, "Authx");
        assert_eq!(ed.lookup.query(), "K! Authx");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The Cover free-text query and the Lookup seed fields are independent (#4):
    // typing in one must not change the other.
    #[test]
    fn cover_and_lookup_queries_are_independent() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_q2_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/k.epub", "K", "Auth", None, 1, 1, 1, "", None, "", "", "", "",
                )
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e'));
        // Details → Cover.
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.meta_edit.as_ref().unwrap().tab, EditTab::Cover);
        app.on_key(key('a'));
        app.on_key(key('b'));
        app.on_key(code(KeyCode::Esc)); // leave the cover query
        assert_eq!(app.meta_edit.as_ref().unwrap().cover_search.q, "ab");

        // Cover → Lookup: the seeded Title field is untouched, not in edit mode.
        app.on_key(code(KeyCode::Tab));
        let ed = app.meta_edit.as_ref().unwrap();
        assert_eq!(ed.tab, EditTab::Online);
        assert_eq!(ed.lookup.name, "K");
        assert!(!ed.lookup.editing);

        // Typing here edits the Title field; the cover query keeps "ab".
        app.on_key(key('c'));
        let ed = app.meta_edit.as_ref().unwrap();
        assert_eq!(ed.lookup.name, "Kc");
        assert_eq!(ed.cover_search.q, "ab");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The composed Lookup query flattens punctuation noise into clean words.
    // (The naming heuristics themselves are tested in delryn-model::naming.)
    #[test]
    fn lookup_query_sanitizes() {
        let f = LookupForm {
            name: "Deep Learning With Python".into(),
            author: ", Kissinger".into(),
            ..LookupForm::default()
        };
        assert_eq!(f.query(), "Deep Learning With Python Kissinger");
        // Useful punctuation (C++, #) survives.
        let g = LookupForm {
            name: "C++ Primer".into(),
            ..LookupForm::default()
        };
        assert_eq!(g.query(), "C++ Primer");
    }

    // A numeric/ID metadata title falls back to the (real) filename for the
    // Lookup seed, so these converted books become searchable.
    #[test]
    fn lookup_seed_falls_back_to_filename_for_id_titles() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_idseed_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let books = tmp.join("books");
        std::fs::create_dir_all(&books).unwrap();
        let file = books.join("Building Chatbots with Python.epub");
        std::fs::write(&file, b"x").unwrap();
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    &file.to_string_lossy(),
                    "503392068",
                    "Unknown",
                    None,
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e'));
        let ed = app.meta_edit.as_ref().unwrap();
        // Title from the filename; author placeholder "Unknown" → empty; the
        // Cover query is the same clean title, not the ID-like metadata title.
        assert_eq!(ed.lookup.name, "Building Chatbots with Python");
        assert_eq!(ed.lookup.author, "");
        assert_eq!(ed.cover_search.q, "Building Chatbots with Python");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The Lookup/Cover searches re-seed from the current Details title/author
    // when those change (e.g. after `x` extract), not the stale open-time seed.
    #[test]
    fn search_reseeds_from_edited_details() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_reseed_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/k.epub",
                    "503392068",
                    "Unknown",
                    None,
                    1,
                    1,
                    1,
                    "",
                    None,
                    "",
                    "",
                    "",
                    "",
                )
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e'));
        // Simulate `x` filling the Details with real values.
        {
            let ed = app.meta_edit.as_mut().unwrap();
            ed.values[0] = "Building Chatbots with Python".into();
            ed.values[1] = "Sumit Raj".into();
        }
        app.on_key(key('2')); // → Cover: re-seeds from the new Details
        let ed = app.meta_edit.as_ref().unwrap();
        assert_eq!(ed.cover_search.q, "Building Chatbots with Python Sumit Raj");
        assert_eq!(ed.lookup.name, "Building Chatbots with Python");
        assert_eq!(ed.lookup.author, "Sumit Raj");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Editing a multi-selection steps through each book: ^S saves + advances,
    // and the editor closes after the last.
    #[test]
    fn bulk_edit_steps_through_selection() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_bulkedit_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            for (p, t) in [("/a.epub", "A"), ("/b.epub", "B"), ("/c.epub", "C")] {
                store
                    .upsert_book(p, t, "Auth", None, 1, 1, 1, "", None, "", "", "", "")
                    .unwrap();
            }
        }

        let mut app = App::library();
        assert_eq!(app.lib_books.len(), 3);
        app.on_key(key('V')); // visual from book 0
        app.on_key(key('j')); // extend to book 1
        assert_eq!(app.lib_marked.len(), 2);

        app.on_key(key('e')); // start the bulk edit
        assert!(app.meta_edit.is_some());
        assert_eq!(app.edit_total, 2);
        assert_eq!(app.edit_queue.len(), 1, "one book still queued");
        let first = app.meta_edit.as_ref().unwrap().path.clone();

        // ^S → confirm → save and advance to the next book.
        app.on_key(ctrl('s'));
        app.on_key(key('y'));
        assert!(app.meta_edit.is_some(), "advanced to the next book");
        assert_eq!(app.edit_queue.len(), 0);
        let second = app.meta_edit.as_ref().unwrap().path.clone();
        assert_ne!(first, second);

        // ^S on the last → confirm → editor closes, queue reset.
        app.on_key(ctrl('s'));
        app.on_key(key('y'));
        assert!(app.meta_edit.is_none(), "editor closes after the last book");
        assert_eq!(app.edit_total, 0);
    }

    // Esc skips to the next book in a bulk edit (rather than closing).
    #[test]
    fn bulk_edit_esc_skips_to_next() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_bulkskip_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            for (p, t) in [("/a.epub", "A"), ("/b.epub", "B")] {
                store
                    .upsert_book(p, t, "Auth", None, 1, 1, 1, "", None, "", "", "", "")
                    .unwrap();
            }
        }

        let mut app = App::library();
        app.on_key(key('A')); // select all
        assert_eq!(app.lib_marked.len(), 2);
        app.on_key(key('e'));
        assert!(app.meta_edit.is_some());
        app.on_key(code(KeyCode::Esc)); // skip first → next opens
        assert!(app.meta_edit.is_some(), "Esc advanced, not closed");
        assert_eq!(app.edit_queue.len(), 0);
        app.on_key(code(KeyCode::Esc)); // skip last → closes
        assert!(app.meta_edit.is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
