//! Application state and event dispatch.
//!
//! Two top-level modes behaving like tabs (Library | Reader). For now the
//! Library is a stub; the Reader is the working EPUB vertical slice. See
//! `DESIGN.md` §4, §6.

use std::sync::mpsc::Receiver;
use std::time::Instant;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::config::{Config, ViewMode};
use crate::document::epub::{self, EpubDocument};
use crate::document::epub_write;
use crate::input::{self, Action, Pending};
use crate::media::{self, ImageBuilder};
use crate::online;
use crate::store::{Annotation, Store};
use ratatui_image::picker::Picker;

mod confirm;
pub use confirm::PendingConfirm;

mod settings;
pub use settings::{
    SettingItem, SettingRow, SettingTab, Settings, first_setting_row, settings_tabs, tab_rows,
    visible_rows,
};

mod mouse;
pub use mouse::{LayoutMetrics, MouseHits};

mod rename;
pub use rename::{BulkRename, BulkTarget};

mod select;

mod collections;
pub use collections::{CollInput, ShelfPicker};

mod tags;
pub use tags::TagInput;

mod dup_resolve;
mod dup_scan;
mod scan;
pub use dup_resolve::{DupGroup, DupMember, DupResolve, IgnoredView};

mod editor;
pub use editor::{
    DiffRow, EditMode, EditTab, LOOKUP_FIELDS, LookupForm, META_FIELDS, MetaDiff, MetaEdit,
    ONLINE_LIMIT, OnlineMsg, Search,
};

mod reader;
pub use page_deck::PageTarget;
pub use reader::{
    AnchorHit, Hint, HintKind, HintStart, ImageGeom, PageView, PanRoom, Reader, Viewport,
    place_page, raster_width_for_crispness,
};

mod image_view;
pub use image_view::{Figure, ImageViewer};

mod cover_loader;
mod library;
pub use library::{LibPane, LibView, SortKey};

mod state;
pub use state::Overlay;
use state::{LibraryState, Session};

mod dispatch;

mod palette;
pub use palette::{Command, Palette, PaletteItem};

mod word_lookup;
pub use word_lookup::{LookupState, WordLookup};

mod code_view;
pub use code_view::{CodeFocus, CodeSnippet, CodeView};

mod page_deck;
use page_deck::PageDeck;
pub(crate) mod inline_deck;
use inline_deck::InlineDeck;

/// Point the reader's persistent equation-image ink-profile cache at `<root>/ink-vN`
/// (typically `<config>/rasters`). Call once at startup, before any book opens, so the
/// background section loader and the start-section decode both consult it. `None` (or a
/// create failure) leaves ink caching off. Thin re-export so the binary can reach the
/// otherwise crate-private reader module.
pub fn reader_ink_cache_set_dir(root: Option<std::path::PathBuf>) {
    reader::ink_cache::set_dir(root);
}

/// How long the library selection must hold still before the detail-pane cover
/// is (re)built, so holding j/k stays smooth.
const COVER_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(110);

/// Number of decoded sections kept in memory (current ± neighbours). Sized to hold
/// a PDF's full pre-rasterization window (continuous scroll prefetches ± 6 pages) so
/// the direct-Kitty deck can transmit them ahead for fast navigation without the
/// cache thrashing a page it just rasterized.
const CACHE_CAP: usize = 15;
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

/// The three tabs of the annotations overlay: bookmarks (places), notes
/// (commentary), and highlights (coloured marks) are kept in separate lists, not
/// mixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotTab {
    Bookmarks,
    Notes,
    Highlights,
}

impl AnnotTab {
    /// The tabs in display / cycle order.
    pub const ALL: [AnnotTab; 3] = [AnnotTab::Bookmarks, AnnotTab::Notes, AnnotTab::Highlights];

