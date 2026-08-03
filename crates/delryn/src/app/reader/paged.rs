//! Paged-image (PDF) navigation and single-page interaction.
//!
//! The reflowable path treats a document as wrapped lines; a paged-image document
//! is instead a sequence of full-page rasters, one per section. This module drives
//! that view: which page is current, spread pairing / cover offset, whole-page
//! turns (throttled to the drawn frame), the readiness / placement queries the
//! view consults before transmitting to the direct-Kitty deck, and — for a single
//! zoomed page — the zoom / pan input handlers. The pixels themselves (crop,
//! viewport-matched crisp re-raster, theming) are resolved in `reader::crisp`; the
//! pure placement geometry lives in `reader::page_view`.

use super::*;

impl Reader {
    /// Total pages in the current section (for the page indicator). A two-page spread
    /// counts *spreads*, not columns — the indicator should agree with what one turn
    /// advances, or a 10-spread chapter reads as 20 pages that take 10 presses.
    pub fn page_count(&self) -> usize {
        self.lines.len().div_ceil(self.reading_step()).max(1)
    }

    /// 1-based page number of the current position within the section.
    pub fn current_page(&self) -> usize {
        self.scroll / self.reading_step() + 1
    }

    /// Whether the document renders each section as a full-page image (PDF), so
    /// two-page mode shows a facing-page spread rather than two text columns.
    pub fn is_paged_image(&self) -> bool {
        self.doc.paged_image()
    }

    /// Number of sections (= pages, for a paged-image document).
    pub fn section_count(&self) -> usize {
        self.doc.section_count()
    }

    /// The page bytes the direct-Kitty [`PageDeck`] should transmit for `section`
    /// under the active policy: the original raster in Faithful mode, otherwise the
    /// themed PNG once it's built (`None` until then, so the deck holds the previous
    /// page rather than flashing an unthemed one). Serves the crisp raster once the
    /// view chose it for this section this frame (see [`Reader::resolve_page_width`]),
    /// else the base raster — matching the crop the view computed. See
    /// [`Reader::sync_pages`].
    pub fn page_png(&self, section: usize) -> Option<Vec<u8>> {
        let width = self.effective_width(section);
        if self.pages.policy.mode == media::ImageMode::Faithful {
            return self.raw_raster_at(section, width);
        }
        self.pages
            .themed
            .peek(&(section, width, self.pages.policy))
            .map(|b| b.as_ref().clone())
    }

    /// The raster width the view chose to display `section` at this frame — a crisp
    /// width once its raster + theming are ready, else the base width (also the
    /// default when the section wasn't placed this frame).
    pub fn effective_width(&self, section: usize) -> u32 {
        self.crisp
            .effective
            .get(&section)
            .copied()
            .unwrap_or(BASE_RASTER_WIDTH)
    }

    /// The raw (un-themed) PNG for `section` at `width`: the base raster from the
    /// section cache at the base width, otherwise a cached crisp raster.
    pub(super) fn raw_raster_at(&self, section: usize, width: u32) -> Option<Vec<u8>> {
        if width == BASE_RASTER_WIDTH {
            self.raster_png(section)
        } else {
            self.crisp
                .rasters
                .peek(&(section, width))
                .map(|b| b.as_ref().clone())
        }
    }

    /// The theme/image policy the visible page(s) are themed and shown under (set
    /// each frame by [`Reader::sync_pages`]). The [`PageDeck`] keys its transmit on
    /// this so a theme/mode change re-sends the re-themed pages.
    pub fn page_policy(&self) -> media::RenderPolicy {
        self.pages.policy
    }

    /// The raw rasterized PNG bytes of `section`'s page from the section cache —
    /// the un-themed source the page themer adapts (and what Faithful mode shows).
    pub(super) fn raster_png(&self, section: usize) -> Option<Vec<u8>> {
        self.sections
            .sections
            .get(&section)?
            .iter()
            .find_map(|b| match b {
                Block::Image { data, .. } if !data.is_empty() => Some(data.clone()),
                _ => None,
            })
    }

