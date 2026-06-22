//! Application state and event dispatch.
//!
//! Two top-level modes behaving like tabs (Library | Reader). For now the
//! Library is a stub; the Reader is the working EPUB vertical slice. See
//! `DESIGN.md` §4, §6.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Instant;

use lru::LruCache;

use anyhow::Result;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::config::{Config, LibLayout};
use crate::document::epub::{self, EpubDocument};
use crate::document::epub_write;
use crate::document::{Block, Document, Metadata, OutlineItem};
use crate::input::{self, Action, Pending};
use crate::layout::{DisplayLine, WrapOpts, wrap_blocks};
use crate::library;
use crate::media::{self, ImageBuilder, ImagePlan, ImageView, ImgKey};
use crate::online::{self, Candidate};
use crate::search::{Matcher, SearchMode};
use crate::store::{Annotation, BookRow, LibrarySection, Store};
use crate::theme;
use ratatui_image::picker::Picker;

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

/// Open settings popup state. Settings are scoped to the mode they were opened
/// from — Reading settings in the reader, Library settings in the library — so
/// the two never mix.
pub struct Settings {
    pub scope: Mode,
    pub row: usize,
}

/// Open annotations (bookmarks/notes) overlay state.
pub struct AnnotState {
    pub items: Vec<Annotation>,
    pub sel: usize,
}

/// Editable book-metadata fields, in display order. `Year` and `Series #`
/// hold numeric text, validated on save.
pub const META_FIELDS: [&str; 9] = [
    "Title", "Author", "Year", "Series", "Series #", "Publisher", "Subtitle", "ISBN", "Language",
];
/// Field index of the Year field (validated as an integer).
const F_YEAR: usize = 2;
/// Field index of the Series-position field (validated as a float).
const F_INDEX: usize = 4;

/// Most online matches to fetch (a short list to pick from).
pub const ONLINE_LIMIT: usize = 5;

/// File-tab row layout: the template, the resulting name, then the Rename action.
pub const FILE_TEMPLATE: usize = 0;
pub const FILE_NAME: usize = 1;

/// Default rename template (`Title.epub`). Placeholders are filled from the
/// edited metadata; see [`fill_template`].
pub const DEFAULT_RENAME_TEMPLATE: &str = "%T.%E";

/// Tabs of the metadata editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTab {
    Details,
    Cover,
    Collections,
    Online,
    File,
}

impl EditTab {
    pub const ALL: [EditTab; 5] = [
        EditTab::Details,
        EditTab::Cover,
        EditTab::Collections,
        EditTab::Online,
        EditTab::File,
    ];
    pub fn label(self) -> &'static str {
        match self {
            EditTab::Details => "Details",
            EditTab::Cover => "Cover",
            EditTab::Collections => "Collections",
            EditTab::Online => "Online",
            EditTab::File => "File",
        }
    }
}

/// Whether the focused text field is being navigated between or typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    Nav,
    Edit,
}

/// A message from a background Open Library worker.
pub enum OnlineMsg {
    Results(Vec<Candidate>),
    /// Cover-tab candidates from the multi-source cover search.
    Covers(Vec<online::CoverHit>),
    Cover(Option<Vec<u8>>),
    /// Cover-tab preview: (cover URL, bytes) for the highlighted result.
    Preview(String, Option<Vec<u8>>),
}

/// One search bar's state — query, edit flag, results, selection. The Online and
/// Cover tabs each own an independent instance so typing in one never disturbs
/// the other.
#[derive(Default)]
pub struct Search {
    /// Free-text query (the search bar).
    pub q: String,
    /// Editing the query (vs. browsing results).
    pub editing: bool,
    /// Selected result index.
    pub row: usize,
    pub results: Vec<Candidate>,
    /// A search is in flight.
    pub fetching: bool,
}

/// One book queued for a bulk rename: its path, the metadata values used to fill
/// the template (in [`META_FIELDS`] order), its extension, and its current name
/// (for the preview).
pub struct BulkTarget {
    pub path: String,
    pub values: Vec<String>,
    pub ext: String,
    pub old_name: String,
}

/// Bulk-rename popup: one editable template applied to every marked book.
pub struct BulkRename {
    pub template: String,
    pub cursor: usize,
    pub targets: Vec<BulkTarget>,
}

/// Outcome of renaming a single book file.
enum RenameOutcome {
    Renamed,
    Unchanged,
    /// Skipped, with a short reason (name empty / clashes / move failed).
    Skipped(&'static str),
}

/// Open metadata-edit form: a tabbed, scalable editor over one book.
pub struct MetaEdit {
    pub path: String,
    /// Book title for the popup header.
    pub book_title: String,
    pub tab: EditTab,
    /// Navigate vs. type-into-field (Details/Online query fields).
    pub mode: EditMode,

    // Details tab ---------------------------------------------------------
    /// Current value of each field, indexed to match [`META_FIELDS`].
    pub values: Vec<String>,
    /// Values as declared by the EPUB file, for reset-to-source.
    pub original: Vec<String>,
    /// Focused field.
    pub row: usize,
    /// Cursor position (char index) within the field being edited.
    pub cursor: usize,

    // Collections tab -----------------------------------------------------
    /// (collection name, whether this book is a member).
    pub shelves: Vec<(String, bool)>,
    pub shelf_sel: usize,
    /// Buffer while typing a new collection name (`None` otherwise).
    pub new_shelf: Option<String>,

    // Online / Cover tabs — independent search state per tab --------------
    /// Online (metadata) tab search.
    pub online: Search,
    /// Cover tab search.
    pub cover_search: Search,
    /// Cover-tab candidates (multi-source cover URLs), shown in the Cover list.
    pub cover_hits: Vec<online::CoverHit>,
    /// A cover download is in flight.
    pub cover_pending: bool,
    /// Cover bytes to persist on save (the chosen / previewed cover).
    pub cover: Option<Vec<u8>>,
    /// Bytes of the cover currently previewed on the Cover tab, and its URL
    /// (so Enter can stage it without re-fetching).
    pub preview_cover: Option<Vec<u8>>,
    pub preview_url: String,

    // File tab ------------------------------------------------------------
    /// Rename template (placeholders filled from the edited metadata).
    pub rename_template: String,
    /// The resulting filename — recomputed from the template, or hand-edited.
    pub rename_name: String,
    /// Focused File-tab row (template / name / Rename action).
    pub file_row: usize,

    /// Transient one-line status (search progress, results, errors).
    pub status: Option<String>,
}

impl MetaEdit {
    /// The active tab's search state (Cover has its own; everything else uses
    /// the Online search).
    pub fn search(&self) -> &Search {
        match self.tab {
            EditTab::Cover => &self.cover_search,
            _ => &self.online,
        }
    }

    fn search_mut(&mut self) -> &mut Search {
        match self.tab {
            EditTab::Cover => &mut self.cover_search,
            _ => &mut self.online,
        }
    }

    /// Char length of the focused Details field's value.
    fn field_len(&self) -> usize {
        self.values.get(self.row).map_or(0, |s| s.chars().count())
    }

    /// Char length of whichever field is currently being typed into.
    fn cur_field_len(&self) -> usize {
        match self.tab {
            EditTab::Online | EditTab::Cover => self.search().q.chars().count(),
            EditTab::File => match self.file_row {
                FILE_TEMPLATE => self.rename_template.chars().count(),
                FILE_NAME => self.rename_name.chars().count(),
                _ => 0,
            },
            _ => self.field_len(),
        }
    }

    /// The string currently being typed into (a Details field, the search query,
    /// or a File-tab field).
    fn edit_target(&mut self) -> Option<&mut String> {
        match self.tab {
            EditTab::Details => self.values.get_mut(self.row),
            EditTab::Online | EditTab::Cover => Some(&mut self.search_mut().q),
            EditTab::File => match self.file_row {
                FILE_TEMPLATE => Some(&mut self.rename_template),
                FILE_NAME => Some(&mut self.rename_name),
                _ => None,
            },
            EditTab::Collections => None,
        }
    }

    /// Is field `i`'s current value invalid (a numeric field with unparsable,
    /// non-empty text)?
    pub fn field_invalid(&self, i: usize) -> bool {
        let Some(s) = self.values.get(i) else {
            return false;
        };
        let t = s.trim();
        if t.is_empty() {
            return false;
        }
        match i {
            F_YEAR => t.parse::<i32>().is_err(),
            F_INDEX => t.parse::<f32>().is_err(),
            _ => false,
        }
    }

    /// Has field `i` been changed from its EPUB original?
    pub fn changed(&self, i: usize) -> bool {
        self.original.get(i).map(String::as_str) != self.values.get(i).map(String::as_str)
    }

    /// Any field currently invalid (blocks save).
    pub fn has_invalid(&self) -> bool {
        (0..self.values.len()).any(|i| self.field_invalid(i))
    }

    /// The "new collection" row index (one past the existing collections).
    pub fn new_shelf_row(&self) -> usize {
        self.shelves.len()
    }
}

/// Add-to-collection picker: toggle the focused book's membership in existing
/// collections, or type a new collection name. The last row is "new".
pub struct ShelfPicker {
    /// Book being filed.
    pub path: String,
    /// Title, for the popup header.
    pub title: String,
    /// (collection name, whether the book is currently on it).
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

/// One adjustable setting (identity, not position — so section headers can be
/// inserted freely without re-indexing the change handler).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingItem {
    Theme,
    ViewMode,
    SidePadding,
    LineSpacing,
    ParagraphSpacing,
    ShowSidebar,
    ShowStatus,
    StatusTheme,
    StatusView,
    StatusPosition,
    StatusPercent,
    StatusGauge,
    ImageMaxPx,
    CodeWrap,
    ChapterLock,
    Mouse,
    LibLayout,
    GridSize,
}

impl SettingItem {
    pub fn label(self) -> &'static str {
        match self {
            SettingItem::Theme => "Theme",
            SettingItem::ViewMode => "View mode",
            SettingItem::SidePadding => "Side margin %",
            SettingItem::LineSpacing => "Line spacing",
            SettingItem::ParagraphSpacing => "Paragraph spacing",
            SettingItem::ShowSidebar => "Sidebar by default",
            SettingItem::ShowStatus => "Status bar by default",
            SettingItem::StatusTheme => "Theme",
            SettingItem::StatusView => "View",
            SettingItem::StatusPosition => "Position",
            SettingItem::StatusPercent => "Percent",
            SettingItem::StatusGauge => "Gauge",
            SettingItem::ImageMaxPx => "Max resolution (px)",
            SettingItem::CodeWrap => "Wrap code blocks",
            SettingItem::ChapterLock => "Chapter lock",
            SettingItem::Mouse => "Mouse",
            SettingItem::LibLayout => "Layout",
            SettingItem::GridSize => "Cover size",
        }
    }

    /// The current value, formatted for display.
    pub fn value(self, c: &Config) -> String {
        let onoff = |b: bool| if b { "on" } else { "off" }.to_string();
        match self {
            SettingItem::Theme => c.theme.name.to_string(),
            SettingItem::ViewMode => c.view_mode.label().to_string(),
            SettingItem::SidePadding => c.side_padding.to_string(),
            SettingItem::LineSpacing => c.line_spacing.to_string(),
            SettingItem::ParagraphSpacing => c.paragraph_spacing.to_string(),
            SettingItem::ShowSidebar => onoff(c.show_sidebar),
            SettingItem::ShowStatus => onoff(c.show_status),
            SettingItem::StatusTheme => onoff(c.status.theme),
            SettingItem::StatusView => onoff(c.status.view),
            SettingItem::StatusPosition => onoff(c.status.position),
            SettingItem::StatusPercent => onoff(c.status.percent),
            SettingItem::StatusGauge => onoff(c.status.gauge),
            SettingItem::ImageMaxPx => {
                if c.image_max_px == 0 {
                    "off".into()
                } else {
                    c.image_max_px.to_string()
                }
            }
            SettingItem::CodeWrap => onoff(c.code_wrap),
            SettingItem::ChapterLock => onoff(c.chapter_lock),
            SettingItem::Mouse => onoff(c.mouse_enabled),
            SettingItem::LibLayout => c.library_layout.label().to_string(),
            SettingItem::GridSize => c.library_grid_size.label().to_string(),
        }
    }
}

