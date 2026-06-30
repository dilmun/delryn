//! Library mode: browsing the indexed collection. Owns the view/section model
//! (smart sections + user collections), book list refresh + sort, cursor and
//! pane navigation, grid/list/detail layout, detail-pane + grid cover loading,
//! and the library-mode key dispatch.

use std::collections::HashSet;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::LibLayout;
use crate::media;
use crate::store::LibrarySection;

use super::{App, COVER_DEBOUNCE, Mode, load_cover_bytes};

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

/// Sidebar width bounds (percent of the window).
pub const SIDEBAR_PCT_MIN: u16 = 10;
pub const SIDEBAR_PCT_MAX: u16 = 36;
/// Detail-pane width bounds (percent of the window).
pub const DETAIL_PCT_MIN: u16 = 18;
pub const DETAIL_PCT_MAX: u16 = 45;

/// How the book list is sorted. `Default` keeps each section's natural order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Default,
    Title,
    Author,
    Year,
    Type,
    Source,
    Progress,
    Size,
    Rating,
    Status,
    Tags,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Default => "default",
            SortKey::Title => "title",
            SortKey::Author => "author",
            SortKey::Year => "year",
            SortKey::Type => "type",
            SortKey::Source => "source",
            SortKey::Progress => "progress",
            SortKey::Size => "size",
            SortKey::Rating => "rating",
            SortKey::Status => "status",
            SortKey::Tags => "tags",
        }
    }
}

impl App {
    pub(crate) fn refresh_library(&mut self) {
        let Some(store) = &self.store else {
            self.library.books.clear();
            self.library.shelves.clear();
            return;
        };
        // Computed while the immutable `store` borrow is live, assigned after.
        let shelves = store.all_shelves();
        // If the active collection just lost its last book it no longer exists;
        // fall back to All so the view and sidebar stay consistent.
        let view = match &self.library.view {
            LibView::Shelf(name) if !shelves.iter().any(|(n, _)| n == name) => {
                LibView::Section(LibrarySection::All)
            }
            v => v.clone(),
        };
        // Loaded once per refresh and reused below for the Duplicates view, the
        // search filter, and the Duplicates sidebar count — so a refresh scans
        // the whole table at most once. The duplicate set uses cross-format
        // detection (canonical ISBN-13 and/or normalized title+author, grouped by
        // connected components), richer than a title-only SQL match.
        let all_books = store.all_books();
        // Cover-scan links fold in books with no shared metadata; dismissed
        // groups ("keep both") are not flagged again.
        let dismissed = store.dismissed_duplicate_groups();
        let links = store.dup_links();
        let dup_paths =
            crate::library::dedup::duplicate_paths_excluding(&all_books, &links, &dismissed);
        let books = if self.library.filter.trim().is_empty() {
            match &view {
                LibView::Section(LibrarySection::Duplicates) => all_books
                    .into_iter()
                    .filter(|b| dup_paths.contains(&b.path))
                    .collect(),
                LibView::Section(s) => store.list_books(*s),
                LibView::Shelf(name) => store.books_in_shelf(name),
            }
        } else {
            // Library-wide search (ignores the active section, by design). A
            // structured query (`author:knuth year>=1990`, flags, AND/OR/NOT)
            // is evaluated field-by-field; a plain query keeps the title/author/
            // series/publisher substring + full-text body match.
            let q = crate::library::query::parse(&self.library.filter);
            if q.is_structured() {
                all_books.into_iter().filter(|b| q.matches(b)).collect()
            } else {
                let f = self.library.filter.to_lowercase();
                let fts: HashSet<String> =
                    store.fts_paths(&self.library.filter).into_iter().collect();
                all_books
                    .into_iter()
                    .filter(|b| {
                        b.title.to_lowercase().contains(&f)
                            || b.author.to_lowercase().contains(&f)
                            || b.series.to_lowercase().contains(&f)
                            || b.publisher.to_lowercase().contains(&f)
                            || fts.contains(&b.path)
                    })
                    .collect()
            }
        };
        // Per-section totals for the sidebar (computed under the live `store`
        // borrow). Duplicates reuses the cross-format set above so its count
        // matches the view, not the title-only SQL filter.
        let section_counts: Vec<usize> = LibrarySection::ALL
            .iter()
            .map(|s| match s {
                LibrarySection::Duplicates => dup_paths.len(),
                s => store.count_books(*s),
            })
            .collect();
        self.library.shelves = shelves;
        self.library.section_counts = section_counts;
        self.library.view = view;
        self.library.books = books;
        // A width-agnostic sort cycle (all enabled columns) so `s` works before
        // the first render and in the grid; the book table refines it per width.
        let compact = self.config.library_layout == LibLayout::Compact;
        self.library.sort_cycle = crate::view::library::sort_cycle(&self.config, compact, u16::MAX);
        self.sort_books();
        if self.library.sel >= self.library.books.len() {
            self.library.sel = self.library.books.len().saturating_sub(1);
        }
    }

