//! The reading view model: a paginated, image-aware, searchable view over one
//! open `Document`. Owns section decoding (background loader + LRU cache), line
//! wrapping, the TOC sidebar with scroll-spy, image protocol lifecycle,
//! navigation history, and in-book search. Pure view-model — no terminal I/O.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use anyhow::Result;
use lru::LruCache;

use crate::document::{Block, Document, OutlineItem};
use crate::layout::{DisplayLine, LineKind, WrapOpts, wrap_blocks};
use crate::media::{self, ImageBuilder, ImagePlan, ImgKey};
use crate::search::{Matcher, SearchMode};
use crate::theme;
use delryn_model::{Anchor, find_footnote};
use ratatui_image::picker::Picker;

use super::{CACHE_CAP, Focus, IMAGE_CACHE_CAP};

// Separable reader concerns; each contributes an `impl Reader` block and reaches
// the core's helpers (find_line, fetch_blocks, …) via the parent module.
mod images;
mod search;
mod sidebar;

/// A reading position, for the navigation (back/forward) history.
#[derive(Clone, Copy)]
struct Pos {
    section: usize,
    scroll: usize,
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
    /// Wrapped display lines of the current section, valid for `wrap_width`.
    pub lines: Vec<DisplayLine>,
    pub wrap_width: usize,
    /// syntect theme desired for code (set each render from the active theme).
    pub code_theme: String,
    /// syntect theme the current `lines` were wrapped with.
    wrap_theme: String,
    /// Desired spacing (set each render from config).
    pub line_spacing: u8,
    pub paragraph_spacing: u8,
    wrap_line_spacing: u8,
    wrap_para_spacing: u8,
    /// Code rendering (set each render from config / panning).
    pub code_wrap: bool,
    pub code_hscroll: usize,
    wrap_code_wrap: bool,
    wrap_code_hscroll: usize,
    /// Word-wrap table cells (set each render from config).
    pub table_wrap: bool,
    wrap_table_wrap: bool,
    /// Keep scrolling within the current chapter (set each render from config).
    pub chapter_lock: bool,
    /// Cached (outline index, line) for the current section's entries, recomputed
    /// on re-wrap; drives the TOC scroll-spy cheaply.
    heading_lines: Vec<(usize, usize)>,
    /// Followable inline anchors in reading order (rebuilt on re-wrap).
    anchors: Vec<AnchorHit>,
    /// Footnote id → its definition's first display line (rebuilt on re-wrap).
    footnote_def_line: HashMap<String, usize>,
    /// Cross-reference/citation targets for one section: `(section, id→locator)`,
    /// cached so repeated lookups in the current section don't re-parse it.
    targets_cache: Option<(usize, Vec<(String, String)>)>,
    /// Link-cursor position: index into `anchors`, set in link-follow mode.
    anchor_sel: Option<usize>,
    /// Built image protocols, reused across sections (revisiting a section
    /// reuses the already-uploaded image instead of re-transmitting). LRU.
    image_cache: LruCache<ImgKey, ImagePlan>,
    /// Current section's image index → cache key.
    section_images: HashMap<usize, ImgKey>,
    /// Reserved rows per image index, estimated up front so reflow doesn't wait
    /// on the background build.
    image_rows_estimate: Vec<u16>,
    /// (section, avail-cols, max-rows, max-px) the current estimates are for.
    images_key: (usize, u16, u16, u16),
    /// Theme tint + mode the current image builds used; a change re-requests them
    /// so images re-render when the theme cycles or the image mode changes.
    images_policy: media::RenderPolicy,
    /// Image builds currently in flight (avoid dispatching duplicates).
    img_requested: HashSet<ImgKey>,
    /// Image builds that failed (so we stop waiting / re-requesting).
    img_failed: HashSet<ImgKey>,
    /// Terminal image ids evicted from the cache, to be deleted from the
    /// terminal by the main loop.
    pending_deletes: Vec<u32>,
    /// Text queued to be copied to the system clipboard by the main loop.
    pending_clipboard: Option<String>,
    /// A transient status-bar message (e.g. "copied"), cleared on next key.
    pub flash: Option<String>,
    /// images_key the current `lines` were wrapped against.
    wrap_images_key: (usize, u16, u16, u16),
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
    /// Last active (scroll-spy) row the TOC auto-followed to.
    last_active: Option<usize>,
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
    /// Collapsed parent rows (outline indices) in the sidebar tree.
    collapsed: HashSet<usize>,
    /// Navigation history (jump list).
    back_stack: Vec<Pos>,
    fwd_stack: Vec<Pos>,
    /// In-book search state.
    pub searching: bool,
    pub search_input: String,
    pub search_mode: SearchMode,
    /// The active matcher (set when a search runs); drives highlighting.
    pub search_matcher: Option<Matcher>,
    search_matches: Vec<(usize, usize)>,
    pub search_idx: usize,
    /// Recent queries, most-recent last; recalled with Up/Down in the prompt.
    search_history: Vec<String>,
    /// Position while browsing history in the prompt (None = editing fresh).
    pub(crate) history_pos: Option<usize>,
    /// Decoded section blocks, keyed by section index (bounded LRU).
    cache: HashMap<usize, Vec<Block>>,
    /// Sections requested from the loader but not yet returned.
    requested: HashSet<usize>,
    /// Channel to ask the background loader for a section.
    req_tx: Sender<usize>,
    /// Channel of decoded sections from the background loader.
    res_rx: Receiver<(usize, Vec<Block>)>,
}