/// A row in the settings popup: a non-selectable section header or a setting.
pub enum SettingRow {
    Section(&'static str),
    Item(SettingItem),
}

/// The grouped rows for a settings scope (section headers + items). Each scope
/// is self-contained: the reader shows only reading settings, the library only
/// library settings (global toggles like Theme/Mouse appear in both).
pub fn settings_rows(scope: Mode) -> Vec<SettingRow> {
    use SettingItem::*;
    use SettingRow::{Item as I, Section as S};
    match scope {
        Mode::Reader => vec![
            S("Typography"),
            I(Theme),
            I(ViewMode),
            I(SidePadding),
            I(LineSpacing),
            I(ParagraphSpacing),
            S("Chrome"),
            I(ShowSidebar),
            I(ShowStatus),
            S("Status bar segments"),
            I(StatusTheme),
            I(StatusView),
            I(StatusPosition),
            I(StatusPercent),
            I(StatusGauge),
            S("Content"),
            I(ImageMaxPx),
            I(CodeWrap),
            I(ChapterLock),
            S("Input"),
            I(Mouse),
        ],
        Mode::Library => vec![
            S("View"),
            I(LibLayout),
            I(GridSize),
            S("Appearance"),
            I(Theme),
            S("Input"),
            I(Mouse),
        ],
    }
}

/// Index of the first selectable item in a scope (skips a leading section header).
pub fn first_setting_row(scope: Mode) -> usize {
    settings_rows(scope)
        .iter()
        .position(|r| matches!(r, SettingRow::Item(_)))
        .unwrap_or(0)
}

/// Rects from the last render, used for mouse hit-testing.
#[derive(Default)]
pub struct LayoutRects {
    pub sidebar: Option<Rect>,
    pub content: Option<Rect>,
}

/// Clickable regions captured during the last render, for mouse hit-testing.
/// Rebuilt every frame by the view layer; consulted by `on_mouse`.
#[derive(Default)]
pub struct MouseHits {
    /// Library: (book index, screen rect) for each visible row / grid cell.
    pub books: Vec<(usize, Rect)>,
    /// Editor tab strip: (tab, rect).
    pub edit_tabs: Vec<(EditTab, Rect)>,
    /// Editor Details/File fields: (row index, value-start column, rect).
    pub edit_fields: Vec<(usize, u16, Rect)>,
    /// Editor Online/Cover results: (result index, rect).
    pub edit_results: Vec<(usize, Rect)>,
    /// Editor search bar rect.
    pub edit_search: Option<Rect>,
}

impl MouseHits {
    pub fn clear(&mut self) {
        self.books.clear();
        self.edit_tabs.clear();
        self.edit_fields.clear();
        self.edit_results.clear();
        self.edit_search = None;
    }
}

/// A reading position, for the navigation (back/forward) history.
#[derive(Clone, Copy)]
struct Pos {
    section: usize,
    scroll: usize,
}

pub struct Reader {
    pub doc: Box<dyn Document>,
    pub outline: Vec<OutlineItem>,
    pub section: usize,
    pub blocks: Vec<Block>,
    /// Wrapped display lines of the current section, valid for `wrap_width`.
    pub lines: Vec<DisplayLine>,
    pub wrap_width: usize,
    /// syntect theme desired for code (set each render from the active theme).
    pub code_theme: String,
    /// syntect theme the current `lines` were wrapped with.
    wrap_theme: String,
    /// Desired spacing (set each render from config).
    pub line_spacing: u8,
    pub paragraph_spacing: u8,
    wrap_line_spacing: u8,
    wrap_para_spacing: u8,
    /// Code rendering (set each render from config / panning).
    pub code_wrap: bool,
    pub code_hscroll: usize,
    wrap_code_wrap: bool,
    wrap_code_hscroll: usize,
    /// Keep scrolling within the current chapter (set each render from config).
    pub chapter_lock: bool,
    /// Cached (outline index, line) for the current section's entries, recomputed
    /// on re-wrap; drives the TOC scroll-spy cheaply.
    heading_lines: Vec<(usize, usize)>,
    /// Built image protocols, reused across sections (revisiting a section
    /// reuses the already-uploaded image instead of re-transmitting). LRU.
    image_cache: LruCache<ImgKey, ImagePlan>,
    /// Current section's image index → cache key.
    section_images: HashMap<usize, ImgKey>,
    /// Reserved rows per image index, estimated up front so reflow doesn't wait
    /// on the background build.
    image_rows_estimate: Vec<u16>,
    /// (section, avail-cols, max-rows, max-px) the current estimates are for.
    images_key: (usize, u16, u16, u16),
    /// Image builds currently in flight (avoid dispatching duplicates).
    img_requested: HashSet<ImgKey>,
    /// Image builds that failed (so we stop waiting / re-requesting).
    img_failed: HashSet<ImgKey>,
    /// Terminal image ids evicted from the cache, to be deleted from the
    /// terminal by the main loop.
    pending_deletes: Vec<u32>,
    /// Text queued to be copied to the system clipboard by the main loop.
    pending_clipboard: Option<String>,
    /// A transient status-bar message (e.g. "copied"), cleared on next key.
    pub flash: Option<String>,
    /// images_key the current `lines` were wrapped against.
    wrap_images_key: (usize, u16, u16, u16),
    /// Index of the top visible line within `lines`.
    pub scroll: usize,
    /// Requested but not-yet-applied line movement; eased a few lines per frame
    /// so a flood of held-key repeats scrolls smoothly instead of jumping.
    scroll_pending: isize,
    pub focus: Focus,
    pub sidebar_sel: usize,
    /// Top visible row of the TOC viewport (free mouse scroll / centered cursor).
    pub sidebar_offset: usize,
    /// TOC viewport height in rows, refreshed each draw.
    pub sidebar_h: usize,
    /// Last active (scroll-spy) row the TOC auto-followed to.
    last_active: Option<usize>,
    /// Height of one column in lines, refreshed each draw.
    pub viewport_lines: usize,
    /// Total lines visible at once (2 columns in two-page mode), for scroll math.
    pub page_lines: usize,
    /// Wrap width used by the last render; used to locate jump targets.
    pub last_measure: usize,
    /// A saved within-section fraction to restore on the next draw (resume).
    pub pending_frac: Option<f32>,
    /// Collapsed parent rows (outline indices) in the sidebar tree.
    collapsed: HashSet<usize>,
    /// Navigation history (jump list).
    back_stack: Vec<Pos>,
    fwd_stack: Vec<Pos>,
    /// In-book search state.
    pub searching: bool,
    pub search_input: String,
    pub search_mode: SearchMode,
    /// The active matcher (set when a search runs); drives highlighting.
    pub search_matcher: Option<Matcher>,
    search_matches: Vec<(usize, usize)>,
    pub search_idx: usize,
    /// Recent queries, most-recent last; recalled with Up/Down in the prompt.
    search_history: Vec<String>,
    /// Position while browsing history in the prompt (None = editing fresh).
    history_pos: Option<usize>,
    /// Decoded section blocks, keyed by section index (bounded LRU).
    cache: HashMap<usize, Vec<Block>>,
    /// Sections requested from the loader but not yet returned.
    requested: HashSet<usize>,
    /// Channel to ask the background loader for a section.
    req_tx: Sender<usize>,
    /// Channel of decoded sections from the background loader.
    res_rx: Receiver<(usize, Vec<Block>)>,
}

impl Reader {
    pub fn new(mut doc: Box<dyn Document>) -> Result<Self> {
        let outline = doc.outline().to_vec();

        // Background loader: a worker thread that decodes sections on request.
        let mut loader = doc.loader();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<usize>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<(usize, Vec<Block>)>();
        thread::spawn(move || {
            while let Ok(index) = req_rx.recv() {
                let blocks = loader.load(index);
                if res_tx.send((index, blocks)).is_err() {
                    break; // reader dropped
                }
            }
        });

        let first = doc.load_section(0).unwrap_or_default().blocks;
        let mut cache = HashMap::new();
        cache.insert(0usize, first.clone());

        let mut reader = Self {
            doc,
            outline,
            section: 0,
            blocks: first,
            lines: Vec::new(),
            wrap_width: 0,
            code_theme: theme::default_theme().syntect.to_string(),
            wrap_theme: String::new(),
            line_spacing: 0,
            paragraph_spacing: 1,
            wrap_line_spacing: 0,
            wrap_para_spacing: 1,
            code_wrap: true,
            code_hscroll: 0,
            wrap_code_wrap: true,
            wrap_code_hscroll: 0,
            chapter_lock: false,
            heading_lines: Vec::new(),
            image_cache: LruCache::new(NonZeroUsize::new(IMAGE_CACHE_CAP).unwrap()),
            section_images: HashMap::new(),
            image_rows_estimate: Vec::new(),
            images_key: (usize::MAX, 0, 0, 0),
            img_requested: HashSet::new(),
            img_failed: HashSet::new(),
            pending_deletes: Vec::new(),
            pending_clipboard: None,
            flash: None,
            wrap_images_key: (usize::MAX, 0, 0, 0),
            scroll: 0,
            scroll_pending: 0,
            focus: Focus::Content,
            sidebar_sel: 0,
            sidebar_offset: 0,
            sidebar_h: 1,
            last_active: None,
            viewport_lines: 1,
            page_lines: 1,
            last_measure: 72,
            pending_frac: None,
            collapsed: HashSet::new(),
            back_stack: Vec::new(),
            fwd_stack: Vec::new(),
            searching: false,
            search_input: String::new(),
            search_mode: SearchMode::Plain,
            search_matcher: None,
            search_matches: Vec::new(),
            search_idx: 0,
            search_history: Vec::new(),
            history_pos: None,
            cache,
            requested: HashSet::new(),
            req_tx,
            res_rx,
        };
        reader.prefetch_neighbors();
        Ok(reader)
    }

    /// Collect any sections the loader has finished into the cache.
    fn drain_loader(&mut self) {
        while let Ok((index, blocks)) = self.res_rx.try_recv() {
            self.requested.remove(&index);
            self.cache.insert(index, blocks);
        }
    }

    /// Blocks for a section: cache first, else decode synchronously.
    fn fetch_blocks(&mut self, section: usize) -> Vec<Block> {
        self.drain_loader();
        if let Some(blocks) = self.cache.get(&section) {
            return blocks.clone();
        }
        let blocks = self
            .doc
            .load_section(section)
            .map(|s| s.blocks)
            .unwrap_or_default();
        self.cache.insert(section, blocks.clone());
        blocks
    }

    /// Ask the loader to pre-decode the adjacent chapters, and bound the cache.
    fn prefetch_neighbors(&mut self) {
        self.drain_loader();
        let n = self.doc.section_count();
        let mut targets = Vec::new();
        if self.section + 1 < n {
            targets.push(self.section + 1);
        }
        if self.section > 0 {
            targets.push(self.section - 1);
        }
        for t in targets {
            if !self.cache.contains_key(&t) && self.requested.insert(t) {
                let _ = self.req_tx.send(t);
            }
        }
        self.evict();
    }

    /// Drop cached sections farthest from the current one when over capacity.
    fn evict(&mut self) {
        while self.cache.len() > CACHE_CAP {
            let current = self.section;
            match self
                .cache
                .keys()
                .copied()
                .filter(|&k| k != current)
                .max_by_key(|&k| k.abs_diff(current))
            {
                Some(far) => {
                    self.cache.remove(&far);
                }
                None => break,
            }
        }
    }

    /// Re-wrap the current section if the measure changed.
    pub fn ensure_wrapped(&mut self, width: usize) {
        if width != self.wrap_width
            || self.code_theme != self.wrap_theme
            || self.line_spacing != self.wrap_line_spacing
            || self.paragraph_spacing != self.wrap_para_spacing
            || self.code_wrap != self.wrap_code_wrap
            || self.code_hscroll != self.wrap_code_hscroll
            || self.images_key != self.wrap_images_key
        {
            self.lines = wrap_blocks(
                &self.blocks,
                &WrapOpts {
                    width,
                    code_theme: &self.code_theme,
                    line_spacing: self.line_spacing,
                    para_spacing: self.paragraph_spacing,
                    code_wrap: self.code_wrap,
                    code_hscroll: self.code_hscroll,
                },
                &self.image_rows_estimate,
            );
            self.wrap_width = width;
            self.wrap_theme = self.code_theme.clone();
            self.wrap_line_spacing = self.line_spacing;
            self.wrap_para_spacing = self.paragraph_spacing;
            self.wrap_code_wrap = self.code_wrap;
            self.wrap_code_hscroll = self.code_hscroll;
            self.wrap_images_key = self.images_key;
            self.recompute_heading_lines();
        }
    }

