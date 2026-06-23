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

use crate::config::{Config, LibLayout};
use crate::document::epub::{self, EpubDocument};
use crate::document::epub_write;
use crate::input::{self, Action, Pending};
use crate::library;
use crate::media::{self, ImageBuilder, ImageView};
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

/// The active library view: one of the fixed smart sections, or a user
/// collection (shelf). Tab cycles through the sections then the collections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibView {
    Section(LibrarySection),
    Shelf(String),
}

impl LibView {
    /// Display label for the status bar.
    pub fn label(&self) -> String {
        match self {
            LibView::Section(s) => s.label().to_string(),
            LibView::Shelf(name) => name.clone(),
        }
    }
}

/// Which library pane has the keyboard. Tab cycles through the visible ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibPane {
    Sidebar,
    List,
    Detail,
}

/// Sidebar width bounds (cells).
pub const SIDEBAR_W_MIN: u16 = 16;
pub const SIDEBAR_W_MAX: u16 = 44;
/// Detail-pane width bounds (cells).
pub const DETAIL_W_MIN: u16 = 24;
pub const DETAIL_W_MAX: u16 = 60;

/// How the book list is sorted. `Default` keeps each section's natural order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Default,
    Title,
    Author,
    Year,
    Progress,
    Size,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Default => "default",
            SortKey::Title => "title",
            SortKey::Author => "author",
            SortKey::Year => "year",
            SortKey::Progress => "progress",
            SortKey::Size => "size",
        }
    }

    /// Cycle to the next sort key (wraps through `Default`).
    pub fn next(self) -> SortKey {
        match self {
            SortKey::Default => SortKey::Title,
            SortKey::Title => SortKey::Author,
            SortKey::Author => SortKey::Year,
            SortKey::Year => SortKey::Progress,
            SortKey::Progress => SortKey::Size,
            SortKey::Size => SortKey::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Content,
    Sidebar,
}

/// Open annotations (bookmarks/notes) overlay state.
pub struct AnnotState {
    pub items: Vec<Annotation>,
    pub sel: usize,
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
    /// Active note-entry buffer, if typing a note.
    pub note_input: Option<String>,
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
    pub image_view: Option<ImageView>,
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
            note_input: None,
            meta_edit: None,
            bulk_rename: None,
            lib_coll_edit: None,
            pending_confirm: None,
            edit_queue: Vec::new(),
            edit_total: 0,
            image_view: None,
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
            library::scan(&config.library_paths, s);
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
            note_input: None,
            meta_edit: None,
            bulk_rename: None,
            lib_coll_edit: None,
            pending_confirm: None,
            edit_queue: Vec::new(),
            edit_total: 0,
            image_view: None,
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

    fn refresh_library(&mut self) {
        let Some(store) = &self.store else {
            self.lib_books.clear();
            self.lib_shelves.clear();
            return;
        };
        // Computed while the immutable `store` borrow is live, assigned after.
        let shelves = store.all_shelves();
        // If the active collection just lost its last book it no longer exists;
        // fall back to All so the view and sidebar stay consistent.
        let view = match &self.lib_view {
            LibView::Shelf(name) if !shelves.iter().any(|(n, _)| n == name) => {
                LibView::Section(LibrarySection::All)
            }
            v => v.clone(),
        };
        let f = self.lib_filter.to_lowercase();
        let books = if f.is_empty() {
            match &view {
                LibView::Section(s) => store.list_books(*s),
                LibView::Shelf(name) => store.books_in_shelf(name),
            }
        } else {
            // Library-wide search: title/author/series/publisher substring OR
            // full-text match (ignores the active section, by design).
            let fts: HashSet<String> = store.fts_paths(&self.lib_filter).into_iter().collect();
            store
                .all_books()
                .into_iter()
                .filter(|b| {
                    b.title.to_lowercase().contains(&f)
                        || b.author.to_lowercase().contains(&f)
                        || b.series.to_lowercase().contains(&f)
                        || b.publisher.to_lowercase().contains(&f)
                        || fts.contains(&b.path)
                })
                .collect()
        };
        self.lib_shelves = shelves;
        self.lib_view = view;
        self.lib_books = books;
        self.sort_books();
        if self.lib_sel >= self.lib_books.len() {
            self.lib_sel = self.lib_books.len().saturating_sub(1);
        }
    }

    /// Apply the active sort key/direction to the loaded book list. `Default`
    /// keeps the section's own order.
    fn sort_books(&mut self) {
        if self.lib_sort == SortKey::Default {
            return;
        }
        let key = self.lib_sort;
        let desc = self.lib_sort_desc;
        self.lib_books.sort_by(|a, b| {
            let ord = match key {
                SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                SortKey::Author => a.author.to_lowercase().cmp(&b.author.to_lowercase()),
                SortKey::Year => a.year.cmp(&b.year),
                SortKey::Progress => a.pct.cmp(&b.pct),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Default => std::cmp::Ordering::Equal,
            };
            if desc { ord.reverse() } else { ord }
        });
    }

    /// Cycle the sort key (`s`) keeping the selected book in view.
    fn cycle_sort(&mut self) {
        self.lib_exit_visual();
        self.lib_sort = self.lib_sort.next();
        self.refresh_library();
    }

    /// Flip the sort direction (`S`).
    fn toggle_sort_dir(&mut self) {
        self.lib_exit_visual();
        self.lib_sort_desc = !self.lib_sort_desc;
        self.refresh_library();
    }

    fn lib_move(&mut self, delta: isize) {
        if self.lib_books.is_empty() {
            return;
        }
        let last = self.lib_books.len() as isize - 1;
        self.lib_sel = (self.lib_sel as isize + delta).clamp(0, last) as usize;
    }

    fn lib_favorite(&mut self) {
        if let (Some(store), Some(book)) = (&self.store, self.lib_books.get(self.lib_sel)) {
            store.set_favorite(&book.path, !book.favorite);
        }
        self.refresh_library();
    }

    /// The book path the detail cover should show (empty when no cover pane is
    /// relevant, so we treat it as "nothing to do").
    fn cover_target_path(&self) -> String {
        if self.mode != Mode::Library
            || !self.lib_detail
            || self.config.library_layout == LibLayout::Grid
        {
            return self.lib_cover_path.clone();
        }
        self.lib_books
            .get(self.lib_sel)
            .map(|b| b.path.clone())
            .unwrap_or_default()
    }

    /// Is the detail cover stale (wants rebuilding)? Keeps the loop ticking.
    pub fn cover_pending(&self) -> bool {
        self.cover_target_path() != self.lib_cover_path
    }

    /// Debounced detail-cover build: only (re)decode once the selection has held
    /// still briefly, so holding j/k never pays the per-book zip-read + decode.
    /// Returns whether the cover changed (the loop should redraw).
    pub fn tick_cover(&mut self) -> bool {
        let target = self.cover_target_path();
        if target == self.lib_cover_path {
            return false;
        }
        if target != self.lib_cover_target {
            // Selection moved — restart the settle timer, build nothing yet.
            self.lib_cover_target = target;
            self.lib_cover_at = Instant::now();
            return false;
        }
        if self.lib_cover_at.elapsed() < COVER_DEBOUNCE {
            return false;
        }
        self.lib_cover_path = target.clone();
        self.lib_cover = match (&self.picker, load_cover_bytes(&target)) {
            (Some(picker), Some(bytes)) => media::build_cover(picker, &bytes),
            _ => None,
        };
        true
    }

    /// Build cover protocols for the visible grid `paths`, up to `limit` per
    /// call so a screenful pops in over a few frames instead of freezing. Sets
    /// `lib_grid_pending` while any visible cover is still unbuilt.
    pub fn ensure_grid_covers(&mut self, paths: &[String], limit: usize) {
        let mut built = 0;
        let mut pending = false;
        for path in paths {
            if self.lib_grid_covers.contains_key(path) {
                continue;
            }
            if built >= limit {
                pending = true;
                break;
            }
            let cover = match (&self.picker, load_cover_bytes(path)) {
                (Some(picker), Some(bytes)) => media::build_cover(picker, &bytes),
                _ => None,
            };
            self.lib_grid_covers.insert(path.clone(), cover);
            built += 1;
        }
        self.lib_grid_pending = pending;
    }

    /// Whether the grid is still building visible covers (keeps the loop drawing).
    pub fn lib_grid_pending(&self) -> bool {
        self.mode == Mode::Library
            && self.config.library_layout == LibLayout::Grid
            && self.lib_grid_pending
    }

    pub fn is_grid(&self) -> bool {
        self.config.library_layout == LibLayout::Grid
    }

    /// Vertical step for j/k: one grid row in grid view, else one list row.
    fn grid_step(&self) -> isize {
        if self.is_grid() {
            self.lib_grid_cols.max(1) as isize
        } else {
            1
        }
    }

    /// Visible panes, left → right, given show flags and the active layout.
    fn lib_visible_panes(&self) -> Vec<LibPane> {
        let mut panes = Vec::new();
        if self.lib_show_sidebar {
            panes.push(LibPane::Sidebar);
        }
        panes.push(LibPane::List);
        // The detail pane only exists alongside the list views.
        if self.lib_detail && self.config.library_layout != LibLayout::Grid {
            panes.push(LibPane::Detail);
        }
        panes
    }

    /// Move the keyboard focus to the next/previous visible pane.
    fn lib_cycle_pane(&mut self, delta: isize) {
        let panes = self.lib_visible_panes();
        if panes.is_empty() {
            return;
        }
        let cur = panes.iter().position(|p| *p == self.lib_pane).unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(panes.len() as isize) as usize;
        self.lib_pane = panes[next];
    }

    /// Keep the focused pane valid when one is hidden.
    fn lib_ensure_pane_visible(&mut self) {
        if !self.lib_visible_panes().contains(&self.lib_pane) {
            self.lib_pane = LibPane::List;
        }
    }

    /// Grow/shrink the focused side pane (`[`/`]`); the list takes the slack.
    fn lib_resize(&mut self, delta: i16) {
        match self.lib_pane {
            LibPane::Sidebar => {
                self.lib_sidebar_w = (self.lib_sidebar_w as i16 + delta)
                    .clamp(SIDEBAR_W_MIN as i16, SIDEBAR_W_MAX as i16)
                    as u16;
            }
            LibPane::Detail => {
                self.lib_detail_w = (self.lib_detail_w as i16 + delta)
                    .clamp(DETAIL_W_MIN as i16, DETAIL_W_MAX as i16)
                    as u16;
            }
            LibPane::List => {}
        }
    }

    /// Total entries in the sidebar (fixed sections + collections).
    fn lib_view_count(&self) -> usize {
        LibrarySection::ALL.len() + self.lib_shelves.len()
    }

    /// Select the sidebar entry at ring index `i` (clamped) and load its books.
    /// Index `lib_view_count()` is the trailing "＋ New collection" row, which
    /// parks the cursor without changing the view.
    fn lib_set_view_index(&mut self, i: usize) {
        let total = self.lib_view_count();
        if total == 0 {
            return;
        }
        self.lib_exit_visual();
        if i >= total {
            self.lib_side_new = true; // parked on "＋ New collection"
            return;
        }
        self.lib_side_new = false;
        self.lib_view = self.lib_view_at(i);
        self.lib_sel = 0;
        self.refresh_library();
    }

    /// Move the sidebar cursor by `delta` (clamped), switching the view live.
    /// The cursor ranges over the views plus the trailing "＋ New" row.
    fn lib_side_move(&mut self, delta: isize) {
        let max = self.lib_view_count(); // index of "＋ New collection"
        let cur = if self.lib_side_new {
            max
        } else {
            self.lib_view_index()
        };
        let next = (cur as isize + delta).clamp(0, max as isize) as usize;
        self.lib_set_view_index(next);
    }

    /// Position of the active view within the section+collection ring.
    fn lib_view_index(&self) -> usize {
        let n = LibrarySection::ALL.len();
        match &self.lib_view {
            LibView::Section(s) => LibrarySection::ALL.iter().position(|x| x == s).unwrap_or(0),
            LibView::Shelf(name) => self
                .lib_shelves
                .iter()
                .position(|(nm, _)| nm == name)
                .map(|p| n + p)
                .unwrap_or(0),
        }
    }

    /// The view at ring index `i` (sections first, then collections).
    fn lib_view_at(&self, i: usize) -> LibView {
        let n = LibrarySection::ALL.len();
        if i < n {
            LibView::Section(LibrarySection::ALL[i])
        } else {
            LibView::Shelf(self.lib_shelves[i - n].0.clone())
        }
    }

    fn open_selected(&mut self) {
        let Some(path) = self.lib_books.get(self.lib_sel).map(|b| b.path.clone()) else {
            return;
        };
        self.flush_reading_time();
        if let Ok((reader, config, book_path)) = build_reader(&path, &self.store) {
            self.reader = Some(reader);
            self.config = config;
            self.book_path = book_path;
            self.mode = Mode::Reader;
            self.session_start = Some(Instant::now());
            if let Some(s) = &self.store {
                s.mark_opened(&self.book_path);
            }
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
        if self.note_input.is_some() {
            self.note_key(key);
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
        if self.mode == Mode::Reader && key.code == KeyCode::Char('i') {
            self.open_images();
            return;
        }
        if self.mode == Mode::Reader && self.reader.as_ref().is_some_and(|r| r.searching) {
            self.search_key(key);
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
        let (Some(picker), Some(reader)) = (self.picker.as_ref(), self.reader.as_mut()) else {
            return;
        };
        let images = reader.doc.section_images(reader.section);
        self.image_view = ImageView::new(picker, &images);
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

    fn note_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.note_input = None,
            KeyCode::Enter => {
                if let Some(text) = self.note_input.take()
                    && let (Some(store), Some(r)) = (&self.store, &self.reader)
                    && !self.book_path.is_empty()
                {
                    store.add_annotation(
                        &self.book_path,
                        r.section,
                        &r.current_quote(),
                        text.trim(),
                    );
                }
            }
            KeyCode::Backspace => {
                if let Some(s) = self.note_input.as_mut() {
                    s.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(s) = self.note_input.as_mut() {
                    s.push(c);
                }
            }
            _ => {}
        }
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
            KeyCode::Char('d') => {
                let id = self
                    .annot
                    .as_ref()
                    .and_then(|a| a.items.get(a.sel))
                    .map(|i| i.id);
                if let (Some(id), Some(store)) = (id, &self.store) {
                    store.delete_annotation(id);
                    let items = store.list_annotations(&self.book_path);
                    if let Some(a) = self.annot.as_mut() {
                        a.items = items;
                        if a.sel >= a.items.len() {
                            a.sel = a.items.len().saturating_sub(1);
                        }
                    }
                }
            }
            _ => {}
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

    fn library_key(&mut self, key: KeyEvent) {
        if self.lib_filtering {
            match key.code {
                KeyCode::Esc => {
                    self.lib_filter.clear();
                    self.lib_filtering = false;
                    self.refresh_library();
                }
                KeyCode::Enter => self.lib_filtering = false,
                KeyCode::Backspace => {
                    self.lib_filter.pop();
                    self.refresh_library();
                }
                KeyCode::Char(c) => {
                    self.lib_filter.push(c);
                    self.refresh_library();
                }
                _ => {}
            }
            return;
        }
        let pane = self.lib_pane;
        let grid = self.is_grid();
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Esc => {
                if self.lib_visual.is_some() || !self.lib_marked.is_empty() {
                    self.lib_exit_visual();
                } else if self.lib_filter.is_empty() {
                    self.should_quit = true;
                } else {
                    self.lib_filter.clear();
                    self.refresh_library();
                }
            }
            // Select: Space toggles individual books; V is a vim-style range;
            // A selects all (e.g. to bulk-rename/sanitize the whole library).
            KeyCode::Char(' ') => self.lib_toggle_mark(),
            KeyCode::Char('V') => self.lib_toggle_visual(),
            KeyCode::Char('A') => self.lib_mark_all(),
            // Tab cycles focus through the visible panes.
            KeyCode::Tab => self.lib_cycle_pane(1),
            KeyCode::BackTab => self.lib_cycle_pane(-1),
            // Up/down: sidebar cursor, or one list row (grid rows are `cols` wide).
            KeyCode::Char('j') | KeyCode::Down => match pane {
                LibPane::Sidebar => self.lib_side_move(1),
                LibPane::List => self.lib_move(self.grid_step()),
                LibPane::Detail => {}
            },
            KeyCode::Char('k') | KeyCode::Up => match pane {
                LibPane::Sidebar => self.lib_side_move(-1),
                LibPane::List => self.lib_move(-self.grid_step()),
                LibPane::Detail => {}
            },
            // Left/right: move a grid cell when browsing the grid, else move the
            // pane focus left/right.
            KeyCode::Char('h') | KeyCode::Left => {
                if pane == LibPane::List && grid {
                    self.lib_move(-1)
                } else {
                    self.lib_cycle_pane(-1)
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if pane == LibPane::List && grid {
                    self.lib_move(1)
                } else {
                    self.lib_cycle_pane(1)
                }
            }
            // Enter: from the sidebar step into the list (or create a collection
            // when parked on the "＋ New" row); else open the book.
            KeyCode::Enter => {
                if pane == LibPane::Sidebar {
                    if self.lib_side_new {
                        self.lib_coll_begin_new();
                    } else {
                        self.lib_pane = LibPane::List;
                    }
                } else {
                    self.open_selected();
                }
            }
            KeyCode::Char('o') => self.open_selected(),
            KeyCode::Char('g') => match pane {
                LibPane::Sidebar => self.lib_set_view_index(0),
                _ => self.lib_sel = 0,
            },
            KeyCode::Char('G') => match pane {
                LibPane::Sidebar => {
                    self.lib_set_view_index(self.lib_view_count().saturating_sub(1))
                }
                _ => self.lib_sel = self.lib_books.len().saturating_sub(1),
            },
            // Resize the focused side pane (Shift+</>); show/hide sidebar/detail.
            KeyCode::Char('<') => self.lib_resize(-2),
            KeyCode::Char('>') => self.lib_resize(2),
            KeyCode::Char('b') => {
                self.lib_show_sidebar = !self.lib_show_sidebar;
                self.lib_ensure_pane_visible();
            }
            KeyCode::Char('d') => {
                self.lib_detail = !self.lib_detail;
                self.lib_ensure_pane_visible();
            }
            // Book actions operate on the selected book regardless of focus.
            KeyCode::Char('f') => {
                if self.lib_marked.is_empty() {
                    self.lib_favorite()
                } else {
                    self.bulk_favorite()
                }
            }
            // `e` edits the current book; with a selection, edits each in turn.
            KeyCode::Char('e') => {
                if self.lib_marked.is_empty() {
                    self.open_meta_edit();
                } else {
                    self.start_bulk_edit();
                }
            }
            KeyCode::Char('c') => self.open_shelf_picker(),
            // `r` renames: the focused collection in place (sidebar), else the
            // selected book(s) — the current one when nothing is marked.
            KeyCode::Char('r')
                if pane == LibPane::Sidebar
                    && !self.lib_side_new
                    && matches!(self.lib_view, LibView::Shelf(_)) =>
            {
                self.lib_coll_begin_rename()
            }
            KeyCode::Char('r') => self.open_bulk_rename(),
            KeyCode::Char('x') => self.remove_from_current_shelf(),
            KeyCode::Char('v') => {
                self.config.library_layout = self.config.library_layout.next();
                self.config.save();
                self.lib_ensure_pane_visible();
            }
            // Grid view: grow/shrink the cover cards.
            KeyCode::Char('+') | KeyCode::Char('=') if grid => {
                self.config.library_grid_size = self.config.library_grid_size.next();
                self.config.save();
            }
            KeyCode::Char('-') | KeyCode::Char('_') if grid => {
                self.config.library_grid_size = self.config.library_grid_size.prev();
                self.config.save();
            }
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('S') => self.toggle_sort_dir(),
            KeyCode::Char('t') => {
                self.config.theme = self.config.theme.next();
                self.config.save();
            }
            KeyCode::Char('/') => {
                self.lib_exit_visual();
                self.lib_filtering = true;
            }
            _ => {}
        }
        // After any movement, extend the visual-mode range to the new cursor.
        self.lib_visual_sync();
    }

    // --- Inline sidebar collection editing ------------------------------

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
            Action::Down(n) => match reader.focus {
                Focus::Content => reader.queue_scroll(n as isize),
                Focus::Sidebar => reader.sidebar_move(n as isize),
            },
            Action::Up(n) => match reader.focus {
                Focus::Content => reader.queue_scroll(-(n as isize)),
                Focus::Sidebar => reader.sidebar_move(-(n as isize)),
            },
            Action::HalfDown => reader.scroll_down(reader.page_lines.max(2) / 2),
            Action::HalfUp => reader.scroll_up(reader.page_lines.max(2) / 2),
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
                }
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
                    store.add_annotation(
                        &self.book_path,
                        reader.section,
                        &reader.current_quote(),
                        "",
                    );
                }
            }
            Action::AddNote => self.note_input = Some(String::new()),
            Action::OpenAnnotations => {
                if let Some(store) = &self.store {
                    let items = store.list_annotations(&self.book_path);
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
            Action::NextChapter => reader.next_chapter(),
            Action::PrevChapter => reader.prev_chapter(),
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

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(app.lib_sidebar_w, SIDEBAR_W_MIN, "clamped at the minimum");

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