    /// The sections that should be on screen now — matching exactly what the view
    /// places (so the deck keep-alive doesn't wait on or spin over the wrong
    /// pages): the continuous-paged vertical stack (anchor onward), the spread's
    /// pages (cover-offset aware) in `spread` mode, else the current page alone.
    pub fn visible_sections(&self, spread: bool) -> Vec<usize> {
        if self.continuous_paged_active() {
            self.visible_stack.clone()
        } else if spread {
            self.spread_pages()
        } else {
            vec![self.section]
        }
    }

    /// Whether `section`'s page is rasterized (the raw PNG is cached) — the
    /// expensive PDFium step, on top of which theming runs. Doesn't clone.
    pub(super) fn raster_ready(&self, section: usize) -> bool {
        self.sections.sections.get(&section).is_some_and(|bs| {
            bs.iter()
                .any(|b| matches!(b, Block::Image { data, .. } if !data.is_empty()))
        })
    }

    /// Whether `section`'s page is ready to *place* under the active policy: the
    /// raster in Faithful mode, otherwise the themed PNG. Gates the [`PageDeck`]
    /// spread swap so a turn never shows a half-themed page.
    pub fn page_ready(&self, section: usize) -> bool {
        if self.pages.policy.mode == media::ImageMode::Faithful {
            self.raster_ready(section)
        } else {
            // Gate on the *base* raster's theming only — the crisp raster is an
            // enhancement that pops in a frame later, never a turn blocker.
            self.pages
                .themed
                .contains(&(section, BASE_RASTER_WIDTH, self.pages.policy))
        }
    }

    /// Whether `section` resolved but can't be shown (the loader returned it with
    /// no image — a rasterize failure). Keyed on the *raster*, not theming, since a
    /// raster failure is policy-independent. The flip throttle treats it as "done"
    /// so a broken page can't soft-lock navigation.
    pub fn page_unrenderable(&self, section: usize) -> bool {
        self.sections.sections.contains_key(&section) && !self.raster_ready(section)
    }

    /// Whether any visible page is still being prepared — rasterized, or themed on
    /// top of a ready raster — so the render loop keeps spinning until it lands. A
    /// page whose raster *failed* (unrenderable) is excluded so it can't spin
    /// forever.
    pub fn pages_loading(&self, spread: bool) -> bool {
        self.visible_sections(spread)
            .iter()
            .any(|&s| !self.page_ready(s) && !self.page_unrenderable(s))
    }

    /// The visible pages the deck should place. A single page / spread swaps
    /// atomically — all ready or nothing (so a spread never flickers half). The
    /// continuous stack instead shows its **ready subset**: with several pages
    /// sharing the viewport, holding everything until the far edge's next page
    /// themes would stall scrolling, so a not-yet-ready band just shows the themed
    /// background for a frame. It reports exactly the sections the view emitted this
    /// frame (`pdf_targets`), so the loop's "deck caught up" check settles cleanly
    /// even when the anchor has scrolled into the inter-page gap and dropped out of
    /// the visible bands.
    pub fn placeable_sections(&self, spread: bool) -> Vec<usize> {
        if self.continuous_paged_active() {
            return self.pdf_targets.iter().map(|t| t.section).collect();
        }
        let v = self.visible_sections(spread);
        if v.iter().all(|&s| self.page_ready(s)) {
            v
        } else {
            Vec::new()
        }
    }

    /// Mirror the terminal cell pixel size from the picker (called each frame in
    /// continuous-paged mode) so the scroll math can size pages off the view.
    pub fn set_cell_px(&mut self, cell: (u16, u16)) {
        self.cell_px = cell;
    }

    /// Clear the continuous-paged stack (no image protocol available, or leaving
    /// the mode), so the deck tears its pages down.
    pub fn clear_page_stack(&mut self) {
        self.pdf_targets.clear();
        self.visible_stack.clear();
    }

