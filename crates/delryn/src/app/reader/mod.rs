//! The reading view model: a paginated, image-aware, searchable view over one
//! open `Document`. Owns section decoding (background loader + LRU cache), line
//! wrapping, the TOC sidebar with scroll-spy, image protocol lifecycle,
//! navigation history, and in-book search. Pure view-model — no terminal I/O.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use anyhow::Result;

use crate::config::ViewMode;
use crate::document::{Block, Document, OutlineItem};
use crate::layout::{DisplayLine, LineKind, WrapOpts, wrap_blocks};
use crate::media;
use crate::theme;
use delryn_model::{Anchor, find_footnote};

use super::page_deck::PageTarget;
use super::{CACHE_CAP, Focus};

// Separable reader concerns; each contributes an `impl Reader` block and reaches
// the core's helpers (find_line, fetch_blocks, …) via the parent module.
mod anchors;
mod continuous;
mod crisp;
mod elements;
mod images;
pub(crate) mod math;
pub use images::ImageGeom;
mod page_stack;
mod page_view;
pub use page_view::{PageView, PanRoom, Viewport, place_page, raster_width_for_crispness};
mod paged;
mod pages;
mod raster;
mod search;
mod sidebar;
mod state;

use state::{
    ImageState, NavState, PageRasterState, PageThemeState, Pos, SearchState, SectionCache, WrapKey,
};

/// The nominal width component of the base raster's theme/display cache key — a
/// discriminator distinguishing the base raster from the larger viewport-matched
/// crisp rasters (which key on their own width). Placement always uses the
/// raster's *actual* decoded dimensions, so this need only be a stable label, not
/// the exact pixel width of every page. See [`crate::document::pdf::PAGE_RASTER_WIDTH`].
const BASE_RASTER_WIDTH: u32 = crate::document::pdf::PAGE_RASTER_WIDTH as u32;

/// A followable inline anchor located in the wrapped lines (reading order). The
/// link cursor steps through these; the view highlights the selected one.
pub struct AnchorHit {
    pub line: usize,
    /// Column range within the line, in display chars `[start, end)`.
    pub start: usize,
    pub end: usize,
    pub anchor: Anchor,
}

pub struct Reader {
    pub doc: Box<dyn Document>,
    pub outline: Vec<OutlineItem>,
    pub section: usize,
    pub blocks: Vec<Block>,
    /// Wrapped display lines of the current section, valid for `wrapped`.
    pub lines: Vec<DisplayLine>,
    /// syntect theme desired for code (set each render from the active theme).
    pub code_theme: String,
    /// Desired spacing (set each render from config).
    pub line_spacing: u8,
    pub paragraph_spacing: u8,
    /// Code rendering (set each render from config / panning).
    pub code_wrap: bool,
    pub code_hscroll: usize,
    /// Word-wrap table cells (set each render from config).
    pub table_wrap: bool,
    /// Full justification + converter-spacing tidy (set each render from config).
    pub justify: bool,
    pub tidy_spacing: bool,
    /// Keep scrolling within the current chapter (set each render from config).
    pub chapter_lock: bool,
    /// Paginated reading (set each render from config): vertical nav flips whole
    /// pages snapped to page boundaries instead of scrolling line by line.
    pub paged: bool,
    /// Continuous scroll across sections (set each render, the raw config flag):
    /// the anchor's tail and the following heads share the viewport so a boundary
    /// scrolls seamlessly. Reflow uses it single-column only; paged (PDF) uses it in
    /// both Center (one stack) and TwoPage (spread stack). Inert in page-mode /
    /// chapter-lock. See [`continuous_active`](Self::continuous_active) /
    /// [`continuous_paged_active`](Self::continuous_paged_active).
    pub continuous: bool,
    /// Active view mode (set each render) — lets the continuous checks tell
    /// single-column (Center) from spread (TwoPage) without the view pre-gating the
    /// `continuous` flag.
    pub view_mode: ViewMode,
    /// Inter-page gap in cells for a two-page layout (mirrored from `config.page_gap`
    /// each render) — the horizontal gutter between a continuous spread's pages.
    pub page_gap: u16,
    /// Left/right reading margin as a percent of the pane, mirrored from
    /// `config.side_padding` each render — the continuous-paged stack insets its
    /// pages by this so they don't touch the screen edges.
    pub side_padding: u16,
    /// Right-to-left (manga) reading, mirrored from `config.reading_direction` each
    /// render. A continuous two-page spread swaps its facing pages so the earlier
    /// page sits on the right; the vertical scroll order is unchanged.
    pub rtl: bool,
    /// Continuous-paged zoom: the page scale relative to fit-page (1.0 = the whole
    /// page fits the viewport, centred with side padding). < 1 shrinks the pages; > 1
    /// enlarges past the viewport (single-column: taller → scroll, wider → `h`/`l`
    /// pan). Set by `+`/`-`/`0`.
    cont_scale: f32,
    /// Continuous-paged horizontal pan ∈ [0, 1] when a single zoomed-in page is
    /// wider than the viewport (0 = left edge). Set by `h`/`l`.
    cont_pan_x: f32,
    /// A two-page paged-image spread is on screen (set each render): a page flip
    /// turns a whole leaf (2 sections) so consecutive spreads don't overlap.
    pub spread: bool,
    /// Show the first page alone in a spread (book cover), then pair (2,3),
    /// (4,5)… so facing pages line up as in a physical book (set each render).
    pub cover_offset: bool,
    /// PDF page placements for this frame (section + cell rect + optional source
    /// crop), captured by the view and consumed by the direct-Kitty [`PageDeck`].
    /// One entry single-page, two for a spread.
    pub pdf_targets: Vec<PageTarget>,
    /// Zoom / pan / fit for the current paged page (single-page view only).
    pub page_view: PageView,
    /// Pixel size `(w, h)` of one terminal cell, mirrored each frame from the image
    /// picker. Lets the continuous-paged scroll math size a page's display height
    /// off the render thread (the picker is only reachable from the view).
    cell_px: (u16, u16),
    /// Last computed **fit-page** display height (cells, at zoom 1) of a paged page,
    /// the canonical page height reused as the estimate for pages not yet rasterized
    /// so continuous-paged scroll math + layout stay stable (PDF pages are
    /// near-uniform). Scaled by the zoom for a tile. Self-corrects once the real
    /// raster lands.
    est_page_rows: u16,
    /// Sections in the current continuous-paged vertical stack (anchor onward),
    /// set each frame by the view. Drives the deck readiness / load checks
    /// ([`visible_sections`](Self::visible_sections)); empty outside that mode.
    visible_stack: Vec<usize>,
    /// Pan room remaining this frame (from the placement), so nav pans while
    /// there's room and flips the page at the edge. A render fact — set each
    /// frame by the view.
    page_room: PanRoom,
    /// Pan step per keypress (fraction of the pan range), from the placement.
    page_step: (f32, f32),
    /// Trim baked-in whitespace margins from paged (PDF) pages (mirrored from
    /// config each render), so the content fills the viewport.
    trim_margins: bool,
    /// PDF margin-trim crop, in percent per edge (mirrored from config each
    /// render). A *constant* crop applied to every page, so the displayed page
    /// width is identical across pages (see [`page_content_box`](Self::page_content_box)).
    trim_pct: u16,
    /// The inputs the current `lines` were wrapped against; a change re-wraps.
    wrapped: WrapKey,
    /// Continuous scroll: cached wrapped lines of the sections *following* the
    /// anchor, assembled into the render buffer so a chapter boundary scrolls
    /// seamlessly. Keyed by section; invalidated wholesale when `cont_key` (the
    /// wrap inputs) changes. Empty/unused outside continuous mode.
    cont_cache: HashMap<usize, Vec<DisplayLine>>,
    cont_key: WrapKey,
    /// Continuous scroll: `(section, buffer-row offset)` of each following section
    /// joined into the render buffer this frame, so the view can place that
    /// section's images at the right rows. Anchor images are placed from `lines`.
    cont_spans: Vec<(usize, usize)>,
    /// Signature of the following sections' reserved image rows the `cont_cache`
    /// was wrapped under; when a build refines a following image's height this
    /// changes and the cache is dropped so those sections re-wrap.
    cont_img_sig: u64,
    /// Inline-image lifecycle (built protocols, row estimates, in-flight builds).
    images: ImageState,
    /// Paged-image (PDF) page theming (themer + themed-PNG cache + active policy).
    pages: PageThemeState,
    /// Viewport-matched crisp re-raster (worker + raw crisp-PNG cache + the
    /// per-section display width chosen this frame). Inert for reflowable docs.
    crisp: PageRasterState,
    /// In-book navigation (heading/anchor/bookmark indexes, link cursor, history).
    nav: NavState,
    /// In-book search (prompt, history, matcher, matches, cursor).
    pub search: SearchState,
    /// Decoded section blocks + the background loader (cache, channels, worker).
    sections: SectionCache,
    /// Terminal image ids evicted from the cache, to be deleted from the
    /// terminal by the main loop.
    pending_deletes: Vec<u32>,
    /// Text queued to be copied to the system clipboard by the main loop.
    pending_clipboard: Option<String>,
    /// An external link the user activated, awaiting the app's open-in-browser
    /// confirmation.
    pending_open: Option<String>,
    /// A transient status-bar message (e.g. "copied"), cleared on next key.
    pub flash: Option<String>,
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
    /// Height of one column in lines, refreshed each draw. Locates content within
    /// a single column (image centring, visible-code detection).
    pub viewport_lines: usize,
    /// Lines per page for all scroll / paging math (max-scroll, page snap, page
    /// count). One column's height — in a two-page spread, paging advances one
    /// column at a time. Currently equal to `viewport_lines`.
    pub page_lines: usize,
    /// Wrap width used by the last render; used to locate jump targets.
    pub last_measure: usize,
    /// Region an open overlay/popup covers this frame, if any. An inline image
    /// whose *left edge* falls under it is skipped: the kitty protocol keeps each
    /// row's placeholder in its first cell, so a clobbered left edge kills the
    /// image and leaves its right side (Skip cells) as a black box. Images whose
    /// left edge is clear render fine — the opaque popup just covers their edge.
    pub overlay_occlude: Option<ratatui::layout::Rect>,
    /// A saved within-section fraction to restore on the next draw (resume).
    pub pending_frac: Option<f32>,
    /// A figure to scroll to once the section is wrapped with image rows (a
    /// jump from the image viewer; resolved one-shot on the next draw).
    pending_image: Option<usize>,
    /// Collapsed parent rows (outline indices) in the sidebar tree.
    collapsed: HashSet<usize>,
}

