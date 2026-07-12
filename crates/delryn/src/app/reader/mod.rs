//! The reading view model: a paginated, image-aware, searchable view over one
//! open `Document`. Owns section decoding (background loader + LRU cache), line
//! wrapping, the TOC sidebar with scroll-spy, image protocol lifecycle,
//! navigation history, and in-book search. Pure view-model — no terminal I/O.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use anyhow::Result;

use crate::HighlightColor;
use crate::config::ViewMode;
use crate::document::{Block, Document, OutlineItem};
use crate::layout::{DisplayLine, LineKind, WrapOpts, wrap_blocks};
use crate::media;
use crate::store::Annotation;
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
mod selection;
mod sidebar;
mod state;

use selection::Selection;
use state::{
    ImageState, NavState, PageRasterState, PageThemeState, Pos, SearchState, SectionCache, WrapKey,
};

/// The nominal width component of the base raster's theme/display cache key — a
/// discriminator distinguishing the base raster from the larger viewport-matched
/// crisp rasters (which key on their own width). Placement always uses the
/// raster's *actual* decoded dimensions, so this need only be a stable label, not
/// the exact pixel width of every page. See [`crate::document::pdf::PAGE_RASTER_WIDTH`].
const BASE_RASTER_WIDTH: u32 = crate::document::pdf::PAGE_RASTER_WIDTH as u32;

/// Which kind of element a [`Hint`] pick-mode is labelling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HintKind {
    /// Code blocks — a pick toggles the fold.
    Code,
    /// Figures — a pick opens the image viewer on that figure.
    Image,
}

/// The active "press a number to act on that element" pick-mode: `F`/`I` label
/// each visible element `1..=9`, then a digit acts on the chosen one. Shared by
/// code-fold and image-open so both feel the same (see [`Reader::hint_start`]).
pub struct Hint {
    pub kind: HintKind,
    /// The visible elements' section-local indices, in reading order — badge `n`
    /// is `targets[n - 1]`.
    pub targets: Vec<usize>,
}