    /// Assemble the continuous-paged vertical stack for `body` this frame: walk the
    /// anchor band downward (one page per band in Center, a facing pair in TwoPage),
    /// resolving each tile's zoom/centre/pan horizontally and its height, until the
    /// viewport is filled; record the covered sections (for the loader / readiness
    /// checks) and emit the [`PageTarget`]s for the pages whose pixels are ready.
    /// Still-loading pages are left as gaps this frame and picked up once they land.
    /// The pure vertical slicing is [`page_stack::stack_targets`].
    pub fn capture_page_stack(&mut self, body: ratatui::layout::Rect) {
        let n = self.doc.section_count();
        let vh = body.height as i64;
        let two_page = self.continuous_two_page();
        let mut bands = Vec::new();
        let mut visible = Vec::new();
        let mut cursor: i64 = -(self.scroll as i64);
        let mut anchor = if two_page {
            self.spread_left(self.section)
        } else {
            self.section
        };
        while anchor < n && cursor < vh {
            // The sections this band covers (a single page, or a spread's pair) —
            // recorded + requested whether or not they've rasterized yet, so a
            // still-loading page still drives the loader and shows once it lands.
            let sections = if two_page {
                self.spread_at(self.spread_left(anchor))
            } else {
                vec![anchor]
            };
            for &s in &sections {
                visible.push(s);
                self.request_page(s);
            }
            // Band height from the estimate-aware metrics (stable even while a page
            // in the band is still loading), so the stack doesn't reflow when it lands.
            let rows = self.band_rows_of(anchor);
            let tiles = self.build_tiles(body.width, two_page, &sections);
            bands.push(page_stack::StackBand { tiles, rows });
            cursor += rows.max(1) as i64 + page_stack::STACK_GAP as i64;
            match self.next_band_anchor(anchor) {
                Some(next) => anchor = next,
                None => break,
            }
        }
        self.visible_stack = visible;
        let targets = page_stack::stack_targets(body, self.scroll, &bands);
        // Only place pages whose themed bytes are ready; the rest fill in next frame.
        self.pdf_targets = targets
            .into_iter()
            .filter(|t| self.page_ready(t.section))
            .collect();
    }

    /// Build the ready tiles of a band: a single fit-page page centred in the padded
    /// content region (Center), or a facing pair fit-page in two columns (TwoPage),
    /// each tile's horizontal placement + height resolved at the current zoom.
    /// Unloaded pages are omitted (their slot stays blank until they rasterize).
    fn build_tiles(
        &mut self,
        viewport_cols: u16,
        two_page: bool,
        sections: &[usize],
    ) -> Vec<page_stack::StackTile> {
        let pan_x = self.continuous_pan_x();
        let mut tiles = Vec::new();
        if !two_page || sections.len() == 1 {
            // Single page (Center, or a lone cover / trailing odd page).
            let (slot_x, slot_w) = self.continuous_single_slot();
            if let Some(tile) = self.build_tile(sections[0], slot_x, slot_w, viewport_cols, pan_x) {
                tiles.push(tile);
            }
            return tiles;
        }
        // A facing pair. LTR: earlier page left, later page right. Manga (RTL):
        // swap sides so the earlier page sits on the right and the spread reads
        // right-to-left (the vertical scroll order is unchanged).
        let (left_x, col_w, right_x) = self.continuous_column_slot();
        for (i, &section) in sections.iter().enumerate() {
            let in_left_col = if self.rtl { i == 1 } else { i == 0 };
            let slot_x = if in_left_col { left_x } else { right_x };
            if let Some(tile) = self.build_tile(section, slot_x, col_w, viewport_cols, pan_x) {
                tiles.push(tile);
            }
        }
        tiles
    }

    /// Ask the background loader to rasterize `section` if it isn't cached or already
    /// in flight — so every visible band drives the loader, not just the prefetch
    /// window (a tall/zoomed-out stack can show more pages than the ±neighbour
    /// prefetch reaches).
    fn request_page(&mut self, section: usize) {
        if section >= self.doc.section_count() || self.sections.sections.contains_key(&section) {
            return;
        }
        if self.sections.requested.insert(section) {
            let _ = self.sections.req_tx.send(section);
        }
    }