impl Reader {
    pub fn new(mut doc: Box<dyn Document>) -> Result<Self> {
        let outline = doc.outline().to_vec();

        // Background loader: a worker thread that decodes sections on request.
        let mut loader = doc.loader();
        let (req_tx, req_rx) = std::sync::mpsc::channel::<usize>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<(usize, Vec<Block>)>();
        thread::spawn(move || {
            while let Ok(index) = req_rx.recv() {
                let blocks = loader.load(index);
                if res_tx.send((index, blocks)).is_err() {
                    break; // reader dropped
                }
            }
        });

        // Open at the body-matter start (skipping front matter) when the book
        // declares it; saved progress, if any, overrides this afterwards.
        let start = doc
            .start_section()
            .min(doc.section_count().saturating_sub(1));
        let first = doc.load_section(start).unwrap_or_default().blocks;
        let mut cache = HashMap::new();
        cache.insert(start, first.clone());

        let mut reader = Self {
            doc,
            outline,
            section: start,
            blocks: first,
            lines: Vec::new(),
            wrap_width: 0,
            code_theme: theme::default_theme().syntect.to_string(),
            wrap_theme: String::new(),
            line_spacing: 0,
            paragraph_spacing: 1,
            wrap_line_spacing: 0,
            wrap_para_spacing: 1,
            code_wrap: true,
            code_hscroll: 0,
            wrap_code_wrap: true,
            wrap_code_hscroll: 0,
            table_wrap: true,
            wrap_table_wrap: true,
            chapter_lock: false,
            heading_lines: Vec::new(),
            anchors: Vec::new(),
            footnote_def_line: HashMap::new(),
            targets_cache: None,
            anchor_sel: None,
            image_cache: LruCache::new(NonZeroUsize::new(IMAGE_CACHE_CAP).unwrap()),
            section_images: HashMap::new(),
            image_rows_estimate: Vec::new(),
            images_key: (usize::MAX, 0, 0, 0),
            images_policy: media::RenderPolicy {
                tint: media::Ink {
                    ink: [0, 0, 0],
                    paper: [255, 255, 255],
                },
                mode: media::ImageMode::default(),
            },
            img_requested: HashSet::new(),
            img_failed: HashSet::new(),
            pending_deletes: Vec::new(),
            pending_clipboard: None,
            flash: None,
            wrap_images_key: (usize::MAX, 0, 0, 0),
            scroll: 0,
            scroll_pending: 0,
            focus: Focus::Content,
            sidebar_sel: 0,
            sidebar_offset: 0,
            sidebar_h: 1,
            last_active: None,
            viewport_lines: 1,
            page_lines: 1,
            last_measure: 72,
            pending_frac: None,
            overlay_occlude: None,
            collapsed: HashSet::new(),
            back_stack: Vec::new(),
            fwd_stack: Vec::new(),
            searching: false,
            search_input: String::new(),
            search_mode: SearchMode::Plain,
            search_matcher: None,
            search_matches: Vec::new(),
            search_idx: 0,
            search_history: Vec::new(),
            history_pos: None,
            cache,
            requested: HashSet::new(),
            req_tx,
            res_rx,
        };
        reader.prefetch_neighbors();
        Ok(reader)
    }

    /// Collect any sections the loader has finished into the cache.
    fn drain_loader(&mut self) {
        while let Ok((index, blocks)) = self.res_rx.try_recv() {
            self.requested.remove(&index);
            self.cache.insert(index, blocks);
        }
    }