    /// Apply the active sort key/direction to the loaded book list. `Default`
    /// keeps the section's own order.
    fn sort_books(&mut self) {
        if self.library.sort == SortKey::Default {
            return;
        }
        let key = self.library.sort;
        let desc = self.library.sort_desc;
        self.library.books.sort_by(|a, b| {
            let ord = match key {
                SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                SortKey::Author => a.author.to_lowercase().cmp(&b.author.to_lowercase()),
                SortKey::Year => a.year.cmp(&b.year),
                SortKey::Type => crate::document::BookFormat::from_path(&a.path)
                    .label()
                    .cmp(crate::document::BookFormat::from_path(&b.path).label()),
                SortKey::Source => a.converted.cmp(&b.converted),
                SortKey::Progress => a.pct.cmp(&b.pct),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Rating => a.rating.cmp(&b.rating),
                SortKey::Status => {
                    use delryn_model::ReadingStatus as RS;
                    RS::effective(a.pct, &a.status)
                        .order()
                        .cmp(&RS::effective(b.pct, &b.status).order())
                }
                // Untagged books sort last; otherwise alphabetical by tag string.
                SortKey::Tags => a
                    .tags
                    .is_empty()
                    .cmp(&b.tags.is_empty())
                    .then_with(|| a.tags.cmp(&b.tags)),
                SortKey::Default => std::cmp::Ordering::Equal,
            };
            if desc { ord.reverse() } else { ord }
        });
    }

    /// Cycle sort with `s` over the *currently visible* columns only: each column
    /// steps ascending → descending before advancing to the next, wrapping the
    /// last column's descending straight back to the first (Title). Columns the
    /// user has hidden, or that collapsed on a narrow window, are skipped — so the
    /// arrow always lands on a visible header (`lib_sort_cycle` is set at render).
    fn cycle_sort(&mut self) {
        self.lib_exit_visual();
        let cycle = &self.library.sort_cycle;
        if cycle.is_empty() {
            return;
        }
        match cycle.iter().position(|&k| k == self.library.sort) {
            // On a visible column: descending advances to the next (wrapping);
            // ascending flips the same column to descending.
            Some(i) if self.library.sort_desc => {
                self.library.sort = cycle[(i + 1) % cycle.len()];
                self.library.sort_desc = false;
            }
            Some(_) => self.library.sort_desc = true,
            // Coming from Default or a non-visible key: start at the first column.
            None => {
                self.library.sort = cycle[0];
                self.library.sort_desc = false;
            }
        }
        self.refresh_library();
    }

    /// Flip the sort direction (`S`) without changing the key.
    fn toggle_sort_dir(&mut self) {
        self.lib_exit_visual();
        self.library.sort_desc = !self.library.sort_desc;
        self.refresh_library();
    }

    pub(crate) fn lib_move(&mut self, delta: isize) {
        if self.library.books.is_empty() {
            return;
        }
        if delta > 0 {
            self.library.nav_down = true;
        } else if delta < 0 {
            self.library.nav_down = false;
        }
        let last = self.library.books.len() as isize - 1;
        self.library.sel = (self.library.sel as isize + delta).clamp(0, last) as usize;
    }

    /// Move the focused list/sidebar cursor by `rows` rows (signed) — for
    /// half/full-page vim navigation. In the grid a row is `cols` cells wide.
    fn lib_page_move(&mut self, rows: isize) {
        match self.library.pane {
            LibPane::Sidebar => self.lib_side_move(rows),
            LibPane::List => self.lib_move(rows * self.grid_step()),
            LibPane::Detail => {}
        }
    }