/// The result of pressing the pick key ([`Reader::hint_start`]): nothing in view,
/// exactly one (act now, no badges), or several (badges shown, awaiting a digit).
pub enum HintStart {
    None,
    Single(usize),
    Entered(usize),
}

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
    /// Show the code line-number gutter / language tag (set each render from config).
    pub code_line_numbers: bool,
    pub code_label: bool,
    /// Fold long code blocks to a preview (set each render from config).
    pub code_fold: bool,
    pub code_fold_threshold: usize,
    /// Section-local code-block indices whose fold state is flipped from the default
    /// by a per-block `F` toggle. Cleared on section change (indices are local).
    pub code_fold_flip: Vec<usize>,
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
    /// A following continuous section is requested but not yet decoded, so the
    /// buffer stopped short this frame — keep redrawing until it lands (mirrors
    /// [`images_pending`](Self::images_pending)), so it never blocks the scroll.
    cont_pending: bool,
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
    /// Set by an in-place reflow that repositions inline images (a fold/unfold):
    /// terminal graphics don't compose with the cell-diff, so the loop must force a
    /// full repaint or the old image placement lingers until the next scroll.
    pub pending_repaint: bool,
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
    /// Total display lines visible across all text columns this frame (one column
    /// in Center, two in a spread). Drives the visual-selection caret follow so
    /// moving onto the second page doesn't scroll. Written by the view each draw.
    pub visible_span: usize,
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
    /// A code block to scroll to once wrapped (a jump from the code viewer's
    /// Enter; resolved one-shot alongside `pending_image`).
    pending_code: Option<usize>,
    /// A folded/unfolded code block to keep pinned across the re-wrap: `(code
    /// index, rows below the viewport top)`. Resolved one-shot so the toggled block
    /// starts at the same screen row it was on — the reflow grows/shrinks *below* it
    /// instead of shoving the reader's focus around. Overrides `pending_frac`.
    pending_code_hold: Option<(usize, usize)>,
    /// The active number-badge pick-mode (`F` for folds, `I` for figures), or
    /// `None`. While set, the view badges each visible element and a digit acts on it.
    pub hint: Option<Hint>,
    /// Collapsed parent rows (outline indices) in the sidebar tree.
    collapsed: HashSet<usize>,
    /// The active visual text selection (vim `V`), or `None` in normal reading.
    select: Option<Selection>,
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
        math::profile_equation_images(&mut first);
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
            code_line_numbers: true,
            code_label: true,
            code_fold: true,
            code_fold_threshold: 20,
            code_fold_flip: Vec::new(),
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
            cont_pending: false,
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
            pending_repaint: false,
            scroll: 0,
            scroll_pending: 0,
            focus: Focus::Content,
            sidebar_sel: 0,
            sidebar_offset: 0,
            sidebar_h: 1,
            viewport_lines: 1,
            page_lines: 1,
            visible_span: 1,
            last_measure: 72,
            pending_frac: None,
            pending_image: None,
            pending_code: None,
            pending_code_hold: None,
            hint: None,
            overlay_occlude: None,
            collapsed: HashSet::new(),
            select: None,
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
        math::profile_equation_images(&mut blocks);
        self.sections.sections.insert(section, blocks.clone());
        blocks
    }

    /// Non-blocking [`fetch_blocks`]: the section's blocks if already decoded, else
    /// `None` after requesting them from the background loader (which parses and
    /// renders display math *off the main thread*). The continuous following buffer
    /// uses this so a not-yet-decoded — or math-heavy — section never blocks the
    /// scroll on the render thread; it fills in a frame or two later once the loader
    /// finishes (`cont_pending` keeps the loop redrawing until then).
    fn fetch_blocks_async(&mut self, section: usize) -> Option<Vec<Block>> {
        self.drain_loader();
        if let Some(blocks) = self.sections.sections.get(&section) {
            return Some(blocks.clone());
        }
        if self.sections.requested.insert(section) {
            let _ = self.sections.req_tx.send(section);
        }
        None
    }

    /// Index of the code block nearest the viewport centre among the current
    /// section's code blocks (matches `LineKind::Code`) — used to pre-select it in
    /// the code viewer. `None` if none is in view.
    pub fn current_code_index(&self) -> Option<usize> {
        let center = self.scroll + self.viewport_lines / 2;
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l.kind {
                LineKind::Code(idx) => Some((i, idx)),
                _ => None,
            })
            .min_by_key(|(i, _)| (*i as isize - center as isize).unsigned_abs())
            .map(|(_, idx)| idx)
    }

    /// Lines of `self.lines` on screen at once. A two-page reflow spread stacks two
    /// column-heights side by side, so its second column is in view too; every other
    /// view shows a single column-height.
    fn visible_span(&self) -> usize {
        let cols = if self.view_mode == ViewMode::TwoPage && !self.is_paged_image() {
            2
        } else {
            1
        };
        self.viewport_lines * cols
    }

    /// The code block a fold toggle acts on: the one nearest the centre of the whole
    /// visible area (both spread columns), so `F` reaches a right-column block, not
    /// only the left one.
    pub fn fold_target(&self) -> Option<usize> {
        let center = self.scroll + self.visible_span() / 2;
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l.kind {
                LineKind::Code(idx) => Some((i, idx)),
                _ => None,
            })
            .min_by_key(|(i, _)| (*i as isize - center as isize).unsigned_abs())
            .map(|(_, idx)| idx)
    }

    /// The section-local indices of every visible element of `kind`, in reading
    /// order (left spread column before right), capped at 9 so each gets a `1..=9`
    /// badge. Shared by the `F`/`I` pick-modes.
    fn visible_elements(&self, kind: HintKind) -> Vec<usize> {
        let end = self.scroll + self.visible_span();
        let mut out: Vec<usize> = Vec::new();
        for line in self.lines.iter().skip(self.scroll).take(end - self.scroll) {
            let idx = match (kind, line.kind) {
                (HintKind::Code, LineKind::Code(x)) => x,
                (HintKind::Image, LineKind::Image(x)) => x,
                _ => continue,
            };
            if !out.contains(&idx) {
                out.push(idx);
                if out.len() == 9 {
                    break;
                }
            }
        }
        out
    }

    /// Open the number-badge pick-mode for `kind`: nothing in view, one element (act
    /// now — no badges for a single choice), or several (badges shown, awaiting a
    /// digit; the caller stashes the state). See [`HintStart`].
    pub fn hint_start(&mut self, kind: HintKind) -> HintStart {
        let targets = self.visible_elements(kind);
        match targets.len() {
            0 => HintStart::None,
            1 => HintStart::Single(targets[0]),
            n => {
                self.hint = Some(Hint { kind, targets });
                HintStart::Entered(n)
            }
        }
    }

    /// Resolve badge `n` (1-based) to its `(kind, element index)` and close the
    /// pick-mode. `None` (leaving the mode open) when `n` is out of range.
    pub fn hint_pick(&mut self, n: usize) -> Option<(HintKind, usize)> {
        let hint = self.hint.as_ref()?;
        let idx = *hint.targets.get(n.checked_sub(1)?)?;
        let kind = hint.kind;
        self.hint = None;
        Some((kind, idx))
    }

    /// Close the pick-mode without acting (Esc / any non-digit key).
    pub fn hint_cancel(&mut self) {
        self.hint = None;
    }

    /// Whether a pick-mode is open (so the key router captures digits for it).
    pub fn hint_active(&self) -> bool {
        self.hint.is_some()
    }

    /// The open pick-mode's kind and its ordered targets, for the badge renderer.
    pub fn hint(&self) -> Option<(HintKind, &[usize])> {
        self.hint.as_ref().map(|h| (h.kind, h.targets.as_slice()))
    }

    /// Pin code block `idx` across the next re-wrap so its first row stays at the
    /// same screen offset (the reflow grows/shrinks below it). Captured from the
    /// current `lines`, before the toggle re-wraps. A no-op for paged docs.
    pub fn hold_code_block(&mut self, idx: usize) {
        if self.is_paged_image() {
            return;
        }
        if let Some(start) = self
            .lines
            .iter()
            .position(|l| l.kind == LineKind::Code(idx))
        {
            self.pending_code_hold = Some((idx, start.saturating_sub(self.scroll)));
        }
    }

    /// Line count of the `idx`-th code block in the current section, if any.
    fn code_block_len(&self, idx: usize) -> Option<usize> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Code { lines, .. } => Some(lines.len()),
                _ => None,
            })
            .nth(idx)
    }

    /// Toggle the fold of code block `idx` (its per-block override against the global
    /// `code_fold` default), pin it to its screen row, and refresh the moved images.
    /// Returns a status line for the flash. A short block (at or under the threshold)
    /// can't fold, so it reports that instead.
    pub fn toggle_fold_at(&mut self, idx: usize) -> String {
        if self
            .code_block_len(idx)
            .is_some_and(|n| n <= self.code_fold_threshold)
        {
            return "code block is short — nothing to fold".into();
        }
        let now_flipped = match self.code_fold_flip.iter().position(|&i| i == idx) {
            Some(pos) => {
                self.code_fold_flip.remove(pos);
                false
            }
            None => {
                self.code_fold_flip.push(idx);
                true
            }
        };
        // Folded now iff the default and the (new) flip state agree on folding.
        let folded = self.code_fold ^ now_flipped;
        // Keep the toggled block pinned at its screen row so the reflow doesn't
        // shove the reader's focus.
        self.hold_code_block(idx);
        // The line count changed, so inline images below move to new rows. Kitty
        // images composite above the cell grid, so the old placement lingers unless
        // its id is deleted — `restage` deletes + rebuilds them at the new rows.
        self.restage_visible_images();
        self.request_repaint();
        if folded {
            "code block folded"
        } else {
            "code block unfolded"
        }
        .into()
    }

    /// Request a full repaint next frame (see [`pending_repaint`](Self::pending_repaint)).
    pub fn request_repaint(&mut self) {
        self.pending_repaint = true;
    }

    /// Take the pending-repaint request; the loop clears the terminal when it's set.
    pub fn take_repaint(&mut self) -> bool {
        std::mem::take(&mut self.pending_repaint)
    }

    /// Collect the code blocks for the viewer: the current chapter, or every
    /// section when `whole_book`. Mirrors [`figures`](Self::figures).
    pub fn code_blocks(&mut self, whole_book: bool) -> Vec<super::CodeSnippet> {
        let mut out = Vec::new();
        if whole_book {
            for s in 0..self.doc.section_count() {
                let blocks = self.fetch_blocks(s);
                super::code_view::collect_code_blocks(&blocks, s, &mut out);
            }
        } else {
            super::code_view::collect_code_blocks(&self.blocks, self.section, &mut out);
        }
        out
    }

    /// Stage `text` for the OS clipboard (drained by the event loop) and flash the
    /// count. Shared by the selection copy and the code viewer's copy-all.
    pub fn stage_clipboard(&mut self, text: String) {
        let n = text.chars().count();
        self.pending_clipboard = Some(text);
        self.flash = Some(format!(
            "✓ copied {n} char{}",
            if n == 1 { "" } else { "s" }
        ));
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
        } else if self.reflow_flows() {
            // Continuous reflow stitches the *following* sections into the scroll
            // buffer, so pre-decode a few each side (their math renders off-thread)
            // before they scroll into view — otherwise a math-heavy following/previous
            // section cold-renders on the loader at the boundary and the buffer waits
            // (a redraw spin) for it. Kept within the loader's stale radius.
            3
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
            code_fold: self.code_fold,
            code_fold_threshold: self.code_fold_threshold,
            code_fold_flip: self.code_fold_flip.clone(),
            table_wrap: self.table_wrap,
            justify: self.justify,
            tidy: self.tidy_spacing,
            images_key: self.images.images_key,
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
        // Per-block fold overrides are section-local, so they apply to the anchor.
        self.wrap_at_with_rows(
            blocks,
            width,
            &self.images.rows_estimate,
            &self.code_fold_flip,
        )
    }

    /// Wrap `blocks` reserving `image_rows` blank rows per image, applying the
    /// per-block fold overrides in `flip`. The anchor passes its own `rows_estimate`
    /// and `code_fold_flip` (via [`wrap_at`]); a *following* continuous section passes
    /// its own rows and an empty `flip` (the overrides are the anchor's local indices)
    /// so its figures reserve the right space and its code folds by the default.
    fn wrap_at_with_rows(
        &self,
        blocks: &[Block],
        width: usize,
        image_rows: &[u16],
        flip: &[usize],
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
                code_line_numbers: self.code_line_numbers,
                code_label: self.code_label,
                code_fold: self.code_fold,
                code_fold_threshold: self.code_fold_threshold,
                code_fold_flip: flip,
                table_wrap: self.table_wrap,
                justify: self.justify,
                tidy_spacing: self.tidy_spacing,
            },
            image_rows,
        )
    }

    /// Set the open book's annotations, splitting them by kind into bookmark,
    /// note, and highlight streams, then resolve the current section's into gutter
    /// lines. Called by the app on any change (and on open). Takes the raw store
    /// rows so the kind/colour split lives in one place.
    pub fn set_annotations(&mut self, items: Vec<Annotation>) {
        let (mut bookmarks, mut notes, mut highlights) = (Vec::new(), Vec::new(), Vec::new());
        for a in items {
            if a.is_highlight() {
                highlights.push((a.section, a.quote, HighlightColor::from_index(a.color)));
            } else if a.is_note() {
                notes.push((a.section, a.quote));
            } else {
                bookmarks.push((a.section, a.quote));
            }
        }
        self.nav.bookmarks = bookmarks;
        self.nav.notes = notes;
        self.nav.highlights = highlights;
        self.recompute_annotation_lines();
    }

    /// Resolve this section's bookmark / note / highlight quotes to display lines
    /// (once per re-wrap), so the gutter and line wash can be applied cheaply.
    fn recompute_annotation_lines(&mut self) {
        let (section, lines) = (self.section, &self.lines);
        let resolve = |marks: &[(usize, String)]| {
            marks
                .iter()
                .filter(|(s, _)| *s == section)
                .filter_map(|(_, quote)| find_line(lines, quote))
                .collect::<HashSet<usize>>()
        };
        self.nav.bookmark_lines = resolve(&self.nav.bookmarks);
        self.nav.note_lines = resolve(&self.nav.notes);
        // Highlights resolve to exact character spans (a whole-line `H` highlight
        // covers its line; a `V` selection highlight covers just its characters),
        // grouped per display line so the view can wash them.
        let mut spans: HashMap<usize, Vec<(usize, usize, HighlightColor)>> = HashMap::new();
        for (_, quote, color) in self.nav.highlights.iter().filter(|(s, _, _)| *s == section) {
            for (line, (start, end)) in selection::resolve_spans(quote, &self.lines) {
                spans.entry(line).or_default().push((start, end, *color));
            }
        }
        self.nav.highlight_spans = spans;
    }

    /// Whether a display line carries a bookmark (for the left-gutter marker).
    pub fn is_bookmark_line(&self, line: usize) -> bool {
        self.nav.bookmark_lines.contains(&line)
    }

    /// Whether a display line carries a note (drawn with a pen glyph in the gutter).
    pub fn is_note_line(&self, line: usize) -> bool {
        self.nav.note_lines.contains(&line)
    }

    /// The highlight colour on a display line, if any — for the gutter chip.
    pub fn highlight_line(&self, line: usize) -> Option<HighlightColor> {
        self.nav
            .highlight_spans
            .get(&line)
            .and_then(|v| v.first())
            .map(|(_, _, c)| *c)
    }

    /// The highlight spans on a display line (`(start, end, colour)`), for washing.
    pub fn highlight_spans(&self, line: usize) -> &[(usize, usize, HighlightColor)] {
        self.nav
            .highlight_spans
            .get(&line)
            .map(Vec::as_slice)
            .unwrap_or(&[])
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
                Some(loc) => find_heading_line(&self.lines, loc).unwrap_or(0),
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
        if self.reflow_flows() && self.section + 1 < self.doc.section_count() {
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
        self.code_fold_flip.clear(); // per-block fold overrides are section-local
        self.hint = None; // any open pick-mode is stale in a new section
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
        if self.reflow_flows() {
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
        if self.reflow_flows() {
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
            if let Some(line) = find_heading_line(&self.lines, text) {
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

    /// Navigate to a code block's location (from the code viewer's Enter): its
    /// section, then scroll to the block once wrapped. Mirrors [`jump_to_image`].
    pub fn jump_to_code(&mut self, section: usize, code_index: usize) {
        self.push_history();
        if section != self.section {
            self.load(section);
        } else {
            self.scroll = 0;
        }
        self.pending_code = Some(code_index);
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
        // One-shot: scroll to the jumped-to code block's first line.
        if let Some(idx) = self.pending_code.take()
            && let Some(line) = self
                .lines
                .iter()
                .position(|l| l.kind == LineKind::Code(idx))
        {
            self.scroll = line;
        }
        // One-shot: keep a folded/unfolded block at its old screen row (applied
        // last so it wins over a fraction resume set by the generic reflow hold).
        if let Some((idx, offset)) = self.pending_code_hold.take()
            && let Some(start) = self
                .lines
                .iter()
                .position(|l| l.kind == LineKind::Code(idx))
        {
            self.scroll = start.saturating_sub(offset);
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

    /// A short text quote of the anchor line ([`current_line_text`]), used to
    /// anchor bookmarks/notes so they survive reflow.
    pub fn current_quote(&self) -> String {
        self.current_line_text().chars().take(80).collect()
    }

    /// The full (whitespace-normalized) text of the *anchor line* — the caret's
    /// line when the cursor is active (so an annotation lands where the cursor is,
    /// including on the second page of a spread), otherwise the first non-blank
    /// visible line. A whole-line `H` highlight stores this, so it re-washes the
    /// entire line via [`selection::resolve_spans`] at any width.
    pub fn current_line_text(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let from = self
            .select
            .map(|s| s.caret.line)
            .unwrap_or(self.scroll)
            .min(self.lines.len() - 1);
        self.lines[from..]
            .iter()
            .map(|l| l.text())
            .find(|t| !t.trim().is_empty())
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
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
    find_line_matching(lines, needle, false)
}

/// Like [`find_line`] but, for a TOC entry, matches an actual **heading** line in
/// preference to body text — so a chapter whose title also appears in an on-page
/// "Table of Contents" listing resolves to the real chapter heading, not the
/// listing. Falls back to any line when no heading matches.
fn find_heading_line(lines: &[DisplayLine], needle: &str) -> Option<usize> {
    find_line_matching(lines, needle, true)
}

fn find_line_matching(lines: &[DisplayLine], needle: &str, headings_only: bool) -> Option<usize> {
    let n = loose_key(needle);
    if n.is_empty() {
        return None;
    }
    let matches = |l: &DisplayLine| {
        let line = loose_key(&l.text());
        !line.is_empty() && (line == n || line.contains(&n) || (n.len() >= 8 && n.contains(&line)))
    };
    // A TOC locator prefers the heading line (exact first, then loose); only if no
    // heading matches does it fall through to the any-line search below.
    if headings_only {
        let is_heading = |l: &&DisplayLine| matches!(l.kind, LineKind::Heading(_));
        if let Some(i) = lines
            .iter()
            .position(|l| matches!(l.kind, LineKind::Heading(_)) && loose_key(&l.text()) == n)
        {
            return Some(i);
        }
        if let Some((i, _)) = lines
            .iter()
            .enumerate()
            .find(|(_, l)| is_heading(l) && matches(l))
        {
            return Some(i);
        }
    }
    if let Some(i) = lines.iter().position(|l| loose_key(&l.text()) == n) {
        return Some(i);
    }
    lines.iter().position(matches)
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

    /// The "Math size %" knob must actually resize rendered equations: lowering the
    /// scale re-renders the equation smaller (regression for the size knob).
    #[test]
    fn math_scale_resizes_rendered_equations() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_mscale_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // SAFETY: serialised by `_env`; scopes the math cache dir to this test.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };

        let mut r = reader_with(vec![Block::Math {
            unicode: "x".into(),
            latex: Some("\\frac{x^2}{y}".into()),
        }]);
        let dims = |r: &mut Reader| -> (u32, u32) {
            r.fetch_blocks(0)
                .iter()
                .find_map(|b| match b {
                    Block::Image {
                        data, math: true, ..
                    } => crate::media::image_dimensions(data),
                    _ => None,
                })
                .expect("a rendered math image")
        };

        r.sync_graphical_math(true, 16, 100);
        let big = dims(&mut r);
        r.sync_graphical_math(true, 16, 50);
        let small = dims(&mut r);
        r.sync_graphical_math(false, 16, 100); // reset ENABLED so it doesn't leak
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(
            small.1 < big.1,
            "50% math size must render shorter than 100%: {small:?} vs {big:?}"
        );
    }

    #[test]
    fn toc_locator_prefers_heading_over_listing() {
        // A single-section book with an on-page "Table of Contents" lists chapter
        // titles as body text before the real chapter headings. A TOC locator must
        // resolve to the actual heading, not its earlier listing occurrence.
        let title = "Chapter 6: The Relational Level";
        let heading = |t: &str| Block::Heading {
            level: 1,
            spans: vec![Span::plain(t)],
        };
        let listing = |t: &str| Block::Para {
            spans: vec![Span::plain(t)],
            indent: 0,
            quote: false,
            marker: None,
        };
        let r = reader_with(vec![
            heading("Table of Contents"),
            listing(title), // the on-page ToC listing (body text)
            listing("some intervening body text to separate the two"),
            heading(title), // the real chapter heading
            listing("actual chapter body"),
        ]);

        let listing_line = find_line(&r.lines, title).unwrap();
        let heading_line = find_heading_line(&r.lines, title).unwrap();
        assert!(
            heading_line > listing_line,
            "heading ({heading_line}) should resolve after the listing ({listing_line})"
        );
        assert!(matches!(r.lines[heading_line].kind, LineKind::Heading(_)));
        assert!(matches!(r.lines[listing_line].kind, LineKind::Body));
    }

    #[test]
    fn continuation_section_maps_to_containing_chapter() {
        // Chapters start at sections 0 and 1; section 2 is a continuation of
        // chapter 2 with no TOC entry of its own. The scroll-spy / chapter title
        // must still report "Chapter 2", not a bare "Section 3".
        let heading = |t: &str| Block::Heading {
            level: 1,
            spans: vec![Span::plain(t)],
        };
        let doc = MockDoc {
            sections: vec![
                vec![heading("Chapter 1")],
                vec![heading("Chapter 2")],
                vec![para()],
            ],
            meta: Metadata::default(),
            toc: Vec::new(),
            outline: vec![
                OutlineItem {
                    label: "Chapter 1".into(),
                    depth: 0,
                    section: 0,
                    locator: Some("Chapter 1".into()),
                },
                OutlineItem {
                    label: "Chapter 2".into(),
                    depth: 0,
                    section: 1,
                    locator: Some("Chapter 2".into()),
                },
            ],
            paged: false,
        };
        let mut r = Reader::new(Box::new(doc)).unwrap();
        r.last_measure = 40;
        r.jump_to(2, None); // read into the continuation section
        r.ensure_wrapped(40);

        assert!(
            r.active_outline().is_some(),
            "highlight should not disappear"
        );
        assert_eq!(
            r.active_outline().map(|oi| r.outline[oi].label.as_str()),
            Some("Chapter 2")
        );
        assert_eq!(r.chapter_title(), "Chapter 2");
    }

    // Cursor mode: `V` starts a movable caret with no anchor (nothing selected);
    // `v`/Space drops the anchor, then extending right grows the selected text, and
    // copying stages it for the clipboard.
    #[test]
    fn visual_selection_extends_and_copies() {
        let mut r = reader_with(vec![para()]);
        r.viewport_lines = 10;
        r.page_lines = 10;
        r.visible_span = 10;
        r.start_selection();
        assert!(r.selection_active());
        assert!(!r.selection_selecting(), "cursor mode has no selection yet");
        assert_eq!(r.selection_text(), "", "nothing anchored → no text");

        r.toggle_selection_anchor(); // begin selecting at the caret
        assert!(r.selection_selecting());
        // Extend right over "lorem " (6 cells, cols 0..=5); normalized → "lorem".
        for _ in 0..5 {
            r.selection_right();
        }
        assert_eq!(r.selection_text(), "lorem");
        assert!(r.copy_selection(), "non-empty selection copies");
        assert!(!r.selection_active(), "copying leaves the mode");
        assert_eq!(r.take_clipboard().as_deref(), Some("lorem"));
    }

    // The caret follow uses the full visible span (both pages of a spread), so
    // moving onto the second page positions the caret there instead of scrolling —
    // the two-page runaway-scroll bug.
    #[test]
    fn caret_reaches_second_page_without_scrolling() {
        let big = Block::Para {
            spans: vec![Span::plain("lorem ipsum dolor sit amet ".repeat(40))],
            indent: 0,
            quote: false,
            marker: None,
        };
        let mut r = reader_with(vec![big]);
        r.page_lines = 8;
        r.visible_span = 16; // a two-page spread: two 8-line columns
        assert!(r.lines.len() > 20, "enough lines to fill two pages");
        r.start_selection();
        // Move the caret down onto the second page (line 12) — still within the
        // visible span, so the spread must not scroll.
        for _ in 0..12 {
            r.selection_down();
        }
        assert_eq!(
            r.scroll, 0,
            "caret on the 2nd page doesn't scroll the spread"
        );
        // Past the full span, the follow finally scrolls to keep the caret visible.
        for _ in 0..8 {
            r.selection_down();
        }
        assert!(r.scroll > 0, "past the visible span the follow scrolls");
    }

    // Ctrl-d / Ctrl-u jump the caret by half the visible span.
    #[test]
    fn half_page_caret_jump() {
        let big = Block::Para {
            spans: vec![Span::plain("lorem ipsum dolor sit amet ".repeat(40))],
            indent: 0,
            quote: false,
            marker: None,
        };
        let mut r = reader_with(vec![big]);
        r.page_lines = 8;
        r.visible_span = 16; // half = 8
        r.start_selection();
        assert_eq!(r.selection_caret().map(|(l, _)| l), Some(0));
        r.selection_half_down();
        assert_eq!(r.selection_caret().map(|(l, _)| l), Some(8));
        r.selection_half_up();
        assert_eq!(r.selection_caret().map(|(l, _)| l), Some(0));
    }

    // A stored highlight resolves its quote back to a character span on its line,
    // so it re-washes after reflow (the render path behind the gutter/wash).
    #[test]
    fn stored_highlight_resolves_to_a_span() {
        let mut r = reader_with(vec![para()]);
        let quote = r.current_line_text();
        assert!(!quote.is_empty());
        r.set_annotations(vec![Annotation {
            id: 1,
            section: 0,
            quote,
            note: String::new(),
            name: String::new(),
            folder: String::new(),
            kind: crate::store::KIND_HIGHLIGHT,
            color: 2,
        }]);
        assert!(r.highlight_line(0).is_some(), "line 0 is highlighted");
        assert!(!r.highlight_spans(0).is_empty(), "with a concrete span");
    }

    // Returning from the image viewer must force the current section's images to
    // rebuild (fresh terminal ids) — otherwise a same-section figure jump leaves
    // them blank until an unrelated redraw, because transmit-once never re-sends
    // an image the viewer's big figure evicted from the terminal.
    #[test]
    fn restage_invalidates_the_remap_key_so_images_rebuild() {
        use crate::media::{ImageFit, ImgKey, Ink, RenderPolicy};
        let mut r = reader_with(vec![]);
        let key = ImgKey {
            section: 0,
            idx: 0,
            avail: 40,
            max_rows: 20,
            max_px: 0,
            target_pct: 85,
            math_scale: 100,
            fit_mode: ImageFit::default(),
            policy: RenderPolicy {
                tint: Ink {
                    ink: [0, 0, 0],
                    paper: [255, 255, 255],
                },
                mode: crate::media::ImageMode::default(),
            },
        };
        r.images.section_images.insert(0, key);
        r.images.requested.insert(key);
        r.images.images_key.0 = 0; // pretend section 0 is currently mapped

        r.restage_visible_images();

        assert_eq!(
            r.images.images_key.0,
            usize::MAX,
            "remap key invalidated so sync_images re-dispatches the builds"
        );
        assert!(
            !r.images.requested.contains(&key),
            "the in-flight request is cleared so the rebuild re-dispatches it"
        );
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
    fn reflow_flows_for_two_page_always_center_needs_the_toggle() {
        let mut r = continuous_reader(3); // continuous = true, view_mode = Center
        assert!(r.reflow_flows(), "center + continuous flows");
        r.continuous = false;
        assert!(!r.reflow_flows(), "center without the toggle does not flow");
        r.view_mode = ViewMode::TwoPage;
        assert!(
            r.reflow_flows(),
            "two-page flows across chapters even off-toggle"
        );
        r.chapter_lock = true;
        assert!(!r.reflow_flows(), "chapter-lock keeps per-section paging");
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
            ink: None,
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

    // Folding all blocks pins the block the reader was on: its first row stays at the
    // same screen offset even though a block *above* it collapsed and shifted the
    // text up. (The old fraction-based hold would have drifted it.)
    #[test]
    fn global_fold_pins_the_central_block_to_its_screen_row() {
        let long = |p: &str| Block::Code {
            lang: Some("text".into()),
            lines: (0..30).map(|i| format!("{p}{i}")).collect(),
        };
        // Two long code blocks (idx 0 above, idx 1 below) with a paragraph between.
        let mut r = reader_with(vec![long("a"), para(), long("b")]);
        r.viewport_lines = 20;
        r.code_fold = false; // start fully unfolded
        r.code_fold_threshold = 20;
        r.ensure_wrapped(40);

        // Park so block B's first row sits 4 rows below the viewport top.
        let b_start = r
            .lines
            .iter()
            .position(|l| l.kind == LineKind::Code(1))
            .unwrap();
        r.scroll = b_start.saturating_sub(4);
        let offset = b_start - r.scroll;
        assert_eq!(offset, 4);

        // The `Z` path: flip the global default, pin the central block, re-wrap.
        r.code_fold = true;
        let target = r.fold_target();
        assert_eq!(target, Some(1), "the block near the centre is B");
        r.hold_code_block(target.unwrap());
        r.ensure_wrapped(40);
        r.resolve_pending();

        let b_new = r
            .lines
            .iter()
            .position(|l| l.kind == LineKind::Code(1))
            .unwrap();
        assert_eq!(
            b_new - r.scroll,
            offset,
            "block B stays at its screen row after everything folds"
        );
    }

    // A following continuous section already decoded is served from the cache with
    // no synchronous load and no re-request — the fast path of the non-blocking fetch
    // that keeps a math-heavy section from freezing the scroll.
    #[test]
    fn async_fetch_serves_a_cached_section_without_requesting() {
        let mut r =
            Reader::new(Box::new(MockDoc::new(vec![vec![para()], vec![code("x")]]))).unwrap();
        r.sections.sections.insert(1, vec![code("x")]);
        r.sections.requested.clear(); // drop the constructor's prefetch request
        assert!(
            r.fetch_blocks_async(1).is_some(),
            "cached section returns now"
        );
        assert!(
            !r.sections.requested.contains(&1),
            "a cached section issues no new loader request"
        );
        assert!(
            !r.following_pending(),
            "nothing pending when everything's cached"
        );
    }

    #[test]
    fn hint_labels_visible_blocks_in_reading_order() {
        let mut r = reader_with(vec![code("a"), para(), code("b"), para(), code("c")]);
        r.viewport_lines = 100; // the whole section is on screen
        r.scroll = 0;
        // Three code blocks in view → the pick-mode opens with three badges.
        match r.hint_start(HintKind::Code) {
            HintStart::Entered(n) => assert_eq!(n, 3),
            _ => panic!("expected a multi-block pick-mode"),
        }
        assert!(r.hint_active());
        // Badge 2 resolves to the second code block (index 1) and closes the mode.
        assert_eq!(r.hint_pick(2), Some((HintKind::Code, 1)));
        assert!(!r.hint_active(), "a pick closes the mode");
    }

    #[test]
    fn hint_acts_directly_on_a_single_block() {
        let mut r = reader_with(vec![code("only"), para()]);
        r.viewport_lines = 100;
        r.scroll = 0;
        match r.hint_start(HintKind::Code) {
            HintStart::Single(idx) => assert_eq!(idx, 0),
            _ => panic!("one block → act directly, no badges"),
        }
        assert!(!r.hint_active(), "a single choice never enters the mode");
    }

    #[test]
    fn hint_pick_out_of_range_keeps_the_mode_open() {
        let mut r = reader_with(vec![code("a"), para(), code("b")]);
        r.viewport_lines = 100;
        r.scroll = 0;
        assert!(matches!(
            r.hint_start(HintKind::Code),
            HintStart::Entered(2)
        ));
        assert_eq!(r.hint_pick(5), None, "no 5th block");
        assert!(
            r.hint_active(),
            "an out-of-range digit leaves the mode open"
        );
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
            ink: None,
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
            math_scale: 100,
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