    /// The next tab (wraps) — `⇥` / `→` in the overlay.
    pub fn next(self) -> AnnotTab {
        let i = Self::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The previous tab (wraps) — `⇤` / `←` in the overlay.
    pub fn prev(self) -> AnnotTab {
        let i = Self::ALL.iter().position(|&t| t == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    /// The tab an annotation belongs on (its `kind`).
    pub fn of(a: &Annotation) -> AnnotTab {
        if a.is_highlight() {
            AnnotTab::Highlights
        } else if a.is_note() {
            AnnotTab::Notes
        } else {
            AnnotTab::Bookmarks
        }
    }

    /// Whether an annotation belongs on this tab.
    fn accepts(self, a: &Annotation) -> bool {
        AnnotTab::of(a) == self
    }

    pub fn label(self) -> &'static str {
        match self {
            AnnotTab::Bookmarks => "Bookmarks",
            AnnotTab::Notes => "Notes",
            AnnotTab::Highlights => "Highlights",
        }
    }
}

/// Open annotations overlay state: the full annotation list, the active tab
/// (bookmarks vs notes), a cursor into the current tab's filtered view, and a
/// search filter.
pub struct AnnotState {
    pub items: Vec<Annotation>,
    /// The active tab.
    pub tab: AnnotTab,
    /// Cursor index into the *filtered* view (see [`AnnotState::filtered`]).
    pub sel: usize,
    /// Current search text (`''` = show everything in the tab).
    pub filter: String,
    /// Whether keystrokes are being typed into the filter.
    pub filtering: bool,
}

impl AnnotState {
    /// A fresh state showing `items` on the given tab, no filter.
    pub fn new(items: Vec<Annotation>, tab: AnnotTab) -> AnnotState {
        AnnotState {
            items,
            tab,
            sel: 0,
            filter: String::new(),
            filtering: false,
        }
    }

    /// The active tab's items matching the current filter — a case-insensitive
    /// substring over the name, quote, note body, and folder.
    pub fn filtered(&self) -> Vec<&Annotation> {
        let tab = self.tab;
        let needle = self.filter.to_lowercase();
        self.items
            .iter()
            .filter(|a| tab.accepts(a))
            .filter(|a| {
                self.filter.is_empty()
                    || a.name.to_lowercase().contains(&needle)
                    || a.quote.to_lowercase().contains(&needle)
                    || a.note.to_lowercase().contains(&needle)
                    || a.folder.to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// How many annotations belong to `tab` (ignoring the filter) — for the tab
    /// bar's counts.
    pub fn count(&self, tab: AnnotTab) -> usize {
        self.items.iter().filter(|a| tab.accepts(a)).count()
    }

    /// The annotation the cursor is on (within the filtered view).
    pub fn selected(&self) -> Option<Annotation> {
        self.filtered().get(self.sel).map(|a| (*a).clone())
    }
}

/// What a one-line text prompt's typed text becomes when committed.
pub enum PromptKind {
    /// The custom name of annotation `id` (empty clears it back to the quote).
    Name(i64),
    /// The folder annotation `id` belongs to (empty = ungrouped).
    Folder(i64),
    /// Commentary for a new note being created at `(section, quote)` in the reader.
    NewNote { section: usize, quote: String },
    /// New commentary for existing note `id`.
    EditNote(i64),
}

/// A one-line text prompt shown at the bottom of the reader (rename a bookmark /
/// file it into a folder).
pub struct Prompt {
    pub kind: PromptKind,
    pub input: crate::ui::TextInput,
}

pub struct App {
    pub mode: Mode,
    pub config: Config,
    pub reader: Option<Reader>,
    pub last_layout: LayoutMetrics,
    /// Clickable regions from the last render (mouse hit-testing).
    pub mouse: MouseHits,
    /// The last library book clicked and when — for double-click-to-open detection.
    pub last_click: Option<(usize, Instant)>,
    pub pending: Pending,
    pub should_quit: bool,
    /// The single open overlay/popup (settings, prompt, metadata editor,
    /// bookmarks, palette, …), or [`Overlay::None`]. Exactly one is open at a
    /// time, so the borrow checker — and the code — can't represent two at once.
    /// `pending_confirm` (modal above any overlay) and `dup_preview` (the parked
    /// resolver) stay separate fields below.
    pub overlay: Overlay,
    /// Whether bordered overlay windows open at the larger size (toggled with
    /// `f`); a single session-wide preference so every popup is sized the same.
    pub overlay_large: bool,
    /// The duplicate-resolution overlay stashed while previewing a book from it —
    /// kept out of `overlay` so the dispatcher and renderer ignore it during the
    /// preview; restored when the reader returns (`q`/Esc).
    pub dup_preview: Option<DupResolve>,
    /// A destructive action awaiting a yes/no confirmation, if any. Intercepts
    /// input ahead of every popup and is answered with y/⏎ or n/Esc.
    pub pending_confirm: Option<PendingConfirm>,
    /// Remaining book paths to edit after the current one, when editing a
    /// multi-selection one book at a time (`^S` saves+advances, `Esc` skips).
    pub edit_queue: Vec<String>,
    /// Total books in the current edit queue (for the `N/total` header).
    pub edit_total: usize,
    /// An image queued for the system clipboard (`(w, h, RGBA)`), set by the
    /// viewer's copy action and drained by the main loop.
    pub pending_clipboard_image: Option<(u32, u32, Vec<u8>)>,
    /// Terminal image ids left behind by a *closed* image viewer (its last shown
    /// figure), merged into [`take_image_deletes`](Self::take_image_deletes). The
    /// still-open viewer is drained directly from its overlay each frame; this
    /// only carries ids across the drop when the overlay is torn down.
    overlay_image_deletes: Vec<u32>,
    /// Detected terminal image protocol (None if unsupported / headless).
    pub picker: Option<Picker>,
    /// Background builder for inline-image protocols.
    pub image_builder: Option<ImageBuilder>,
    /// Direct-Kitty manager for full PDF page images (transmit-once + place).
    page_deck: PageDeck,
    /// Direct-Kitty manager for the reader's inline images — equation rasters and
    /// inline figures (transmit-once + place + re-place on scroll + free on leave).
    inline_deck: InlineDeck,
    /// The open book, its persistence handle, and session start — see [`Session`].
    session: Session,
    /// Library list, selection, sort, filter, and covers — see [`LibraryState`].
    pub library: LibraryState,
    /// Background loader for library covers (load + decode off the render loop).
    cover_loader: cover_loader::CoverLoader,
    /// Receiver for async Open Library results (search / cover), if a request
    /// from the editor's Online tab is in flight.
    pub online_rx: Option<Receiver<OnlineMsg>>,
    /// Receiver for an in-flight word lookup (dictionary + Wikipedia), while the
    /// `K` lookup panel is fetching. See `app/word_lookup.rs`.
    pub define_rx: Option<Receiver<online::LookupResult>>,
    /// In-flight thorough duplicate scan (cover hashing on a worker thread), if
    /// the reader triggered one from the Duplicates view.
    pub dup_scan: Option<dup_scan::DupScan>,
    /// In-flight background library scan (folder (re)indexing on a worker thread),
    /// so a large scan never blocks the UI. See `app/scan.rs`.
    pub scan: Option<scan::ScanJob>,
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

/// Cover image bytes for a book: a fetched cover from the cache if present, else
/// the file's own cover — an EPUB's embedded cover, or a PDF's first page
/// rendered to an image (and cached, since rasterizing is comparatively dear).
/// `None` if none is available.
fn load_cover_bytes(path: &str) -> Option<Vec<u8>> {
    if path.is_empty() {
        return None;
    }
    let cache = online::cover_cache_path(path);
    match std::fs::read(&cache) {
        Ok(bytes) if !bytes.is_empty() => return Some(bytes),
        _ => {}
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if ext == "pdf" {
        // A PDF has no embedded cover; render its first page, then cache it so we
        // don't re-rasterize next session. (A later online fetch overwrites it.)
        let bytes = crate::document::pdf::render_cover(path)?;
        if let Some(dir) = cache.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&cache, &bytes);
        return Some(bytes);
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

/// Build a reader for `path`, applying global config and any saved per-book
/// overrides (theme, view mode, resume position).
fn build_reader(
    path: &str,
    store: &Option<Store>,
    has_graphics: bool,
) -> Result<(Reader, Config, String)> {
    // Dispatch by file format. EPUB and PDF have `Document` backends; other
    // recognized formats report cleanly here instead of failing deep in a parser
    // with a cryptic error. See the Phase 5 plan in `TODO.md`.
    let fmt = crate::document::BookFormat::from_path(path);
    if !fmt.is_readable() {
        anyhow::bail!(
            "{} files aren't supported — EPUB, PDF, and MOBI/AZW3 open",
            fmt.label()
        );
    }
    // PDF renders each page as an image, so it needs a graphics-capable terminal
    // (Kitty/iTerm2/sixel). Report cleanly rather than opening to blank pages.
    if fmt == crate::document::BookFormat::Pdf && !has_graphics {
        anyhow::bail!("PDF needs a graphics-capable terminal (e.g. Ghostty, Kitty, iTerm2)");
    }
    let doc: Box<dyn crate::document::Document> = match fmt {
        crate::document::BookFormat::Pdf => {
            Box::new(crate::document::pdf::PdfDocument::open(path)?)
        }
        crate::document::BookFormat::Mobi | crate::document::BookFormat::Azw3 => {
            Box::new(crate::document::mobi::MobiDocument::open(path)?)
        }
        _ => Box::new(EpubDocument::open(path)?),
    };
    let mut reader = Reader::new(doc)?;
    let mut config = Config::load();
    let book_path = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    if let Some(store) = store
        && let Some(p) = store.load_progress(&book_path)
    {
        config.view_mode = p.view_mode;
        // The theme is a single global setting: a book must never change it — the
        // active theme colours every book. Resume only position + view mode.
        reader.load(p.section);
        reader.pending_frac = Some(p.frac);
    }
    // Seed the gutter with this book's annotations (independent of saved progress).
    if let Some(store) = store {
        reader.set_annotations(store.list_annotations(&book_path));
    }
    Ok((reader, config, book_path))
}

impl App {
    pub fn open_book(path: &str, has_graphics: bool) -> Result<Self> {
        let store = Store::open_default().ok();
        let (reader, config, book_path) = build_reader(path, &store, has_graphics)?;
        if let Some(s) = &store {
            s.mark_opened(&book_path);
        }
        Ok(Self {
            mode: Mode::Reader,
            config,
            reader: Some(reader),
            last_layout: LayoutMetrics::default(),
            mouse: MouseHits::default(),
            last_click: None,
            pending: Pending::default(),
            should_quit: false,
            overlay: Overlay::None,
            overlay_large: false,
            dup_preview: None,
            pending_confirm: None,
            edit_queue: Vec::new(),
            edit_total: 0,
            pending_clipboard_image: None,
            overlay_image_deletes: Vec::new(),
            picker: None,
            image_builder: None,
            page_deck: PageDeck::default(),
            inline_deck: InlineDeck::default(),
            session: Session {
                store,
                book_path,
                started: Some(Instant::now()),
            },
            library: LibraryState::default(),
            cover_loader: cover_loader::CoverLoader::new(),
            online_rx: None,
            define_rx: None,
            dup_scan: None,
            scan: None,
            edit_cover: None,
            edit_cover_url: String::new(),
            edit_cover_target: String::new(),
            edit_cover_at: Instant::now(),
        })
    }

    pub fn library() -> Self {
        let config = Config::load();
        // The library shows immediately from the already-indexed store; the folder
        // (re)scan runs in the background (`start_scan_startup`, kicked off by the
        // caller) so a large scan never delays the first frame.
        let store = Store::open_default().ok();
        let mut app = Self {
            mode: Mode::Library,
            config,
            reader: None,
            last_layout: LayoutMetrics::default(),
            mouse: MouseHits::default(),
            last_click: None,
            pending: Pending::default(),
            should_quit: false,
            overlay: Overlay::None,
            overlay_large: false,
            dup_preview: None,
            pending_confirm: None,
            edit_queue: Vec::new(),
            edit_total: 0,
            pending_clipboard_image: None,
            overlay_image_deletes: Vec::new(),
            picker: None,
            image_builder: None,
            page_deck: PageDeck::default(),
            inline_deck: InlineDeck::default(),
            session: Session {
                store,
                book_path: String::new(),
                started: None,
            },
            library: LibraryState::default(),
            cover_loader: cover_loader::CoverLoader::new(),
            online_rx: None,
            define_rx: None,
            dup_scan: None,
            scan: None,
            edit_cover: None,
            edit_cover_url: String::new(),
            edit_cover_target: String::new(),
            edit_cover_at: Instant::now(),
        };
        app.refresh_library();
        app
    }

    fn open_selected(&mut self) {
        let Some(path) = self
            .library
            .books
            .get(self.library.sel)
            .map(|b| b.path.clone())
        else {
            return;
        };
        self.flush_reading_time();
        match build_reader(&path, &self.session.store, self.picker.is_some()) {
            Ok((reader, config, book_path)) => {
                self.reader = Some(reader);
                self.config = config;
                self.session.book_path = book_path;
                self.mode = Mode::Reader;
                self.session.started = Some(Instant::now());
                if let Some(s) = &self.session.store {
                    s.mark_opened(&self.session.book_path);
                }
            }
            // Surface the reason (e.g. an unsupported format) on the status row
            // rather than silently doing nothing on Enter.
            Err(e) => self.library.flash = Some(e.to_string()),
        }
    }

    /// Persist the current reading position (best-effort).
    pub fn save_progress(&self) {
        if let (Some(store), Some(reader)) = (&self.session.store, &self.reader)
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

    /// Accumulate elapsed reading time into the open book and reset the clock.
    fn flush_reading_time(&mut self) {
        if let (Some(start), Some(store)) = (self.session.started, &self.session.store) {
            let secs = start.elapsed().as_secs() as i64;
            if secs > 0 && !self.session.book_path.is_empty() {
                store.add_read_time(&self.session.book_path, secs);
            }
        }
        if self.session.started.is_some() {
            self.session.started = Some(Instant::now());
        }
    }

    /// Save progress + reading time on quit.
    pub fn on_exit(&mut self) {
        self.flush_reading_time();
        self.save_progress();
    }

    pub fn total_read_seconds(&self) -> i64 {
        self.session
            .store
            .as_ref()
            .map(|s| s.total_read_seconds())
            .unwrap_or(0)
    }

    /// Terminal image ids to delete: covers evicted from the library grid cache, and
    /// figures the image viewer has finished with (superseded while open, or its last
    /// image once closed). The reader's inline/figure images are freed by the InlineDeck
    /// directly (it emits `d=I` when an image leaves the screen), not through this queue.
    pub fn take_image_deletes(&mut self) -> Vec<u32> {
        let mut ids = std::mem::take(&mut self.library.grid_deletes);
        // The open viewer frees each figure it moves off (mode toggle / navigation);
        // a closed viewer's last image is carried across the drop in this queue.
        if let Overlay::ImageView(v) = &mut self.overlay {
            ids.extend(v.take_deletes());
        }
        ids.append(&mut self.overlay_image_deletes);
        ids
    }

    /// Whether an in-place reflow (a code fold/unfold) asked for a full repaint this
    /// frame — the loop clears the terminal so a moved inline image doesn't leave its
    /// old placement behind (terminal graphics don't compose with the cell-diff).
    pub fn take_repaint(&mut self) -> bool {
        self.reader.as_mut().is_some_and(|r| r.take_repaint())
    }

    /// Whether full PDF pages should be on screen right now: reading a PDF with
    /// no overlay open. Kitty images draw *above* the cell grid, so while a popup
    /// is up we take the page down (it's torn down and re-placed on resume) so
    /// the popup isn't hidden behind it.
    fn in_pdf(&self) -> bool {
        self.mode == Mode::Reader
            && !self.modal_open()
            && self.reader.as_ref().is_some_and(|r| r.is_paged_image())
    }

    /// Drain finished page rasterizations each frame (the direct path skips
    /// `sync_images`, which is what otherwise drains them) and report whether the
    /// terminal isn't yet showing the target pages — so the loop keeps drawing
    /// until a turn's new pages have rasterized and been placed.
    pub fn poll_pages(&mut self) -> bool {
        if !self.in_pdf() {
            return false;
        }
        if let Some(r) = self.reader.as_mut() {
            r.poll_loader();
        }
        self.pdf_pages_pending()
    }

    /// Whether a PDF page flip should be honoured right now: only when the deck is
    /// actually displaying the current page. The deck updates only on a draw, so
    /// while a held `j`/`k` drains a burst of key-repeats (no draw in between) the
    /// deck stays "behind" and this gates out every flip after the first — so the
    /// page advances one *visible* page per drawn frame instead of racing ahead
    /// and skipping pages. Always true for reflowable/page-snap (non-PDF) modes,
    /// which have nothing to throttle.
    fn pdf_flip_ready(&self) -> bool {
        let Some(r) = self.reader.as_ref() else {
            return true;
        };
        if !r.is_paged_image() {
            return true;
        }
        // Normal: the deck is showing the current page → ok to advance.
        // Escape hatch: the current page resolved but can't be shown (render
        // failed), so don't soft-lock — let the user move past it.
        self.page_deck.shown_sections().contains(&r.section) || r.page_unrenderable(r.section)
    }

    /// Whether the PDF page deck needs another frame: a visible page is still
    /// rasterizing (loaded async off the main thread), or every visible page is
    /// ready but the deck hasn't placed them yet (capture + place happen on the
    /// draw that follows the load). Pure — no draining. Drives both the redraw
    /// flag and the loop's busy/timeout, so async pages pop in without a keypress.
    fn pdf_pages_pending(&self) -> bool {
        if !self.in_pdf() {
            return false;
        }
        let Some(r) = self.reader.as_ref() else {
            return false;
        };
        let spread = matches!(self.config.view_mode, ViewMode::TwoPage);
        if r.pages_loading(spread) {
            return true;
        }
        // A viewport-matched crisp re-raster is in flight (after a zoom / resize):
        // keep drawing so it pops in without a keypress. Self-limiting.
        if r.crisp_awaiting() {
            return true;
        }
        let placeable = r.placeable_sections(spread);
        // Nothing placeable (e.g. a page failed to rasterize): settle and keep
        // whatever's up, rather than spinning forever on a page that won't show.
        !placeable.is_empty() && self.page_deck.shown_sections() != placeable
    }

    /// Escapes to reconcile the terminal's PDF page images with the current
    /// frame: show the visible page(s) and tear everything down when leaving the
    /// reader. Written inside the synchronized frame, alongside the chrome.
    pub fn page_escapes(&mut self) -> Vec<String> {
        if !self.in_pdf() {
            // Leaving a PDF (or never in one): free any page images. No-op once
            // the deck is empty.
            return if self.page_deck.is_empty() {
                Vec::new()
            } else {
                self.page_deck.clear()
            };
        }
        let Some(r) = self.reader.as_ref() else {
            return Vec::new();
        };
        let targets = r.pdf_targets.clone();
        // No targets means a visible page isn't rasterized yet — hold the current
        // page(s) up rather than tearing them down (which would blank the screen).
        if targets.is_empty() {
            return Vec::new();
        }
        let policy = r.page_policy();
        self.page_deck.render(&targets, policy, |s| r.page_png(s))
    }

    /// The Kitty escapes to reconcile the reader's inline images (equation rasters +
    /// inline figures) to what the view collected this frame — via [`InlineDeck`], the
    /// direct-placement analogue of [`Self::page_escapes`]. Leaving the reader frees
    /// every inline image (so no ghost survives into the library / a PDF).
    pub fn inline_escapes(&mut self) -> Vec<String> {
        // Leaving the reader, or a popup is up: Kitty images draw *above* the cell grid, so
        // an inline image would render over the popup — and a rebuild while it's open (e.g.
        // toggling image sizing, which re-places every equation) corrupts it. Take the inline
        // images down, like the PDF page (`in_pdf`); they re-place when the reader is shown
        // again / the popup closes (the overlay toggle forces a full repaint). Still drain the
        // frame's targets so they don't accumulate behind the popup.
        if self.mode != Mode::Reader || self.modal_open() {
            if let Some(r) = self.reader.as_ref() {
                let _ = r.take_inline_targets();
            }
            return if self.inline_deck.is_empty() {
                Vec::new()
            } else {
                self.inline_deck.clear()
            };
        }
        let Some(r) = self.reader.as_ref() else {
            return Vec::new();
        };
        let targets = r.take_inline_targets();
        // A restage dropped the built PNGs: clear the deck first so the rebuilt images
        // re-transmit rather than being assumed still resident.
        let mut out = if r.take_inline_clear() {
            self.inline_deck.clear()
        } else {
            Vec::new()
        };
        out.extend(self.inline_deck.render(&targets, |key| r.image_png(key)));
        out
    }

    /// Forget which images the terminal is showing so the next frame re-places them
    /// all (data stays resident — no re-transmit). The loop calls this with every
    /// `terminal.clear()`: the clear drops the terminal's placements, so without it
    /// each deck's "nothing changed" fast path would leave the screen imageless.
    pub fn restage_images(&mut self) {
        self.inline_deck.restage();
        self.page_deck.restage();
    }

    /// Text queued for the system clipboard (OSC 52), if any.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.reader.as_mut().and_then(|r| r.take_clipboard())
    }

    /// An image queued for the system clipboard (`(w, h, RGBA)`), if any.
    pub fn take_clipboard_image(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        self.pending_clipboard_image.take()
    }

    /// Whether anything modal is drawn over the page — a popup, a confirmation, or
    /// one of the text prompts. Terminal images composite *above* the cell grid, so
    /// while this holds the PDF page and inline images are taken down; otherwise
    /// they render on top of the very thing asking for input. The main loop also
    /// forces a full repaint when it toggles, so a dismissed modal leaves no ghost.
    ///
    /// The dialogs count as much as the popups do: they were status-bar lines when
    /// this list was written, which is why they were missing from it.
    pub fn modal_open(&self) -> bool {
        if self.pending_confirm.is_some() {
            return true;
        }
        if self.mode == Mode::Library && self.library.filtering {
            return true;
        }
        if self.reader.as_ref().is_some_and(|r| r.search.searching) {
            return true;
        }
        matches!(
            self.overlay,
            Overlay::Settings(_)
                | Overlay::Annot(_)
                | Overlay::Stats(_)
                | Overlay::Palette(_)
                | Overlay::MetaEdit(_)
                | Overlay::BulkRename(_)
                | Overlay::ShelfPicker(_)
                | Overlay::ImageView(_)
                | Overlay::WordLookup(_)
                | Overlay::CodeView(_)
                | Overlay::TagEdit(_)
                | Overlay::Prompt(_)
        )
    }

    /// Is a smooth scroll in progress, or are inline images still building (so
    /// the loop should keep drawing until things settle)?
    pub fn animating(&self) -> bool {
        let Some(r) = self.reader.as_ref() else {
            return false;
        };
        let in_reader = self.mode == Mode::Reader;
        r.is_scrolling()
            || (in_reader && r.images_pending())
            // Keep redrawing while a following continuous section is still decoding
            // (its blocks/math render off the main thread), so it fills the buffer
            // tail as soon as it lands instead of after the next keypress.
            || (in_reader && r.following_pending())
            // Keep redrawing while the inline deck still has new images to upload
            // (its per-frame transmit cap spreads a maths-dense page over a few
            // frames), so they all land without a keypress.
            || (in_reader && self.inline_deck.deferred())
            // PDF: keep redrawing while a visible page is still rasterizing
            // (async, off the main thread) or is ready but not yet placed, so it
            // pops in without needing a keypress.
            || self.pdf_pages_pending()
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
    use crate::store::LibrarySection;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
    use ratatui::layout::Rect;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    // Pull the open overlay out as its concrete type, panicking if a different
    // overlay is open — the same contract `app.X.as_ref().unwrap()` gave before
    // the overlays were folded into the single `app.overlay` enum.
    fn meta(app: &App) -> &MetaEdit {
        match &app.overlay {
            Overlay::MetaEdit(e) => e,
            _ => panic!("metadata editor not open"),
        }
    }
    fn meta_mut(app: &mut App) -> &mut MetaEdit {
        match &mut app.overlay {
            Overlay::MetaEdit(e) => e,
            _ => panic!("metadata editor not open"),
        }
    }
    /// Terminal images composite *above* the cell grid, so anything drawn over the
    /// page must report itself here or the images render on top of it. The dialogs
    /// were missed when they were status-bar lines: an in-book search opened over
    /// a figure came out with the equation painted across the prompt.
    #[test]
    fn every_modal_reports_itself_so_images_are_taken_down() {
        let mut app = App::library();
        assert!(!app.modal_open(), "a plain library view occludes nothing");

        app.library.filtering = true;
        assert!(app.modal_open(), "the filter prompt is modal");
        app.library.filtering = false;
        assert!(!app.modal_open());

        app.ask_confirm("Delete?", super::confirm::ConfirmAction::Rename);
        assert!(app.modal_open(), "a confirmation is modal");
        app.pending_confirm = None;
        assert!(!app.modal_open());

        app.overlay = Overlay::TagEdit(crate::app::tags::TagInput {
            input: crate::ui::TextInput::new(),
            targets: Vec::new(),
            multi: false,
        });
        assert!(app.modal_open(), "the tag prompt is modal");
    }

    fn settings_state(app: &App) -> &Settings {
        match &app.overlay {
            Overlay::Settings(s) => s,
            _ => panic!("settings not open"),
        }
    }
    fn tag_edit_mut(app: &mut App) -> &mut TagInput {
        match &mut app.overlay {
            Overlay::TagEdit(t) => t,
            _ => panic!("tag editor not open"),
        }
    }
    fn shelf_picker(app: &App) -> &ShelfPicker {
        match &app.overlay {
            Overlay::ShelfPicker(p) => p,
            _ => panic!("collection picker not open"),
        }
    }
    fn dup_resolve(app: &App) -> &DupResolve {
        match &app.overlay {
            Overlay::DupResolve(d) => d,
            _ => panic!("duplicate resolver not open"),
        }
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
        assert_eq!(
            app.library.books.len(),
            1,
            "seeded book loads into the list"
        );

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
        assert!(
            matches!(app.overlay, Overlay::MetaEdit(_)),
            "e opens the metadata editor"
        );
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            !matches!(app.overlay, Overlay::MetaEdit(_)),
            "Esc closes the editor"
        );

        // c opens the add-to-collection picker.
        app.on_key(key('c'));
        assert!(
            matches!(app.overlay, Overlay::ShelfPicker(_)),
            "c opens the collection picker"
        );
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
        assert_eq!(app.library.pane, LibPane::List, "starts in the list");
        assert_eq!(app.library.view, LibView::Section(LibrarySection::All));

        // h moves the keyboard left into the sidebar.
        app.on_key(key('h'));
        assert_eq!(app.library.pane, LibPane::Sidebar);

        // j/k now walk the sections (All → PDFs → All), not the book list.
        app.on_key(key('j'));
        assert_eq!(app.library.view, LibView::Section(LibrarySection::Pdf));
        app.on_key(key('k'));
        assert_eq!(app.library.view, LibView::Section(LibrarySection::All));

        // g jumps to the first section; k there is clamped (no wrap).
        app.on_key(key('g'));
        assert_eq!(app.library.view, LibView::Section(LibrarySection::Recent));
        app.on_key(key('k'));
        assert_eq!(
            app.library.view,
            LibView::Section(LibrarySection::Recent),
            "clamped at top"
        );

        // Enter steps into the list.
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.library.pane, LibPane::List);

        // b hides the sidebar; focus falls back to the list (it can't stay there).
        app.on_key(key('h')); // focus sidebar
        assert_eq!(app.library.pane, LibPane::Sidebar);
        app.on_key(key('b')); // hide sidebar
        assert!(!app.library.show_sidebar);
        assert_eq!(
            app.library.pane,
            LibPane::List,
            "focus leaves the hidden pane"
        );

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
        assert_eq!(app.library.pane, LibPane::List);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.library.pane, LibPane::Detail);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.library.pane, LibPane::Sidebar);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.library.pane, LibPane::List);

        // Resize the sidebar (focus it first).
        app.on_key(key('h'));
        assert_eq!(app.library.pane, LibPane::Sidebar);
        let w0 = app.library.sidebar_pct;
        app.on_key(key('>'));
        assert_eq!(app.library.sidebar_pct, w0 + 2);
        app.on_key(key('<'));
        assert_eq!(app.library.sidebar_pct, w0);
        for _ in 0..40 {
            app.on_key(key('<'));
        }
        assert_eq!(
            app.library.sidebar_pct,
            library::SIDEBAR_PCT_MIN,
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
        assert_eq!(meta(&app).mode, EditMode::Nav, "opens in nav mode");

        // In nav mode, 'j' moves fields (does NOT type); Enter enters edit mode.
        app.on_key(key('j')); // → Author
        assert_eq!(meta(&app).row, 1);
        app.on_key(key('k')); // back to Title
        app.on_key(code(KeyCode::Enter)); // edit Title
        assert_eq!(meta(&app).mode, EditMode::Edit);
        // Mid-string insert: cursor at end of "K"; Left then 'X' → "XK".
        app.on_key(code(KeyCode::Left));
        app.on_key(key('X'));
        assert_eq!(meta(&app).values[0].text(), "XK");
        app.on_key(code(KeyCode::Esc)); // back to nav (not closed)
        assert!(matches!(app.overlay, Overlay::MetaEdit(_)));
        assert_eq!(meta(&app).mode, EditMode::Nav);

        // Navigate to Year, edit it to garbage → invalid.
        app.on_key(key('j')); // Author
        app.on_key(key('j')); // Year
        app.on_key(code(KeyCode::Enter));
        app.on_key(ctrl('u'));
        app.on_key(key('a'));
        app.on_key(code(KeyCode::Esc));
        assert!(meta(&app).has_invalid(), "non-numeric year invalid");

        // ^S must NOT even prompt to save while invalid.
        app.on_key(ctrl('s'));
        assert!(
            app.pending_confirm.is_none(),
            "no save prompt while invalid"
        );
        assert!(
            matches!(app.overlay, Overlay::MetaEdit(_)),
            "save blocked while invalid"
        );

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
        assert!(
            matches!(app.overlay, Overlay::MetaEdit(_)),
            "editor open while confirming"
        );
        app.on_key(key('y')); // confirm
        assert!(app.pending_confirm.is_none(), "prompt dismissed");
        assert!(
            !matches!(app.overlay, Overlay::MetaEdit(_)),
            "valid edit saves & closes"
        );
        let b = &app.library.books[0];
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
        assert!(
            matches!(app.overlay, Overlay::MetaEdit(_)),
            "editor stays open after cancel"
        );
        assert_eq!(
            app.library.books[0].title, "K",
            "nothing persisted on cancel"
        );

        // Esc also cancels the prompt (and keeps the editor).
        app.on_key(ctrl('s'));
        app.on_key(code(KeyCode::Esc));
        assert!(
            app.pending_confirm.is_none() && matches!(app.overlay, Overlay::MetaEdit(_)),
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
            a.library
                .books
                .iter()
                .map(|b| b.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(&app), ["A", "B", "C"], "All section sorts by title");

        // `s` steps each key ascending → descending before advancing to the
        // next key: Default → Title↑ → Title↓ → Author↑ → …
        app.on_key(key('s'));
        assert_eq!(app.library.sort, SortKey::Title);
        assert!(!app.library.sort_desc);
        assert_eq!(titles(&app), ["A", "B", "C"], "title ascending");

        app.on_key(key('s'));
        assert_eq!(app.library.sort, SortKey::Title);
        assert!(
            app.library.sort_desc,
            "second press flips the same key to descending"
        );
        assert_eq!(titles(&app), ["C", "B", "A"], "title descending");

        app.on_key(key('s'));
        assert_eq!(
            app.library.sort,
            SortKey::Author,
            "third press advances the key"
        );
        assert!(
            !app.library.sort_desc,
            "advancing a key resets to ascending"
        );

        // `S` flips direction in place without changing the key.
        app.library.sort = SortKey::Year;
        app.library.sort_desc = false;
        app.refresh_library();
        assert_eq!(titles(&app), ["B", "C", "A"], "year ascending");
        app.on_key(key('S'));
        assert!(app.library.sort_desc);
        assert_eq!(titles(&app), ["A", "C", "B"], "year descending");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // `T` tags: single-book edit replaces (normalised); a multi-selection adds the
    // typed tags to each book (union with what it already had).
    #[test]
    fn tag_editing_replaces_single_and_adds_to_selection() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_tag_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            for p in ["/a.epub", "/b.epub"] {
                store
                    .upsert_book(p, "T", "x", None, 1, 1, 1, "", None, "", "", "", "")
                    .unwrap();
            }
        }
        let mut app = App::library();
        let tags_of = |a: &App, path: &str| {
            a.library
                .books
                .iter()
                .find(|b| b.path == path)
                .map(|b| b.tags.clone())
                .unwrap_or_default()
        };

        // Single book: T opens the prompt; typing replaces; commit normalises.
        app.library.sel = 0;
        let first = app.library.books[0].path.clone();
        app.on_key(key('T'));
        tag_edit_mut(&mut app).input.set("Fiction, FICTION, sci-fi");
        app.on_key(code(KeyCode::Enter));
        assert!(
            !matches!(app.overlay, Overlay::TagEdit(_)),
            "prompt closed on commit"
        );
        assert_eq!(
            tags_of(&app, &first),
            "fiction, sci-fi",
            "deduped + lowercased"
        );

        // Multi-selection: typed tag is added to each (union), not replaced.
        let other = if first == "/a.epub" {
            "/b.epub"
        } else {
            "/a.epub"
        };
        app.library.marked.insert("/a.epub".into());
        app.library.marked.insert("/b.epub".into());
        app.on_key(key('T'));
        tag_edit_mut(&mut app).input.set("classic");
        app.on_key(code(KeyCode::Enter));
        assert_eq!(
            tags_of(&app, &first),
            "fiction, sci-fi, classic",
            "added to the already-tagged book"
        );
        assert_eq!(tags_of(&app, other), "classic", "added to the untagged one");

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
        app.last_layout.grid_cols = 3; // normally set by the renderer
        assert_eq!(app.library.sel, 0);

        app.on_key(key('l')); // → 1
        app.on_key(key('l')); // → 2
        assert_eq!(app.library.sel, 2);
        app.on_key(key('j')); // down a row: 2 + 3 = 5
        assert_eq!(app.library.sel, 5);
        app.on_key(key('k')); // up a row: 5 - 3 = 2
        assert_eq!(app.library.sel, 2);
        app.on_key(key('h')); // ← 1
        assert_eq!(app.library.sel, 1);
        app.on_key(key('j')); // 1 + 3 = 4
        app.on_key(key('j')); // 4 + 3 = 7 → clamped to 5
        assert_eq!(app.library.sel, 5, "clamped to last");

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
        assert_eq!(app.library.books.len(), 1);
        // `r` renames the current book (no need to mark it) via the popup.
        app.on_key(key('r')); // rename popup (default "%T.%E")
        assert!(matches!(app.overlay, Overlay::BulkRename(_)));
        app.on_key(ctrl('s')); // ^S asks to confirm
        assert!(app.pending_confirm.is_some(), "rename asks to confirm");
        app.on_key(key('y')); // confirm + apply

        let new = books.join("Clean Title.epub");
        assert!(new.exists(), "renamed file exists");
        assert!(!old.exists(), "old file is gone");
        assert_eq!(
            app.library.books[0].path,
            new.to_string_lossy(),
            "DB path repointed and reloaded"
        );
        assert!(
            app.library.books[0].favorite,
            "favorite preserved across rename"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Duplicates: `D` opens the resolution overlay, auto-selects the worse copy
    // (smaller here), and `d`+`y` deletes the checked file + row, keeping the best.
    #[test]
    fn dup_overlay_auto_selects_and_deletes_checked() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_dup_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let books = tmp.join("books");
        std::fs::create_dir_all(&books).unwrap();
        let big = books.join("keep-big.epub");
        let small = books.join("del-small.epub");
        std::fs::write(&big, b"x").unwrap();
        std::fs::write(&small, b"x").unwrap();
        let (sbig, ssmall) = (
            big.to_string_lossy().into_owned(),
            small.to_string_lossy().into_owned(),
        );
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    &sbig, "Same", "Auth", None, 9_000_000, 1, 1, "", None, "", "", "", "",
                )
                .unwrap();
            store
                .upsert_book(
                    &ssmall, "Same", "Auth", None, 1000, 1, 1, "", None, "", "", "", "",
                )
                .unwrap();
            // Grouping is content-scan only; link the two so they form a group.
            // Distinct sizes → deterministic keep (larger wins the tiebreak).
            store.replace_scan_dup_links(&[(sbig.clone(), ssmall.clone())]);
        }

        let mut app = App::library();
        app.library.pane = LibPane::List;
        app.on_key(key('D')); // open the overlay (works from any section)
        let dr = dup_resolve(&app);
        assert_eq!(dr.groups.len(), 1, "one duplicate group");
        assert_eq!(dr.checked_count(), 1, "auto-selects one to delete");
        // The smaller copy is the one checked for deletion.
        let checked: Vec<&str> = dr.groups[0]
            .members
            .iter()
            .filter(|m| m.checked)
            .map(|m| m.path.as_str())
            .collect();
        assert_eq!(checked, vec![ssmall.as_str()], "keeps the larger copy");

        app.on_key(key('d')); // delete checked → confirm
        assert!(app.pending_confirm.is_some(), "asks to confirm");
        app.on_key(key('y'));

        assert!(std::path::Path::new(&sbig).exists(), "larger copy kept");
        assert!(
            !std::path::Path::new(&ssmall).exists(),
            "smaller copy deleted"
        );
        let all = app.session.store.as_ref().unwrap().all_books();
        assert!(all.iter().any(|b| b.path == sbig));
        assert!(!all.iter().any(|b| b.path == ssmall));
        // No duplicates remain → the overlay closes itself.
        assert!(
            !matches!(app.overlay, Overlay::DupResolve(_)),
            "overlay closes when nothing's left"
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
        assert_eq!(app.library.books.len(), 2);
        app.on_key(key('V')); // visual select from book 0
        app.on_key(key('j')); // extend to book 1
        assert_eq!(app.library.marked.len(), 2);
        app.on_key(key('r')); // rename the selection (not the editor)
        assert!(matches!(app.overlay, Overlay::BulkRename(_)));
        assert!(!matches!(app.overlay, Overlay::MetaEdit(_)));
        app.on_key(ctrl('s')); // ^S asks to confirm
        app.on_key(key('y')); // confirm + apply default "%T.%E"

        assert!(books.join("Alpha.epub").exists(), "Alpha renamed");
        assert!(books.join("Beta.epub").exists(), "Beta renamed");
        assert!(!books.join("a old.epub").exists(), "old file gone");
        assert!(
            !matches!(app.overlay, Overlay::BulkRename(_)),
            "popup closed after apply"
        );
        assert!(
            app.library.marked.is_empty(),
            "selection cleared after apply"
        );

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
        assert_eq!(app.library.sel, 1, "click on the second row selects it");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn library_mouse_multiselect() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_msel_{}", std::process::id()));
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
        app.mouse.books = vec![(0, Rect::new(0, 0, 20, 1)), (1, Rect::new(0, 1, 20, 1))];
        let ev = |kind, row| crossterm::event::MouseEvent {
            kind,
            column: 5,
            row,
            modifiers: KeyModifiers::NONE,
        };
        use crossterm::event::MouseButton;
        // Right-click toggles a book into the multi-selection (no advance).
        app.on_mouse(ev(MouseEventKind::Down(MouseButton::Right), 0));
        assert_eq!(app.library.marked.len(), 1, "right-click marks the book");
        assert!(app.library.marked.contains(&app.library.books[0].path));
        // Right-click again clears it.
        app.on_mouse(ev(MouseEventKind::Down(MouseButton::Right), 0));
        assert!(app.library.marked.is_empty(), "right-click again unmarks");
        // Shift+left-click range-selects from the cursor to the clicked row.
        app.library.sel = 0;
        app.on_mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::SHIFT,
        });
        assert_eq!(app.library.marked.len(), 2, "shift-click selects the range");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn library_wheel_only_scrolls_pane_under_cursor() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_wheel_{}", std::process::id()));
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
        // Stand in for the render capturing the pane rects: list left, detail right.
        app.last_layout.lib_list = Some(Rect::new(0, 0, 40, 10));
        app.last_layout.lib_detail = Some(Rect::new(40, 0, 20, 10));
        let wheel = |col| crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: col,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        // Over the detail pane: the book list must not scroll.
        app.library.sel = 0;
        assert!(
            !app.on_mouse(wheel(50)),
            "wheel over detail is inert for the list"
        );
        assert_eq!(app.library.sel, 0);
        // Over the list: it scrolls (moves the cursor, clamped to the last book).
        assert!(app.on_mouse(wheel(10)), "wheel over the list scrolls it");
        assert_eq!(app.library.sel, 1);

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
            a.session
                .store
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
        assert!(matches!(app.library.view, LibView::Shelf(ref n) if n == "Sci"));

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
        assert!(app.library.marked.contains("/a.epub"));
        assert!(app.library.marked.contains("/c.epub"));
        assert!(
            !app.library.marked.contains("/b.epub"),
            "B was skipped — non-contiguous"
        );
        assert!(
            app.library.visual.is_none(),
            "Space doesn't enter visual mode"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Clicking a sidebar row switches the active section/collection and focuses
    // the sidebar — the mouse counterpart to j/k there. The trailing "＋ New" row
    // begins a collection.
    #[test]
    fn sidebar_click_selects_section_and_collection() {
        use crossterm::event::{MouseButton, MouseEvent};

        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_sideclick_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/a.epub", "A", "Auth", None, 1, 1, 1, "", None, "", "", "", "",
                )
                .unwrap();
            store.create_collection("Shelf1");
            store.add_to_shelf("/a.epub", "Shelf1");
        }

        let mut app = App::library();
        let n = LibrarySection::ALL.len();
        assert_eq!(app.library.shelves.len(), 1, "one collection loaded");

        // Dispatch a left-click routed to a sidebar row at ring index `ring` (the
        // renderer normally captures these rects; seed one directly).
        fn click(app: &mut App, ring: usize) {
            app.mouse.side_rows = vec![(ring, Rect::new(0, 0, 10, 1))];
            app.on_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 0,
                modifiers: KeyModifiers::NONE,
            });
        }

        // A fixed section: ring index is its position in `ALL`.
        let pdf = LibrarySection::ALL
            .iter()
            .position(|s| *s == LibrarySection::Pdf)
            .unwrap();
        click(&mut app, pdf);
        assert_eq!(app.library.view, LibView::Section(LibrarySection::Pdf));
        assert_eq!(
            app.library.pane,
            LibPane::Sidebar,
            "click focuses the sidebar"
        );

        // The lone collection sits just past the fixed sections.
        click(&mut app, n);
        assert_eq!(app.library.view, LibView::Shelf("Shelf1".into()));

        // The trailing "＋ New collection" row begins a new collection inline.
        let new_row = n + app.library.shelves.len();
        click(&mut app, new_row);
        assert!(
            matches!(app.overlay, Overlay::CollEdit(_)),
            "＋ New starts a collection"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The annotations overlay is fully mouse-drivable: a tab click switches lists,
    // a row click selects, and a second click on the same row jumps (closing it).
    #[test]
    fn annot_tabs_split_by_kind_and_cycle() {
        let annot = |id: i64, kind: i64, color: i64| crate::store::Annotation {
            id,
            section: 0,
            quote: format!("q{id}"),
            note: String::new(),
            name: String::new(),
            folder: String::new(),
            kind,
            color,
        };
        let items = vec![
            annot(1, crate::store::KIND_BOOKMARK, 0),
            annot(2, crate::store::KIND_NOTE, 0),
            annot(3, crate::store::KIND_HIGHLIGHT, 2),
            annot(4, crate::store::KIND_HIGHLIGHT, 4),
        ];

        // Each kind lands on its own tab (and nowhere else).
        assert_eq!(AnnotTab::of(&items[0]), AnnotTab::Bookmarks);
        assert_eq!(AnnotTab::of(&items[1]), AnnotTab::Notes);
        assert_eq!(AnnotTab::of(&items[2]), AnnotTab::Highlights);

        let state = AnnotState::new(items, AnnotTab::Highlights);
        assert_eq!(state.count(AnnotTab::Bookmarks), 1);
        assert_eq!(state.count(AnnotTab::Notes), 1);
        assert_eq!(state.count(AnnotTab::Highlights), 2);
        // The active (Highlights) tab's filtered view shows only its two rows.
        let shown: Vec<i64> = state.filtered().iter().map(|a| a.id).collect();
        assert_eq!(shown, vec![3, 4]);

        // Tabs cycle forward and back through all three, wrapping.
        assert_eq!(AnnotTab::Bookmarks.next(), AnnotTab::Notes);
        assert_eq!(AnnotTab::Notes.next(), AnnotTab::Highlights);
        assert_eq!(AnnotTab::Highlights.next(), AnnotTab::Bookmarks);
        assert_eq!(AnnotTab::Bookmarks.prev(), AnnotTab::Highlights);
    }

    #[test]
    fn annotations_click_switches_tab_and_selects() {
        use crossterm::event::{MouseButton, MouseEvent};

        let _env = crate::test_env_guard();
        let mut app = App::library();
        let bm = |id: i64, name: &str| crate::store::Annotation {
            id,
            section: id as usize,
            quote: format!("q{id}"),
            note: String::new(),
            name: name.into(),
            folder: String::new(),
            kind: crate::store::KIND_BOOKMARK,
            color: 0,
        };
        let note = crate::store::Annotation {
            id: 3,
            section: 2,
            quote: "q3".into(),
            note: "hello".into(),
            name: String::new(),
            folder: String::new(),
            kind: crate::store::KIND_NOTE,
            color: 0,
        };
        app.overlay = Overlay::Annot(AnnotState::new(
            vec![bm(1, "B1"), bm(2, "B2"), note],
            AnnotTab::Bookmarks,
        ));

        // Hit rects the renderer would capture (two tabs, two bookmark rows). Tabs
        // are indexed 0 = Bookmarks, 1 = Notes.
        app.mouse.overlay_tabs = vec![(0, Rect::new(0, 0, 15, 1)), (1, Rect::new(17, 0, 12, 1))];
        app.mouse.overlay_rows = vec![(0, Rect::new(0, 3, 30, 1)), (1, Rect::new(0, 4, 30, 1))];

        let down = |c: u16, r: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: c,
            row: r,
            modifiers: KeyModifiers::NONE,
        };

        // Click the second bookmark row → the cursor moves to it.
        app.on_mouse(down(2, 4));
        let Overlay::Annot(a) = &app.overlay else {
            panic!("overlay closed")
        };
        assert_eq!(a.sel, 1, "row click selects that item");

        // Click the Notes tab → the active tab switches and the cursor resets.
        app.on_mouse(down(18, 0));
        let Overlay::Annot(a) = &app.overlay else {
            panic!("overlay closed")
        };
        assert_eq!(a.tab, AnnotTab::Notes, "tab click switches tab");
        assert_eq!(a.sel, 0, "switching tabs resets the cursor");

        // Back to Bookmarks, then double-click a row → jump + close (no reader here,
        // so the jump is a no-op but the overlay still closes).
        app.on_mouse(down(2, 0));
        app.mouse.overlay_rows = vec![(0, Rect::new(0, 3, 30, 1))];
        app.on_mouse(down(2, 3)); // first click selects
        app.on_mouse(down(2, 3)); // second, within the window → jump
        assert!(
            matches!(app.overlay, Overlay::None),
            "double-click on a row jumps and closes the overlay"
        );
    }

    // The command palette is mouse-drivable through the shared overlay hit rects:
    // a click selects a command, a second click on it runs it (closing the palette).
    #[test]
    fn palette_click_selects_then_runs() {
        use crossterm::event::{MouseButton, MouseEvent};

        let _env = crate::test_env_guard();
        let mut app = App::library();
        app.open_palette();
        assert!(matches!(app.overlay, Overlay::Palette(_)), "palette open");
        app.mouse.overlay_rows = vec![(0, Rect::new(0, 2, 40, 1)), (1, Rect::new(0, 3, 40, 1))];
        let down = |c: u16, r: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: c,
            row: r,
            modifiers: KeyModifiers::NONE,
        };

        app.on_mouse(down(2, 3)); // first click selects row 1
        let Overlay::Palette(p) = &app.overlay else {
            panic!("palette closed early")
        };
        assert_eq!(p.sel, 1, "row click moves the selection");

        app.on_mouse(down(2, 3)); // second click within the window runs it
        assert!(
            matches!(app.overlay, Overlay::None),
            "double-click runs the command and closes the palette"
        );
    }

    // Settings tabs and option rows route through the same shared hit channels.
    #[test]
    fn settings_click_switches_tab_and_row() {
        use crossterm::event::{MouseButton, MouseEvent};

        let _env = crate::test_env_guard();
        let mut app = App::library();
        app.overlay = Overlay::Settings(Settings {
            scope: Mode::Library,
            tab: 0,
            row: first_setting_row(Mode::Library, 0, &app.config),
            adding: None,
            filter: None,
            query: String::new(),
        });
        // Two tabs + a couple of option rows (real geometry is captured at render).
        app.mouse.overlay_tabs = vec![(0, Rect::new(0, 0, 12, 1)), (1, Rect::new(13, 0, 12, 1))];
        app.mouse.overlay_rows = vec![(1, Rect::new(0, 4, 40, 1)), (2, Rect::new(0, 5, 40, 1))];
        let down = |c: u16, r: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: c,
            row: r,
            modifiers: KeyModifiers::NONE,
        };

        // Click the second tab.
        app.on_mouse(down(14, 0));
        let Overlay::Settings(s) = &app.overlay else {
            panic!("settings closed")
        };
        assert_eq!(s.tab, 1, "tab click switches the active tab");

        // Click an option row (its index is into that tab's rows).
        app.on_mouse(down(2, 5));
        let Overlay::Settings(s) = &app.overlay else {
            panic!("settings closed")
        };
        assert_eq!(s.row, 2, "row click moves the option cursor");
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
        assert_eq!(app.library.marked.len(), 2);
        app.on_key(key('c')); // bulk shelf picker
        assert_eq!(shelf_picker(&app).targets.len(), 2);
        // Select the "＋ New collection" row, create "Unread", filing both books.
        app.on_key(code(KeyCode::Enter)); // onto +New row → start typing
        for c in "Unread".chars() {
            app.on_key(key(c));
        }
        app.on_key(code(KeyCode::Enter)); // create + file all
        app.on_key(key('q')); // close (clears the selection)

        let store = app.session.store.as_ref().unwrap();
        assert!(store.shelves_for("/a.epub").contains(&"Unread".to_string()));
        assert!(store.shelves_for("/b.epub").contains(&"Unread".to_string()));
        assert!(
            app.library.marked.is_empty(),
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
        assert_eq!(settings_state(&app).scope, Mode::Library);
        assert_eq!(settings_state(&app).tab, 0, "opens on the first tab");

        // Walk every tab: the cursor only ever rests on items, and Tab advances
        // to the next group (parking on its first option).
        for t in 0..settings_tabs(Mode::Library, &app.config).len() {
            assert_eq!(settings_state(&app).tab, t);
            let rows = tab_rows(Mode::Library, t, &app.config);
            assert!(
                matches!(rows[0], SettingRow::Section(_)),
                "tab {t} starts with a header"
            );
            // Move down past the end; the cursor must never land on a header.
            for _ in 0..rows.len() + 2 {
                let row = settings_state(&app).row;
                assert!(
                    matches!(rows[row], SettingRow::Item(_)),
                    "cursor never rests on a section header (tab {t}, row {row})"
                );
                app.on_key(key('j'));
            }
            app.on_key(code(KeyCode::Tab)); // next tab
        }
        // Tab wraps back to the first group.
        assert_eq!(settings_state(&app).tab, 0, "Tab wraps around");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The Sources tab: adding a folder registers + scans it; deleting drops it and
    // its books from the index.
    #[test]
    fn sources_manager_add_scans_and_remove_drops_books() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_srcmgr_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let books = tmp.join("books");
        std::fs::create_dir_all(&books).unwrap();
        // A non-EPUB is indexed by filename, so no valid container is needed.
        std::fs::write(books.join("novel.pdf"), b"x").unwrap();

        let mut app = App::library();
        assert!(app.config.library_paths.is_empty());
        app.open_sources_settings();
        assert_eq!(settings_state(&app).scope, Mode::Library);

        // The empty Sources tab parks the cursor on "Add folder…"; Enter opens the
        // inline input, then the typed path commits on the next Enter.
        app.on_key(code(KeyCode::Enter));
        assert!(settings_state(&app).adding.is_some(), "add input opened");
        for ch in books.to_string_lossy().chars() {
            app.on_key(key(ch));
        }
        app.on_key(code(KeyCode::Enter)); // commit

        let root = crate::library::normalize_root(&books.to_string_lossy());
        assert_eq!(app.config.library_paths, vec![root], "folder registered");
        // Scanning is now off-thread; wait for the worker before checking the list.
        app.await_scan();
        assert!(
            app.library
                .books
                .iter()
                .any(|b| b.path.ends_with("novel.pdf")),
            "the added folder was scanned"
        );

        // Committing parks the cursor on the new folder row; `d` removes it.
        assert!(
            matches!(
                tab_rows(Mode::Library, settings_state(&app).tab, &app.config)
                    .into_iter()
                    .nth(settings_state(&app).row),
                Some(SettingRow::Item(SettingItem::Source(_)))
            ),
            "cursor rests on the folder row after adding"
        );
        app.on_key(key('d'));
        assert!(app.config.library_paths.is_empty(), "folder removed");
        assert!(app.library.books.is_empty(), "its books left the index");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The launch scan is off the UI thread: construction indexes nothing, and the
    // background scan populates the list once awaited.
    #[test]
    fn startup_scan_runs_in_background() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_bgscan_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let books = tmp.join("books");
        std::fs::create_dir_all(&books).unwrap();
        std::fs::write(books.join("deferred.pdf"), b"x").unwrap();
        {
            let mut config = Config::load();
            config
                .library_paths
                .push(books.to_string_lossy().into_owned());
            config.save();
        }

        let mut app = App::library();
        // The list shows immediately from the (empty) store — no synchronous scan.
        assert!(
            app.library.books.is_empty(),
            "construction does not scan on the UI thread"
        );

        app.start_scan_startup();
        assert!(app.scan_pending(), "a background scan is in flight");
        app.await_scan();
        assert!(
            app.library
                .books
                .iter()
                .any(|b| b.path.ends_with("deferred.pdf")),
            "the background scan indexed the folder"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Delete raises a confirmation naming the book; only on `y` is the row cleared
    // (a missing file is used, so the test never touches the real OS trash).
    #[test]
    fn trash_selected_confirms_then_clears_row() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_trash_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        std::fs::create_dir_all(&tmp).unwrap();
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book(
                    "/gone/dead.epub",
                    "Dead Book",
                    "A",
                    None,
                    0,
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
        assert_eq!(app.library.books.len(), 1);
        app.library.sel = 0;

        // Delete raises the confirmation, removing nothing yet.
        app.on_key(code(KeyCode::Delete));
        let confirm = app.pending_confirm.as_ref().expect("confirmation raised");
        assert!(confirm.question.contains("Dead Book"), "names the book");
        assert_eq!(
            app.library.books.len(),
            1,
            "nothing removed before confirming"
        );

        // Confirm: the missing file's dead row is cleared.
        app.on_key(key('y'));
        assert!(app.pending_confirm.is_none());
        assert!(app.library.books.is_empty(), "row cleared on confirm");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // First run — an empty library — opens the Sources manager; once a folder is
    // configured it's a no-op.
    #[test]
    fn empty_library_opens_sources_manager() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_firstrun_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        std::fs::create_dir_all(&tmp).unwrap();

        let mut app = App::library();
        assert!(matches!(app.overlay, Overlay::None));
        app.open_sources_if_empty();
        let Overlay::Settings(s) = &app.overlay else {
            panic!("first run should open the Sources manager");
        };
        assert_eq!(
            settings_tabs(Mode::Library, &app.config)[s.tab].title,
            "Sources",
            "lands on the Sources tab"
        );

        // Once a folder exists it must not hijack the library view.
        app.overlay = Overlay::None;
        app.config.library_paths.push("/some/lib".into());
        app.open_sources_if_empty();
        assert!(
            matches!(app.overlay, Overlay::None),
            "no-op once a folder is configured"
        );

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
        let ed = meta(&app);
        assert_eq!(ed.tab, EditTab::Online);
        // Seeded from metadata: name=title, author=first author, year from book;
        // ISBN stays inactive (empty) until its field is focused.
        assert_eq!(ed.lookup.name.text(), "K");
        assert_eq!(ed.lookup.author.text(), "Auth");
        assert_eq!(ed.lookup.year.text(), "2010");
        assert_eq!(ed.lookup.query(), "K Auth 2010");
        assert_eq!(ed.lookup.focus, 0); // Title focused

        // Typing edits the focused field (Title), entering edit mode.
        app.on_key(key('!'));
        let ed = meta(&app);
        assert!(ed.lookup.editing);
        assert_eq!(ed.lookup.name.text(), "K!");
        app.on_key(code(KeyCode::Esc));
        assert!(!meta(&app).lookup.editing);

        // j moves focus to Author; editing it changes only that field.
        app.on_key(key('j'));
        assert_eq!(meta(&app).lookup.focus, 1);
        app.on_key(code(KeyCode::Enter)); // edit Author
        app.on_key(key('x'));
        let ed = meta(&app);
        assert_eq!(ed.lookup.author.text(), "Authx");
        assert_eq!(ed.lookup.query(), "K! Authx 2010");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // An ISBN turns the lookup into an exact-edition search (ISBN alone), with the
    // ISBN normalized to digits; without it, title + author + year compose.
    #[test]
    fn lookup_isbn_query_is_exact() {
        let mut f = LookupForm {
            name: "Some Title".into(),
            author: "Auth".into(),
            year: "2010".into(),
            ..Default::default()
        };
        assert_eq!(f.query(), "Some Title Auth 2010");
        f.isbn = "978-0-441-01359-3".into();
        assert_eq!(f.query(), "isbn:9780441013593");
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
        assert_eq!(meta(&app).tab, EditTab::Cover);
        app.on_key(key('a'));
        app.on_key(key('b'));
        app.on_key(code(KeyCode::Esc)); // leave the cover query
        assert_eq!(meta(&app).cover_search.q.text(), "ab");

        // Cover → Lookup: the seeded Title field is untouched, not in edit mode.
        app.on_key(code(KeyCode::Tab));
        let ed = meta(&app);
        assert_eq!(ed.tab, EditTab::Online);
        assert_eq!(ed.lookup.name.text(), "K");
        assert!(!ed.lookup.editing);

        // Typing here edits the Title field; the cover query keeps "ab".
        app.on_key(key('c'));
        let ed = meta(&app);
        assert_eq!(ed.lookup.name.text(), "Kc");
        assert_eq!(ed.cover_search.q.text(), "ab");

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
        let ed = meta(&app);
        // Title from the filename; author placeholder "Unknown" → empty; the
        // Cover query is the same clean title, not the ID-like metadata title.
        assert_eq!(ed.lookup.name.text(), "Building Chatbots with Python");
        assert_eq!(ed.lookup.author.text(), "");
        assert_eq!(ed.cover_search.q.text(), "Building Chatbots with Python");

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
            let ed = meta_mut(&mut app);
            ed.values[0].set("Building Chatbots with Python");
            ed.values[1].set("Sumit Raj");
        }
        app.on_key(key('2')); // → Cover: re-seeds from the new Details
        let ed = meta(&app);
        assert_eq!(
            ed.cover_search.q.text(),
            "Building Chatbots with Python Sumit Raj"
        );
        assert_eq!(ed.lookup.name.text(), "Building Chatbots with Python");
        assert_eq!(ed.lookup.author.text(), "Sumit Raj");

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
        assert_eq!(app.library.books.len(), 3);
        app.on_key(key('V')); // visual from book 0
        app.on_key(key('j')); // extend to book 1
        assert_eq!(app.library.marked.len(), 2);

        app.on_key(key('e')); // start the bulk edit
        assert!(matches!(app.overlay, Overlay::MetaEdit(_)));
        assert_eq!(app.edit_total, 2);
        assert_eq!(app.edit_queue.len(), 1, "one book still queued");
        let first = meta(&app).path.clone();

        // ^S → confirm → save and advance to the next book.
        app.on_key(ctrl('s'));
        app.on_key(key('y'));
        assert!(
            matches!(app.overlay, Overlay::MetaEdit(_)),
            "advanced to the next book"
        );
        assert_eq!(app.edit_queue.len(), 0);
        let second = meta(&app).path.clone();
        assert_ne!(first, second);

        // ^S on the last → confirm → editor closes, queue reset.
        app.on_key(ctrl('s'));
        app.on_key(key('y'));
        assert!(
            !matches!(app.overlay, Overlay::MetaEdit(_)),
            "editor closes after the last book"
        );
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
        assert_eq!(app.library.marked.len(), 2);
        app.on_key(key('e'));
        assert!(matches!(app.overlay, Overlay::MetaEdit(_)));
        app.on_key(code(KeyCode::Esc)); // skip first → next opens
        assert!(
            matches!(app.overlay, Overlay::MetaEdit(_)),
            "Esc advanced, not closed"
        );
        assert_eq!(app.edit_queue.len(), 0);
        app.on_key(code(KeyCode::Esc)); // skip last → closes
        assert!(!matches!(app.overlay, Overlay::MetaEdit(_)));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