impl Reader {
    pub fn new(mut doc: Box<dyn Document>) -> Result<Self> {
        let outline = doc.outline().to_vec();

        // Open at the body-matter start (skipping front matter) when the book
        // declares it; saved progress, if any, overrides this afterwards.
        let start = doc
            .start_section()
            .min(doc.section_count().saturating_sub(1));

        // Background loader: a worker thread that decodes sections on request,
        // owned by the section cache (see `SectionCache::new`). Seed the cache with
        // the start section's blocks, decoded inline so the first frame can wrap.
        let mut sections = SectionCache::new(doc.loader(), start);
        let mut first = doc.load_section(start).unwrap_or_default().blocks;
        math::convert_math_blocks(&mut first);
        sections.sections.insert(start, first.clone());

        // Paged (PDF) documents get an off-thread rasterizer for the viewport-
        // matched crisp path; reflowable ones return `None` (nothing to re-render).
        let crisp = PageRasterState {
            worker: doc.page_rasterizer().map(raster::PageRasterWorker::new),
            ..PageRasterState::default()
        };

        let mut reader = Self {
            doc,
            outline,
            section: start,
            blocks: first,
            lines: Vec::new(),
            code_theme: theme::default_theme().syntect.to_string(),
            line_spacing: 0,
            paragraph_spacing: 1,
            code_wrap: true,
            code_hscroll: 0,
            table_wrap: true,
            justify: false,
            tidy_spacing: true,
            chapter_lock: false,
            paged: false,
            continuous: false,
            view_mode: ViewMode::Center,
            page_gap: 0,
            side_padding: 0,
            rtl: false,
            cont_scale: 1.0,
            cont_pan_x: 0.0,
            spread: false,
            cover_offset: false,
            pdf_targets: Vec::new(),
            page_view: PageView::default(),
            cell_px: (10, 20),
            est_page_rows: 0,
            visible_stack: Vec::new(),
            page_room: PanRoom::default(),
            page_step: (0.0, 0.0),
            trim_margins: true,
            trim_pct: 6,
            wrapped: WrapKey::invalid(),
            cont_cache: HashMap::new(),
            cont_key: WrapKey::invalid(),
            cont_spans: Vec::new(),
            cont_img_sig: 0,
            images: ImageState::default(),
            pages: PageThemeState::default(),
            crisp,
            nav: NavState::default(),
            search: SearchState::default(),
            sections,
            pending_deletes: Vec::new(),
            pending_clipboard: None,
            pending_open: None,
            flash: None,
            scroll: 0,
            scroll_pending: 0,
            focus: Focus::Content,
            sidebar_sel: 0,
            sidebar_offset: 0,
            sidebar_h: 1,
            viewport_lines: 1,
            page_lines: 1,
            last_measure: 72,
            pending_frac: None,
            pending_image: None,
            overlay_occlude: None,
            collapsed: HashSet::new(),
        };
        reader.prefetch_neighbors();
        Ok(reader)
    }

    /// Collect any sections the loader has finished into the cache. A `None`
    /// payload is a stale request the loader dropped; clear it from `requested`
    /// (don't cache) so it can be re-requested if the reader returns to it.
    fn drain_loader(&mut self) {
        while let Ok((index, blocks)) = self.sections.res_rx.try_recv() {
            self.sections.requested.remove(&index);
            if let Some(blocks) = blocks {
                self.sections.sections.insert(index, blocks);
            }
        }
    }

