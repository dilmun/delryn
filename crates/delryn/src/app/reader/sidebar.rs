//! Reader TOC sidebar: cursor movement, viewport scroll/centering, the
//! collapsible outline tree, and scroll-spy (active-entry tracking).

use super::*;

impl Reader {
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
            if active != self.nav.last_active {
                self.nav.last_active = active;
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
        // Paged-image (PDF): the position *is* the section, and there are no text
        // locators (so `heading_lines` is empty). Spy by section instead — the
        // last outline entry whose page is at or before the current one.
        if self.is_paged_image() {
            return self.outline_for_section(self.section);
        }
        let mut best: Option<(usize, usize)> = None; // (line, outline index)
        for &(oi, line) in &self.nav.heading_lines {
            // Greatest line at/above the viewport top; on ties keep the earlier
            // entry (strictly greater to replace).
            if line <= self.scroll && best.is_none_or(|(bl, _)| line > bl) {
                best = Some((line, oi));
            }
        }
        best.map(|(_, oi)| oi)
            .or_else(|| self.nav.heading_lines.first().map(|&(oi, _)| oi))
            // A continuation section (a chapter spanning several sections) has no
            // outline entry of its own, so `heading_lines` is empty — fall back to
            // the chapter that contains it (last entry at/before this section).
            .or_else(|| self.outline_for_section(self.section))
    }

    /// The outline index whose target page is the greatest at or before
    /// `section` — scroll-spy for paged-image (PDF) documents, where the outline
    /// targets sections directly. On ties (several entries on one page) the last
    /// (deepest) wins.
    fn outline_for_section(&self, section: usize) -> Option<usize> {
        self.outline
            .iter()
            .enumerate()
            .filter(|(_, it)| it.section <= section)
            .max_by_key(|(_, it)| it.section)
            .map(|(i, _)| i)
    }

    /// Position of `active_outline` within the visible (collapsed-aware) list.
    pub fn active_outline_row(&self) -> Option<usize> {
        let active = self.active_outline()?;
        self.outline_visible().iter().position(|&oi| oi == active)
    }

    /// The active sidebar filter, lowercased, or `None` when not filtering.
    pub fn sidebar_query(&self) -> Option<String> {
        let q = self.sidebar_filter.as_ref()?.text().trim().to_lowercase();
        (!q.is_empty()).then_some(q)
    }

    /// Open the contents filter (`/` with the sidebar focused), starting empty.
    pub fn start_sidebar_filter(&mut self) {
        self.sidebar_filter = Some(crate::ui::TextInput::new());
        self.sidebar_sel = 0;
        self.sidebar_offset = 0;
    }

    /// Close the filter, keeping the cursor on whatever it had landed on. Returns
    /// whether a filter was actually open.
    pub fn clear_sidebar_filter(&mut self) -> bool {
        if self.sidebar_filter.is_none() {
            return false;
        }
        // Resolve the highlighted entry *before* dropping the filter, then restore
        // the cursor onto that same entry in the unfiltered list — otherwise the
        // index would point at a different chapter once the list grows back.
        let landed = self.selected_outline_index();
        self.sidebar_filter = None;
        if let Some(oi) = landed
            && let Some(row) = self.outline_visible().iter().position(|&i| i == oi)
        {
            self.sidebar_sel = row;
        }
        self.center_sidebar();
        true
    }

    /// The outline index under the sidebar cursor, filter-aware.
    pub fn selected_outline_index(&self) -> Option<usize> {
        self.outline_visible().get(self.sidebar_sel).copied()
    }

    /// Outline indices currently visible (respecting collapsed parents).
    pub fn outline_visible(&self) -> Vec<usize> {
        // A filter searches the whole outline and ignores collapse state: a match
        // hidden inside a folded parent would otherwise be unreachable.
        if let Some(q) = self.sidebar_query() {
            return self
                .outline
                .iter()
                .enumerate()
                .filter(|(_, e)| e.label.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect();
        }
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
        if let Some(oi) = self.selected_outline()
            && let Some(item) = self.outline.get(oi).cloned()
        {
            self.jump_to(item.section, item.locator.as_deref());
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
            if depth > 0
                && let Some(pi) = (0..oi).rev().find(|&j| self.outline[j].depth < depth)
                && let Some(pos) = self.outline_visible().iter().position(|&x| x == pi)
            {
                self.sidebar_sel = pos;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{para, reader_with};
    use crate::document::OutlineItem;

    fn entry(label: &str, depth: usize, section: usize) -> OutlineItem {
        OutlineItem {
            label: label.into(),
            depth,
            section,
            locator: None,
        }
    }

    /// A nested outline where the parent is collapsed, so the filter has something
    /// to reach past.
    fn outlined() -> crate::app::Reader {
        let mut r = reader_with(vec![para()]);
        r.outline = vec![
            entry("Boolean Algebra", 0, 0),
            entry("Karnaugh Maps", 1, 0),
            entry("Logic Gates", 1, 0),
            entry("Functions", 0, 0),
            entry("Karnaugh Revisited", 1, 0),
        ];
        r
    }

    /// The filter matches on label, case-insensitively, across the whole outline.
    #[test]
    fn filter_narrows_the_contents_to_matches() {
        let mut r = outlined();
        r.start_sidebar_filter();
        if let Some(i) = r.sidebar_filter.as_mut() {
            i.set("karn");
        }
        let vis = r.outline_visible();
        assert_eq!(vis, vec![1, 4], "both Karnaugh entries, nothing else");
    }

    /// A match inside a collapsed parent must still be reachable — filtering
    /// deliberately ignores fold state, or the entry could not be selected at all.
    #[test]
    fn filter_reaches_inside_a_collapsed_parent() {
        let mut r = outlined();
        r.collapsed.insert(0);
        assert!(
            !r.outline_visible().contains(&1),
            "precondition: the child is folded away"
        );
        r.start_sidebar_filter();
        if let Some(i) = r.sidebar_filter.as_mut() {
            i.set("karnaugh maps");
        }
        assert_eq!(
            r.outline_visible(),
            vec![1],
            "the folded match is reachable"
        );
    }

    /// Clearing the filter keeps the cursor on the entry it landed on, rather than
    /// leaving the index pointing at whatever now occupies that row.
    #[test]
    fn clearing_the_filter_keeps_the_highlighted_entry() {
        let mut r = outlined();
        r.start_sidebar_filter();
        if let Some(i) = r.sidebar_filter.as_mut() {
            i.set("revisited");
        }
        assert_eq!(r.selected_outline_index(), Some(4));
        assert!(r.clear_sidebar_filter());
        assert!(r.sidebar_filter.is_none());
        assert_eq!(
            r.selected_outline_index(),
            Some(4),
            "cursor follows the entry, not the row number"
        );
    }

    /// An empty query is not a filter — the collapse-aware list comes back.
    #[test]
    fn an_empty_query_leaves_the_outline_alone() {
        let mut r = outlined();
        let full = r.outline_visible();
        r.start_sidebar_filter();
        assert_eq!(r.outline_visible(), full);
        assert!(r.sidebar_query().is_none());
    }
}
