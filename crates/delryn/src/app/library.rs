//! Library mode: browsing the indexed collection. Owns the view/section model
//! (smart sections + user collections), book list refresh + sort, cursor and
//! pane navigation, grid/list/detail layout, detail-pane + grid cover loading,
//! and the library-mode key dispatch.

use std::collections::HashSet;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};

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
    Rating,
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
            SortKey::Rating => "rating",
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
            SortKey::Size => SortKey::Rating,
            SortKey::Rating => SortKey::Default,
        }
    }
}

impl App {
    pub(crate) fn refresh_library(&mut self) {
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
        let books = if self.lib_filter.trim().is_empty() {
            match &view {
                // Cross-format duplicate detection (ISBN, else normalized
                // title+author) — richer than a title-only SQL match.
                LibView::Section(LibrarySection::Duplicates) => {
                    let all = store.all_books();
                    let dups = crate::library::dedup::duplicate_paths(&all);
                    all.into_iter().filter(|b| dups.contains(&b.path)).collect()
                }
                LibView::Section(s) => store.list_books(*s),
                LibView::Shelf(name) => store.books_in_shelf(name),
            }
        } else {
            // Library-wide search (ignores the active section, by design). A
            // structured query (`author:knuth year>=1990`, flags, AND/OR/NOT)
            // is evaluated field-by-field; a plain query keeps the title/author/
            // series/publisher substring + full-text body match.
            let q = crate::library::query::parse(&self.lib_filter);
            if q.is_structured() {
                store
                    .all_books()
                    .into_iter()
                    .filter(|b| q.matches(b))
                    .collect()
            } else {
                let f = self.lib_filter.to_lowercase();
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
            }
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
                SortKey::Rating => a.rating.cmp(&b.rating),
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

    pub(crate) fn lib_move(&mut self, delta: isize) {
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

    /// Export the current (filtered) book list to a CSV in the config dir.
    fn export_library(&mut self) {
        let csv = crate::library::export::to_csv(&self.lib_books);
        let path = crate::paths::config_dir().join("delryn-export.csv");
        self.lib_flash = Some(match std::fs::write(&path, csv) {
            Ok(()) => format!(
                "exported {} book{} → {}",
                self.lib_books.len(),
                if self.lib_books.len() == 1 { "" } else { "s" },
                path.display()
            ),
            Err(e) => format!("export failed: {e}"),
        });
    }

    /// Open the library-statistics overlay (computed over all books).
    fn open_stats(&mut self) {
        if let Some(store) = &self.store {
            let books = store.all_books();
            let secs = store.total_read_seconds();
            self.stats = Some(crate::library::stats::compute(&books, secs));
        }
    }

    /// Set the selected book's rating (0 clears), flashing the result.
    fn lib_set_rating(&mut self, rating: u8) {
        if let (Some(store), Some(book)) = (&self.store, self.lib_books.get(self.lib_sel)) {
            store.set_rating(&book.path, rating);
        }
        self.lib_flash = Some(if rating == 0 {
            "rating cleared".to_string()
        } else {
            format!("rated {}", "★".repeat(rating as usize))
        });
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

    pub(crate) fn library_key(&mut self, key: KeyEvent) {
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
}
