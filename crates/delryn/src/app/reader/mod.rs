//! The reading view model: a paginated, image-aware, searchable view over one
//! open `Document`. Owns section decoding (background loader + LRU cache), line
//! wrapping, the TOC sidebar with scroll-spy, image protocol lifecycle,
//! navigation history, and in-book search. Pure view-model — no terminal I/O.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use anyhow::Result;

use crate::document::{Block, Document, OutlineItem};
use crate::layout::{DisplayLine, LineKind, WrapOpts, wrap_blocks};
use crate::media;
use crate::theme;
use delryn_model::{Anchor, find_footnote};

use super::{CACHE_CAP, Focus};

// Separable reader concerns; each contributes an `impl Reader` block and reaches
// the core's helpers (find_line, fetch_blocks, …) via the parent module.
mod images;
pub use images::ImageGeom;
mod pages;
mod search;
mod sidebar;
mod state;

use state::{ImageState, NavState, PageThemeState, Pos, SearchState, SectionCache, WrapKey};

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
    /// A two-page paged-image spread is on screen (set each render): a page flip
    /// turns a whole leaf (2 sections) so consecutive spreads don't overlap.
    pub spread: bool,
    /// Show the first page alone in a spread (book cover), then pair (2,3),
    /// (4,5)… so facing pages line up as in a physical book (set each render).
    pub cover_offset: bool,
    /// PDF page placements for this frame (section + absolute cell rect),
    /// captured by the view and consumed by the direct-Kitty [`PageDeck`]. One
    /// entry single-page, two for a spread.
    pub pdf_targets: Vec<(usize, ratatui::layout::Rect)>,
    /// The inputs the current `lines` were wrapped against; a change re-wraps.
    wrapped: WrapKey,
    /// Inline-image lifecycle (built protocols, row estimates, in-flight builds).
    images: ImageState,
    /// Paged-image (PDF) page theming (themer + themed-PNG cache + active policy).
    pages: PageThemeState,
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
    /// Height of one column in lines, refreshed each draw.
    pub viewport_lines: usize,
    /// Total lines visible at once (2 columns in two-page mode), for scroll math.
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
        let first = doc.load_section(start).unwrap_or_default().blocks;
        sections.sections.insert(start, first.clone());

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
            spread: false,
            cover_offset: false,
            pdf_targets: Vec::new(),
            wrapped: WrapKey::invalid(),
            images: ImageState::default(),
            pages: PageThemeState::default(),
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
        let blocks = self
            .doc
            .load_section(section)
            .map(|s| s.blocks)
            .unwrap_or_default();
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
        let ahead = if self.is_paged_image() { 4 } else { 1 };
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
        };
        if key != self.wrapped {
            self.lines = wrap_blocks(
                &self.blocks,
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
                &self.images.rows_estimate,
            );
            self.wrapped = key;
            self.recompute_heading_lines();
            self.recompute_anchors();
            self.recompute_bookmark_lines();
        }
    }

    /// Set the open book's bookmarks (`(section, quote)`), then resolve the
    /// current section's into gutter lines. Called by the app on any change.
    pub fn set_bookmarks(&mut self, bookmarks: Vec<(usize, String)>) {
        self.nav.bookmarks = bookmarks;
        self.recompute_bookmark_lines();
    }

    /// Resolve this section's bookmark quotes to display lines (once per re-wrap).
    fn recompute_bookmark_lines(&mut self) {
        self.nav.bookmark_lines = self
            .nav
            .bookmarks
            .iter()
            .filter(|(section, _)| *section == self.section)
            .filter_map(|(_, quote)| find_line(&self.lines, quote))
            .collect();
    }

    /// Whether a display line carries a bookmark (for the left-gutter marker).
    pub fn is_bookmark_line(&self, line: usize) -> bool {
        self.nav.bookmark_lines.contains(&line)
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

    /// Rebuild the inline-anchor index and footnote definition map from the
    /// freshly wrapped lines. Done once per re-wrap.
    fn recompute_anchors(&mut self) {
        // Anchors in reading order, merging adjacent runs that share one anchor
        // (e.g. a multi-word link "Chapter 3") into a single followable target,
        // even across the whitespace runs that wrapping inserts between words.
        let mut hits: Vec<AnchorHit> = Vec::new();
        for (li, line) in self.lines.iter().enumerate() {
            let mut col = 0usize;
            // Whether everything since the last anchor run on this line was blank,
            // so a same-anchor run after a space still merges into one target.
            let mut gap_blank = false;
            for run in &line.runs {
                let len = run.text.chars().count();
                match &run.anchor {
                    Some(a) => {
                        match hits.last_mut() {
                            Some(last) if last.line == li && last.anchor == *a && gap_blank => {
                                last.end = col + len;
                            }
                            _ => hits.push(AnchorHit {
                                line: li,
                                start: col,
                                end: col + len,
                                anchor: a.clone(),
                            }),
                        }
                        gap_blank = true;
                    }
                    // Non-anchor run keeps the run mergeable only if it's blank.
                    None => gap_blank = gap_blank && run.text.trim().is_empty(),
                }
                col += len;
            }
        }
        self.nav.anchors = hits;
        self.nav.anchor_sel = self.nav.anchor_sel.filter(|&i| i < self.nav.anchors.len());

        // Footnote definitions: first display line per section-local index, then
        // id → line via the blocks (same top-level order the layout numbered them).
        let mut idx_line: HashMap<usize, usize> = HashMap::new();
        for (li, l) in self.lines.iter().enumerate() {
            if let LineKind::Footnote(k) = l.kind {
                idx_line.entry(k).or_insert(li);
            }
        }
        let mut map = HashMap::new();
        let mut k = 0usize;
        for b in &self.blocks {
            if let Block::Footnote { id, .. } = b {
                if let Some(&line) = idx_line.get(&k)
                    && !id.is_empty()
                {
                    map.insert(id.clone(), line);
                }
                k += 1;
            }
        }
        self.nav.footnote_def_line = map;
    }

    /// Step the link cursor to the next/previous inline anchor and scroll it into
    /// view. With no selection yet, starts from the viewport.
    pub fn next_anchor(&mut self) {
        self.step_anchor(true);
    }

    pub fn prev_anchor(&mut self) {
        self.step_anchor(false);
    }

    fn step_anchor(&mut self, forward: bool) {
        self.ensure_wrapped(self.last_measure.max(1));
        if self.nav.anchors.is_empty() {
            self.flash = Some("no links or footnotes in this chapter".to_string());
            return;
        }
        let n = self.nav.anchors.len();
        let next = match self.nav.anchor_sel {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None if forward => self
                .nav
                .anchors
                .iter()
                .position(|a| a.line >= self.scroll)
                .unwrap_or(0),
            None => self
                .nav
                .anchors
                .iter()
                .rposition(|a| a.line < self.scroll + self.page_lines.max(1))
                .unwrap_or(n - 1),
        };
        self.nav.anchor_sel = Some(next);
        self.scroll_into_view(self.nav.anchors[next].line);
        let kind = anchor_kind_label(&self.nav.anchors[next].anchor);
        self.flash = Some(format!("{kind} {}/{n} · Enter to follow", next + 1));
    }

    /// Scroll so `line` is within the visible page (top if above, half-page from
    /// the top if below).
    fn scroll_into_view(&mut self, line: usize) {
        let page = self.page_lines.max(1);
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + page {
            self.scroll = line.saturating_sub(page / 2);
        }
        self.scroll_pending = 0;
        self.clamp_scroll();
    }

    /// The anchor the link cursor is on, for the view to highlight.
    pub fn selected_anchor(&self) -> Option<&AnchorHit> {
        self.nav.anchor_sel.and_then(|i| self.nav.anchors.get(i))
    }

    /// Clear the link cursor; returns whether anything was selected (so the key
    /// is "consumed" only when it actually dismissed the cursor).
    pub fn clear_anchor(&mut self) -> bool {
        self.nav.anchor_sel.take().is_some()
    }

    /// Follow the selected anchor: footnote ref → its definition (with history for
    /// return); link → copy the URL; cross-ref/citation → a status note (jump
    /// targets aren't indexed yet). Returns whether an anchor was selected.
    pub fn activate_anchor(&mut self) -> bool {
        let Some(i) = self.nav.anchor_sel else {
            return false;
        };
        let Some(hit) = self.nav.anchors.get(i) else {
            return false;
        };
        match hit.anchor.clone() {
            Anchor::Footnote(target) => self.follow_footnote(&target),
            Anchor::Link(url) => {
                // Surfaced to the app, which confirms before opening it in the
                // browser (an outward action).
                self.pending_open = Some(url);
                self.nav.anchor_sel = None;
            }
            Anchor::CrossRef(id) => {
                if self.goto_target(&id) {
                    self.flash = Some("→ cross-reference (Ctrl+o to return)".to_string());
                } else {
                    self.flash = Some(format!("cross-reference target #{id} not found"));
                }
            }
            Anchor::Citation(key) => {
                if self.goto_target(&key) {
                    self.flash = Some("→ citation (Ctrl+o to return)".to_string());
                } else {
                    self.flash = Some(format!("citation [{key}] not found"));
                }
            }
        }
        true
    }

    /// Jump to a footnote definition for reference `target`: current section
    /// first, then any other section (endnotes collected elsewhere), pushing
    /// history so Ctrl+o returns to the reference.
    fn follow_footnote(&mut self, target: &str) {
        if let Some(line) = self.footnote_line_here(target) {
            self.push_history();
            self.scroll = line;
            self.scroll_pending = 0;
            self.clamp_scroll();
            self.nav.anchor_sel = None;
            self.flash = Some("→ footnote (Ctrl+o to return)".to_string());
        } else if let Some(sec) = self.find_footnote_section(target) {
            self.push_history();
            self.load(sec);
            self.ensure_wrapped(self.last_measure.max(1));
            let line = self.footnote_line_here(target).unwrap_or(0);
            self.scroll = line;
            self.scroll_pending = 0;
            self.clamp_scroll();
            self.nav.anchor_sel = None;
            self.flash = Some("→ endnote (Ctrl+o to return)".to_string());
        } else {
            self.flash = Some("footnote definition not found".to_string());
        }
    }

    /// The definition line in the *current* section for a footnote `target`.
    fn footnote_line_here(&self, target: &str) -> Option<usize> {
        match find_footnote(&self.blocks, target)? {
            Block::Footnote { id, .. } => self.nav.footnote_def_line.get(id).copied(),
            _ => None,
        }
    }

    /// The first other section whose blocks define footnote `target` (endnotes).
    /// Decodes sections on demand — only when the footnote isn't defined locally.
    fn find_footnote_section(&mut self, target: &str) -> Option<usize> {
        let here = self.section;
        (0..self.doc.section_count())
            .find(|&sec| sec != here && find_footnote(&self.fetch_blocks(sec), target).is_some())
    }

    /// The text locator for element `id` (`#`-fragment) in section `sec`, caching
    /// the last section's targets so repeated current-section lookups are cheap.
    fn target_locator(&mut self, sec: usize, frag: &str) -> Option<String> {
        if self.nav.targets_cache.as_ref().map(|(s, _)| *s) != Some(sec) {
            self.nav.targets_cache = Some((sec, self.doc.section_targets(sec)));
        }
        let (_, list) = self.nav.targets_cache.as_ref()?;
        list.iter()
            .find(|(id, _)| id == frag)
            .map(|(_, l)| l.clone())
    }

    /// The first *other* section that defines element `frag` — only used when a
    /// reference targets another file (the current section is tried first, since
    /// EPUB fragment ids are file-scoped and a bare `#id` is always local).
    fn find_target_section(&mut self, frag: &str) -> Option<usize> {
        let here = self.section;
        (0..self.doc.section_count()).find(|&sec| {
            sec != here
                && self
                    .doc
                    .section_targets(sec)
                    .iter()
                    .any(|(id, _)| id == frag)
        })
    }

    /// Jump to a cross-reference / citation target `href` (`#frag`, `file#frag`,
    /// or `file`), pushing history for return. A bare `#frag` is local (EPUB ids
    /// are file-scoped); a `file#frag` resolves the file to its spine section
    /// (not by scanning the colliding id), then the fragment within it.
    fn goto_target(&mut self, href: &str) -> bool {
        let file = href.split('#').next().unwrap_or("").trim();
        let frag = href
            .split('#')
            .nth(1)
            .map(str::trim)
            .filter(|s| !s.is_empty());

        if file.is_empty() {
            // Same-file fragment → current section; the id must exist here.
            let Some(loc) = frag.and_then(|f| self.target_locator(self.section, f)) else {
                return false;
            };
            self.push_history();
            if let Some(line) = find_target_line(&self.lines, &loc) {
                self.scroll = line;
                self.scroll_pending = 0;
                self.clamp_scroll();
            }
            self.nav.anchor_sel = None;
            return true;
        }

        // Cross-file: resolve the file to its section (fall back to an id scan
        // only if the path doesn't resolve), then locate the fragment within it.
        let Some(sec) = self
            .doc
            .section_for_href(self.section, href)
            .or_else(|| frag.and_then(|f| self.find_target_section(f)))
        else {
            return false;
        };
        self.push_history();
        if sec != self.section {
            self.load(sec);
            self.ensure_wrapped(self.last_measure.max(1));
        }
        let line = frag
            .and_then(|f| self.target_locator(sec, f))
            .and_then(|loc| find_target_line(&self.lines, &loc))
            .unwrap_or(0);
        self.scroll = line;
        self.scroll_pending = 0;
        self.clamp_scroll();
        self.nav.anchor_sel = None;
        true
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// An external link the user just activated (to confirm + open in browser).
    pub fn take_pending_open(&mut self) -> Option<String> {
        self.pending_open.take()
    }

    /// Raw lines of the `n`-th code block in the current section.
    fn code_block(&self, n: usize) -> Option<&[String]> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Code { lines, .. } => Some(lines.as_slice()),
                _ => None,
            })
            .nth(n)
    }

    /// The "rich element" kind a display line belongs to (code/table/math/figure/
    /// footnote), or `None` for prose.
    fn element_label(kind: LineKind) -> Option<&'static str> {
        match kind {
            LineKind::Code(_) => Some("code"),
            LineKind::Table { .. } => Some("table"),
            LineKind::Math => Some("math"),
            LineKind::Image(_) => Some("figure"),
            LineKind::Footnote(_) => Some("footnote"),
            _ => None,
        }
    }

    /// `(display-line, kind-label)` for the first line of each rich element
    /// (code/table/math/figure/footnote) in the section, in document order.
    fn element_starts(&self) -> Vec<(usize, &'static str)> {
        let mut starts = Vec::new();
        let mut prev: Option<&'static str> = None;
        for (i, l) in self.lines.iter().enumerate() {
            let cur = Self::element_label(l.kind);
            if let Some(lbl) = cur
                && prev != Some(lbl)
            {
                starts.push((i, lbl));
            }
            prev = cur;
        }
        starts
    }

    /// Jump to the next (`forward`) or previous rich element (code/table/math/
    /// figure/footnote) in the chapter, flashing "`kind N/M`". Returns whether
    /// it moved.
    fn jump_element(&mut self, forward: bool) -> bool {
        self.ensure_wrapped(self.last_measure.max(1));
        let starts = self.element_starts();
        let pos = if forward {
            starts.iter().position(|(line, _)| *line > self.scroll)
        } else {
            starts.iter().rposition(|(line, _)| *line < self.scroll)
        };
        match pos {
            Some(i) => {
                let (line, label) = starts[i];
                self.push_history();
                self.scroll = line;
                self.scroll_pending = 0;
                self.clamp_scroll();
                self.flash = Some(format!("{label} {}/{}", i + 1, starts.len()));
                true
            }
            None => {
                self.flash = Some(if starts.is_empty() {
                    "no code/tables/figures in this chapter".to_string()
                } else if forward {
                    "no elements below — G or J for more".to_string()
                } else {
                    "no elements above".to_string()
                });
                false
            }
        }
    }

    /// Jump to the next rich element in the chapter (key `w`).
    pub fn next_element(&mut self) -> bool {
        self.jump_element(true)
    }

    /// Jump to the previous rich element in the chapter (key `b`).
    pub fn prev_element(&mut self) -> bool {
        self.jump_element(false)
    }

    /// Copy the code block currently in view (the topmost visible one) to the
    /// system clipboard. Returns the number of lines copied.
    pub fn copy_visible_code(&mut self) -> Option<usize> {
        let end = (self.scroll + self.viewport_lines).min(self.lines.len());
        let idx = self.lines[self.scroll.min(self.lines.len())..end]
            .iter()
            .find_map(|l| match l.kind {
                crate::layout::LineKind::Code(i) => Some(i),
                _ => None,
            })?;
        let lines = self.code_block(idx)?;
        let text = lines.join("\n");
        let n = lines.len();
        self.pending_clipboard = Some(text);
        self.flash = Some(format!(
            "✓ copied {n} line{} of code",
            if n == 1 { "" } else { "s" }
        ));
        Some(n)
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

    /// Scroll down, flowing into the next chapter at the bottom edge.
    pub fn scroll_down(&mut self, n: usize) {
        let max = self.max_scroll();
        if self.scroll < max {
            self.scroll = (self.scroll + n).min(max);
        } else if !self.chapter_lock && self.section + 1 < self.doc.section_count() {
            self.load(self.section + 1);
        }
    }

    /// Scroll up, flowing into the previous chapter at the top edge (unless
    /// chapter-locked).
    pub fn scroll_up(&mut self, n: usize) {
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_sub(n);
        } else if !self.chapter_lock && self.section > 0 {
            self.load(self.section - 1);
            self.scroll = usize::MAX; // clamped to the bottom on next draw
        }
    }

    /// Total pages in the current section (for the page indicator).
    pub fn page_count(&self) -> usize {
        self.lines.len().div_ceil(self.page_lines.max(1)).max(1)
    }

    /// 1-based page number of the current position within the section.
    pub fn current_page(&self) -> usize {
        self.scroll / self.page_lines.max(1) + 1
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
    /// page rather than flashing an unthemed one). See [`Reader::sync_pages`].
    pub fn page_png(&self, section: usize) -> Option<Vec<u8>> {
        if self.pages.policy.mode == media::ImageMode::Faithful {
            return self.raster_png(section);
        }
        self.pages
            .themed
            .peek(&(section, self.pages.policy))
            .map(|b| b.as_ref().clone())
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
    /// pages): the spread's pages (cover-offset aware) in `spread` mode, else the
    /// current page alone.
    pub fn visible_sections(&self, spread: bool) -> Vec<usize> {
        if spread {
            self.spread_pages()
        } else {
            vec![self.section]
        }
    }

    /// Whether `section`'s page is rasterized (the raw PNG is cached) — the
    /// expensive PDFium step, on top of which theming runs. Doesn't clone.
    fn raster_ready(&self, section: usize) -> bool {
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
            self.pages.themed.contains(&(section, self.pages.policy))
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

    /// The visible pages the deck should place — all of them once every one is
    /// ready (an atomic spread swap), otherwise none (hold the previous pages).
    pub fn placeable_sections(&self, spread: bool) -> Vec<usize> {
        let v = self.visible_sections(spread);
        if v.iter().all(|&s| self.page_ready(s)) {
            v
        } else {
            Vec::new()
        }
    }

    /// Collect any pages the background loader and the page themer have finished
    /// into their caches. Driven each frame in PDF mode (the direct path skips
    /// `sync_images`, which is what otherwise drains the loader).
    pub fn poll_loader(&mut self) {
        self.drain_loader();
        self.drain_page_themer();
    }

    /// Snap the position to the start of its page (so paged mode shows a clean
    /// page boundary after toggling in or resizing).
    pub fn snap_to_page(&mut self) {
        let page = self.page_lines.max(1);
        self.scroll = self.scroll / page * page;
        self.scroll_pending = 0;
    }

    /// The left page of the two-page tile containing `section`. Without a cover
    /// offset, tiles are (0,1),(2,3)…; with one, page 0 is alone, then (1,2),
    /// (3,4)… so the left page is odd.
    fn spread_left(&self, section: usize) -> usize {
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

    /// The page(s) of the current two-page spread: one for a lone page (the cover
    /// under a cover offset, or a trailing odd page), else the facing pair.
    pub fn spread_pages(&self) -> Vec<usize> {
        let left = self.spread_left(self.section);
        if self.cover_offset && left == 0 {
            return vec![0];
        }
        let mut v = vec![left];
        if left + 1 < self.doc.section_count() {
            v.push(left + 1);
        }
        v
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

    fn table() -> Block {
        Block::Table {
            header: Some(vec![vec![Span::plain("H")]]),
            rows: vec![vec![vec![Span::plain("r")]]],
        }
    }
    fn math() -> Block {
        Block::Math {
            tex: r"\alpha".to_string(),
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
}
