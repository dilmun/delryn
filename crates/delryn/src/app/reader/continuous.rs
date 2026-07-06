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
//! Following sections' **figures are drawn too** (via
//! [`Reader::continuous_following_images`] and the view's `draw_following_images`):
//! each joined section's images are built, their rows reserved up front from a
//! stable decode estimate (never re-wrapped when the build lands, so scrolling
//! never shifts), and sliced at their buffer row like the anchor's — a figure near
//! a chapter boundary scrolls smoothly rather than leaving a blank gap until its
//! section becomes the anchor.
//!
//! v1 limits (documented, graceful — no crashes): the bookmark gutter and the link
//! cursor follow the **anchor** section only. Reflow continuous is single-column
//! (Center) only; page mode and
//! chapter-lock fall back to per-section scrolling. The **paged** variant
//! (`continuous_paged_*`) works in Center (one page per band) and TwoPage (a facing
//! pair per band), with zoom / centre / horizontal pan; page-snap and chapter-lock
//! disable it.

use std::sync::atomic::Ordering;

use crate::config::ViewMode;
use crate::layout::{DisplayLine, LineKind};

use super::{Reader, page_stack};

/// Continuous-paged zoom: multiplicative step per keypress, and the bounds relative
/// to fit-width (1.0). Zoom-out below 1.0 shrinks + centres pages (see more at
/// once); zoom-in above 1.0 enlarges a single page past the viewport (pan to read).
const CONT_ZOOM_STEP: f32 = 1.25;
const CONT_ZOOM_MIN: f32 = 0.25;
const CONT_ZOOM_MAX: f32 = 4.0;

/// Horizontal pan step (fraction of the overflow) per `h`/`l` press on a zoomed-in
/// single page.
const CONT_PAN_STEP: f32 = 0.2;

impl Reader {
    /// Whether continuous cross-section scroll is active right now: the flag is on
    /// and the document is reflowable, single-column (Center), not page-mode, and
    /// not chapter-locked.
    pub fn continuous_active(&self) -> bool {
        self.continuous
            && !self.is_paged_image()
            && self.view_mode == ViewMode::Center
            && !self.paged
            && !self.chapter_lock
    }

    /// Whether continuous *paged* (PDF page-stacking) scroll is active: the flag is
    /// on for a paged-image document, not page-snap and not chapter-locked. Works in
    /// both Center (one page per band) and TwoPage (a facing pair per band, see
    /// [`continuous_two_page`](Self::continuous_two_page)). Mutually exclusive with
    /// [`continuous_active`](Self::continuous_active) (which excludes paged docs).
    pub fn continuous_paged_active(&self) -> bool {
        self.continuous && self.is_paged_image() && !self.paged && !self.chapter_lock
    }

    /// Whether the continuous-paged stack shows a facing pair per band (TwoPage) vs.
    /// a single page (Center).
    pub fn continuous_two_page(&self) -> bool {
        self.continuous_paged_active() && self.view_mode == ViewMode::TwoPage
    }

    /// Continuous-paged scroll-down by `n` rows: advance the anchor band's scroll
    /// offset, rolling to the next band (next page, or next spread in two-page) each
    /// time the offset passes the band's slot (its display height plus the gap). At
    /// the last band it clamps so the content bottom can't scroll above the viewport.
    pub(super) fn continuous_paged_scroll_down(&mut self, n: usize) {
        self.scroll += n;
        while let Some(next) = self.next_band_anchor(self.section) {
            let slot = self.band_slot_rows(self.section);
            if self.scroll >= slot {
                self.scroll -= slot;
                self.move_paged_anchor(next);
            } else {
                break;
            }
        }
        if self.next_band_anchor(self.section).is_none() {
            let bottom = self
                .band_rows_of(self.section)
                .saturating_sub(self.viewport_lines.max(1) as u16);
            self.scroll = self.scroll.min(bottom as usize);
        }
    }

    /// Continuous-paged scroll-up by `n` rows: retreat the anchor's offset, crossing
    /// into the previous band's slot at its top (landing just below the boundary, the
    /// exact inverse of the down-roll threshold). Stops at the book start.
    pub(super) fn continuous_paged_scroll_up(&mut self, n: usize) {
        let mut up = n;
        while up > 0 {
            if self.scroll >= up {
                self.scroll -= up;
                break;
            }
            up -= self.scroll;
            self.scroll = 0;
            let Some(prev) = self.prev_band_anchor(self.section) else {
                break; // at the very start of the book
            };
            up -= 1;
            self.move_paged_anchor(prev);
            self.scroll = self.band_slot_rows(self.section).saturating_sub(1);
        }
    }

    /// The anchor of the band below `left` (the next page, or the next spread's left
    /// page in two-page), or `None` at the last band.
    pub(super) fn next_band_anchor(&self, left: usize) -> Option<usize> {
        let n = self.doc.section_count();
        let next = if self.continuous_two_page() {
            self.spread_left(left) + self.spread_width(left)
        } else {
            left + 1
        };
        (next < n).then_some(next)
    }

