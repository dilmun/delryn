//! Library multi-selection: vim-style visual range (`V`), individual picks
//! (`Space`), select-all (`A`), and bulk actions over the selection.

use super::App;

impl App {
    /// Toggle vim-style visual (range) select. Entering anchors at the cursor;
    /// exiting commits the live range into the individual selection so it sticks.
    pub(crate) fn lib_toggle_visual(&mut self) {
        if self.library.visual.is_some() {
            self.library.marked_base = self.library.marked.clone(); // commit the range
            self.library.visual = None;
        } else {
            self.library.visual = Some(self.library.sel);
            self.lib_visual_sync();
        }
    }

    /// Toggle the current book in the individual (Space) selection, then advance
    /// — so non-contiguous picks build up, file-manager style. Finalises any live
    /// visual range first.
    pub(crate) fn lib_toggle_mark(&mut self) {
        if self.library.visual.is_some() {
            self.library.marked_base = self.library.marked.clone();
            self.library.visual = None;
        }
        if let Some(b) = self.library.books.get(self.library.sel) {
            let path = b.path.clone();
            if !self.library.marked_base.remove(&path) {
                self.library.marked_base.insert(path);
            }
        }
        self.library.marked = self.library.marked_base.clone();
        self.lib_move(1);
    }

    /// Toggle the book at `idx` in the individual selection **without advancing** —
    /// the mouse (right-click) counterpart to `Space`, which moves on after marking.
    pub(crate) fn lib_mouse_toggle_mark(&mut self, idx: usize) {
        if self.library.visual.is_some() {
            self.library.marked_base = self.library.marked.clone();
            self.library.visual = None;
        }
        if let Some(b) = self.library.books.get(idx) {
            let path = b.path.clone();
            if !self.library.marked_base.remove(&path) {
                self.library.marked_base.insert(path);
            }
        }
        self.library.marked = self.library.marked_base.clone();
        self.library.sel = idx;
    }

    /// Range-select from the current cursor to `idx` (Shift-click), adding the span
    /// to the individual selection — the mouse counterpart to a visual range.
    pub(crate) fn lib_mouse_range_to(&mut self, idx: usize) {
        let n = self.library.books.len();
        if n == 0 {
            return;
        }
        let idx = idx.min(n - 1);
        let (lo, hi) = (self.library.sel.min(idx), self.library.sel.max(idx));
        let span: Vec<String> = self.library.books[lo..=hi]
            .iter()
            .map(|b| b.path.clone())
            .collect();
        self.library.marked_base.extend(span);
        self.library.marked = self.library.marked_base.clone();
        self.library.sel = idx;
    }

    /// Select every book in the current list (for bulk actions over the library).
    pub(crate) fn lib_mark_all(&mut self) {
        self.library.visual = None;
        self.library.marked_base = self.library.books.iter().map(|b| b.path.clone()).collect();
        self.library.marked = self.library.marked_base.clone();
    }

    /// Leave visual mode and clear the whole selection (individual + range).
    pub(crate) fn lib_exit_visual(&mut self) {
        self.library.visual = None;
        self.library.marked.clear();
        self.library.marked_base.clear();
    }

    /// Recompute the effective selection: the individual picks plus, in visual
    /// mode, the contiguous range between the anchor and the cursor. Called after
    /// cursor movement.
    pub(crate) fn lib_visual_sync(&mut self) {
        let Some(anchor) = self.library.visual else {
            return;
        };
        let mut sel = self.library.marked_base.clone();
        if !self.library.books.is_empty() {
            let last = self.library.books.len() - 1;
            let (lo, hi) = (
                anchor.min(self.library.sel).min(last),
                anchor.max(self.library.sel).min(last),
            );
            sel.extend(self.library.books[lo..=hi].iter().map(|b| b.path.clone()));
        }
        self.library.marked = sel;
    }

    /// Favorite all marked books (or unfavorite them if all are already
    /// favorites), then clear the selection.
    pub(crate) fn bulk_favorite(&mut self) {
        let marked: Vec<String> = self
            .library
            .books
            .iter()
            .filter(|b| self.library.marked.contains(&b.path))
            .map(|b| b.path.clone())
            .collect();
        if marked.is_empty() {
            return;
        }
        let all_fav = self
            .library
            .books
            .iter()
            .filter(|b| self.library.marked.contains(&b.path))
            .all(|b| b.favorite);
        let target = !all_fav;
        if let Some(store) = &self.session.store {
            for p in &marked {
                store.set_favorite(p, target);
            }
        }
        let n = marked.len();
        self.lib_exit_visual();
        self.refresh_library();
        self.library.flash = Some(format!(
            "{} {n} book{}",
            if target { "favorited" } else { "unfavorited" },
            if n == 1 { "" } else { "s" }
        ));
    }
}