    fn lib_favorite(&mut self) {
        if let (Some(store), Some(book)) = (&self.store, self.library.books.get(self.library.sel)) {
            store.set_favorite(&book.path, !book.favorite);
        }
        self.refresh_library();
    }

    /// Export the current (filtered) book list to a CSV in the config dir.
    pub(crate) fn export_library(&mut self) {
        let csv = crate::library::export::to_csv(&self.library.books);
        let path = crate::paths::config_dir().join("delryn-export.csv");
        self.library.flash = Some(match std::fs::write(&path, csv) {
            Ok(()) => format!(
                "exported {} book{} → {}",
                self.library.books.len(),
                if self.library.books.len() == 1 {
                    ""
                } else {
                    "s"
                },
                path.display()
            ),
            Err(e) => format!("export failed: {e}"),
        });
    }

    /// Open the library-statistics overlay (computed over all books).
    pub(crate) fn open_stats(&mut self) {
        if let Some(store) = &self.store {
            let books = store.all_books();
            let secs = store.total_read_seconds();
            self.stats = Some(crate::library::stats::compute(&books, secs));
        }
    }

    /// Delete each duplicate's file (best-effort) and drop its library row, then
    /// refresh the list and the resolution overlay. Called from the confirmation
    /// handler after the user confirms the checked deletions.
    pub(crate) fn remove_duplicate_files(&mut self, paths: &[String]) {
        let mut removed = 0;
        for p in paths {
            let gone = std::fs::remove_file(p).is_ok();
            if let Some(store) = &self.store {
                store.remove_book(p);
            }
            removed += usize::from(gone);
        }
        self.library.flash = Some(format!("removed {removed} duplicate(s)"));
        self.library.sel = 0;
        self.refresh_library();
        self.refresh_dup_resolve();
    }

    /// Set the selected book's rating (0 clears), flashing the result.
    fn lib_set_rating(&mut self, rating: u8) {
        if let (Some(store), Some(book)) = (&self.store, self.library.books.get(self.library.sel)) {
            store.set_rating(&book.path, rating);
        }
        self.library.flash = Some(if rating == 0 {
            "rating cleared".to_string()
        } else {
            format!("rated {}", "★".repeat(rating as usize))
        });
        self.refresh_library();
    }

    /// Cycle the selected book's manual reading status (none → paused → dropped →
    /// reference → none), flashing the new effective status.
    fn lib_cycle_status(&mut self) {
        let Some(book) = self.library.books.get(self.library.sel) else {
            return;
        };
        let next = delryn_model::ReadingStatus::cycle_manual(&book.status);
        let pct = book.pct;
        if let Some(store) = &self.store {
            store.set_status(&book.path, next);
        }
        let eff = delryn_model::ReadingStatus::effective(pct, next);
        self.library.flash = Some(format!("status: {}", eff.label()));
        self.refresh_library();
    }

    /// The book path the detail cover should show (empty when no cover pane is
    /// relevant, so we treat it as "nothing to do").
    fn cover_target_path(&self) -> String {
        if self.mode != Mode::Library || !self.library.detail || self.is_grid() {
            return self.library.cover_path.clone();
        }
        self.library
            .books
            .get(self.library.sel)
            .map(|b| b.path.clone())
            .unwrap_or_default()
    }

    /// Is the detail cover stale (wants rebuilding)? Keeps the loop ticking.
    pub fn cover_pending(&self) -> bool {
        self.cover_target_path() != self.library.cover_path
    }

