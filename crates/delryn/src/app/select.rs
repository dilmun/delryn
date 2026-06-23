//! Library multi-selection: vim-style visual range (`V`), individual picks
//! (`Space`), select-all (`A`), and bulk actions over the selection.

use super::App;

impl App {
    /// Toggle vim-style visual (range) select. Entering anchors at the cursor;
    /// exiting commits the live range into the individual selection so it sticks.
    pub(crate) fn lib_toggle_visual(&mut self) {
        if self.lib_visual.is_some() {
            self.lib_marked_base = self.lib_marked.clone(); // commit the range
            self.lib_visual = None;
        } else {
            self.lib_visual = Some(self.lib_sel);
            self.lib_visual_sync();
        }
    }

    /// Toggle the current book in the individual (Space) selection, then advance
    /// — so non-contiguous picks build up, file-manager style. Finalises any live
    /// visual range first.
    pub(crate) fn lib_toggle_mark(&mut self) {
        if self.lib_visual.is_some() {
            self.lib_marked_base = self.lib_marked.clone();
            self.lib_visual = None;
        }
        if let Some(b) = self.lib_books.get(self.lib_sel) {
            let path = b.path.clone();
            if !self.lib_marked_base.remove(&path) {
                self.lib_marked_base.insert(path);
            }
        }
        self.lib_marked = self.lib_marked_base.clone();
        self.lib_move(1);
    }

    /// Select every book in the current list (for bulk actions over the library).
    pub(crate) fn lib_mark_all(&mut self) {
        self.lib_visual = None;
        self.lib_marked_base = self.lib_books.iter().map(|b| b.path.clone()).collect();
        self.lib_marked = self.lib_marked_base.clone();
    }

    /// Leave visual mode and clear the whole selection (individual + range).
    pub(crate) fn lib_exit_visual(&mut self) {
        self.lib_visual = None;
        self.lib_marked.clear();
        self.lib_marked_base.clear();
    }

    /// Recompute the effective selection: the individual picks plus, in visual
    /// mode, the contiguous range between the anchor and the cursor. Called after
    /// cursor movement.
    pub(crate) fn lib_visual_sync(&mut self) {
        let Some(anchor) = self.lib_visual else {
            return;
        };
        let mut sel = self.lib_marked_base.clone();
        if !self.lib_books.is_empty() {
            let last = self.lib_books.len() - 1;
            let (lo, hi) = (
                anchor.min(self.lib_sel).min(last),
                anchor.max(self.lib_sel).min(last),
            );
            sel.extend(self.lib_books[lo..=hi].iter().map(|b| b.path.clone()));
        }
        self.lib_marked = sel;
    }

    /// Favorite all marked books (or unfavorite them if all are already
    /// favorites), then clear the selection.
    pub(crate) fn bulk_favorite(&mut self) {
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
}