    /// Resolve a single page tile for the stack: its content box, its fit-page
    /// display size at the current zoom, and its horizontal placement (centred in the
    /// slot, or pan-cropped when zoomed past the viewport). `None` if the page hasn't
    /// rasterized yet (its slot stays blank until it lands — it's still recorded as
    /// visible for loading).
    fn build_tile(
        &mut self,
        section: usize,
        slot_x: u16,
        slot_w: u16,
        viewport_cols: u16,
        pan_x: f32,
    ) -> Option<page_stack::StackTile> {
        let content = self.page_content_of(section)?;
        let (cx, _, cw, _) = content;
        let (disp_w, rows) = self.tile_metrics(section, slot_w);
        let (x, w, src_x, src_w) =
            page_stack::place_tile_h(slot_x, slot_w, disp_w, viewport_cols, cx, cw, pan_x);
        Some(page_stack::StackTile {
            section,
            content,
            x,
            w,
            src_x,
            src_w,
            rows,
        })
    }

    /// Collect any pages the background loader and the page themer have finished
    /// into their caches. Driven each frame in PDF mode (the direct path skips
    /// `sync_images`, which is what otherwise drains the loader).
    pub fn poll_loader(&mut self) {
        self.drain_loader();
        self.drain_page_themer();
        self.drain_crisp();
    }

    /// Snap the position to the start of its page (so paged mode shows a clean
    /// page boundary after toggling in or resizing).
    pub fn snap_to_page(&mut self) {
        // A spread's page unit is the *pair* of columns, not one of them — snapping to a
        // column boundary would leave the right column's text due to slide into the left.
        let page = self.reading_step();
        self.scroll = self.scroll / page * page;
        self.scroll_pending = 0;
    }

    /// The left page of the two-page tile containing `section`. Without a cover
    /// offset, tiles are (0,1),(2,3)…; with one, page 0 is alone, then (1,2),
    /// (3,4)… so the left page is odd.
    pub(super) fn spread_left(&self, section: usize) -> usize {
        if !self.cover_offset {
            section - section % 2
        } else if section == 0 {
            0
        } else if section % 2 == 1 {
            section
        } else {
            section - 1
        }
    }

    /// The page(s) of the spread whose left page is `left`: one for a lone page (the
    /// cover under a cover offset, or a trailing odd page), else the facing pair.
    pub(super) fn spread_at(&self, left: usize) -> Vec<usize> {
        if self.cover_offset && left == 0 {
            return vec![0];
        }
        let mut v = vec![left];
        if left + 1 < self.doc.section_count() {
            v.push(left + 1);
        }
        v
    }

    /// How many pages the spread containing `section` steps over — the distance to
    /// the next spread's left page (1 for a lone cover / trailing page, else 2).
    pub(super) fn spread_width(&self, section: usize) -> usize {
        self.spread_at(self.spread_left(section)).len()
    }

    /// The page(s) of the current two-page spread.
    pub fn spread_pages(&self) -> Vec<usize> {
        self.spread_at(self.spread_left(self.section))
    }

    /// The section a forward flip lands on — the next tile's left in a spread
    /// (cover-offset aware), else the next page.
    fn next_page_section(&self) -> usize {
        if !self.spread {
            return self.section + 1;
        }
        let left = self.spread_left(self.section);
        let width = if self.cover_offset && left == 0 { 1 } else { 2 };
        left + width
    }

    /// The section a backward flip lands on — the previous tile's left.
    fn prev_page_section(&self) -> usize {
        if !self.spread {
            return self.section.saturating_sub(1);
        }
        let left = self.spread_left(self.section);
        if left <= 1 {
            0 // back to the cover (offset) or the first leaf
        } else {
            left - 2
        }
    }

    /// Flip to the next page (snapped to a page boundary), flowing into the next
    /// chapter at the bottom edge. Used in paged mode.
    pub fn page_forward(&mut self) {
        let page = self.page_lines.max(1);
        if self.scroll < self.max_scroll() {
            self.scroll = (self.scroll / page + 1) * page;
            self.clamp_scroll();
        } else if !self.chapter_lock {
            // Advance a leaf, clamped to the last page (so a spread whose facing
            // page is the last one still steps onto it).
            let last = self.doc.section_count().saturating_sub(1);
            let target = self.next_page_section().min(last);
            if target > self.section {
                self.load(target);
            }
        }
        self.scroll_pending = 0;
        self.repaint_after_page_flip();
    }