    /// Recompute each current-section outline entry's line position (for the
    /// TOC scroll-spy). Done once per re-wrap, not per frame.
    fn recompute_heading_lines(&mut self) {
        let mut hl = Vec::new();
        for (oi, e) in self.outline.iter().enumerate() {
            if e.section != self.section {
                continue;
            }
            let line = match &e.locator {
                Some(loc) => find_line(&self.lines, loc).unwrap_or(0),
                None => 0,
            };
            hl.push((oi, line));
        }
        self.heading_lines = hl;
    }

    /// Collect any finished background image builds, and — when the section or
    /// size changes — estimate each image's rows (cheaply, for reflow) and
    /// dispatch the protocol builds to the worker. Never blocks on encoding.
    pub fn sync_images(
        &mut self,
        builder: &ImageBuilder,
        picker: &Picker,
        avail: u16,
        max_rows: u16,
        max_px: u16,
    ) {
        // Tell the worker where we are so it can drop builds for far-away
        // sections (avoids a fast-scroll backlog delaying the current one).
        builder.set_current(self.section);

        // 1. Move finished builds into the cache; evictions free the terminal image.
        for done in builder.poll() {
            self.img_requested.remove(&done.key);
            if done.stale {
                continue; // skipped as far-away; re-requested if it's needed again
            }
            match done.plan {
                Some(plan) => {
                    if let Some((_, evicted)) = self.image_cache.push(done.key, plan) {
                        if let Some(id) = evicted.image_id() {
                            self.pending_deletes.push(id);
                        }
                    }
                }
                None => {
                    self.img_failed.insert(done.key);
                }
            }
        }

        // 2. On section/size change, remap the current section and dispatch any
        //    builds it still needs.
        let key = (self.section, avail, max_rows, max_px);
        if self.images_key != key {
            self.images_key = key;
            self.remap_section_images(builder, picker, avail, max_rows, max_px);
        }

        // 3. Keep the visible section's images most-recently-used so they aren't
        //    evicted while on screen.
        let keys: Vec<ImgKey> = self.section_images.values().copied().collect();
        for k in keys {
            self.image_cache.get(&k);
        }

        // 4. Pre-build neighbouring sections' images once the current one is ready.
        if !self.images_pending() {
            self.prefetch_neighbor_images(builder, avail, max_rows, max_px);
        }
    }

    /// Map the current section's images to cache keys, estimate their rows for
    /// reflow, and request builds for any not already cached/in-flight/failed.
    fn remap_section_images(
        &mut self,
        builder: &ImageBuilder,
        picker: &Picker,
        avail: u16,
        max_rows: u16,
        max_px: u16,
    ) {
        let fs = picker.font_size();
        let (fw, fh) = (fs.width, fs.height);
        let mut section_images = HashMap::new();
        let mut estimates = Vec::new();
        let mut requests: Vec<(ImgKey, Vec<u8>)> = Vec::new();
        let mut idx = 0;
        for block in &self.blocks {
            if let Block::Image { data, .. } = block {
                let key = ImgKey { section: self.section, idx, avail, max_rows, max_px };
                let rows = if let Some(plan) = self.image_cache.peek(&key) {
                    plan.rows
                } else if data.is_empty() {
                    0
                } else {
                    media::image_dimensions(data)
                        .map(|(w, h)| media::target_cells(w, h, fw, fh, avail, max_rows, max_px).1)
                        .unwrap_or(0)
                };
                estimates.push(rows);
                section_images.insert(idx, key);
                if rows > 0
                    && !self.image_cache.contains(&key)
                    && !self.img_requested.contains(&key)
                    && !self.img_failed.contains(&key)
                {
                    requests.push((key, data.clone()));
                }
                idx += 1;
            }
        }
        self.section_images = section_images;
        self.image_rows_estimate = estimates;
        for (k, bytes) in requests {
            self.img_requested.insert(k);
            builder.request(k, bytes);
        }
    }

    /// Build the adjacent sections' images ahead of time (from already-prefetched
    /// blocks) so crossing a chapter boundary is instant. Never forces a load.
    fn prefetch_neighbor_images(
        &mut self,
        builder: &ImageBuilder,
        avail: u16,
        max_rows: u16,
        max_px: u16,
    ) {
        let n = self.doc.section_count();
        let neighbors = [self.section + 1, self.section.wrapping_sub(1)];
        let mut requests: Vec<(ImgKey, Vec<u8>)> = Vec::new();
        for &sec in &neighbors {
            if sec >= n || sec == self.section {
                continue;
            }
            let Some(blocks) = self.cache.get(&sec) else {
                continue;
            };
            let mut idx = 0;
            for block in blocks {
                if let Block::Image { data, .. } = block {
                    if !data.is_empty() {
                        let key = ImgKey { section: sec, idx, avail, max_rows, max_px };
                        if !self.image_cache.contains(&key)
                            && !self.img_requested.contains(&key)
                            && !self.img_failed.contains(&key)
                        {
                            requests.push((key, data.clone()));
                        }
                    }
                    idx += 1;
                }
            }
        }
        for (k, bytes) in requests {
            self.img_requested.insert(k);
            builder.request(k, bytes);
        }
    }

    /// Look up a built plan for the current section's image `idx`.
    pub fn image_plan(&self, idx: usize) -> Option<&ImagePlan> {
        let key = self.section_images.get(&idx)?;
        self.image_cache.peek(key)
    }