    /// Blocks for a section: cache first, else decode synchronously.
    fn fetch_blocks(&mut self, section: usize) -> Vec<Block> {
        self.drain_loader();
        if let Some(blocks) = self.cache.get(&section) {
            return blocks.clone();
        }
        let blocks = self
            .doc
            .load_section(section)
            .map(|s| s.blocks)
            .unwrap_or_default();
        self.cache.insert(section, blocks.clone());
        blocks
    }

    /// Ask the loader to pre-decode the adjacent chapters, and bound the cache.
    fn prefetch_neighbors(&mut self) {
        self.drain_loader();
        let n = self.doc.section_count();
        let mut targets = Vec::new();
        if self.section + 1 < n {
            targets.push(self.section + 1);
        }
        if self.section > 0 {
            targets.push(self.section - 1);
        }
        for t in targets {
            if !self.cache.contains_key(&t) && self.requested.insert(t) {
                let _ = self.req_tx.send(t);
            }
        }
        self.evict();
    }

    /// Drop cached sections farthest from the current one when over capacity.
    fn evict(&mut self) {
        while self.cache.len() > CACHE_CAP {
            let current = self.section;
            match self
                .cache
                .keys()
                .copied()
                .filter(|&k| k != current)
                .max_by_key(|&k| k.abs_diff(current))
            {
                Some(far) => {
                    self.cache.remove(&far);
                }
                None => break,
            }
        }
    }

