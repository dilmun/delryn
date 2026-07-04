//! Continuous scroll across section boundaries.
//!
//! Two variants share this concern. The **reflowable** one (the bulk of this file)
//! joins wrapped-text chapters. The **paged** one (`continuous_paged_*`) stacks PDF
//! page images vertically and rolls the anchor page in row units — the layout for
//! it is the pure `reader::page_stack` geometry, assembled in `reader::paged`.
//!
//! The reader normally shows one section's wrapped lines and *jumps* to the next
//! at the bottom edge (the new chapter restarts at the top). Continuous mode makes
//! that boundary seamless: the anchor section's tail and the following sections'
//! heads share the viewport, and scrolling rolls the **anchor** — the section at
//! the top of the viewport — across boundaries.
//!
//! The design is deliberately additive, so the proven single-section path is
//! untouched when continuous mode is off:
//!
//! * `self.section` + `self.scroll` stay the canonical position — now the *anchor*
//!   section and an offset into it (no longer clamped to that section's last page).
//!   All the per-section machinery (headings, anchors, bookmarks, images, search)
//!   keeps working against the anchor exactly as before.
//! * [`Reader::continuous_lines`] assembles the render buffer — the anchor lines
//!   from `scroll` onward, then following sections' wrapped lines — filling the
//!   viewport. Following sections are wrapped with the same options and cached.
//! * [`scroll_down`](Reader::scroll_down) / [`scroll_up`](Reader::scroll_up) roll
//!   the anchor across boundaries instead of stopping at a per-section edge.
//!
//! v1 limits (documented, graceful — no crashes): the bookmark gutter, the link
//! cursor, and inline-image transmit follow the **anchor** section only; a
//! following section's figures reserve their space (blank rows) until it becomes
//! the anchor. Continuous is reflow + single-column (Center) only; page mode,
//! chapter-lock, and paged (PDF) documents fall back to per-section scrolling.

use std::sync::atomic::Ordering;

use crate::layout::DisplayLine;

use super::{Reader, page_stack};

impl Reader {
    /// Whether continuous cross-section scroll is active right now: the flag is on
    /// and the document is reflowable, not page-mode, and not chapter-locked.
    pub fn continuous_active(&self) -> bool {
        self.continuous && !self.is_paged_image() && !self.paged && !self.chapter_lock
    }

    /// Whether continuous *paged* (PDF page-stacking) scroll is active: the flag is
    /// on for a paged-image document in single-column (Center) mode, not page-snap
    /// and not chapter-locked. Mutually exclusive with
    /// [`continuous_active`](Self::continuous_active) (which excludes paged docs).
    /// `self.continuous` is only set in Center mode and `self.spread` only in
    /// TwoPage, so `!spread` restates single-column; `!paged` keeps page-snap and
    /// continuous exclusive, the same rule as the reflow variant.
    pub fn continuous_paged_active(&self) -> bool {
        self.continuous
            && self.is_paged_image()
            && !self.spread
            && !self.paged
            && !self.chapter_lock
    }

    /// Continuous-paged scroll-down by `n` rows: advance the anchor page's scroll
    /// offset, rolling to the next page each time the offset passes the anchor's
    /// slot (its display height plus the inter-page gap). At the last page it clamps
    /// so the page's bottom can't scroll above the viewport bottom.
    pub(super) fn continuous_paged_scroll_down(&mut self, n: usize) {
        let last = self.doc.section_count().saturating_sub(1);
        self.scroll += n;
        while self.section < last {
            let slot = self.paged_slot_rows(self.section);
            if self.scroll >= slot {
                self.scroll -= slot;
                self.move_paged_anchor(self.section + 1);
            } else {
                break;
            }
        }
        if self.section >= last {
            let bottom = self
                .page_rows_of(last)
                .saturating_sub(self.viewport_lines.max(1) as u16);
            self.scroll = self.scroll.min(bottom as usize);
        }
    }

    /// Continuous-paged scroll-up by `n` rows: retreat the anchor's offset, crossing
    /// into the previous page's slot at its top (landing just below the boundary,
    /// the exact inverse of the down-roll threshold). Stops at the book start.
    pub(super) fn continuous_paged_scroll_up(&mut self, n: usize) {
        let mut up = n;
        while up > 0 {
            if self.scroll >= up {
                self.scroll -= up;
                break;
            }
            up -= self.scroll;
            self.scroll = 0;
            if self.section == 0 {
                break; // at the very start of the book
            }
            up -= 1;
            self.move_paged_anchor(self.section - 1);
            self.scroll = self.paged_slot_rows(self.section).saturating_sub(1);
        }
    }

    /// Move the continuous-paged anchor to `section` without resetting the scroll
    /// offset: unlike a reflow anchor there are no lines to re-wrap, so this just
    /// re-points the section, steers the background loader, and prefetches the new
    /// neighbourhood so upcoming pages rasterize ahead.
    fn move_paged_anchor(&mut self, section: usize) {
        if section >= self.doc.section_count() {
            return;
        }
        self.nav.nav_back = section < self.section;
        self.section = section;
        self.sections
            .loader_current
            .store(section, Ordering::Relaxed);
        self.prefetch_neighbors();
    }