    /// Flip to the previous page boundary, flowing into the previous chapter's
    /// last page at the top edge. Used in paged mode.
    pub fn page_backward(&mut self) {
        let page = self.page_lines.max(1);
        if self.scroll > 0 {
            // Previous boundary, or this page's start when mid-page.
            self.scroll = self.scroll.saturating_sub(1) / page * page;
        } else if !self.chapter_lock && self.section > 0 {
            self.load(self.prev_page_section());
            self.scroll = usize::MAX;
            self.clamp_scroll();
            self.snap_to_page();
        }
        self.scroll_pending = 0;
        self.repaint_after_page_flip();
    }

    /// A reflowable page-snap flip jumps the scroll a whole page, so every inline equation
    /// moves at once. Kitty images composite above the cell grid and don't survive the
    /// cell-diff across a jump that big (the previous page's rasters would linger as residue),
    /// so force a full repaint — as a code fold does. A paged *image* (PDF) manages its pages
    /// through the [`PageDeck`](crate::app::page_deck), which handles this itself, so skip it.
    fn repaint_after_page_flip(&mut self) {
        if !self.is_paged_image() {
            self.request_repaint();
        }
    }

    /// Flip `pages` pages at once (a count-prefixed `j`/`k`, e.g. `10j`). For a
    /// paged-image doc one page is one section, so it jumps the section directly
    /// (clamped) — bypassing the per-frame flip throttle, which is only meant to
    /// pace a *held* key. Reflowable page mode steps page by page.
    pub fn page_jump(&mut self, pages: isize) {
        if pages == 0 {
            return;
        }
        if self.is_paged_image() {
            let max = self.doc.section_count().saturating_sub(1) as isize;
            let target = (self.section as isize + pages).clamp(0, max) as usize;
            self.load(target);
        } else {
            for _ in 0..pages.unsigned_abs() {
                if pages > 0 {
                    self.page_forward();
                } else {
                    self.page_backward();
                }
            }
        }
    }

    /// Store the pan room + step the view computed for this frame's page, so
    /// navigation can pan the zoomed page while there's room (and flip at the
    /// edge). No-op geometry when the page is at fit-page (all room false).
    pub fn set_page_room(&mut self, room: PanRoom, step: (f32, f32)) {
        self.page_room = room;
        self.page_step = step;
    }

    /// Whether the current paged page is zoomed (so nav pans rather than flips).
    pub fn page_zoomed(&self) -> bool {
        self.page_view.is_zoomed()
    }

    pub fn zoom_in(&mut self) {
        self.page_view.zoom_in();
    }
    pub fn zoom_out(&mut self) {
        self.page_view.zoom_out();
    }
    pub fn zoom_reset(&mut self) {
        self.page_view.reset();
    }
    pub fn cycle_fit(&mut self) {
        self.page_view.cycle_fit();
    }

    /// Pan the zoomed page down/up by `n` steps if there's room; returns `false`
    /// when already at that vertical edge (the caller then flips the page).
    pub fn try_pan_down(&mut self, n: usize) -> bool {
        if self.page_room.down {
            self.page_view.pan_y = (self.page_view.pan_y + self.page_step.1 * n as f32).min(1.0);
            true
        } else {
            false
        }
    }
    pub fn try_pan_up(&mut self, n: usize) -> bool {
        if self.page_room.up {
            self.page_view.pan_y = (self.page_view.pan_y - self.page_step.1 * n as f32).max(0.0);
            true
        } else {
            false
        }
    }

    /// Pan the zoomed page horizontally by `n` steps (clamped; no-op at the edge).
    pub fn pan_left(&mut self, n: usize) {
        self.page_view.pan_x = (self.page_view.pan_x - self.page_step.0 * n as f32).max(0.0);
    }
    pub fn pan_right(&mut self, n: usize) {
        self.page_view.pan_x = (self.page_view.pan_x + self.page_step.0 * n as f32).min(1.0);
    }
    /// Whether the page currently has horizontal pan room (for the h/l keys).
    pub fn can_pan_horizontally(&self) -> bool {
        self.page_room.left || self.page_room.right
    }

    /// After flipping a page while zoomed, start the new page at the top (forward)
    /// or bottom (backward) so vertical panning reads continuously.
    pub fn reset_pan_to(&mut self, top: bool) {
        self.page_view.pan_y = if top { 0.0 } else { 1.0 };
    }
}