    /// Drain terminal image ids that should be deleted (evicted from cache).
    pub fn take_image_deletes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending_deletes)
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// Raw lines of the `n`-th code block in the current section.
    fn code_block(&self, n: usize) -> Option<&[String]> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Code { lines, .. } => Some(lines.as_slice()),
                _ => None,
            })
            .nth(n)
    }

    /// Copy the code block currently in view (the topmost visible one) to the
    /// system clipboard. Returns the number of lines copied.
    pub fn copy_visible_code(&mut self) -> Option<usize> {
        let end = (self.scroll + self.viewport_lines).min(self.lines.len());
        let idx = self.lines[self.scroll.min(self.lines.len())..end]
            .iter()
            .find_map(|l| match l.kind {
                crate::layout::LineKind::Code(i) => Some(i),
                _ => None,
            })?;
        let lines = self.code_block(idx)?;
        let text = lines.join("\n");
        let n = lines.len();
        self.pending_clipboard = Some(text);
        self.flash = Some(format!("✓ copied {n} line{} of code", if n == 1 { "" } else { "s" }));
        Some(n)
    }

    /// Whether a smooth scroll is currently in progress (so heavy image
    /// transmits can be deferred until it settles).
    pub fn is_scrolling(&self) -> bool {
        self.scroll_pending != 0
    }

    /// Are any of the current section's images still building (so the loop
    /// should keep redrawing until they pop in)?
    pub fn images_pending(&self) -> bool {
        self.image_rows_estimate.iter().enumerate().any(|(i, &rows)| {
            rows > 0
                && self
                    .section_images
                    .get(&i)
                    .is_some_and(|k| !self.image_cache.contains(k) && !self.img_failed.contains(k))
        })
    }

    pub fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.page_lines.max(1))
    }

    pub fn clamp_scroll(&mut self) {
        let m = self.max_scroll();
        if self.scroll > m {
            self.scroll = m;
        }
    }

    pub fn load(&mut self, section: usize) {
        if section >= self.doc.section_count() {
            return;
        }
        self.section = section;
        self.blocks = self.fetch_blocks(section);
        self.scroll = 0;
        self.wrap_width = 0; // force a re-wrap on next draw
        self.prefetch_neighbors();
    }

    /// Request a smooth line movement (eased by `step_scroll`).
    pub fn queue_scroll(&mut self, delta: isize) {
        self.scroll_pending += delta;
    }

    /// Apply a few lines of the pending movement; called once per frame.
    /// Returns whether anything moved (so the loop keeps animating).
    pub fn step_scroll(&mut self) -> bool {
        if self.scroll_pending == 0 {
            return false;
        }
        let step = self.scroll_pending.clamp(-3, 3);
        let before = (self.section, self.scroll);
        if step > 0 {
            self.scroll_down(step as usize);
        } else {
            self.scroll_up((-step) as usize);
        }
        self.scroll_pending -= step;
        let moved = before != (self.section, self.scroll);
        if !moved {
            self.scroll_pending = 0; // hit the start/end of the book
        }
        moved
    }

    /// Scroll down, flowing into the next chapter at the bottom edge.
    pub fn scroll_down(&mut self, n: usize) {
        let max = self.max_scroll();
        if self.scroll < max {
            self.scroll = (self.scroll + n).min(max);
        } else if !self.chapter_lock && self.section + 1 < self.doc.section_count() {
            self.load(self.section + 1);
        }
    }

    /// Scroll up, flowing into the previous chapter at the top edge (unless
    /// chapter-locked).
    pub fn scroll_up(&mut self, n: usize) {
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_sub(n);
        } else if !self.chapter_lock && self.section > 0 {
            self.load(self.section - 1);
            self.scroll = usize::MAX; // clamped to the bottom on next draw
        }
    }

    /// Jump to the next/previous chapter (works regardless of chapter-lock).
    pub fn next_chapter(&mut self) {
        if self.section + 1 < self.doc.section_count() {
            self.jump_to(self.section + 1, None);
        }
    }

    pub fn prev_chapter(&mut self) {
        if self.section > 0 {
            self.jump_to(self.section - 1, None);
        }
    }

    /// Navigate to a section and, if given, scroll to the line whose text
    /// matches `locator` (a heading). Misses fall back to the section top.
    pub fn jump_to(&mut self, section: usize, locator: Option<&str>) {
        self.push_history();
        if section != self.section {
            self.load(section);
        } else {
            self.scroll = 0;
        }
        if let Some(text) = locator {
            self.ensure_wrapped(self.last_measure.max(1));
            if let Some(line) = find_line(&self.lines, text) {
                self.scroll = line;
            }
        }
        self.focus = Focus::Content;
    }

    pub fn sidebar_move(&mut self, delta: isize) {
        let n = self.outline_visible().len();
        if n == 0 {
            return;
        }
        let last = n as isize - 1;
        self.sidebar_sel = (self.sidebar_sel as isize + delta).clamp(0, last) as usize;
        self.center_sidebar();
    }

    /// Centre the TOC viewport on the keyboard cursor (so it doesn't ride the
    /// top/bottom edge), clamped to the list bounds.
    pub fn center_sidebar(&mut self) {
        let h = self.sidebar_h.max(1);
        let len = self.outline_visible().len();
        let max_off = len.saturating_sub(h);
        self.sidebar_offset = self.sidebar_sel.saturating_sub(h / 2).min(max_off);
    }

    /// Free-scroll the TOC viewport by `delta` rows (mouse wheel) without moving
    /// the selection.
    pub fn sidebar_wheel(&mut self, delta: isize) {
        let len = self.outline_visible().len();
        let max_off = len.saturating_sub(self.sidebar_h.max(1)) as isize;
        self.sidebar_offset = (self.sidebar_offset as isize + delta).clamp(0, max_off) as usize;
    }

    /// Refresh the TOC viewport each draw: record its height, and while reading
    /// (content focused) keep it following the scroll-spy position — but only
    /// when that position moves, so a manual wheel-scroll isn't fought.
    pub fn update_sidebar_view(&mut self, height: usize) {
        self.sidebar_h = height.max(1);
        let len = self.outline_visible().len();
        let max_off = len.saturating_sub(self.sidebar_h);
        // While reading, keep the active entry in view by scrolling minimally
        // (not re-centering), so the TOC stays stable and clicks land where
        // they're shown. Only act when the position actually moves.
        if self.focus == Focus::Content {
            let active = self.active_outline_row();
            if active != self.last_active {
                self.last_active = active;
                if let Some(a) = active {
                    if a < self.sidebar_offset {
                        self.sidebar_offset = a;
                    } else if a >= self.sidebar_offset + self.sidebar_h {
                        self.sidebar_offset = a + 1 - self.sidebar_h;
                    }
                }
            }
        }
        self.sidebar_offset = self.sidebar_offset.min(max_off);
    }

    /// The outline entry matching the current reading position: the deepest
    /// heading in the current section at or above the top of the viewport
    /// (scroll-spy), falling back to the section's first entry. Reads the cached
    /// `heading_lines` so it's cheap per frame.
    pub fn active_outline(&self) -> Option<usize> {
        let mut best: Option<(usize, usize)> = None; // (line, outline index)
        for &(oi, line) in &self.heading_lines {
            // Greatest line at/above the viewport top; on ties keep the earlier
            // entry (strictly greater to replace).
            if line <= self.scroll && best.is_none_or(|(bl, _)| line > bl) {
                best = Some((line, oi));
            }
        }
        best.map(|(_, oi)| oi)
            .or_else(|| self.heading_lines.first().map(|&(oi, _)| oi))
    }

    /// Position of `active_outline` within the visible (collapsed-aware) list.
    pub fn active_outline_row(&self) -> Option<usize> {
        let active = self.active_outline()?;
        self.outline_visible().iter().position(|&oi| oi == active)
    }

    /// Outline indices currently visible (respecting collapsed parents).
    pub fn outline_visible(&self) -> Vec<usize> {
        let mut vis = Vec::new();
        let mut hide_depth: Option<usize> = None;
        for (i, item) in self.outline.iter().enumerate() {
            if let Some(d) = hide_depth {
                if item.depth > d {
                    continue;
                }
                hide_depth = None;
            }
            vis.push(i);
            if self.outline_is_parent(i) && self.collapsed.contains(&i) {
                hide_depth = Some(item.depth);
            }
        }
        vis
    }

    /// Does the row at `i` have nested children?
    pub fn outline_is_parent(&self, i: usize) -> bool {
        self.outline
            .get(i + 1)
            .is_some_and(|n| n.depth > self.outline[i].depth)
    }

    pub fn outline_collapsed(&self, i: usize) -> bool {
        self.collapsed.contains(&i)
    }

    fn selected_outline(&self) -> Option<usize> {
        self.outline_visible().get(self.sidebar_sel).copied()
    }

    /// Jump to the selected sidebar row.
    pub fn sidebar_activate(&mut self) {
        if let Some(oi) = self.selected_outline() {
            if let Some(item) = self.outline.get(oi).cloned() {
                self.jump_to(item.section, item.locator.as_deref());
            }
        }
    }

    /// `l`/→: expand a collapsed parent, otherwise jump.
    pub fn sidebar_expand(&mut self) {
        let Some(oi) = self.selected_outline() else {
            return;
        };
        if self.outline_is_parent(oi) && self.collapsed.contains(&oi) {
            self.collapsed.remove(&oi);
        } else {
            self.sidebar_activate();
        }
    }

    /// `h`/←: collapse an expanded parent, otherwise move to the parent row.
    pub fn sidebar_collapse(&mut self) {
        let Some(oi) = self.selected_outline() else {
            return;
        };
        if self.outline_is_parent(oi) && !self.collapsed.contains(&oi) {
            self.collapsed.insert(oi);
        } else {
            let depth = self.outline[oi].depth;
            if depth > 0 {
                if let Some(pi) = (0..oi).rev().find(|&j| self.outline[j].depth < depth) {
                    if let Some(pos) = self.outline_visible().iter().position(|&x| x == pi) {
                        self.sidebar_sel = pos;
                    }
                }
            }
        }
    }

    fn push_history(&mut self) {
        self.back_stack.push(Pos {
            section: self.section,
            scroll: self.scroll,
        });
        if self.back_stack.len() > 200 {
            self.back_stack.remove(0);
        }
        self.fwd_stack.clear();
    }

    pub fn history_back(&mut self) {
        if let Some(pos) = self.back_stack.pop() {
            self.fwd_stack.push(Pos {
                section: self.section,
                scroll: self.scroll,
            });
            self.goto(pos);
        }
    }

    pub fn history_forward(&mut self) {
        if let Some(pos) = self.fwd_stack.pop() {
            self.back_stack.push(Pos {
                section: self.section,
                scroll: self.scroll,
            });
            self.goto(pos);
        }
    }

    fn goto(&mut self, pos: Pos) {
        if pos.section != self.section {
            self.load(pos.section);
        }
        self.scroll = pos.scroll;
        self.focus = Focus::Content;
    }

    pub fn start_search(&mut self) {
        self.searching = true;
        self.search_input.clear();
        self.history_pos = None;
    }

    pub fn search_count(&self) -> usize {
        self.search_matches.len()
    }

    /// Cycle the search mode (plain → regex → fuzzy) while typing a query.
    pub fn cycle_search_mode(&mut self) {
        self.search_mode = self.search_mode.next();
    }

    /// Recall the previous (`-1`) or next (`+1`) query from history into the
    /// prompt.
    pub fn search_history_recall(&mut self, dir: i32) {
        if self.search_history.is_empty() {
            return;
        }
        let len = self.search_history.len();
        let pos = match (self.history_pos, dir) {
            (None, -1) => len - 1,
            (Some(p), -1) => p.saturating_sub(1),
            (Some(p), 1) if p + 1 < len => p + 1,
            (Some(_), 1) => {
                // Past the newest → back to a fresh, empty prompt.
                self.history_pos = None;
                self.search_input.clear();
                return;
            }
            _ => return,
        };
        self.history_pos = Some(pos);
        self.search_input = self.search_history[pos].clone();
    }

    /// Run the typed query across the whole book in the current mode, recording
    /// matching (section, line) positions and jumping to the first.
    pub fn run_search(&mut self) {
        self.searching = false;
        let query = self.search_input.trim().to_string();
        self.search_matches.clear();
        self.search_idx = 0;
        self.history_pos = None;
        if query.is_empty() {
            self.search_matcher = None;
            return;
        }

        // Record in history (dedup, most-recent last, bounded).
        self.search_history.retain(|q| q != &query);
        self.search_history.push(query.clone());
        if self.search_history.len() > 50 {
            self.search_history.remove(0);
        }

        let matcher = Matcher::new(self.search_mode, &query);
        if matcher.is_valid() {
            let width = self.last_measure.max(1);
            for s in 0..self.doc.section_count() {
                let blocks = self.fetch_blocks(s);
                let lines = wrap_blocks(
                    &blocks,
                    &WrapOpts {
                        width,
                        code_theme: &self.code_theme,
                        line_spacing: self.line_spacing,
                        para_spacing: self.paragraph_spacing,
                        // Search always wraps code so no matches are hidden off-screen.
                        code_wrap: true,
                        code_hscroll: 0,
                    },
                    &[],
                );
                for (li, line) in lines.iter().enumerate() {
                    if matcher.matches(&line.text()) {
                        self.search_matches.push((s, li));
                    }
                }
            }
        }
        self.search_matcher = Some(matcher);
        if !self.search_matches.is_empty() {
            self.goto_match(0);
        }
    }

    pub fn search_next(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        let i = (self.search_idx + 1) % self.search_matches.len();
        self.goto_match(i);
    }

    pub fn search_prev(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        let n = self.search_matches.len();
        let i = (self.search_idx + n - 1) % n;
        self.goto_match(i);
    }

    fn goto_match(&mut self, i: usize) {
        let Some(&(section, line)) = self.search_matches.get(i) else {
            return;
        };
        self.search_idx = i;
        if section != self.section {
            self.load(section);
        }
        self.scroll = line;
        self.focus = Focus::Content;
    }

    /// Scroll position within the current section as a fraction `[0, 1]`.
    pub fn within_frac(&self) -> f32 {
        if self.lines.is_empty() {
            0.0
        } else {
            self.scroll as f32 / self.lines.len() as f32
        }
    }

    /// Apply a pending resume fraction once the section is wrapped.
    pub fn resolve_pending(&mut self) {
        if let Some(frac) = self.pending_frac.take() {
            let n = self.lines.len();
            self.scroll = ((frac * n as f32).round() as usize).min(n.saturating_sub(1));
        }
    }

    /// Overall reading progress in `[0, 1]`.
    pub fn progress(&self) -> f32 {
        let n = self.doc.section_count().max(1) as f32;
        let within = if self.lines.is_empty() {
            0.0
        } else {
            self.scroll as f32 / self.lines.len() as f32
        };
        ((self.section as f32) + within) / n
    }

    /// A short text quote of the first non-blank visible line, used to anchor
    /// annotations so they survive reflow.
    pub fn current_quote(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let start = self.scroll.min(self.lines.len() - 1);
        self.lines[start..]
            .iter()
            .map(|l| l.text())
            .find(|t| !t.trim().is_empty())
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(80)
            .collect()
    }

    pub fn chapter_title(&self) -> String {
        // Prefer the entry at the current reading position (handles single-file
        // books where the section never changes); else the section's first entry.
        self.active_outline()
            .or_else(|| self.outline.iter().position(|e| e.section == self.section))
            .and_then(|oi| self.outline.get(oi))
            .map(|e| e.label.clone())
            .unwrap_or_else(|| format!("Section {}", self.section + 1))
    }
}

/// First wrapped line whose normalized text matches `needle`. Prefers a line
/// that *is* the heading before falling back to a substring match, so a short
/// heading like "Linux" lands on the header rather than an earlier mention.
fn find_line(lines: &[DisplayLine], needle: &str) -> Option<usize> {
    let n = loose_key(needle);
    if n.is_empty() {
        return None;
    }
    if let Some(i) = lines.iter().position(|l| loose_key(&l.text()) == n) {
        return Some(i);
    }
    lines.iter().position(|l| {
        let line = loose_key(&l.text());
        !line.is_empty() && (line.contains(&n) || (n.len() >= 8 && n.contains(&line)))
    })
}

/// Lowercase, drop punctuation, collapse whitespace — a tolerant key so TOC
/// labels match body headings that differ only in punctuation (e.g. a stray
/// comma) or spacing.
fn loose_key(s: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.extend(c.to_lowercase());
        } else {
            pending_space = true;
        }
    }
    out
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
    /// Multi-selection for bulk actions, keyed by book path (stable across sort).
    /// Populated by the vim-style visual mode below.
    pub lib_marked: HashSet<String>,
    /// Visual-select anchor (book index) while in visual mode; `None` otherwise.
    /// The selection is the contiguous range between the anchor and `lib_sel`.
    pub lib_visual: Option<usize>,
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
    epub::read_metadata(path).ok().and_then(|(m, _)| m.cover.map(|(b, _)| b))
}

/// Fill a rename template from the edited metadata `values` (in [`META_FIELDS`]
/// order) and the file `ext`. Placeholders: `%T` title, `%A` author, `%Y` year,
/// `%S` series, `%I` series index, `%P` publisher, `%E` extension; `%%` → `%`.
/// The result is sanitized for use as a filename.
pub fn fill_template(template: &str, values: &[String], ext: &str) -> String {
    let get = |i: usize| values.get(i).map(String::as_str).unwrap_or("").trim();
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('T') => out.push_str(get(0)),
            Some('A') => out.push_str(get(1)),
            Some('Y') => out.push_str(get(2)),
            Some('S') => out.push_str(get(3)),
            Some('I') => out.push_str(get(4)),
            Some('P') => out.push_str(get(5)),
            Some('E') => out.push_str(ext),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    sanitize_filename(&out)
}

/// Make a string safe as a single filename: drop path separators and characters
/// that misbehave across filesystems, collapse whitespace, trim.
fn sanitize_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        let mapped = match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => Some(' '),
            c if c.is_control() => Some(' '),
            c => Some(c),
        };
        if let Some(m) = mapped {
            if m == ' ' {
                if !last_space {
                    out.push(' ');
                }
                last_space = true;
            } else {
                out.push(m);
                last_space = false;
            }
        }
    }
    out.trim().to_string()
}

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