    /// Debounced detail-cover build: only (re)decode once the selection has held
    /// still briefly, so holding j/k never pays the per-book zip-read + decode.
    /// Returns whether the cover changed (the loop should redraw).
    pub fn tick_cover(&mut self) -> bool {
        let target = self.cover_target_path();
        if target == self.library.cover_path {
            return false;
        }
        if target != self.library.cover_target {
            // Selection moved — restart the settle timer, build nothing yet.
            self.library.cover_target = target;
            self.library.cover_at = Instant::now();
            return false;
        }
        if self.library.cover_at.elapsed() < COVER_DEBOUNCE {
            return false;
        }
        self.library.cover_path = target.clone();
        self.library.cover = match (&self.picker, load_cover_bytes(&target)) {
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
            if self.library.grid_covers.contains(path) {
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
            // Bounded LRU: an eviction must free its terminal image too, else
            // image memory grows until the terminal blanks everything.
            if let Some((_, Some(evicted))) = self.library.grid_covers.push(path.clone(), cover)
                && let Some(id) = evicted.image_id()
            {
                self.library.grid_deletes.push(id);
            }
            built += 1;
        }
        self.library.grid_pending = pending;
    }

    /// Whether the grid is still building visible covers (keeps the loop drawing).
    pub fn lib_grid_pending(&self) -> bool {
        self.mode == Mode::Library && self.is_grid() && self.library.grid_pending
    }

    /// The cover-grid view: navigates by grid columns, lazily builds cover
    /// thumbnails, and has no detail pane.
    pub fn is_grid(&self) -> bool {
        self.config.library_layout == LibLayout::Grid
    }

    /// Vertical step for j/k: one grid row in grid view, else one list row.
    fn grid_step(&self) -> isize {
        if self.is_grid() {
            self.library.grid_cols.max(1) as isize
        } else {
            1
        }
    }

    /// Visible panes, left → right, given show flags and the active layout.
    fn lib_visible_panes(&self) -> Vec<LibPane> {
        let mut panes = Vec::new();
        if self.library.show_sidebar {
            panes.push(LibPane::Sidebar);
        }
        panes.push(LibPane::List);
        // The detail pane only exists alongside the list views (cover views have
        // no detail pane).
        if self.library.detail && !self.is_grid() {
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
        let cur = panes
            .iter()
            .position(|p| *p == self.library.pane)
            .unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(panes.len() as isize) as usize;
        self.library.pane = panes[next];
    }

    /// Keep the focused pane valid when one is hidden.
    fn lib_ensure_pane_visible(&mut self) {
        if !self.lib_visible_panes().contains(&self.library.pane) {
            self.library.pane = LibPane::List;
        }
    }

    /// Grow/shrink the focused side pane's percentage (`<`/`>`); the responsive
    /// split turns it into cells and the list takes the slack.
    fn lib_resize(&mut self, delta: i16) {
        match self.library.pane {
            LibPane::Sidebar => {
                self.library.sidebar_pct = (self.library.sidebar_pct as i16 + delta)
                    .clamp(SIDEBAR_PCT_MIN as i16, SIDEBAR_PCT_MAX as i16)
                    as u16;
            }
            LibPane::Detail => {
                self.library.detail_pct = (self.library.detail_pct as i16 + delta)
                    .clamp(DETAIL_PCT_MIN as i16, DETAIL_PCT_MAX as i16)
                    as u16;
            }
            LibPane::List => {}
        }
    }

    /// Total entries in the sidebar (fixed sections + collections).
    fn lib_view_count(&self) -> usize {
        LibrarySection::ALL.len() + self.library.shelves.len()
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
            self.library.side_new = true; // parked on "＋ New collection"
            return;
        }
        self.library.side_new = false;
        self.library.view = self.lib_view_at(i);
        self.library.sel = 0;
        self.refresh_library();
    }

    /// Move the sidebar cursor by `delta` (clamped), switching the view live.
    /// The cursor ranges over the views plus the trailing "＋ New" row.
    fn lib_side_move(&mut self, delta: isize) {
        let max = self.lib_view_count(); // index of "＋ New collection"
        let cur = if self.library.side_new {
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
        match &self.library.view {
            LibView::Section(s) => LibrarySection::ALL.iter().position(|x| x == s).unwrap_or(0),
            LibView::Shelf(name) => self
                .library
                .shelves
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
            LibView::Shelf(self.library.shelves[i - n].0.clone())
        }
    }

    pub(crate) fn library_key(&mut self, key: KeyEvent) {
        if self.library.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.library.filter.clear();
                    self.library.filtering = false;
                    self.refresh_library();
                }
                KeyCode::Enter => self.library.filtering = false,
                KeyCode::Backspace => {
                    self.library.filter.pop();
                    self.refresh_library();
                }
                KeyCode::Char(c) => {
                    self.library.filter.push(c);
                    self.refresh_library();
                }
                _ => {}
            }
            return;
        }
        let pane = self.library.pane;
        let grid = self.is_grid();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Page sizes (rows) for vim-style half/full-page navigation.
        let rows = self.library.visible_rows.max(1) as isize;
        let half = (rows / 2).max(1);
        match key.code {
            // Vim half/full-page nav (Ctrl-d/u/f/b) + Page keys. Guarded on Ctrl
            // so plain d/b/f keep their meanings (detail / sidebar / favorite).
            KeyCode::Char('d') if ctrl => self.lib_page_move(half),
            KeyCode::Char('u') if ctrl => self.lib_page_move(-half),
            KeyCode::Char('f') if ctrl => self.lib_page_move(rows),
            KeyCode::Char('b') if ctrl => self.lib_page_move(-rows),
            KeyCode::PageDown => self.lib_page_move(rows),
            KeyCode::PageUp => self.lib_page_move(-rows),
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Esc => {
                if self.library.visual.is_some() || !self.library.marked.is_empty() {
                    self.lib_exit_visual();
                } else if self.library.filter.is_empty() {
                    self.should_quit = true;
                } else {
                    self.library.filter.clear();
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
                    if self.library.side_new {
                        self.lib_coll_begin_new();
                    } else {
                        self.library.pane = LibPane::List;
                    }
                } else {
                    self.open_selected();
                }
            }
            KeyCode::Char('o') => self.open_selected(),
            KeyCode::Char('g') => match pane {
                LibPane::Sidebar => self.lib_set_view_index(0),
                _ => self.library.sel = 0,
            },
            KeyCode::Char('G') => match pane {
                LibPane::Sidebar => {
                    self.lib_set_view_index(self.lib_view_count().saturating_sub(1))
                }
                _ => self.library.sel = self.library.books.len().saturating_sub(1),
            },
            // Resize the focused side pane (Shift+</>); show/hide sidebar/detail.
            KeyCode::Char('<') => self.lib_resize(-2),
            KeyCode::Char('>') => self.lib_resize(2),
            KeyCode::Char('b') => {
                self.library.show_sidebar = !self.library.show_sidebar;
                self.lib_ensure_pane_visible();
            }
            KeyCode::Char('d') => {
                self.library.detail = !self.library.detail;
                self.lib_ensure_pane_visible();
            }
            // Cycle the manual reading status (none → paused → dropped → reference).
            KeyCode::Char('m') => self.lib_cycle_status(),
            // `D` opens the duplicate-resolution overlay (all groups, checkboxes,
            // smart auto-select + manual select, bulk delete). A library-wide
            // action, so it works from any pane — including the sidebar.
            KeyCode::Char('D') => self.open_dup_resolve(),
            // `R` (Duplicates view) runs a thorough cover scan, finding duplicates
            // the metadata pass misses — chiefly PDFs matched by cover. Works from
            // any pane: with zero current duplicates the focus sits on the sidebar,
            // which is exactly where the reader presses it.
            KeyCode::Char('R')
                if matches!(
                    self.library.view,
                    LibView::Section(LibrarySection::Duplicates)
                ) =>
            {
                self.start_dup_scan()
            }
            // `I` (Duplicates view) manages the groups you've ignored (restore/clear).
            KeyCode::Char('I')
                if matches!(
                    self.library.view,
                    LibView::Section(LibrarySection::Duplicates)
                ) =>
            {
                self.open_ignored_view()
            }
            // Book actions operate on the selected book regardless of focus.
            KeyCode::Char('f') => {
                if self.library.marked.is_empty() {
                    self.lib_favorite()
                } else {
                    self.bulk_favorite()
                }
            }
            // 0–5 rate the selected book (0 clears the rating).
            KeyCode::Char(c @ '0'..='5') if pane != LibPane::Sidebar => {
                self.lib_set_rating(c as u8 - b'0');
            }
            // `i` opens the library statistics overlay.
            KeyCode::Char('i') => self.open_stats(),
            // `X` exports the current (filtered) view to CSV.
            KeyCode::Char('X') => self.export_library(),
            // `e` edits the current book; with a selection, edits each in turn.
            KeyCode::Char('e') => {
                if self.library.marked.is_empty() {
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
                    && !self.library.side_new
                    && matches!(self.library.view, LibView::Shelf(_)) =>
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
            // `T` edits tags for the selected book (or all marked books).
            KeyCode::Char('T') if pane != LibPane::Sidebar => self.open_tag_edit(),
            KeyCode::Char('/') => {
                self.lib_exit_visual();
                self.library.filtering = true;
            }
            _ => {}
        }
        // After any movement, extend the visual-mode range to the new cursor.
        self.lib_visual_sync();
    }
}