    /// Re-wrap the current section if the measure changed.
    pub fn ensure_wrapped(&mut self, width: usize) {
        if width != self.wrap_width
            || self.code_theme != self.wrap_theme
            || self.line_spacing != self.wrap_line_spacing
            || self.paragraph_spacing != self.wrap_para_spacing
            || self.code_wrap != self.wrap_code_wrap
            || self.code_hscroll != self.wrap_code_hscroll
            || self.table_wrap != self.wrap_table_wrap
            || self.images_key != self.wrap_images_key
        {
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
                },
                &self.image_rows_estimate,
            );
            self.wrap_width = width;
            self.wrap_theme = self.code_theme.clone();
            self.wrap_line_spacing = self.line_spacing;
            self.wrap_para_spacing = self.paragraph_spacing;
            self.wrap_code_wrap = self.code_wrap;
            self.wrap_code_hscroll = self.code_hscroll;
            self.wrap_table_wrap = self.table_wrap;
            self.wrap_images_key = self.images_key;
            self.recompute_heading_lines();
            self.recompute_anchors();
        }
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
        self.heading_lines = hl;
    }

    /// Rebuild the inline-anchor index and footnote definition map from the
    /// freshly wrapped lines. Done once per re-wrap.
    fn recompute_anchors(&mut self) {
        // Anchors in reading order, merging adjacent runs that share one anchor
        // (e.g. a multi-glyph link) into a single followable target.
        let mut hits: Vec<AnchorHit> = Vec::new();
        for (li, line) in self.lines.iter().enumerate() {
            let mut col = 0usize;
            for run in &line.runs {
                let len = run.text.chars().count();
                if let Some(a) = &run.anchor {
                    match hits.last_mut() {
                        Some(last) if last.line == li && last.end == col && last.anchor == *a => {
                            last.end += len;
                        }
                        _ => hits.push(AnchorHit {
                            line: li,
                            start: col,
                            end: col + len,
                            anchor: a.clone(),
                        }),
                    }
                }
                col += len;
            }
        }
        self.anchors = hits;
        self.anchor_sel = self.anchor_sel.filter(|&i| i < self.anchors.len());

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
        self.footnote_def_line = map;
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
        if self.anchors.is_empty() {
            self.flash = Some("no links or footnotes in this chapter".to_string());
            return;
        }
        let n = self.anchors.len();
        let next = match self.anchor_sel {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None if forward => self
                .anchors
                .iter()
                .position(|a| a.line >= self.scroll)
                .unwrap_or(0),
            None => self
                .anchors
                .iter()
                .rposition(|a| a.line < self.scroll + self.page_lines.max(1))
                .unwrap_or(n - 1),
        };
        self.anchor_sel = Some(next);
        self.scroll_into_view(self.anchors[next].line);
        let kind = anchor_kind_label(&self.anchors[next].anchor);
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
        self.anchor_sel.and_then(|i| self.anchors.get(i))
    }

    /// Clear the link cursor; returns whether anything was selected (so the key
    /// is "consumed" only when it actually dismissed the cursor).
    pub fn clear_anchor(&mut self) -> bool {
        self.anchor_sel.take().is_some()
    }

    /// Follow the selected anchor: footnote ref → its definition (with history for
    /// return); link → copy the URL; cross-ref/citation → a status note (jump
    /// targets aren't indexed yet). Returns whether an anchor was selected.
    pub fn activate_anchor(&mut self) -> bool {
        let Some(i) = self.anchor_sel else {
            return false;
        };
        let Some(hit) = self.anchors.get(i) else {
            return false;
        };
        match hit.anchor.clone() {
            Anchor::Footnote(target) => self.follow_footnote(&target),
            Anchor::Link(url) => {
                let shown: String = if url.chars().count() > 48 {
                    format!("{}…", url.chars().take(47).collect::<String>())
                } else {
                    url.clone()
                };
                self.pending_clipboard = Some(url);
                self.anchor_sel = None;
                self.flash = Some(format!("copied link: {shown}"));
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
            self.anchor_sel = None;
            self.flash = Some("→ footnote (Ctrl+o to return)".to_string());
        } else if let Some(sec) = self.find_footnote_section(target) {
            self.push_history();
            self.load(sec);
            self.ensure_wrapped(self.last_measure.max(1));
            let line = self.footnote_line_here(target).unwrap_or(0);
            self.scroll = line;
            self.scroll_pending = 0;
            self.clamp_scroll();
            self.anchor_sel = None;
            self.flash = Some("→ endnote (Ctrl+o to return)".to_string());
        } else {
            self.flash = Some("footnote definition not found".to_string());
        }
    }

    /// The definition line in the *current* section for a footnote `target`.
    fn footnote_line_here(&self, target: &str) -> Option<usize> {
        match find_footnote(&self.blocks, target)? {
            Block::Footnote { id, .. } => self.footnote_def_line.get(id).copied(),
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
        if self.targets_cache.as_ref().map(|(s, _)| *s) != Some(sec) {
            self.targets_cache = Some((sec, self.doc.section_targets(sec)));
        }
        let (_, list) = self.targets_cache.as_ref()?;
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
    /// or `file`). Resolves the current section first (file-scoped ids), then any
    /// other section; pushes history for return. Returns whether it resolved.
    fn goto_target(&mut self, href: &str) -> bool {
        let frag = href.rsplit('#').next().unwrap_or(href).trim();
        if frag.is_empty() {
            return false;
        }
        // Current section first — a bare `#id` is always local.
        if let Some(loc) = self.target_locator(self.section, frag) {
            self.push_history();
            if let Some(line) = find_target_line(&self.lines, &loc) {
                self.scroll = line;
                self.scroll_pending = 0;
                self.clamp_scroll();
            }
            self.anchor_sel = None;
            return true;
        }
        // Otherwise, a cross-file reference: find the section that defines it.
        if let Some(sec) = self.find_target_section(frag) {
            self.push_history();
            self.load(sec);
            self.ensure_wrapped(self.last_measure.max(1));
            let line = self
                .target_locator(sec, frag)
                .and_then(|loc| find_target_line(&self.lines, &loc))
                .unwrap_or(0);
            self.scroll = line;
            self.scroll_pending = 0;
            self.clamp_scroll();
            self.anchor_sel = None;
            return true;
        }
        false
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
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
        self.section = section;
        self.blocks = self.fetch_blocks(section);
        self.scroll = 0;
        self.anchor_sel = None; // a new section has a different anchor set
        self.wrap_width = 0; // force a re-wrap on next draw
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

    fn push_history(&mut self) {
        self.back_stack.push(Pos {
            section: self.section,
            scroll: self.scroll,
        });
        if self.back_stack.len() > 200 {
            self.back_stack.remove(0);
        }
        self.fwd_stack.clear();
    }

    pub fn history_back(&mut self) {
        if let Some(pos) = self.back_stack.pop() {
            self.fwd_stack.push(Pos {
                section: self.section,
                scroll: self.scroll,
            });
            self.goto(pos);
        }
    }

    pub fn history_forward(&mut self) {
        if let Some(pos) = self.fwd_stack.pop() {
            self.back_stack.push(Pos {
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

    /// Apply a pending resume fraction once the section is wrapped.
    pub fn resolve_pending(&mut self) {
        if let Some(frac) = self.pending_frac.take() {
            let n = self.lines.len();
            self.scroll = ((frac * n as f32).round() as usize).min(n.saturating_sub(1));
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
    }

    impl MockDoc {
        fn new(sections: Vec<Vec<Block>>) -> Self {
            MockDoc {
                sections,
                meta: Metadata::default(),
                toc: Vec::new(),
                outline: Vec::new(),
            }
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
}