/// The six editable metadata fields, in [`META_FIELDS`] order, from a document's
/// [`Metadata`]. Shared by the editor's prefill and reset-to-source.
fn meta_fields_from(m: &Metadata) -> Vec<String> {
    vec![
        m.title.clone(),
        m.author_line(),
        m.year.map(|y| y.to_string()).unwrap_or_default(),
        m.series.clone().unwrap_or_default(),
        m.series_index.map(fmt_series_index).unwrap_or_default(),
        m.publisher.clone().unwrap_or_default(),
        m.subtitle.clone().unwrap_or_default(),
        m.identifier.clone().unwrap_or_default(),
        m.language.clone().unwrap_or_default(),
    ]
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
    if let Some(store) = store {
        if let Some(p) = store.load_progress(&book_path) {
            config.view_mode = p.view_mode;
            if let Some(t) = theme::by_name(&p.theme) {
                config.theme = t;
            }
            reader.load(p.section);
            reader.pending_frac = Some(p.frac);
        }
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
            lib_visual: None,
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
            lib_visual: None,
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

    /// (collection name, is-member) pairs for a book, sorted by name.
    fn shelf_membership(&self, path: &str) -> Vec<(String, bool)> {
        let Some(store) = &self.store else {
            return Vec::new();
        };
        let on: HashSet<String> = store.shelves_for(path).into_iter().collect();
        store
            .all_shelves()
            .into_iter()
            .map(|(name, _)| {
                let member = on.contains(&name);
                (name, member)
            })
            .collect()
    }

    /// Open the tabbed metadata editor on the selected book.
    fn open_meta_edit(&mut self) {
        let Some(b) = self.lib_books.get(self.lib_sel) else {
            return;
        };
        let path = b.path.clone();
        let book_title = b.title.clone();
        let q = format!("{} {}", b.title, b.author).trim().to_string();
        let values = vec![
            b.title.clone(),
            b.author.clone(),
            b.year.map(|y| y.to_string()).unwrap_or_default(),
            b.series.clone(),
            b.series_index.map(fmt_series_index).unwrap_or_default(),
            b.publisher.clone(),
            b.subtitle.clone(),
            b.isbn.clone(),
            b.language.clone(),
        ];
        // The EPUB's declared metadata, for per-field reset (best-effort).
        let original = epub::read_metadata(&path)
            .map(|(m, _)| meta_fields_from(&m))
            .unwrap_or_else(|_| vec![String::new(); META_FIELDS.len()]);
        let cursor = values[0].chars().count();
        let shelves = self.shelf_membership(&path);
        self.meta_edit = Some(MetaEdit {
            path,
            book_title,
            tab: EditTab::Details,
            mode: EditMode::Nav,
            values,
            original,
            row: 0,
            cursor,
            shelves,
            shelf_sel: 0,
            new_shelf: None,
            online: Search {
                q: q.clone(),
                ..Search::default()
            },
            cover_search: Search {
                q,
                ..Search::default()
            },
            cover_hits: Vec::new(),
            cover_pending: false,
            cover: None,
            preview_cover: None,
            preview_url: String::new(),
            rename_template: DEFAULT_RENAME_TEMPLATE.to_string(),
            rename_name: String::new(),
            file_row: FILE_TEMPLATE,
            status: None,
        });
        self.recompute_rename();
    }

    fn meta_edit_key(&mut self, key: KeyEvent) {
        let (mode, tab, typing_shelf) = match &self.meta_edit {
            Some(e) => (e.mode, e.tab, e.new_shelf.is_some()),
            None => return,
        };
        // Typing a new collection name takes precedence.
        if tab == EditTab::Collections && typing_shelf {
            self.meta_edit_new_shelf(key);
            return;
        }
        // Editing the search bar (Online/Cover tabs).
        let editing_query = matches!(tab, EditTab::Online | EditTab::Cover)
            && self.meta_edit.as_ref().is_some_and(|e| e.search().editing);
        if editing_query {
            self.online_query_key(key);
            return;
        }
        // Edit mode: keystrokes go into the focused field.
        if mode == EditMode::Edit {
            self.meta_edit_typing(key);
            return;
        }
        // Navigate mode.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.meta_edit = None,
            KeyCode::Char('s') if ctrl => {
                // On the File tab, ^S applies the rename first (no separate
                // button); a hard rename error keeps the editor open.
                let on_file = self.meta_edit.as_ref().map(|e| e.tab) == Some(EditTab::File);
                if !on_file || self.apply_rename() {
                    self.save_meta_edit();
                }
            }
            KeyCode::Tab => self.meta_edit_switch_tab(1),
            KeyCode::BackTab => self.meta_edit_switch_tab(-1),
            _ => match tab {
                EditTab::Details => self.details_nav_key(key),
                EditTab::Collections => self.collections_nav_key(key),
                EditTab::Online | EditTab::Cover => self.online_nav_key(key),
                EditTab::File => self.file_nav_key(key),
            },
        }
    }

    fn meta_edit_switch_tab(&mut self, delta: isize) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        let i = EditTab::ALL.iter().position(|t| *t == ed.tab).unwrap_or(0) as isize;
        let n = EditTab::ALL.len() as isize;
        ed.tab = EditTab::ALL[(i + delta).rem_euclid(n) as usize];
        ed.mode = EditMode::Nav;
        let tab = ed.tab;
        // Entering the File tab refreshes the previewed name from the template.
        if tab == EditTab::File {
            self.recompute_rename();
        }
        // Entering the Cover tab runs the cover search once, so candidates appear
        // without a manual search (uses the book's ISBN + the seeded query).
        if tab == EditTab::Cover
            && self.meta_edit.as_ref().is_some_and(|e| e.cover_hits.is_empty() && !e.cover_search.fetching)
        {
            self.online_search();
        }
    }

    /// Details tab, navigate mode: move between fields; Enter edits.
    fn details_nav_key(&mut self, key: KeyEvent) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        let last = META_FIELDS.len() - 1;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => ed.row = ed.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => ed.row = (ed.row + 1).min(last),
            KeyCode::Char('g') => ed.row = 0,
            KeyCode::Char('G') => ed.row = last,
            KeyCode::Enter => {
                ed.mode = EditMode::Edit;
                ed.cursor = ed.field_len();
            }
            // Reset the focused field (r) or all fields (R) to the EPUB value.
            KeyCode::Char('r') => {
                if let Some(orig) = ed.original.get(ed.row).cloned() {
                    ed.values[ed.row] = orig;
                }
            }
            KeyCode::Char('R') => ed.values = ed.original.clone(),
            _ => {}
        }
    }

    /// Edit mode: type into the focused field (Details or Online query).
    fn meta_edit_typing(&mut self, key: KeyEvent) {
        let recompute;
        {
            let Some(ed) = self.meta_edit.as_mut() else {
                return;
            };
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Esc | KeyCode::Enter => ed.mode = EditMode::Nav,
                KeyCode::Left => ed.cursor = ed.cursor.saturating_sub(1),
                KeyCode::Right => ed.cursor = (ed.cursor + 1).min(ed.cur_field_len()),
                KeyCode::Home => ed.cursor = 0,
                KeyCode::End => ed.cursor = ed.cur_field_len(),
                KeyCode::Char('u') if ctrl => {
                    if let Some(s) = ed.edit_target() {
                        s.clear();
                    }
                    ed.cursor = 0;
                }
                KeyCode::Backspace => {
                    let cur = ed.cursor;
                    let removed = ed.edit_target().is_some_and(|s| str_delete_before(s, cur));
                    if removed {
                        ed.cursor -= 1;
                    }
                }
                KeyCode::Delete => {
                    let cur = ed.cursor;
                    if let Some(s) = ed.edit_target() {
                        str_delete_at(s, cur);
                    }
                }
                KeyCode::Char(c) => {
                    let cur = ed.cursor;
                    let mut inserted = false;
                    if let Some(s) = ed.edit_target() {
                        str_insert(s, cur, c);
                        inserted = true;
                    }
                    if inserted {
                        ed.cursor += 1;
                    }
                }
                _ => {}
            }
            // Editing the rename template re-derives the previewed filename.
            recompute = ed.tab == EditTab::File && ed.file_row == FILE_TEMPLATE;
        }
        if recompute {
            self.recompute_rename();
        }
    }

    /// Collections tab, navigate mode: move the cursor, toggle membership, or
    /// start typing a new collection.
    fn collections_nav_key(&mut self, key: KeyEvent) {
        let new_row = match &self.meta_edit {
            Some(e) => e.new_shelf_row(),
            None => return,
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.shelf_sel = e.shelf_sel.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.shelf_sel = (e.shelf_sel + 1).min(new_row);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let sel = match &self.meta_edit {
                    Some(e) => e.shelf_sel,
                    None => return,
                };
                if sel == new_row {
                    if let Some(e) = self.meta_edit.as_mut() {
                        e.new_shelf = Some(String::new());
                    }
                } else {
                    self.toggle_editor_shelf(sel);
                }
            }
            _ => {}
        }
    }

    /// Toggle the book's membership in collection `sel`, in place (live to store).
    fn toggle_editor_shelf(&mut self, sel: usize) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        let Some((name, member)) = ed.shelves.get_mut(sel) else {
            return;
        };
        if let Some(store) = &self.store {
            if *member {
                store.remove_from_shelf(&ed.path, name);
            } else {
                store.add_to_shelf(&ed.path, name);
            }
        }
        *member = !*member;
    }

    /// Collections tab: typing/confirming a new collection name.
    fn meta_edit_new_shelf(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.new_shelf = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(b) = self.meta_edit.as_mut().and_then(|e| e.new_shelf.as_mut()) {
                    b.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(b) = self.meta_edit.as_mut().and_then(|e| e.new_shelf.as_mut()) {
                    b.push(c);
                }
            }
            KeyCode::Enter => {
                let (name, path) = match &self.meta_edit {
                    Some(e) => (
                        e.new_shelf.clone().unwrap_or_default().trim().to_string(),
                        e.path.clone(),
                    ),
                    None => return,
                };
                if !name.is_empty() {
                    if let Some(store) = &self.store {
                        store.add_to_shelf(&path, &name);
                    }
                }
                let shelves = self.shelf_membership(&path);
                if let Some(e) = self.meta_edit.as_mut() {
                    e.shelves = shelves;
                    e.new_shelf = None;
                }
            }
            _ => {}
        }
    }

    /// Online/Cover tabs, browsing the results: `/` or typing opens the search
    /// bar; j/k move the selection; Enter applies (metadata on Online, the
    /// previewed cover on Cover).
    fn online_nav_key(&mut self, key: KeyEvent) {
        let (results, tab) = match &self.meta_edit {
            Some(e) => {
                let n = if e.tab == EditTab::Cover {
                    e.cover_hits.len()
                } else {
                    e.search().results.len()
                };
                (n, e.tab)
            }
            None => return,
        };
        let last = results.saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let s = e.search_mut();
                    s.row = s.row.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let s = e.search_mut();
                    s.row = (s.row + 1).min(last);
                }
            }
            // Open the search bar: `/`, or start typing the query directly.
            KeyCode::Char('/') => self.online_begin_query(None),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.online_begin_query(Some(c))
            }
            KeyCode::Enter => {
                if results == 0 {
                    self.online_begin_query(None);
                } else if tab == EditTab::Cover {
                    self.stage_preview_cover();
                } else {
                    let idx = self.meta_edit.as_ref().map_or(0, |e| e.search().row);
                    self.apply_candidate(idx);
                }
            }
            _ => {}
        }
    }

    /// Enter search-bar editing, optionally seeding the query with a first char.
    fn online_begin_query(&mut self, first: Option<char>) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        ed.search_mut().editing = true;
        if let Some(c) = first {
            let s = ed.search_mut();
            s.q.clear();
            s.q.push(c);
        }
        ed.cursor = ed.search().q.chars().count();
    }

    /// Search-bar editing: type the query; Enter runs the search, Esc exits.
    fn online_query_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.search_mut().editing = false;
                }
            }
            KeyCode::Enter => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.search_mut().editing = false;
                }
                self.online_search();
            }
            KeyCode::Left => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.cursor = e.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.cursor = (e.cursor + 1).min(e.search().q.chars().count());
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.search_mut().q.clear();
                    e.cursor = 0;
                }
            }
            KeyCode::Backspace => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let cur = e.cursor;
                    if str_delete_before(&mut e.search_mut().q, cur) {
                        e.cursor -= 1;
                    }
                }
            }
            KeyCode::Char(c) => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let cur = e.cursor;
                    str_insert(&mut e.search_mut().q, cur, c);
                    e.cursor += 1;
                }
            }
            _ => {}
        }
    }

    /// Stage the currently-previewed cover (Cover tab Enter) for save.
    fn stage_preview_cover(&mut self) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        match ed.preview_cover.clone() {
            Some(bytes) => {
                ed.cover = Some(bytes);
                ed.status = Some("cover staged ✓ — ^S to save".into());
            }
            None => ed.status = Some("no cover to use here".into()),
        }
    }

    /// File tab, navigate mode: move between the template, name, and Rename
    /// action; Enter edits a field or performs the rename.
    fn file_nav_key(&mut self, key: KeyEvent) {
        if self.meta_edit.is_none() {
            return;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.file_row = e.file_row.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.file_row = (e.file_row + 1).min(FILE_NAME);
                }
            }
            // Enter edits the focused field; ^S performs the rename + save.
            KeyCode::Enter => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.mode = EditMode::Edit;
                    e.cursor = e.cur_field_len();
                }
            }
            _ => {}
        }
    }

    /// Recompute the previewed filename from the template + current metadata.
    fn recompute_rename(&mut self) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        let ext = std::path::Path::new(&ed.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("epub");
        ed.rename_name = fill_template(&ed.rename_template, &ed.values, ext);
    }

    /// Move one book file to `new_name` (in its own directory), repointing the
    /// database row and cached cover. Pure mechanism — no UI/state side effects
    /// beyond persistence, so both the editor and bulk rename share it.
    fn rename_book_file(&self, old: &str, new_name: &str) -> RenameOutcome {
        let name = sanitize_filename(new_name.trim());
        if name.is_empty() {
            return RenameOutcome::Skipped("name is empty");
        }
        let old_path = std::path::Path::new(old);
        let new_path = match old_path.parent() {
            Some(dir) => dir.join(&name),
            None => std::path::PathBuf::from(&name),
        };
        let new = new_path.to_string_lossy().into_owned();
        if new == old {
            return RenameOutcome::Unchanged;
        }
        if new_path.exists() {
            return RenameOutcome::Skipped("a file with that name already exists");
        }
        if std::fs::rename(old, &new_path).is_err() {
            return RenameOutcome::Skipped("rename failed");
        }
        // Repoint persistence + move the cached cover to the new key.
        if let Some(store) = &self.store {
            store.rename_book_path(old, &new);
        }
        let _ = std::fs::rename(online::cover_cache_path(old), online::cover_cache_path(&new));
        RenameOutcome::Renamed
    }

    /// Editor File-tab rename. Returns `true` when it's safe to proceed with
    /// saving (renamed, or the name was unchanged); `false` on a hard error,
    /// leaving the editor open with the reason in its status line.
    fn apply_rename(&mut self) -> bool {
        let (old, name) = match self.meta_edit.as_ref() {
            Some(ed) => (ed.path.clone(), ed.rename_name.clone()),
            None => return false,
        };
        match self.rename_book_file(&old, &name) {
            RenameOutcome::Renamed => {
                let new = std::path::Path::new(&old)
                    .with_file_name(sanitize_filename(name.trim()))
                    .to_string_lossy()
                    .into_owned();
                if let Some(e) = self.meta_edit.as_mut() {
                    e.status = Some(format!("renamed to {}", sanitize_filename(name.trim())));
                    e.path = new;
                }
                self.refresh_library();
                true
            }
            RenameOutcome::Unchanged => true,
            RenameOutcome::Skipped(reason) => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.status = Some(reason.into());
                }
                false
            }
        }
    }

    /// Toggle vim-style visual select: enter with the anchor at the cursor, or
    /// exit and clear the selection.
    fn lib_toggle_visual(&mut self) {
        if self.lib_visual.is_some() {
            self.lib_exit_visual();
        } else {
            self.lib_visual = Some(self.lib_sel);
            self.lib_visual_sync();
        }
    }

    /// Leave visual mode and clear the selection.
    fn lib_exit_visual(&mut self) {
        self.lib_visual = None;
        self.lib_marked.clear();
    }

    /// Recompute the marked set as the contiguous range between the visual anchor
    /// and the cursor. A no-op outside visual mode; called after cursor movement.
    fn lib_visual_sync(&mut self) {
        let Some(anchor) = self.lib_visual else {
            return;
        };
        if self.lib_books.is_empty() {
            self.lib_marked.clear();
            return;
        }
        let last = self.lib_books.len() - 1;
        let a = anchor.min(last);
        let s = self.lib_sel.min(last);
        let (lo, hi) = (a.min(s), a.max(s));
        self.lib_marked = self.lib_books[lo..=hi].iter().map(|b| b.path.clone()).collect();
    }

    /// Favorite all marked books (or unfavorite them if all are already
    /// favorites), then clear the selection.
    fn bulk_favorite(&mut self) {
        let marked: Vec<String> = self
            .lib_books
            .iter()
            .filter(|b| self.lib_marked.contains(&b.path))
            .map(|b| b.path.clone())
            .collect();
        if marked.is_empty() {
            return;
        }
        let all_fav = self
            .lib_books
            .iter()
            .filter(|b| self.lib_marked.contains(&b.path))
            .all(|b| b.favorite);
        let target = !all_fav;
        if let Some(store) = &self.store {
            for p in &marked {
                store.set_favorite(p, target);
            }
        }
        let n = marked.len();
        self.lib_exit_visual();
        self.refresh_library();
        self.lib_flash = Some(format!(
            "{} {n} book{}",
            if target { "favorited" } else { "unfavorited" },
            if n == 1 { "" } else { "s" }
        ));
    }

    /// Open the bulk-rename popup over the marked books (snapshotting the data
    /// the template needs from each).
    fn open_bulk_rename(&mut self) {
        let targets: Vec<BulkTarget> = self
            .lib_books
            .iter()
            .filter(|b| self.lib_marked.contains(&b.path))
            .map(|b| {
                let ext = std::path::Path::new(&b.path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("epub")
                    .to_string();
                let old_name = std::path::Path::new(&b.path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                BulkTarget {
                    path: b.path.clone(),
                    values: vec![
                        b.title.clone(),
                        b.author.clone(),
                        b.year.map(|y| y.to_string()).unwrap_or_default(),
                        b.series.clone(),
                        b.series_index.map(fmt_series_index).unwrap_or_default(),
                        b.publisher.clone(),
                    ],
                    ext,
                    old_name,
                }
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let template = DEFAULT_RENAME_TEMPLATE.to_string();
        self.bulk_rename = Some(BulkRename {
            cursor: template.chars().count(),
            template,
            targets,
        });
    }

    /// Apply the bulk-rename template to every target, then close + report.
    fn apply_bulk_rename(&mut self) {
        let Some(br) = self.bulk_rename.take() else {
            return;
        };
        let mut renamed = 0usize;
        let mut skipped = 0usize;
        for t in &br.targets {
            let new_name = fill_template(&br.template, &t.values, &t.ext);
            match self.rename_book_file(&t.path, &new_name) {
                RenameOutcome::Renamed => renamed += 1,
                RenameOutcome::Unchanged => {}
                RenameOutcome::Skipped(_) => skipped += 1,
            }
        }
        self.lib_exit_visual();
        self.refresh_library();
        self.lib_flash = Some(if skipped == 0 {
            format!("renamed {renamed} book{}", if renamed == 1 { "" } else { "s" })
        } else {
            format!("renamed {renamed}, skipped {skipped}")
        });
    }

    /// Bulk-rename popup keys: type to edit the template, ^S apply, Esc cancel.
    fn bulk_rename_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.bulk_rename = None,
            KeyCode::Char('s') if ctrl => self.apply_bulk_rename(),
            KeyCode::Left => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    b.cursor = b.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    b.cursor = (b.cursor + 1).min(b.template.chars().count());
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    b.template.clear();
                    b.cursor = 0;
                }
            }
            KeyCode::Backspace => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    let cur = b.cursor;
                    if str_delete_before(&mut b.template, cur) {
                        b.cursor -= 1;
                    }
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(b) = self.bulk_rename.as_mut() {
                    let cur = b.cursor;
                    str_insert(&mut b.template, cur, c);
                    b.cursor += 1;
                }
            }
            _ => {}
        }
    }

    /// Kick off a background search from the query bar: Open Library metadata on
    /// the Online tab, or a multi-source cover search on the Cover tab (which can
    /// run with an empty query, using just the book's ISBN).
    fn online_search(&mut self) {
        let (query, tab, isbn) = {
            let Some(ed) = self.meta_edit.as_mut() else {
                return;
            };
            let tab = ed.tab;
            if ed.search().q.trim().is_empty() && tab != EditTab::Cover {
                return;
            }
            // Cancel any other tab's in-flight search (its result is abandoned
            // when we replace online_rx below), then mark this one fetching.
            ed.online.fetching = false;
            ed.cover_search.fetching = false;
            let isbn = ed.values.get(7).cloned().unwrap_or_default();
            if tab == EditTab::Cover {
                ed.cover_hits.clear();
            }
            let s = ed.search_mut();
            s.fetching = true;
            s.results.clear();
            s.row = 0;
            let q = s.q.clone();
            ed.status = Some("searching…".into());
            (q, tab, isbn)
        };
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let msg = if tab == EditTab::Cover {
                OnlineMsg::Covers(online::cover_candidates(&query, &isbn, ONLINE_LIMIT))
            } else {
                OnlineMsg::Results(online::search(&query, ONLINE_LIMIT))
            };
            let _ = tx.send(msg);
        });
        self.online_rx = Some(rx);
    }

    /// Apply candidate `idx` to the Details fields and fetch its cover.
    fn apply_candidate(&mut self, idx: usize) {
        let cover_url = {
            let Some(ed) = self.meta_edit.as_mut() else {
                return;
            };
            let Some(c) = ed.search().results.get(idx).cloned() else {
                return;
            };
            ed.values[0] = c.title.clone();
            ed.values[1] = c.author_line();
            if let Some(y) = c.year {
                ed.values[2] = y.to_string();
            }
            if let Some(s) = &c.series {
                ed.values[3] = s.clone();
            }
            if let Some(si) = c.series_index {
                ed.values[4] = fmt_series_index(si);
            }
            if let Some(p) = &c.publisher {
                ed.values[5] = p.clone();
            }
            if let Some(isbn) = &c.isbn {
                ed.values[7] = isbn.clone();
            }
            ed.tab = EditTab::Details;
            ed.mode = EditMode::Nav;
            ed.row = 0;
            ed.status = Some("applied — review, then ^S to save".into());
            let url = c.cover_url();
            ed.cover_pending = url.is_some();
            url
        };
        if let Some(url) = cover_url {
            let (tx, rx) = std::sync::mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(OnlineMsg::Cover(online::fetch_cover(&url)));
            });
            self.online_rx = Some(rx);
        }
    }

    /// Drain a finished background Open Library request; returns whether the
    /// view changed. Called from the event loop.
    pub fn poll_online(&mut self) -> bool {
        let Some(rx) = &self.online_rx else {
            return false;
        };
        let Ok(msg) = rx.try_recv() else {
            return false;
        };
        self.online_rx = None;
        let Some(ed) = self.meta_edit.as_mut() else {
            return true;
        };
        match msg {
            OnlineMsg::Results(cands) => {
                ed.status = Some(if cands.is_empty() {
                    "no matches".into()
                } else {
                    format!("{} match(es) — ↑↓ to browse", cands.len())
                });
                // Route to whichever tab's search is in flight (only one is).
                let s = if ed.cover_search.fetching {
                    &mut ed.cover_search
                } else {
                    &mut ed.online
                };
                s.fetching = false;
                s.row = 0;
                s.results = cands;
            }
            OnlineMsg::Covers(hits) => {
                ed.cover_search.fetching = false;
                ed.cover_search.row = 0;
                ed.status = Some(if hits.is_empty() {
                    "no covers found".into()
                } else {
                    format!("{} cover(s) — ↑↓ to browse", hits.len())
                });
                ed.cover_hits = hits;
            }
            OnlineMsg::Cover(bytes) => {
                ed.cover_pending = false;
                match bytes {
                    Some(b) => {
                        ed.status = Some("cover fetched ✓ — ^S to save".into());
                        ed.cover = Some(b);
                    }
                    None => ed.status = Some("no cover found".into()),
                }
            }
            OnlineMsg::Preview(url, bytes) => {
                // A previewed cover arrived for the Cover tab.
                self.edit_cover_url = url.clone();
                ed.preview_url = url;
                ed.preview_cover = bytes.clone();
                self.edit_cover = match (&self.picker, &bytes) {
                    (Some(p), Some(b)) => media::build_cover(p, b),
                    _ => None,
                };
            }
        }
        true
    }

    /// Is an Open Library request in flight (keeps the loop polling)?
    pub fn online_active(&self) -> bool {
        self.meta_edit
            .as_ref()
            .is_some_and(|e| e.online.fetching || e.cover_search.fetching || e.cover_pending)
    }

    /// Cover-tab preview: the cover URL of the highlighted result (or empty).
    fn preview_target_url(&self) -> String {
        let Some(ed) = &self.meta_edit else {
            return self.edit_cover_url.clone();
        };
        if ed.tab != EditTab::Cover {
            return self.edit_cover_url.clone();
        }
        ed.cover_hits
            .get(ed.cover_search.row)
            .map(|h| h.url.clone())
            .unwrap_or_default()
    }

    /// Is the Cover-tab preview stale (wants fetching)? Keeps the loop ticking.
    pub fn preview_pending(&self) -> bool {
        self.preview_target_url() != self.edit_cover_url
    }

    /// Debounced background fetch of the highlighted result's cover for the
    /// Cover-tab preview, so arrow-scrolling the list doesn't spam the network.
    pub fn tick_preview(&mut self) {
        let target = self.preview_target_url();
        if target == self.edit_cover_url {
            return;
        }
        if target != self.edit_cover_target {
            self.edit_cover_target = target;
            self.edit_cover_at = Instant::now();
            return;
        }
        if self.edit_cover_at.elapsed() < COVER_DEBOUNCE {
            return;
        }
        // Mark as handled so we don't re-fire; the result arrives via poll.
        self.edit_cover_url = target.clone();
        if target.is_empty() {
            self.edit_cover = None;
            if let Some(ed) = self.meta_edit.as_mut() {
                ed.preview_cover = None;
                ed.preview_url = String::new();
            }
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let url = target.clone();
        thread::spawn(move || {
            let _ = tx.send(OnlineMsg::Preview(url.clone(), online::fetch_cover(&url)));
        });
        self.online_rx = Some(rx);
    }

    /// Persist the edited fields + any fetched cover (year/index parsed
    /// leniently; blank → unset). Collections are applied live, not here.
    fn save_meta_edit(&mut self) {
        if self.meta_edit.as_ref().is_some_and(MetaEdit::has_invalid) {
            return;
        }
        let Some(ed) = self.meta_edit.take() else {
            return;
        };
        let v = |i: usize| ed.values.get(i).map(|s| s.trim()).unwrap_or("");
        let year = v(2).parse::<i32>().ok();
        let series_index = v(4).parse::<f32>().ok();
        if let Some(store) = &self.store {
            store.update_book_meta(
                &ed.path, v(0), v(1), year, v(3), series_index, v(5), v(6), v(7), v(8),
            );
        }
        if let Some(bytes) = &ed.cover {
            let _ = online::save_cover(&ed.path, bytes);
            self.lib_flash = Some(embed_cover_into_file(&ed.path, bytes));
        }
        self.refresh_library();
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
    fn lib_set_view_index(&mut self, i: usize) {
        let total = self.lib_view_count();
        if total == 0 {
            return;
        }
        self.lib_exit_visual();
        self.lib_view = self.lib_view_at(i.min(total - 1));
        self.lib_sel = 0;
        self.refresh_library();
    }

    /// Move the sidebar cursor by `delta` (clamped), switching the view live.
    fn lib_side_move(&mut self, delta: isize) {
        let next = (self.lib_view_index() as isize + delta).max(0) as usize;
        self.lib_set_view_index(next);
    }

    /// Position of the active view within the section+collection ring.
    fn lib_view_index(&self) -> usize {
        let n = LibrarySection::ALL.len();
        match &self.lib_view {
            LibView::Section(s) => {
                LibrarySection::ALL.iter().position(|x| x == s).unwrap_or(0)
            }
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
        if let (Some(store), Some(reader)) = (&self.store, &self.reader) {
            if !self.book_path.is_empty() {
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
        self.store.as_ref().map(|s| s.total_read_seconds()).unwrap_or(0)
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
        r.scroll_pending != 0 || (self.mode == Mode::Reader && r.images_pending())
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
                if let Some(text) = self.note_input.take() {
                    if let (Some(store), Some(r)) = (&self.store, &self.reader) {
                        if !self.book_path.is_empty() {
                            store.add_annotation(
                                &self.book_path,
                                r.section,
                                &r.current_quote(),
                                text.trim(),
                            );
                        }
                    }
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
                if let Some(a) = self.annot.as_mut() {
                    if len > 0 {
                        a.sel = (sel + 1).min(len - 1);
                    }
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
                let id = self.annot.as_ref().and_then(|a| a.items.get(a.sel)).map(|i| i.id);
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

    fn settings_key(&mut self, key: KeyEvent) {
        if self.settings.is_none() {
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char(';') | KeyCode::Char('q') => {
                self.settings = None;
                self.config.save();
            }
            KeyCode::Char('j') | KeyCode::Down => self.settings_move(1),
            KeyCode::Char('k') | KeyCode::Up => self.settings_move(-1),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.settings_change(1),
            KeyCode::Char('h') | KeyCode::Left => self.settings_change(-1),
            _ => {}
        }
    }

    /// Move the settings cursor by `delta` items, skipping section headers.
    fn settings_move(&mut self, delta: isize) {
        let Some(s) = self.settings.as_ref() else {
            return;
        };
        let rows = settings_rows(s.scope);
        let items: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, SettingRow::Item(_)))
            .map(|(i, _)| i)
            .collect();
        if items.is_empty() {
            return;
        }
        let cur = items.iter().position(|&i| i == s.row).unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, items.len() as isize - 1) as usize;
        if let Some(s) = self.settings.as_mut() {
            s.row = items[next];
        }
    }

    fn settings_change(&mut self, delta: i32) {
        use crate::config::{MAX_LINE_SPACING, MAX_SIDE_PADDING};
        let Some(s) = self.settings.as_ref() else {
            return;
        };
        // Resolve the focused row to a setting identity.
        let Some(SettingRow::Item(item)) = settings_rows(s.scope).into_iter().nth(s.row) else {
            return;
        };
        let c = &mut self.config;
        match item {
            SettingItem::Theme => {
                c.theme = if delta > 0 { c.theme.next() } else { c.theme.prev() }
            }
            SettingItem::ViewMode => {
                c.view_mode = if delta > 0 {
                    c.view_mode.next()
                } else {
                    c.view_mode.prev()
                }
            }
            SettingItem::SidePadding => {
                c.side_padding =
                    (c.side_padding as i32 + delta).clamp(0, MAX_SIDE_PADDING as i32) as u16
            }
            SettingItem::LineSpacing => {
                c.line_spacing =
                    (c.line_spacing as i32 + delta).clamp(0, MAX_LINE_SPACING as i32) as u8
            }
            SettingItem::ParagraphSpacing => {
                c.paragraph_spacing = (c.paragraph_spacing as i32 + delta).clamp(0, 3) as u8
            }
            SettingItem::ShowSidebar => c.show_sidebar = !c.show_sidebar,
            SettingItem::ShowStatus => c.show_status = !c.show_status,
            SettingItem::StatusTheme => c.status.theme = !c.status.theme,
            SettingItem::StatusView => c.status.view = !c.status.view,
            SettingItem::StatusPosition => c.status.position = !c.status.position,
            SettingItem::StatusPercent => c.status.percent = !c.status.percent,
            SettingItem::StatusGauge => c.status.gauge = !c.status.gauge,
            SettingItem::ImageMaxPx => {
                // 0 = off (uncapped); otherwise step in 128px increments.
                c.image_max_px = (c.image_max_px as i32 + delta * 128)
                    .clamp(0, crate::config::MAX_IMAGE_PX as i32)
                    as u16
            }
            SettingItem::CodeWrap => c.code_wrap = !c.code_wrap,
            SettingItem::ChapterLock => c.chapter_lock = !c.chapter_lock,
            SettingItem::Mouse => c.mouse_enabled = !c.mouse_enabled,
            SettingItem::LibLayout => {
                c.library_layout = if delta > 0 {
                    c.library_layout.next()
                } else {
                    c.library_layout.prev()
                }
            }
            SettingItem::GridSize => {
                c.library_grid_size = if delta > 0 {
                    c.library_grid_size.next()
                } else {
                    c.library_grid_size.prev()
                }
            }
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
                if self.lib_visual.is_some() {
                    self.lib_exit_visual();
                } else if self.lib_filter.is_empty() {
                    self.should_quit = true;
                } else {
                    self.lib_filter.clear();
                    self.refresh_library();
                }
            }
            // Vim-style visual select: V starts/stops; movement extends the range.
            KeyCode::Char('V') => self.lib_toggle_visual(),
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
            // Enter: from the sidebar step into the list; else open the book.
            KeyCode::Enter => {
                if pane == LibPane::Sidebar {
                    self.lib_pane = LibPane::List;
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
            // Resize the focused side pane; show/hide the sidebar / detail pane.
            KeyCode::Char('[') => self.lib_resize(-2),
            KeyCode::Char(']') => self.lib_resize(2),
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
            KeyCode::Char('e') => {
                if self.lib_marked.is_empty() {
                    self.open_meta_edit()
                } else {
                    self.open_bulk_rename()
                }
            }
            KeyCode::Char('c') => self.open_shelf_picker(),
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

    /// Open the add-to-collection picker for the selected book, pre-ticking the
    /// collections it already belongs to.
    fn open_shelf_picker(&mut self) {
        let Some((path, title)) = self
            .lib_books
            .get(self.lib_sel)
            .map(|b| (b.path.clone(), b.title.clone()))
        else {
            return;
        };
        let shelves = self.shelf_membership(&path);
        self.shelf_picker = Some(ShelfPicker {
            path,
            title,
            shelves,
            sel: 0,
            new_name: None,
        });
    }

    /// In a collection view, drop the selected book from that collection.
    fn remove_from_current_shelf(&mut self) {
        let LibView::Shelf(name) = &self.lib_view else {
            return;
        };
        let name = name.clone();
        if let (Some(store), Some(book)) = (&self.store, self.lib_books.get(self.lib_sel)) {
            store.remove_from_shelf(&book.path, &name);
        }
        self.refresh_library();
    }

    fn shelf_picker_key(&mut self, key: KeyEvent) {
        let Some(p) = self.shelf_picker.as_mut() else {
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
                        if let Some(store) = &self.store {
                            store.add_to_shelf(&p.path, &name);
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
                self.shelf_picker = None;
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

    /// Toggle the focused book's membership in the selected collection, in place.
    fn toggle_picked_shelf(&mut self) {
        let Some(p) = self.shelf_picker.as_mut() else {
            return;
        };
        let Some((name, member)) = p.shelves.get_mut(p.sel) else {
            return;
        };
        if let Some(store) = &self.store {
            if *member {
                store.remove_from_shelf(&p.path, name);
            } else {
                store.add_to_shelf(&p.path, name);
            }
        }
        *member = !*member;
    }

    /// Rebuild the picker's shelf list after a new collection is created, then
    /// leave creating mode with the cursor on the new entry.
    fn refresh_shelf_picker(&mut self) {
        let path = match &self.shelf_picker {
            Some(p) => p.path.clone(),
            None => return,
        };
        let shelves = self.shelf_membership(&path);
        if let Some(p) = self.shelf_picker.as_mut() {
            p.shelves = shelves;
            p.new_name = None;
            p.sel = p.sel.min(p.new_row());
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
                if let Some(store) = &self.store {
                    if !self.book_path.is_empty() {
                        store.add_annotation(
                            &self.book_path,
                            reader.section,
                            &reader.current_quote(),
                            "",
                        );
                    }
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
                    if self.config.code_wrap { "code: wrap" } else { "code: no-wrap (< > to pan)" }
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
                    if self.config.chapter_lock { "chapter lock: on" } else { "chapter lock: off" }
                        .to_string(),
                );
                save = true;
            }
            Action::NextChapter => reader.next_chapter(),
            Action::PrevChapter => reader.prev_chapter(),
            Action::None => {}
        }

        // Persist on chapter change or a settings change (cheap).
        if save || reader.section != before {
            if let Some(store) = &self.store {
                if !self.book_path.is_empty() {
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
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if !self.config.mouse_enabled {
            return;
        }
        match m.kind {
            // The wheel scrolls whichever reader pane the cursor is over: the TOC
            // (without changing the selection) or the content.
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                if self.mode != Mode::Reader {
                    return;
                }
                let d: isize = if matches!(m.kind, MouseEventKind::ScrollUp) { -3 } else { 3 };
                let over_sidebar = self
                    .last_layout
                    .sidebar
                    .is_some_and(|sb| sb.contains((m.column, m.row).into()));
                if let Some(r) = self.reader.as_mut() {
                    if over_sidebar {
                        r.sidebar_wheel(d);
                    } else {
                        r.queue_scroll(d);
                    }
                }
            }
            MouseEventKind::Down(_) => self.mouse_down(m.column, m.row),
            _ => {}
        }
    }

    /// Route a left-click to the active overlay / mode using the hit rects
    /// captured during the last render.
    fn mouse_down(&mut self, col: u16, row: u16) {
        if self.meta_edit.is_some() {
            self.editor_click(col, row);
            return;
        }
        // Other overlays are keyboard-driven (no hit rects); swallow the click.
        if self.settings.is_some()
            || self.shelf_picker.is_some()
            || self.bulk_rename.is_some()
            || self.annot.is_some()
            || self.image_view.is_some()
            || self.note_input.is_some()
        {
            return;
        }
        match self.mode {
            Mode::Reader => self.mouse_click(col, row),
            Mode::Library => self.library_click(col, row),
        }
    }

    /// Library click: select the clicked book and focus the list pane.
    fn library_click(&mut self, col: u16, row: u16) {
        let pt = (col, row).into();
        if let Some(&(idx, _)) = self.mouse.books.iter().find(|(_, r)| r.contains(pt)) {
            self.lib_sel = idx.min(self.lib_books.len().saturating_sub(1));
            self.lib_pane = LibPane::List;
        }
    }

    /// Editor click: switch tab, focus + edit a field (caret at the click),
    /// open the search bar, or pick a result.
    fn editor_click(&mut self, col: u16, row: u16) {
        let pt = (col, row).into();
        if let Some(&(tab, _)) = self.mouse.edit_tabs.iter().find(|(_, r)| r.contains(pt)) {
            if let Some(e) = self.meta_edit.as_mut() {
                e.tab = tab;
                e.mode = EditMode::Nav;
            }
            if self.meta_edit.as_ref().map(|e| e.tab) == Some(EditTab::File) {
                self.recompute_rename();
            }
            return;
        }
        if self.mouse.edit_search.is_some_and(|r| r.contains(pt)) {
            self.online_begin_query(None);
            return;
        }
        if let Some(&(idx, vstart, _)) = self.mouse.edit_fields.iter().find(|(_, _, r)| r.contains(pt)) {
            if let Some(e) = self.meta_edit.as_mut() {
                match e.tab {
                    EditTab::Details => e.row = idx,
                    EditTab::File => e.file_row = idx,
                    _ => {}
                }
                e.mode = EditMode::Edit;
                let len = e.cur_field_len();
                e.cursor = (col.saturating_sub(vstart) as usize).min(len);
            }
            return;
        }
        let hit = self
            .mouse
            .edit_results
            .iter()
            .find(|(_, r)| r.contains(pt))
            .map(|&(idx, _)| idx);
        if let (Some(idx), Some(e)) = (hit, self.meta_edit.as_mut()) {
            e.search_mut().row = idx;
        }
    }

    /// Click on a sidebar row selects and jumps to that TOC entry.
    fn mouse_click(&mut self, col: u16, row: u16) {
        let Some(sb) = self.last_layout.sidebar else {
            return;
        };
        let in_x = col >= sb.x && col < sb.x + sb.width;
        // Account for the sidebar's top/bottom border.
        let first = sb.y + 1;
        let last = sb.y + sb.height.saturating_sub(1);
        if !in_x || row < first || row >= last {
            return;
        }
        if let Some(r) = self.reader.as_mut() {
            // Screen row → list index, accounting for the scrolled viewport.
            let idx = r.sidebar_offset + (row - first) as usize;
            let vis = r.outline_visible();
            if let Some(&oi) = vis.get(idx) {
                r.sidebar_sel = idx;
                r.focus = Focus::Sidebar;
                if let Some(item) = r.outline.get(oi).cloned() {
                    r.jump_to(item.section, item.locator.as_deref());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
                .upsert_book("/k.epub", "K", "Auth", None, 1, 1, 1, "", None, "", "", "", "")
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
        assert_eq!(app.config.library_layout, LibLayout::List, "v wraps back to list");

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
                .upsert_book("/n.epub", "N", "Auth", None, 1, 1, 1, "", None, "", "", "", "")
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
        assert_eq!(app.lib_view, LibView::Section(LibrarySection::Recent), "clamped at top");

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
        app.on_key(key(']'));
        assert_eq!(app.lib_sidebar_w, w0 + 2);
        app.on_key(key('['));
        assert_eq!(app.lib_sidebar_w, w0);
        for _ in 0..40 {
            app.on_key(key('['));
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
                .upsert_book("/k.epub", "K", "Auth", Some(1999), 1, 1, 1, "", None, "", "", "", "")
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e'));
        assert_eq!(app.meta_edit.as_ref().unwrap().mode, EditMode::Nav, "opens in nav mode");

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
        assert!(app.meta_edit.as_ref().unwrap().has_invalid(), "non-numeric year invalid");

        // ^S must NOT save while invalid.
        app.on_key(ctrl('s'));
        assert!(app.meta_edit.is_some(), "save blocked while invalid");

        // Fix the year, then ^S saves and persists.
        app.on_key(code(KeyCode::Enter));
        app.on_key(ctrl('u'));
        for c in "2001".chars() {
            app.on_key(key(c));
        }
        app.on_key(code(KeyCode::Esc));
        app.on_key(ctrl('s'));
        assert!(app.meta_edit.is_none(), "valid edit saves & closes");
        let b = &app.lib_books[0];
        assert_eq!(b.title, "XK");
        assert_eq!(b.year, Some(2001));

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
            store.upsert_book("/a.epub", "A", "x", Some(2010), 1, 1, 1, "", None, "", "", "", "").unwrap();
            store.upsert_book("/b.epub", "B", "x", Some(1999), 1, 1, 1, "", None, "", "", "", "").unwrap();
            store.upsert_book("/c.epub", "C", "x", Some(2001), 1, 1, 1, "", None, "", "", "", "").unwrap();
        }

        let mut app = App::library();
        let titles = |a: &App| a.lib_books.iter().map(|b| b.title.clone()).collect::<Vec<_>>();
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
                    .upsert_book(&format!("/{t}.epub"), t, "x", None, 1, 1, 1, "", None, "", "", "", "")
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

    #[test]
    fn rename_template_fills_and_sanitizes() {
        let v: Vec<String> = ["Dune", "Frank Herbert", "1965", "Dune", "1", "Ace"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fill_template("%T.%E", &v, "epub"), "Dune.epub");
        assert_eq!(
            fill_template("%A - %T (%Y).%E", &v, "epub"),
            "Frank Herbert - Dune (1965).epub"
        );
        assert_eq!(fill_template("%S %I - %T.%E", &v, "epub"), "Dune 1 - Dune.epub");
        assert_eq!(fill_template("100%% %T.%E", &v, "epub"), "100% Dune.epub");
        // Path separators / illegal chars become spaces (collapsed).
        let bad: Vec<String> = ["A/B:C", "", "", "", "", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(fill_template("%T.%E", &bad, "epub"), "A B C.epub");
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
                .upsert_book(&old_str, "Clean Title", "Auth", Some(2001), 1, 1, 1, "", None, "", "", "", "")
                .unwrap();
            store.set_favorite(&old_str, true);
        }

        let mut app = App::library();
        assert_eq!(app.lib_books.len(), 1);
        app.on_key(key('e'));
        // Details → Cover → Collections → Online → File.
        for _ in 0..4 {
            app.on_key(code(KeyCode::Tab));
        }
        assert_eq!(app.meta_edit.as_ref().unwrap().tab, EditTab::File);
        // Entering the File tab derived the name from "%T.%E".
        assert_eq!(app.meta_edit.as_ref().unwrap().rename_name, "Clean Title.epub");
        // ^S on the File tab renames the file and saves (no separate button).
        app.on_key(ctrl('s'));

        let new = books.join("Clean Title.epub");
        assert!(new.exists(), "renamed file exists");
        assert!(!old.exists(), "old file is gone");
        assert_eq!(
            app.lib_books[0].path,
            new.to_string_lossy(),
            "DB path repointed and reloaded"
        );
        assert!(app.lib_books[0].favorite, "favorite preserved across rename");

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
                .upsert_book(&p.to_string_lossy(), title, "Auth", None, 1, 1, 1, "", None, "", "", "", "")
                .unwrap();
        }

        let mut app = App::library();
        assert_eq!(app.lib_books.len(), 2);
        app.on_key(key('V')); // visual select from book 0
        app.on_key(key('j')); // extend to book 1
        assert_eq!(app.lib_marked.len(), 2);
        app.on_key(key('e')); // marks present → bulk rename, not the editor
        assert!(app.bulk_rename.is_some());
        assert!(app.meta_edit.is_none());
        app.on_key(ctrl('s')); // apply default "%T.%E"

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
        app.mouse.books = vec![
            (0, Rect::new(0, 0, 20, 1)),
            (1, Rect::new(0, 1, 20, 1)),
        ];
        app.on_mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.lib_sel, 1, "click on the second row selects it");

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
        assert!(matches!(rows[0], SettingRow::Section(_)), "first row is a header");
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

    // Online/Cover tab: typing opens the search bar and appends; / reopens it.
    #[test]
    fn online_search_bar_typing() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_srch_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book("/k.epub", "K", "Auth", None, 1, 1, 1, "", None, "", "", "", "")
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e'));
        // Details → Cover → Collections → Online.
        for _ in 0..3 {
            app.on_key(code(KeyCode::Tab));
        }
        assert_eq!(app.meta_edit.as_ref().unwrap().tab, EditTab::Online);

        // Typing a letter opens the query (clearing the prefill) and appends.
        app.on_key(key('z'));
        let ed = app.meta_edit.as_ref().unwrap();
        assert!(ed.search().editing);
        assert_eq!(ed.search().q, "z");
        app.on_key(key('x'));
        assert_eq!(app.meta_edit.as_ref().unwrap().search().q, "zx");
        // Esc stops editing; / reopens without clearing.
        app.on_key(code(KeyCode::Esc));
        assert!(!app.meta_edit.as_ref().unwrap().search().editing);
        app.on_key(key('/'));
        let ed = app.meta_edit.as_ref().unwrap();
        assert!(ed.search().editing);
        assert_eq!(ed.search().q, "zx");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The Cover and Online tabs keep independent search queries (#4): typing in
    // one must not change the other.
    #[test]
    fn cover_and_online_queries_are_independent() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_q2_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book("/k.epub", "K", "Auth", None, 1, 1, 1, "", None, "", "", "", "")
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
        let online_prefill = app.meta_edit.as_ref().unwrap().online.q.clone();

        // Cover → Collections → Online: the online query is untouched.
        app.on_key(code(KeyCode::Tab));
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.meta_edit.as_ref().unwrap().tab, EditTab::Online);
        assert_eq!(app.meta_edit.as_ref().unwrap().online.q, online_prefill);
        assert!(!app.meta_edit.as_ref().unwrap().online.editing);

        // Typing here changes only the online query; cover keeps "ab".
        app.on_key(key('c'));
        let ed = app.meta_edit.as_ref().unwrap();
        assert_eq!(ed.online.q, "c");
        assert_eq!(ed.cover_search.q, "ab");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Collections tab toggles membership and creates new collections.
    #[test]
    fn meta_editor_collections_tab() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_edit3_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        {
            let store = Store::open_default().unwrap();
            store
                .upsert_book("/k.epub", "K", "Auth", None, 1, 1, 1, "", None, "", "", "", "")
                .unwrap();
        }

        let mut app = App::library();
        app.on_key(key('e'));
        // Tab to Collections (Details → Cover → Collections).
        app.on_key(code(KeyCode::Tab));
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.meta_edit.as_ref().unwrap().tab, EditTab::Collections);
        // No shelves yet → cursor sits on the "new" row; Enter starts typing.
        app.on_key(code(KeyCode::Enter));
        assert!(app.meta_edit.as_ref().unwrap().new_shelf.is_some());
        for c in "Sci-Fi".chars() {
            app.on_key(key(c));
        }
        app.on_key(code(KeyCode::Enter)); // create + add
        let ed = app.meta_edit.as_ref().unwrap();
        assert_eq!(ed.shelves.len(), 1);
        assert_eq!(ed.shelves[0], ("Sci-Fi".to_string(), true), "book added to new collection");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