    /// Blocks for a section: cache first, else decode.
    ///
    /// For paged-image documents (PDF) a cache miss is fetched *asynchronously* —
    /// rasterizing a page costs tens of ms and doing it on the main thread would
    /// stall fast `j`/`k` turns. The empty result lets the [`PageDeck`] hold the
    /// previous page up until this one lands. Reflowable formats decode inline so
    /// the text is ready to wrap this frame.
    fn fetch_blocks(&mut self, section: usize) -> Vec<Block> {
        self.drain_loader();
        if let Some(blocks) = self.sections.sections.get(&section) {
            return blocks.clone();
        }
        if self.is_paged_image() {
            if self.sections.requested.insert(section) {
                let _ = self.sections.req_tx.send(section);
            }
            return Vec::new();
        }
        let mut blocks = self
            .doc
            .load_section(section)
            .map(|s| s.blocks)
            .unwrap_or_default();
        math::convert_math_blocks(&mut blocks);
        self.sections.sections.insert(section, blocks.clone());
        blocks
    }

    /// The section image index nearest the current viewport, so the image viewer
    /// can open on the figure you're looking at rather than the chapter's first.
    pub fn current_image_index(&self) -> Option<usize> {
        let center = self.scroll + self.viewport_lines / 2;
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l.kind {
                LineKind::Image(idx) => Some((i, idx)),
                _ => None,
            })
            .min_by_key(|(i, _)| (*i as isize - center as isize).unsigned_abs())
            .map(|(_, idx)| idx)
    }

    /// Gather the renderable figures for the image viewer: the current chapter
    /// only, or every section in the book (decoding as needed) when `whole_book`.
    pub fn figures(&mut self, whole_book: bool) -> Vec<super::Figure> {
        let mut out = Vec::new();
        if whole_book {
            for s in 0..self.doc.section_count() {
                let blocks = self.fetch_blocks(s);
                super::image_view::collect_figures(&blocks, s, &mut out);
            }
        } else {
            super::image_view::collect_figures(&self.blocks, self.section, &mut out);
        }
        out
    }

    /// Ask the loader to pre-decode the adjacent sections, and bound the cache.
    /// A PDF reader pre-rasterizes several pages each side (forward first) so the
    /// direct-Kitty window can transmit them ahead and fast j/k turns are instant.
    fn prefetch_neighbors(&mut self) {
        self.drain_loader();
        let n = self.doc.section_count();
        // Continuous stacking can pull several pages into view at once and scrolls
        // through them fast, so it rasterizes a wider window than page-flipping.
        let ahead = if self.continuous_paged_active() {
            6
        } else if self.is_paged_image() {
            4
        } else {
            1
        };
        let fwd: Vec<usize> = (1..=ahead)
            .map(|d| self.section + d)
            .filter(|&s| s < n)
            .collect();
        let back: Vec<usize> = (1..=ahead)
            .filter(|&d| self.section >= d)
            .map(|d| self.section - d)
            .collect();
        // Prefetch the direction of travel first, so reverse paging (k) isn't
        // starved waiting behind the forward pages.
        let mut targets = Vec::new();
        if self.nav.nav_back {
            targets.extend(back);
            targets.extend(fwd);
        } else {
            targets.extend(fwd);
            targets.extend(back);
        }
        for t in targets {
            if !self.sections.sections.contains_key(&t) && self.sections.requested.insert(t) {
                let _ = self.sections.req_tx.send(t);
            }
        }
        self.evict();
    }

    /// Drop cached sections farthest from the current one when over capacity.
    fn evict(&mut self) {
        while self.sections.sections.len() > CACHE_CAP {
            let current = self.section;
            match self
                .sections
                .sections
                .keys()
                .copied()
                .filter(|&k| k != current)
                .max_by_key(|&k| k.abs_diff(current))
            {
                Some(far) => {
                    self.sections.sections.remove(&far);
                }
                None => break,
            }
        }
    }

    /// Re-wrap the current section if any wrapping input changed.
    pub fn ensure_wrapped(&mut self, width: usize) {
        let key = WrapKey {
            width,
            theme: self.code_theme.clone(),
            line_spacing: self.line_spacing,
            para_spacing: self.paragraph_spacing,
            code_wrap: self.code_wrap,
            code_hscroll: self.code_hscroll,
            table_wrap: self.table_wrap,
            justify: self.justify,
            tidy: self.tidy_spacing,
            images_key: self.images.images_key,
            image_rows_sig: image_rows_sig(&self.images.rows_estimate),
        };
        if key != self.wrapped {
            self.lines = self.wrap_at(&self.blocks, width);
            self.wrapped = key;
            self.recompute_heading_lines();
            self.recompute_anchors();
            self.recompute_annotation_lines();
        }
    }

    /// Wrap `blocks` at `width` under the reader's current typography settings —
    /// the one place the [`WrapOpts`] are assembled, shared by the anchor section
    /// ([`ensure_wrapped`](Self::ensure_wrapped)) and the continuous-scroll buffer.
    fn wrap_at(&self, blocks: &[Block], width: usize) -> Vec<DisplayLine> {
        self.wrap_at_with_rows(blocks, width, &self.images.rows_estimate)
    }

    /// Wrap `blocks` reserving `image_rows` blank rows per image. The anchor uses
    /// its own `rows_estimate` (via [`wrap_at`]); a *following* continuous section
    /// passes its own rows so its figures reserve the right space (and align with
    /// where the view draws them).
    fn wrap_at_with_rows(
        &self,
        blocks: &[Block],
        width: usize,
        image_rows: &[u16],
    ) -> Vec<DisplayLine> {
        wrap_blocks(
            blocks,
            &WrapOpts {
                width,
                code_theme: &self.code_theme,
                line_spacing: self.line_spacing,
                para_spacing: self.paragraph_spacing,
                code_wrap: self.code_wrap,
                code_hscroll: self.code_hscroll,
                table_wrap: self.table_wrap,
                justify: self.justify,
                tidy_spacing: self.tidy_spacing,
            },
            image_rows,
        )
    }

    /// Set the open book's annotations as `(section, quote, is_note)`, splitting
    /// them into bookmarks and notes, then resolve the current section's into
    /// gutter lines. Called by the app on any change.
    pub fn set_annotations(&mut self, items: Vec<(usize, String, bool)>) {
        let (mut bookmarks, mut notes) = (Vec::new(), Vec::new());
        for (section, quote, is_note) in items {
            if is_note {
                notes.push((section, quote));
            } else {
                bookmarks.push((section, quote));
            }
        }
        self.nav.bookmarks = bookmarks;
        self.nav.notes = notes;
        self.recompute_annotation_lines();
    }

    /// Resolve this section's bookmark + note quotes to display lines (once per
    /// re-wrap), so the gutter can mark them cheaply.
    fn recompute_annotation_lines(&mut self) {
        let resolve = |marks: &[(usize, String)], section: usize, lines: &[DisplayLine]| {
            marks
                .iter()
                .filter(|(s, _)| *s == section)
                .filter_map(|(_, quote)| find_line(lines, quote))
                .collect::<std::collections::HashSet<usize>>()
        };
        self.nav.bookmark_lines = resolve(&self.nav.bookmarks, self.section, &self.lines);
        self.nav.note_lines = resolve(&self.nav.notes, self.section, &self.lines);
    }

    /// Whether a display line carries a bookmark (for the left-gutter marker).
    pub fn is_bookmark_line(&self, line: usize) -> bool {
        self.nav.bookmark_lines.contains(&line)
    }

    /// Whether a display line carries a note (drawn with a pen glyph in the gutter).
    pub fn is_note_line(&self, line: usize) -> bool {
        self.nav.note_lines.contains(&line)
    }

    /// Whether a bookmark already exists at this anchor (`section` + `quote`), so
    /// a repeat `m` at the same place doesn't drop a duplicate.
    pub fn has_bookmark(&self, section: usize, quote: &str) -> bool {
        self.nav
            .bookmarks
            .iter()
            .any(|(s, q)| *s == section && q == quote)
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
        self.nav.heading_lines = hl;
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// An external link the user just activated (to confirm + open in browser).
    pub fn take_pending_open(&mut self) -> Option<String> {
        self.pending_open.take()
    }

    /// Whether a smooth scroll is currently in progress (so heavy image
    /// transmits can be deferred until it settles).
    pub fn is_scrolling(&self) -> bool {
        self.scroll_pending != 0
    }

    pub fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.page_lines.max(1))
    }

    pub fn clamp_scroll(&mut self) {
        // Continuous-paged (PDF stacking) manages its own scroll bound in row units
        // against the stacked page heights, not the (empty) wrapped-line count.
        if self.continuous_paged_active() {
            return;
        }
        // Continuous mode mid-book: the anchor's offset intentionally runs past the
        // section's last page (the next section fills the tail), so don't clamp it —
        // only the final section has a hard bottom.
        if self.continuous_active() && self.section + 1 < self.doc.section_count() {
            return;
        }
        let m = self.max_scroll();
        if self.scroll > m {
            self.scroll = m;
        }
    }

    pub fn load(&mut self, section: usize) {
        if section >= self.doc.section_count() {
            return;
        }
        self.nav.nav_back = section < self.section;
        self.section = section;
        self.sections
            .loader_current
            .store(section, Ordering::Relaxed);
        self.blocks = self.fetch_blocks(section);
        self.scroll = 0;
        self.nav.anchor_sel = None; // a new section has a different anchor set
        self.wrapped.width = usize::MAX; // force a re-wrap on next draw
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

    /// Scroll down, flowing into the next chapter at the bottom edge. In continuous
    /// mode the anchor rolls seamlessly across the boundary (tail + next head share
    /// the viewport); otherwise the next chapter is loaded fresh at the top.
    pub fn scroll_down(&mut self, n: usize) {
        if self.continuous_paged_active() {
            self.continuous_paged_scroll_down(n);
            return;
        }
        if self.continuous_active() {
            self.continuous_scroll_down(n);
            return;
        }
        let max = self.max_scroll();
        if self.scroll < max {
            self.scroll = (self.scroll + n).min(max);
        } else if !self.chapter_lock && self.section + 1 < self.doc.section_count() {
            self.load(self.section + 1);
        }
    }

    /// Scroll up, flowing into the previous chapter at the top edge (unless
    /// chapter-locked). Continuous mode rolls the anchor back across the boundary.
    pub fn scroll_up(&mut self, n: usize) {
        if self.continuous_paged_active() {
            self.continuous_paged_scroll_up(n);
            return;
        }
        if self.continuous_active() {
            self.continuous_scroll_up(n);
            return;
        }
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

    /// Jump to a figure: load its section and scroll to the image's display line.
    /// The image rows aren't sized until the next render syncs images, so the
    /// scroll is deferred to `resolve_pending` (one-shot) and lands on the figure
    /// rather than the chapter top.
    pub fn jump_to_image(&mut self, section: usize, image_index: usize) {
        self.push_history();
        if section != self.section {
            self.load(section);
        } else {
            self.scroll = 0;
        }
        self.pending_image = Some(image_index);
        self.focus = Focus::Content;
    }

    fn push_history(&mut self) {
        self.nav.back_stack.push(Pos {
            section: self.section,
            scroll: self.scroll,
        });
        if self.nav.back_stack.len() > 200 {
            self.nav.back_stack.remove(0);
        }
        self.nav.fwd_stack.clear();
    }

    pub fn history_back(&mut self) {
        if let Some(pos) = self.nav.back_stack.pop() {
            self.nav.fwd_stack.push(Pos {
                section: self.section,
                scroll: self.scroll,
            });
            self.goto(pos);
        }
    }

    pub fn history_forward(&mut self) {
        if let Some(pos) = self.nav.fwd_stack.pop() {
            self.nav.back_stack.push(Pos {
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

    /// Scroll position within the current section as a fraction `[0, 1]`.
    pub fn within_frac(&self) -> f32 {
        if self.lines.is_empty() {
            0.0
        } else {
            self.scroll as f32 / self.lines.len() as f32
        }
    }

    /// Before a change that re-wraps the section (a view-mode switch, a reading
    /// preset, a width/spacing tweak), snapshot the reading position as a section
    /// fraction so [`resolve_pending`](Self::resolve_pending) restores the same
    /// spot once the text re-wraps at the new geometry — otherwise the raw
    /// `scroll` line offset points somewhere else in the differently-wrapped text.
    /// A no-op for paged docs (their position is the page index, unaffected by
    /// wrapping) and when there's nothing yet to anchor to.
    pub fn hold_reflow_position(&mut self) {
        if !self.is_paged_image() && !self.lines.is_empty() {
            self.pending_frac = Some(self.within_frac());
        }
    }

    /// Apply a pending resume fraction or figure jump once the section is wrapped.
    pub fn resolve_pending(&mut self) {
        if let Some(frac) = self.pending_frac.take() {
            let n = self.lines.len();
            self.scroll = ((frac * n as f32).round() as usize).min(n.saturating_sub(1));
        }
        // One-shot: scroll to the jumped-to figure's image line (now sized).
        if let Some(idx) = self.pending_image.take()
            && let Some(line) = self
                .lines
                .iter()
                .position(|l| l.kind == LineKind::Image(idx))
        {
            self.scroll = line;
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
/// Find the display line a cross-reference / citation locator points at: the
/// first line equal to it, else the first line containing its leading words.
/// Unlike [`find_line`] (tuned for short TOC headings), it never matches a tiny
/// early line as a substring of a long locator, and tolerates a locator that
/// wraps across lines by matching only its first few words.
/// A cheap order-sensitive signature (FNV-1a) of the per-image reserved row
/// counts, for the [`WrapKey`]. When a built image refines its estimated rows,
/// this changes and the section re-wraps so the reserved blanks match the drawn
/// image height (caption flush beneath — no gap, no overlap).
fn image_rows_sig(rows: &[u16]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &r in rows {
        h = (h ^ u64::from(r)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn find_target_line(lines: &[DisplayLine], locator: &str) -> Option<usize> {
    let n = loose_key(locator);
    if n.is_empty() {
        return None;
    }
    if let Some(i) = lines.iter().position(|l| loose_key(&l.text()) == n) {
        return Some(i);
    }
    let prefix: String = n.split(' ').take(6).collect::<Vec<_>>().join(" ");
    if prefix.len() < 3 {
        return None; // too generic to match reliably
    }
    lines.iter().position(|l| {
        let ll = loose_key(&l.text());
        !ll.is_empty() && ll.contains(&prefix)
    })
}

/// A short label for the link cursor's status flash, by anchor kind.
fn anchor_kind_label(a: &Anchor) -> &'static str {
    match a {
        Anchor::Footnote(_) => "footnote ref",
        Anchor::CrossRef(_) => "cross-ref",
        Anchor::Link(_) => "link",
        Anchor::Citation(_) => "citation",
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Document, Section, SectionLoader, TocEntry};
    use delryn_model::{Metadata, Span};

    #[test]
    fn image_rows_sig_detects_reservation_changes() {
        // A build refining one image's rows must re-key the wrap; the signature is
        // order-sensitive so a reordering re-wraps too, and stable otherwise.
        assert_ne!(image_rows_sig(&[10, 20]), image_rows_sig(&[10, 21]));
        assert_ne!(image_rows_sig(&[10, 20]), image_rows_sig(&[20, 10]));
        assert_eq!(image_rows_sig(&[10, 20]), image_rows_sig(&[10, 20]));
    }

    /// A minimal in-memory `Document` for reader tests: a list of sections, each
    /// a list of blocks. No TOC/outline/images.
    struct MockDoc {
        sections: Vec<Vec<Block>>,
        meta: Metadata,
        toc: Vec<TocEntry>,
        outline: Vec<OutlineItem>,
        paged: bool,
    }

    impl MockDoc {
        fn new(sections: Vec<Vec<Block>>) -> Self {
            MockDoc {
                sections,
                meta: Metadata::default(),
                toc: Vec::new(),
                outline: Vec::new(),
                paged: false,
            }
        }

        /// Mark this as a paged-image document (like PDF), for spread tests.
        fn paged(mut self) -> Self {
            self.paged = true;
            self
        }
    }

    struct MockLoader(Vec<Vec<Block>>);
    impl SectionLoader for MockLoader {
        fn load(&mut self, i: usize) -> Vec<Block> {
            self.0.get(i).cloned().unwrap_or_default()
        }
    }

    impl Document for MockDoc {
        fn metadata(&self) -> &Metadata {
            &self.meta
        }
        fn toc(&self) -> &[TocEntry] {
            &self.toc
        }
        fn outline(&self) -> &[OutlineItem] {
            &self.outline
        }
        fn loader(&self) -> Box<dyn SectionLoader> {
            Box::new(MockLoader(self.sections.clone()))
        }
        fn section_count(&self) -> usize {
            self.sections.len()
        }
        fn paged_image(&self) -> bool {
            self.paged
        }
        fn load_section(&mut self, index: usize) -> anyhow::Result<Section> {
            Ok(Section {
                index,
                blocks: self.sections.get(index).cloned().unwrap_or_default(),
            })
        }
    }

    fn para() -> Block {
        Block::Para {
            spans: vec![Span::plain("lorem ipsum dolor sit amet ".repeat(4))],
            indent: 0,
            quote: false,
            marker: None,
        }
    }
    fn code(s: &str) -> Block {
        Block::Code {
            lang: None,
            lines: vec![s.to_string()],
        }
    }

    fn reader_with(blocks: Vec<Block>) -> Reader {
        let mut r = Reader::new(Box::new(MockDoc::new(vec![blocks]))).unwrap();
        r.last_measure = 40;
        r.ensure_wrapped(40);
        r
    }

    /// A multi-section reflow reader with continuous scroll on (anchor = section 0,
    /// wrapped at width 40). Each section has identical multi-paragraph content, so
    /// its wrapped line count is stable across re-wraps.
    fn continuous_reader(sections: usize) -> Reader {
        let secs: Vec<Vec<Block>> = (0..sections)
            .map(|_| vec![para(), para(), para()])
            .collect();
        let mut r = Reader::new(Box::new(MockDoc::new(secs))).unwrap();
        r.continuous = true;
        r.last_measure = 40;
        r.ensure_wrapped(40);
        r
    }

    #[test]
    fn continuous_scroll_down_rolls_the_anchor_across_a_boundary() {
        let mut r = continuous_reader(3);
        assert!(r.continuous_active());
        let l0 = r.lines.len();
        assert!(l0 > 2, "a multi-paragraph section wraps to several lines");
        // Sit near the section end, then scroll past it: the anchor rolls to the
        // next section keeping the leftover offset.
        r.scroll = l0 - 1;
        r.scroll_down(3);
        assert_eq!(r.section, 1, "rolled into the next section");
        assert_eq!(r.scroll, 2, "kept the leftover offset past the boundary");
    }

    #[test]
    fn continuous_scroll_up_rolls_back_across_a_boundary() {
        let mut r = continuous_reader(3);
        let l0 = r.lines.len();
        r.load(1); // anchor at section 1, top
        assert_eq!((r.section, r.scroll), (1, 0));
        r.scroll_up(2);
        assert_eq!(r.section, 0, "rolled back to the previous section");
        assert_eq!(r.scroll, l0 - 2, "landed two lines up from its top");
    }

    #[test]
    fn continuous_scroll_stops_at_the_book_end() {
        let mut r = continuous_reader(2);
        r.load(1); // the last section
        r.ensure_wrapped(40);
        let max = r.max_scroll();
        r.scroll_down(9999);
        assert_eq!(r.section, 1, "there's no section past the last");
        assert_eq!(
            r.scroll, max,
            "clamped to the final page, no scrolling into the void"
        );
    }

    #[test]
    fn continuous_is_inert_when_chapter_locked_or_paged() {
        let mut r = continuous_reader(3);
        r.chapter_lock = true;
        assert!(!r.continuous_active(), "chapter lock overrides continuous");
        r.chapter_lock = false;
        r.paged = true;
        assert!(!r.continuous_active(), "page mode overrides continuous");
    }

    fn table() -> Block {
        Block::Table {
            header: Some(vec![vec![Span::plain("H")]]),
            rows: vec![vec![vec![Span::plain("r")]]],
        }
    }
    fn math() -> Block {
        Block::Math {
            unicode: r"\alpha".to_string(),
            latex: None,
        }
    }

    /// A small white page encoded as PNG, so the image pipeline can decode +
    /// build it in the spread test.
    fn page_png() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(20, 28, image::Rgb([255, 255, 255]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    /// A small image block, for paged-image (PDF) navigation tests.
    fn image_page() -> Vec<Block> {
        vec![Block::Image {
            src: String::new(),
            alt: String::new(),
            data: page_png(),
            caption: Vec::new(),
            math: false,
            width: delryn_model::ImageWidth::Full,
        }]
    }

    /// A PDF page load must not block: rasterizing on the main thread would stall
    /// fast `j`/`k` turns. So navigating returns immediately with empty blocks and
    /// merely *requests* the page; it lands later via the background loader, at
    /// which point it's ready to place.
    /// A render policy for the paged tests; the default Auto mode themes pages, so
    /// a page is placeable only once *themed* (raster → theme → place).
    fn paged_policy() -> media::RenderPolicy {
        media::RenderPolicy {
            tint: media::Ink {
                ink: [0, 0, 0],
                paper: [255, 255, 255],
            },
            mode: media::ImageMode::Auto,
        }
    }

    #[test]
    fn paged_load_is_async_and_lands_via_loader() {
        let doc = MockDoc::new((0..12).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();

        // Page 8 is outside the start's prefetch window, so it's a fresh miss:
        // the load returns at once without rasterizing it on the main thread.
        r.load(8);
        assert_eq!(r.section, 8);
        // The load returns without rasterizing on the main thread: the blocks are
        // empty (the page is merely requested). It may or may not have landed via
        // the background loader yet — that's checked below — so we don't assert on
        // the still-loading state here (it would race the instant mock loader).
        assert!(
            r.blocks.is_empty(),
            "load must not block rasterizing the page"
        );

        // The loader rasterizes the page and the themer adapts it, both in the
        // background; once drained (driven by `sync_pages` each frame) it's
        // placeable.
        let mut ready = false;
        for _ in 0..200 {
            r.poll_loader();
            r.sync_pages(paged_policy());
            if r.page_ready(8) {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready, "the requested page must land via the loader");
        assert!(!r.pages_loading(false));
        assert_eq!(r.placeable_sections(false), vec![8]);
    }

    /// A spread is placed atomically: until *both* pages are ready, nothing is
    /// placeable (so the deck holds the previous spread rather than flicker a
    /// half one).
    #[test]
    fn paged_spread_placeable_is_all_or_nothing() {
        let doc = MockDoc::new((0..12).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.load(8);

        // Drain until at least the current page (8) has landed and themed.
        for _ in 0..200 {
            r.poll_loader();
            r.sync_pages(paged_policy());
            if r.page_ready(8) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // In spread mode the facing page (9) is also required to place anything.
        if !r.page_ready(9) {
            assert!(
                r.placeable_sections(true).is_empty(),
                "a half-ready spread places nothing"
            );
        }
        for _ in 0..200 {
            r.poll_loader();
            r.sync_pages(paged_policy());
            if r.page_ready(9) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            r.placeable_sections(true),
            vec![8, 9],
            "both pages ready → the whole spread is placeable"
        );
    }

    /// The margin trim is a *constant* percent crop off each edge — the same
    /// fraction of every page and every raster size — so the displayed page width
    /// stays identical across pages (the whole point of the constant crop).
    #[test]
    fn content_box_is_a_constant_percent_crop() {
        let doc = MockDoc::new(vec![image_page()]).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.set_trim(true, 10); // 10% off each edge
        // 10% off each side → origin at 10%, size 80% of the raster — proportional
        // at any raster resolution (base or crisp), independent of the section.
        assert_eq!(r.page_content_box(0, (1000, 2000)), (100, 200, 800, 1600));
        assert_eq!(r.page_content_box(7, (2000, 4000)), (200, 400, 1600, 3200));
        // A different section / page yields the *same* fractional box → same width.
        assert_eq!(
            r.page_content_box(3, (1000, 2000)),
            r.page_content_box(99, (1000, 2000)),
        );
        // Trimming off (or 0%) → the whole raster.
        r.set_trim(false, 10);
        assert_eq!(r.page_content_box(0, (1000, 2000)), (0, 0, 1000, 2000));
        r.set_trim(true, 0);
        assert_eq!(r.page_content_box(0, (1000, 2000)), (0, 0, 1000, 2000));
    }

    /// Without a page rasterizer (any reflowable doc, or a mock) there's no crisp
    /// path: `resolve_page_width` keeps the base width even when a large placement
    /// wants more, and records it so `page_png` serves the base bytes.
    #[test]
    fn resolve_page_width_falls_back_to_base_without_a_worker() {
        let doc = MockDoc::new(vec![image_page()]).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        let base = (2000u32, 2800u32);
        let (w, dims) = r.resolve_page_width(0, base, 4000);
        assert_eq!(
            (w, dims),
            (BASE_RASTER_WIDTH, base),
            "no worker → base raster"
        );
        assert_eq!(r.effective_width(0), BASE_RASTER_WIDTH);
        assert!(
            !r.crisp_awaiting(),
            "nothing requested → the loop can settle"
        );
    }

    /// A continuous-paged (PDF stacking) reader: a paged doc with `continuous` on, no
    /// side padding, a fixed cell size, and a viewport (20 rows) taller than the
    /// page's fit-width height so fit-page keeps the full slot width — a clean 14-row
    /// band (content 20×28 at 20 cols, cell 10×20: 28·(20·10/20)/20 = 14) → slot 15
    /// with the 1-row gap. Margin trim off.
    fn continuous_paged_reader(pages: usize) -> Reader {
        let doc = MockDoc::new((0..pages).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.continuous = true;
        r.set_cell_px((10, 20));
        r.last_measure = 20;
        r.viewport_lines = 20;
        r.side_padding = 0;
        r.set_trim(false, 0);
        // Prime the anchor's height so still-loading pages estimate uniformly.
        assert_eq!(r.band_rows_of(0), 14);
        r
    }

    #[test]
    fn continuous_paged_activation_gates_on_mode() {
        let mut r = continuous_paged_reader(4);
        assert!(r.continuous_paged_active());
        assert!(!r.continuous_two_page(), "Center → single stack");
        // TwoPage is a valid continuous mode (a spread per band).
        r.view_mode = crate::config::ViewMode::TwoPage;
        assert!(r.continuous_paged_active());
        assert!(r.continuous_two_page(), "TwoPage → spread stack");
        r.view_mode = crate::config::ViewMode::Center;
        r.chapter_lock = true;
        assert!(!r.continuous_paged_active(), "chapter lock overrides");
        r.chapter_lock = false;
        r.paged = true;
        assert!(!r.continuous_paged_active(), "page-snap overrides");
        r.paged = false;
        r.continuous = false;
        assert!(!r.continuous_paged_active(), "flag off");
    }

    #[test]
    fn continuous_paged_scroll_down_rolls_the_anchor_across_a_slot() {
        let mut r = continuous_paged_reader(6);
        // Within the first page's slot: the anchor stays, the offset advances.
        r.scroll_down(5);
        assert_eq!((r.section, r.scroll), (0, 5));
        // Past the slot (15 = 14 rows + 1 gap): roll to the next page, keep leftover.
        r.scroll_down(15);
        assert_eq!((r.section, r.scroll), (1, 5), "rolled one page, kept 5");
        // A big jump rolls several pages (mid-book, so no end clamp).
        r.scroll_down(15 + 15);
        assert_eq!((r.section, r.scroll), (3, 5));
    }

    #[test]
    fn continuous_paged_scroll_up_crosses_back_into_the_previous_slot() {
        let mut r = continuous_paged_reader(4);
        r.load(2); // anchor page 2, top
        r.set_trim(false, 0);
        assert_eq!((r.section, r.scroll), (2, 0));
        r.scroll_up(1);
        // Lands just below the boundary: the previous page's slot minus one row.
        assert_eq!(
            (r.section, r.scroll),
            (1, 14),
            "into page 1 near its bottom"
        );
    }

    #[test]
    fn continuous_paged_clamps_at_both_ends() {
        let mut r = continuous_paged_reader(3);
        // At the start, scrolling up is a no-op.
        r.scroll_up(50);
        assert_eq!((r.section, r.scroll), (0, 0));
        // Scrolling far past the end clamps to the last page's bottom. Fit-page keeps
        // the 14-row page inside the 20-row viewport, so it's fully visible and the
        // deepest offset is 0 (nothing to scroll within the last page).
        r.scroll_down(9999);
        assert_eq!(r.section, 2, "no page past the last");
        assert_eq!(r.scroll, 0, "last fit-page page fully visible → floor 0");
    }

    #[test]
    fn continuous_two_page_rolls_by_spread() {
        let mut r = continuous_paged_reader(6);
        r.view_mode = crate::config::ViewMode::TwoPage;
        r.page_gap = 0;
        assert!(r.continuous_two_page());
        // Each page is half-width (10 cols) → 7 rows tall; the band slot is 8.
        assert_eq!(r.band_rows_of(0), 7);
        r.scroll_down(8);
        assert_eq!(
            (r.section, r.scroll),
            (2, 0),
            "rolled to the next spread (0,1)→(2,3)"
        );
        r.scroll_down(3);
        assert_eq!((r.section, r.scroll), (2, 3));
        r.scroll_up(4);
        assert_eq!(r.section, 0, "rolled back a whole spread");
    }

    #[test]
    fn continuous_single_page_fits_whole_and_pads() {
        let mut r = continuous_paged_reader(3);
        r.side_padding = 10; // 10% each side → pad 2, avail 16 of the 20-col pane
        assert_eq!(r.continuous_single_slot(), (2, 16));
        // A viewport shorter than the page makes fit-page shrink it below the slot
        // width, so the whole page shows and it centres (side padding) rather than
        // stretching to fill the width.
        r.viewport_lines = 8;
        let (disp_w, rows) = r.tile_metrics(0, 16);
        assert!(disp_w < 16, "fit-page is narrower than the slot: {disp_w}");
        assert!(rows <= 8, "the whole page fits the viewport height: {rows}");
    }

    /// Manga (RTL) two-page continuous: the facing pages swap sides, so the earlier
    /// page (0) sits to the right of the later page (1). Drives the mock loader +
    /// themer so both pages are placeable, then inspects the emitted tile positions.
    #[test]
    fn continuous_two_page_manga_swaps_columns() {
        let doc = MockDoc::new((0..4).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.continuous = true;
        r.view_mode = crate::config::ViewMode::TwoPage;
        r.rtl = true;
        r.set_cell_px((10, 20));
        r.last_measure = 20;
        r.viewport_lines = 20;
        r.side_padding = 0;
        r.page_gap = 0;
        r.set_trim(false, 0);
        // Rasterize + theme pages 0 and 1 (both needed for the facing spread).
        for _ in 0..200 {
            r.poll_loader();
            r.sync_pages(paged_policy());
            if r.page_ready(0) && r.page_ready(1) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            r.page_ready(0) && r.page_ready(1),
            "both spread pages themed"
        );
        r.capture_page_stack(ratatui::layout::Rect::new(0, 0, 20, 20));
        let x0 = r
            .pdf_targets
            .iter()
            .find(|t| t.section == 0)
            .map(|t| t.rect.x);
        let x1 = r
            .pdf_targets
            .iter()
            .find(|t| t.section == 1)
            .map(|t| t.rect.x);
        assert!(
            matches!((x0, x1), (Some(a), Some(b)) if a > b),
            "RTL: earlier page 0 sits right of later page 1 (x0={x0:?}, x1={x1:?})"
        );
    }

    #[test]
    fn continuous_zoom_scales_pages_and_gates_pan() {
        let mut r = continuous_paged_reader(4);
        assert_eq!(r.band_rows_of(0), 14); // fit-width
        assert!(!r.cont_pannable_x(), "fit-width doesn't overflow");
        r.cont_zoom_in();
        assert!(r.band_rows_of(0) > 14, "zooming in makes pages taller");
        assert!(
            r.cont_pannable_x(),
            "zoomed past fit → horizontally pannable"
        );
        r.cont_zoom_reset();
        assert_eq!(r.band_rows_of(0), 14, "reset returns to fit-width");
        r.cont_zoom_out();
        assert!(r.band_rows_of(0) < 14, "zooming out shrinks pages");
        assert!(!r.cont_pannable_x(), "zoomed out never overflows");
    }

    #[test]
    fn element_starts_finds_each_rich_block() {
        let r = reader_with(vec![para(), code("a"), para(), table(), para(), math()]);
        let starts = r.element_starts();
        let labels: Vec<&str> = starts.iter().map(|(_, l)| *l).collect();
        assert_eq!(labels, ["code", "table", "math"]);
        assert!(
            starts.windows(2).all(|w| w[0].0 < w[1].0),
            "in document order"
        );
    }

    #[test]
    fn next_prev_element_walks_blocks_and_stops_at_edges() {
        let mut r = reader_with(vec![para(), code("a"), para(), math(), para()]);
        let starts = r.element_starts();

        assert!(r.next_element(), "to first element (code)");
        assert_eq!(r.scroll, starts[0].0);
        assert!(r.next_element(), "to second element (math)");
        assert_eq!(r.scroll, starts[1].0);
        assert!(!r.next_element(), "no element below");
        assert_eq!(r.scroll, starts[1].0, "stays put at the last element");

        assert!(r.prev_element(), "back to first element");
        assert_eq!(r.scroll, starts[0].0);
        assert!(!r.prev_element(), "no element above the first");
    }

    #[test]
    fn hold_reflow_position_keeps_the_fraction_across_a_rewrap() {
        // A chapter long enough that changing the measure changes the line count.
        let mut r = reader_with(vec![para(); 40]);
        let wide_len = r.lines.len();
        // Park partway down.
        r.scroll = wide_len / 2;
        let frac = r.within_frac();
        assert!(frac > 0.0 && frac < 1.0);

        // A view-mode switch: hold the position, then re-wrap at a narrower
        // measure (like Center → TwoPage halving the column).
        r.hold_reflow_position();
        assert_eq!(r.pending_frac, Some(frac));
        r.last_measure = 20;
        r.ensure_wrapped(20);
        r.resolve_pending();

        // The reflow changed the line count, and the position tracks the same
        // fraction (within one line of rounding) rather than the stale offset.
        assert_ne!(r.lines.len(), wide_len, "the narrower measure re-wrapped");
        let restored = r.within_frac();
        assert!(
            (restored - frac).abs() < 1.0 / r.lines.len() as f32,
            "fraction {frac} not preserved across re-wrap: got {restored}"
        );
    }

    #[test]
    fn hold_reflow_position_is_a_noop_for_paged_docs() {
        // A paged (PDF) doc: position is the page index, not a wrap fraction.
        let doc = MockDoc::new((0..6).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.load(2);
        r.hold_reflow_position();
        assert_eq!(
            r.pending_frac, None,
            "paged docs don't anchor a wrap fraction"
        );
    }

    #[test]
    fn zoom_and_pan_edge_flip() {
        let doc = MockDoc::new((0..4).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        assert!(!r.page_zoomed());
        r.zoom_in();
        assert!(r.page_zoomed(), "zooming marks the page zoomed");

        // With downward room, `j` pans and reports handled (no flip).
        r.set_page_room(
            PanRoom {
                down: true,
                ..Default::default()
            },
            (0.0, 0.5),
        );
        assert!(r.try_pan_down(1));
        assert!(r.page_view.pan_y > 0.0, "panned down within the page");

        // At the bottom edge (no room) it reports false, so the caller flips.
        r.set_page_room(PanRoom::default(), (0.0, 0.0));
        assert!(!r.try_pan_down(1), "no room → caller flips the page");

        r.zoom_reset();
        assert!(!r.page_zoomed(), "reset returns to fit-page");
    }

    #[test]
    fn element_nav_flashes_when_none() {
        let mut r = reader_with(vec![para(), para()]);
        assert!(!r.next_element());
        assert!(
            r.flash
                .as_deref()
                .unwrap_or("")
                .contains("no code/tables/figures"),
            "flash: {:?}",
            r.flash
        );
    }

    #[test]
    fn paged_nav_moves_in_whole_page_steps() {
        let mut r = reader_with(vec![para(); 40]);
        r.page_lines = 4;
        assert!(r.page_count() >= 2, "multiple pages");
        assert_eq!((r.scroll, r.current_page()), (0, 1));
        r.page_forward();
        assert_eq!(
            (r.scroll, r.current_page()),
            (4, 2),
            "one page down, snapped"
        );
        r.page_forward();
        assert_eq!(r.scroll, 8);
        r.page_backward();
        assert_eq!(r.scroll, 4);
        r.page_backward();
        assert_eq!((r.scroll, r.current_page()), (0, 1));
        // From mid-page, a back-flip snaps to the page start.
        r.scroll = 6;
        r.page_backward();
        assert_eq!(r.scroll, 4, "mid-page back snaps to page start");
    }

    /// A count-prefixed flip (`10j`) jumps that many pages in a paged-image doc,
    /// clamped to the document bounds.
    #[test]
    fn paged_page_jump_moves_by_count_and_clamps() {
        let doc = MockDoc::new((0..10).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.load(2);
        r.page_jump(5);
        assert_eq!(r.section, 7);
        r.page_jump(-3);
        assert_eq!(r.section, 4);
        r.page_jump(100);
        assert_eq!(r.section, 9, "clamps to the last page");
        r.page_jump(-100);
        assert_eq!(r.section, 0, "clamps to the first page");
    }

    /// A two-page spread flips a whole leaf (2 pages) so consecutive spreads
    /// don't overlap, clamping onto the last page at the end.
    #[test]
    fn paged_spread_flips_a_whole_leaf() {
        let doc = MockDoc::new((0..10).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.spread = true;
        r.load(0);
        r.page_forward();
        assert_eq!(r.section, 2, "a spread turns two pages");
        r.page_forward();
        assert_eq!(r.section, 4);
        r.page_backward();
        assert_eq!(r.section, 2);
        r.load(8); // last leaf is the single page 9
        r.page_forward();
        assert_eq!(r.section, 9, "steps onto the last page");
        r.page_forward();
        assert_eq!(r.section, 9, "no-op at the last page");
    }

    /// With a cover offset, a spread shows the first page alone, then facing
    /// pairs (1,2),(3,4)…; flips and back-flips walk those tiles.
    #[test]
    fn paged_spread_cover_offset_shows_first_page_alone() {
        let doc = MockDoc::new((0..10).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.spread = true;
        r.cover_offset = true;
        r.load(0);
        assert_eq!(r.spread_pages(), vec![0], "cover shown alone");
        r.page_forward();
        assert_eq!(r.section, 1);
        assert_eq!(r.spread_pages(), vec![1, 2], "then facing pairs");
        r.page_forward();
        assert_eq!(r.section, 3);
        assert_eq!(r.spread_pages(), vec![3, 4]);
        r.page_backward();
        assert_eq!(r.section, 1);
        r.page_backward();
        assert_eq!(r.section, 0, "back to the cover");
        assert_eq!(r.spread_pages(), vec![0]);
    }

    /// Scroll-spy for a PDF: the sidebar tracks the outline entry whose page is at
    /// or before the current one (PDFs have no text locators, so spy by section).
    #[test]
    fn paged_scroll_spy_tracks_current_page_section() {
        let doc = MockDoc::new((0..10).map(|_| image_page()).collect()).paged();
        let mut r = Reader::new(Box::new(doc)).unwrap();
        let item = |section| OutlineItem {
            label: format!("Ch@{section}"),
            depth: 0,
            section,
            locator: None,
        };
        r.outline = vec![item(0), item(3), item(7)];
        r.section = 0;
        assert_eq!(r.active_outline(), Some(0));
        r.section = 5; // between Ch@3 and Ch@7 → Ch@3
        assert_eq!(r.active_outline(), Some(1));
        r.section = 8; // at/after Ch@7 → Ch@7
        assert_eq!(r.active_outline(), Some(2));
        r.section = 2; // before Ch@3 → Ch@0
        assert_eq!(r.active_outline(), Some(0));
    }

    fn img_block(i: usize) -> Block {
        Block::Image {
            src: format!("{i}.png"),
            alt: String::new(),
            data: vec![1, 2, 3, 4],
            caption: Vec::new(),
            math: false,
            width: delryn_model::ImageWidth::Auto,
        }
    }

    #[test]
    fn neighbour_prefetch_is_bounded_to_cache_spare() {
        // The blank-figures bug: an image-dense neighbour section (a stats textbook
        // has 50+ figures/equations per section) was prefetched wholesale, flooding
        // the cache and evicting the *current* section's visible images. Prefetch
        // must never request more than the cache's spare room.
        use std::num::NonZeroUsize;
        let dense: Vec<Block> = (0..50).map(img_block).collect();
        let doc = MockDoc::new(vec![vec![img_block(0)], dense.clone()]);
        let mut r = Reader::new(Box::new(doc)).unwrap();
        // The neighbour's blocks must be loaded for prefetch to see them.
        r.sections.sections.insert(1, dense);
        // A small cache so the bound clearly bites: spare = 10 with an empty cache.
        r.images.cache.resize(NonZeroUsize::new(10).unwrap());

        let builder = crate::media::ImageBuilder::new(ratatui_image::picker::Picker::halfblocks());
        let geom = ImageGeom {
            avail: 40,
            max_rows: 40,
            max_px: 0,
            width_pct: 85,
            eq_scale: 100,
            fit_mode: media::ImageFit::default(),
            policy: media::RenderPolicy {
                tint: media::Ink {
                    ink: [0, 0, 0],
                    paper: [255, 255, 255],
                },
                mode: media::ImageMode::default(),
            },
        };
        r.request_section_image_builds(1, &builder, geom);

        let n = r.images.requested.len();
        assert!(n > 0, "prefetch still happens");
        assert!(
            n <= 10,
            "prefetch bounded to the cache's spare room, not the 50-image neighbour: {n}"
        );
    }
}