    /// The anchor of the band above `left` (the previous page / spread's left), or
    /// `None` at the book start.
    fn prev_band_anchor(&self, left: usize) -> Option<usize> {
        if self.continuous_two_page() {
            let l = self.spread_left(left);
            (l > 0).then(|| self.spread_left(l - 1))
        } else {
            left.checked_sub(1)
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

    /// A band's "slot" height in rows: its display height plus the inter-page gap.
    /// One slot is the scroll distance from a band's top to the next band's top.
    pub(super) fn band_slot_rows(&mut self, anchor: usize) -> usize {
        self.band_rows_of(anchor) as usize + page_stack::STACK_GAP as usize
    }

    /// A band's display height in rows: the single fit-page height (Center), or the
    /// taller of the spread's two pages (TwoPage), each at the current zoom.
    pub(super) fn band_rows_of(&mut self, anchor: usize) -> u16 {
        if self.continuous_two_page() {
            let col_w = self.continuous_column_slot().1;
            self.spread_at(self.spread_left(anchor))
                .into_iter()
                .map(|s| self.tile_metrics(s, col_w).1)
                .max()
                .unwrap_or(1)
                .max(1)
        } else {
            let slot_w = self.continuous_single_slot().1;
            self.tile_metrics(anchor, slot_w).1
        }
    }

    /// The effective zoom scale for the stack, clamped so a two-page spread never
    /// overflows the viewport (a page split across the fold has no sensible pan).
    pub(super) fn continuous_scale(&self) -> f32 {
        if self.continuous_two_page() {
            self.cont_scale.min(1.0)
        } else {
            self.cont_scale
        }
    }

    pub(super) fn continuous_pan_x(&self) -> f32 {
        self.cont_pan_x
    }

    /// Left/right padding in cells for the continuous paged content (from
    /// `side_padding` %), so pages don't touch the screen edges.
    pub(super) fn continuous_pad(&self) -> u16 {
        let vp = self.last_measure.max(1) as u32;
        (vp * self.side_padding as u32 / 100) as u16
    }

    /// The single-page content slot `(x, width)` — the padded region a fit-page
    /// single page centres within.
    pub(super) fn continuous_single_slot(&self) -> (u16, u16) {
        let vp = self.last_measure.max(1) as u16;
        let pad = self.continuous_pad();
        (pad, vp.saturating_sub(pad * 2).max(1))
    }

    /// A two-page spread's left/right column slots `(left_x, col_w, right_x)` inside
    /// the padded region, split by the inter-page gap.
    pub(super) fn continuous_column_slot(&self) -> (u16, u16, u16) {
        let (pad, avail) = self.continuous_single_slot();
        let col_w = avail.saturating_sub(self.page_gap) / 2;
        let left_x = pad;
        let right_x = pad + col_w + self.page_gap;
        (left_x, col_w.max(1), right_x)
    }

    /// The margin-trimmed content box of `section`'s page, or `None` until it's
    /// rasterized.
    pub(super) fn page_content_of(&mut self, section: usize) -> Option<(u32, u32, u32, u32)> {
        let dims = self.base_raster_dims(section)?;
        Some(self.page_content_box(section, dims))
    }

    /// The `(display width, display height)` in cells of `section`'s page laid out
    /// fit-page in a slot `slot_w` cells wide (and the viewport height), scaled by the
    /// current zoom. Exact once rasterized — and caches the canonical fit-page height
    /// as the estimate; before that it scales that estimate by the zoom so scroll math
    /// and layout stay stable across near-uniform PDF pages.
    pub(super) fn tile_metrics(&mut self, section: usize, slot_w: u16) -> (u16, u16) {
        let cell = self.cell_px;
        let vh = self.viewport_lines.max(1) as u16;
        let scale = self.continuous_scale();
        if let Some(content) = self.page_content_of(section) {
            let fit = page_stack::fit_page_cols(content, slot_w, vh, cell);
            let disp_w = ((fit as f32 * scale).round() as u16).max(1);
            let rows = page_stack::page_rows(content, disp_w, cell).max(1);
            // Canonical fit-page height (zoom 1) → the estimate for unloaded pages.
            self.est_page_rows = page_stack::page_rows(content, fit, cell).max(1);
            (disp_w, rows)
        } else {
            let base = if self.est_page_rows > 0 {
                self.est_page_rows
            } else {
                vh // a portrait page fit-page is about a screen tall
            };
            let rows = ((base as f32 * scale).round() as u16).max(1);
            (((slot_w as f32 * scale).round() as u16).max(1), rows)
        }
    }

    /// Zoom the continuous-paged stack in one step (bounded). Larger pages, and past
    /// fit-width a single page enlarges beyond the viewport with a horizontal pan.
    pub fn cont_zoom_in(&mut self) {
        self.cont_scale = (self.cont_scale * CONT_ZOOM_STEP).min(CONT_ZOOM_MAX);
    }

    /// Zoom the continuous-paged stack out one step (bounded), shrinking + centring
    /// the pages so more of the book is visible at once. Snaps to exactly 1.0 near
    /// fit-width.
    pub fn cont_zoom_out(&mut self) {
        self.cont_scale = (self.cont_scale / CONT_ZOOM_STEP).max(CONT_ZOOM_MIN);
        if (self.cont_scale - 1.0).abs() < 1e-3 {
            self.cont_scale = 1.0;
        }
    }

    /// Reset the continuous-paged zoom to fit-width and clear any horizontal pan.
    pub fn cont_zoom_reset(&mut self) {
        self.cont_scale = 1.0;
        self.cont_pan_x = 0.0;
    }

    /// Whether a single continuous page is zoomed wider than the viewport, so `h`/`l`
    /// pan it horizontally (else they're no-ops).
    pub fn cont_pannable_x(&self) -> bool {
        !self.continuous_two_page() && self.continuous_scale() > 1.0
    }

    /// Pan a zoomed-in continuous page left / right by one step (a fraction of the
    /// overflow), clamped.
    pub fn cont_pan_left(&mut self) {
        self.cont_pan_x = (self.cont_pan_x - CONT_PAN_STEP).max(0.0);
    }
    pub fn cont_pan_right(&mut self) {
        self.cont_pan_x = (self.cont_pan_x + CONT_PAN_STEP).min(1.0);
    }

    /// A short zoom label for the status bar, or `None` at plain fit-width.
    pub fn cont_zoom_label(&self) -> Option<String> {
        if (self.cont_scale - 1.0).abs() < 1e-3 {
            None
        } else {
            Some(format!("{:.0}%", self.cont_scale * 100.0))
        }
    }

    /// The render buffer for continuous mode: the anchor section's lines from
    /// `scroll` onward, then following sections' wrapped lines, until at least
    /// `height` lines are gathered (or the book ends). Owned so the view can render
    /// it directly. Following sections are wrapped once and cached.
    pub fn continuous_lines(&mut self, height: usize) -> Vec<DisplayLine> {
        let scroll = self.scroll.min(self.lines.len());
        let mut out: Vec<DisplayLine> = self.lines[scroll..].to_vec();
        self.cont_spans.clear();
        // Repopulated per shown section by `following_lines` (below), so stale
        // sections no longer on screen drop out of the draw set.
        self.images.following.clear();
        let count = self.doc.section_count();
        let mut s = self.section + 1;
        // One extra line so a partial last row still has content beneath it.
        while out.len() <= height && s < count {
            self.cont_spans.push((s, out.len()));
            let lines = self.following_lines(s);
            out.extend(lines);
            s += 1;
        }
        out
    }

    /// Continuous mode: the *following* sections' images to draw, as `(buffer row,
    /// cache key)`. The buffer row is the offset from the top visible line (anchor
    /// `lines[scroll]`), i.e. the screen row the image reserved — so the view can
    /// place it exactly where the reflow left the blank rows. The anchor section's
    /// images are drawn separately from `lines`.
    pub fn continuous_following_images(&self) -> Vec<(usize, crate::media::ImgKey)> {
        let mut out = Vec::new();
        for &(section, off) in &self.cont_spans {
            let (Some(lines), Some(info)) = (
                self.cont_cache.get(&section),
                self.images.following.get(&section),
            ) else {
                continue;
            };
            let mut i = 0;
            while i < lines.len() {
                let LineKind::Image(idx) = lines[i].kind else {
                    i += 1;
                    continue;
                };
                let start = i;
                while i < lines.len() && lines[i].kind == LineKind::Image(idx) {
                    i += 1;
                }
                if let Some((key, _)) = info.get(idx) {
                    out.push((off + start, *key));
                }
            }
        }
        out
    }

    /// Wrapped lines of following section `s` for the continuous buffer, reserving
    /// that section's own image rows (its stable decode estimate, computed on demand
    /// so a newly-shown section is sized right the first frame — no lag, no shift).
    /// Cached; invalidated wholesale when the wrap inputs (`cont_key`) change.
    fn following_lines(&mut self, s: usize) -> Vec<DisplayLine> {
        if self.cont_key != self.wrapped {
            self.cont_cache.clear();
            self.cont_key = self.wrapped.clone();
        }
        // Size this section's figures now (stable estimate) so the view can draw
        // them — even on a wrap-cache hit, since `following` was cleared this frame.
        if let Some(geom) = self.images.geom {
            let (fw, fh) = self.images.fs;
            let info = self.section_image_info(s, geom, fw, fh);
            self.images.following.insert(s, info);
        }
        if let Some(v) = self.cont_cache.get(&s) {
            return v.clone();
        }
        let blocks = self.fetch_blocks(s);
        let rows: Vec<u16> = self
            .images
            .following
            .get(&s)
            .map(|info| info.iter().map(|(_, r)| *r).collect())
            .unwrap_or_default();
        let lines = self.wrap_at_with_rows(&blocks, self.last_measure.max(1), &rows);
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
