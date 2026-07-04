//! Library-pane state: the book list, selection, multi-select, sort/filter,
//! sidebar/detail panes, and lazily-built cover protocols.
//!
//! Carved out of the `App` god-object (it held ~30 `lib_*` fields inline) so the
//! library's state lives behind one cohesive type, reached as `app.library`. The
//! *behaviour* — `lib_move`, `refresh_library`, selection, sorting — stays on
//! `impl App` (`app/library.rs`, `app/select.rs`, …) and operates through here.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::time::Instant;

use lru::LruCache;

use crate::app::library::{LibPane, LibView, SortKey};
use crate::media::CoverImage;
use crate::store::{BookRow, LibrarySection};

/// Bounded LRU capacity for grid-view cover protocols, so terminal image memory
/// stays capped however large the library is.
pub const COVER_CAP: usize = 96;

/// All state for the library view, owned by `App` as `app.library`.
pub struct LibraryState {
    /// Which slice of the library is shown (a section or a collection).
    pub view: LibView,
    /// Which pane has the keyboard (Sidebar / List / Detail). Tab cycles it.
    pub pane: LibPane,
    /// Show the sections/collections sidebar.
    pub show_sidebar: bool,
    /// Sidebar / detail pane widths as a percentage of the window (resizable with
    /// `<`/`>`); the responsive split clamps and collapses them per window.
    pub sidebar_pct: u16,
    pub detail_pct: u16,
    /// Cached (collection name, book count), refreshed with the book list.
    pub shelves: Vec<(String, usize)>,
    /// Per-section book counts, parallel to `LibrarySection::ALL`, so the sidebar
    /// can show a right-aligned count per group.
    pub section_counts: Vec<usize>,
    /// The currently displayed (filtered, sorted) book list.
    pub books: Vec<BookRow>,
    /// Cursor index into `books`.
    pub sel: usize,
    /// Top visible row of the book list (scroll offset, in table-row units incl.
    /// series headers). Persisted so the view scrolls the cursor *into view* rather
    /// than always re-centring it — a click selects the book in place, the wheel
    /// scrolls without snapping. Maintained by the list renderer.
    pub list_offset: usize,
    /// Effective multi-selection for bulk actions, keyed by book path — the union
    /// of `marked_base` and the live visual range.
    pub marked: HashSet<String>,
    /// Books toggled individually with Space, kept separate so a live visual range
    /// can layer on top without clobbering them.
    pub marked_base: HashSet<String>,
    /// Visual-select anchor (book index) while in visual mode; `None` otherwise.
    pub visual: Option<usize>,
    /// Sidebar cursor parked on the trailing "＋ New collection" row.
    pub side_new: bool,
    /// Active sort key and direction for the book list.
    pub sort: SortKey,
    pub sort_desc: bool,
    /// Active filter query (substring / FTS / DSL).
    pub filter: String,
    /// Editing the filter query.
    pub filtering: bool,
    /// Transient message shown in the library status bar; cleared on next keypress.
    pub flash: Option<String>,
    /// Show the right-hand detail pane (cover + metadata).
    pub detail: bool,
    /// Cover image protocol for the detail pane, rebuilt when the selection
    /// settles (debounced so holding j/k stays smooth).
    pub cover: Option<CoverImage>,
    /// Book path the current `cover` was built for (avoids rebuilds).
    pub cover_path: String,
    /// Path the cover wants to settle on, and when it last changed (debounce).
    pub(crate) cover_target: String,
    pub(crate) cover_at: Instant,
    /// Last list navigation went down (toward higher indices) — so cover prefetch
    /// loads ahead in the direction of travel.
    pub nav_down: bool,
    /// Grid view: lazily-built cover protocols, keyed by book path (`None` = no
    /// cover / failed, so we don't retry every frame). A bounded LRU; evicted
    /// covers feed `grid_deletes`.
    pub grid_covers: LruCache<String, Option<CoverImage>>,
    /// Terminal image ids of covers evicted from `grid_covers`, to delete.
    pub grid_deletes: Vec<u32>,
    /// Grid view: visible covers still waiting to be built (keeps redrawing).
    pub grid_pending: bool,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            view: LibView::Section(LibrarySection::All),
            pane: LibPane::List,
            show_sidebar: true,
            sidebar_pct: 20,
            detail_pct: 30,
            shelves: Vec::new(),
            section_counts: Vec::new(),
            books: Vec::new(),
            sel: 0,
            list_offset: 0,
            marked: HashSet::new(),
            marked_base: HashSet::new(),
            visual: None,
            side_new: false,
            sort: SortKey::Default,
            sort_desc: false,
            filter: String::new(),
            filtering: false,
            flash: None,
            detail: true,
            cover: None,
            cover_path: String::new(),
            cover_target: String::new(),
            cover_at: Instant::now(),
            nav_down: true,
            grid_covers: LruCache::new(NonZeroUsize::new(COVER_CAP).unwrap()),
            grid_deletes: Vec::new(),
            grid_pending: false,
        }
    }
}