    /// A paged page's "slot" height in rows: its fit-width display height plus the
    /// inter-page gap. One slot is the scroll distance from a page's top to the next
    /// page's top.
    pub(super) fn paged_slot_rows(&mut self, section: usize) -> usize {
        self.page_rows_of(section) as usize + page_stack::STACK_GAP as usize
    }

    /// A paged page's fit-width display height in rows (see
    /// [`page_metrics`](Self::page_metrics)).
    pub(super) fn page_rows_of(&mut self, section: usize) -> u16 {
        self.page_metrics(section).1
    }

    /// The margin-trimmed content box and fit-width display height (rows) of
    /// `section`'s page. Once the page is rasterized this is exact and is cached as
    /// the estimate for still-loading pages; before that it falls back to that
    /// estimate (or the viewport height) so scroll math and layout stay stable.
    pub(super) fn page_metrics(&mut self, section: usize) -> ((u32, u32, u32, u32), u16) {
        let body_w = self.last_measure.max(1) as u16;
        let cell = self.cell_px;
        if let Some(dims) = self.base_raster_dims(section) {
            let content = self.page_content_box(section, dims);
            let rows = page_stack::page_rows(content, body_w, cell).max(1);
            self.est_page_rows = rows;
            (content, rows)
        } else {
            let rows = if self.est_page_rows > 0 {
                self.est_page_rows
            } else {
                self.viewport_lines.max(1) as u16
            };
            ((0, 0, 1, 1), rows)
        }
    }

    /// The render buffer for continuous mode: the anchor section's lines from
    /// `scroll` onward, then following sections' wrapped lines, until at least
    /// `height` lines are gathered (or the book ends). Owned so the view can render
    /// it directly. Following sections are wrapped once and cached.
    pub fn continuous_lines(&mut self, height: usize) -> Vec<DisplayLine> {
        let scroll = self.scroll.min(self.lines.len());
        let mut out: Vec<DisplayLine> = self.lines[scroll..].to_vec();
        let count = self.doc.section_count();
        let mut s = self.section + 1;
        // One extra line so a partial last row still has content beneath it.
        while out.len() <= height && s < count {
            let lines = self.following_lines(s);
            out.extend(lines);
            s += 1;
        }
        out
    }

    /// Wrapped lines of following section `s` for the continuous buffer, cached and
    /// invalidated wholesale when the wrap inputs change.
    fn following_lines(&mut self, s: usize) -> Vec<DisplayLine> {
        if self.cont_key != self.wrapped {
            self.cont_cache.clear();
            self.cont_key = self.wrapped.clone();
        }
        if let Some(v) = self.cont_cache.get(&s) {
            return v.clone();
        }
        let blocks = self.fetch_blocks(s);
        let lines = self.wrap_at(&blocks, self.last_measure.max(1));
        self.cont_cache.insert(s, lines.clone());
        lines
    }

    /// Continuous scroll-down: advance the anchor across section boundaries. The
    /// offset may sit anywhere in the anchor section; once the whole anchor has
    /// scrolled above the viewport top, roll to the next section (keeping the
    /// leftover). At the book's final section it clamps to the last page.
    pub(super) fn continuous_scroll_down(&mut self, n: usize) {
        let last = self.doc.section_count().saturating_sub(1);
        self.scroll += n;
        while self.section < last && self.scroll >= self.lines.len().max(1) {
            self.scroll -= self.lines.len().max(1);
            self.move_anchor(self.section + 1);
        }
        if self.section >= last {
            let max = self.max_scroll();
            self.scroll = self.scroll.min(max);
        }
    }

    /// Continuous scroll-up: retreat the anchor across section boundaries. Crossing
    /// the top of a section lands on the last line of the previous one.
    pub(super) fn continuous_scroll_up(&mut self, n: usize) {
        let mut up = n;
        while up > 0 {
            if self.scroll >= up {
                self.scroll -= up;
                break;
            }
            // Consume the lines above the viewport top in this section, then step
            // across the boundary into the previous section's last line.
            up -= self.scroll;
            self.scroll = 0;
            if self.section == 0 {
                break; // at the very start of the book
            }
            up -= 1;
            self.move_anchor(self.section - 1);
            self.scroll = self.lines.len().saturating_sub(1);
        }
    }

    /// Move the continuous anchor to `section` without resetting the scroll offset
    /// or touching navigation history: fetch its blocks and re-wrap immediately, so
    /// the roll loop and the next frame both see the new section's line count.
    fn move_anchor(&mut self, section: usize) {
        if section >= self.doc.section_count() {
            return;
        }
        self.nav.nav_back = section < self.section;
        self.section = section;
        self.sections
            .loader_current
            .store(section, Ordering::Relaxed);
        self.blocks = self.fetch_blocks(section);
        self.nav.anchor_sel = None;
        self.wrapped.width = usize::MAX; // force a re-wrap of the new anchor
        self.ensure_wrapped(self.last_measure.max(1));
        self.prefetch_neighbors();
    }
}
