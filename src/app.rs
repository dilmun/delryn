//! Application state and event dispatch.
//!
//! Two top-level modes behaving like tabs (Library | Reader). For now the
//! Library is a stub; the Reader is the working EPUB vertical slice. See
//! `DESIGN.md` §4, §6.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Instant;

use lru::LruCache;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::config::Config;
use crate::document::epub::EpubDocument;
use crate::document::{Block, Document, OutlineItem};
use crate::input::{self, Action, Pending};
use crate::layout::{DisplayLine, WrapOpts, wrap_blocks};
use crate::library;
use crate::media::{self, ImageBuilder, ImagePlan, ImageView, ImgKey};
use crate::search::{Matcher, SearchMode};
use crate::store::{Annotation, BookRow, LibrarySection, Store};
use crate::theme;
use ratatui_image::picker::Picker;

/// Number of decoded sections kept in memory (current ± neighbours).
const CACHE_CAP: usize = 9;
/// Number of built image protocols kept in memory / GPU-resident in the
/// terminal. Reused across section revisits; LRU-evicted (and deleted from the
/// terminal) beyond this.
const IMAGE_CACHE_CAP: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Library,
    Reader,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Content,
    Sidebar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Reading,
    Library,
}

impl SettingsTab {
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Reading => "Reading",
            SettingsTab::Library => "Library",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SettingsTab::General => SettingsTab::Reading,
            SettingsTab::Reading => SettingsTab::Library,
            SettingsTab::Library => SettingsTab::General,
        }
    }
}

/// Open settings popup state.
pub struct Settings {
    pub tab: SettingsTab,
    pub row: usize,
}

/// Open annotations (bookmarks/notes) overlay state.
pub struct AnnotState {
    pub items: Vec<Annotation>,
    pub sel: usize,
}

/// Labelled (name, value) rows for a settings tab, rendered by the popup.
pub fn settings_rows(config: &Config, tab: SettingsTab) -> Vec<(String, String)> {
    let onoff = |b: bool| if b { "on" } else { "off" }.to_string();
    match tab {
        SettingsTab::Reading => vec![
            ("Theme".into(), config.theme.name.to_string()),
            ("View mode".into(), config.view_mode.label().to_string()),
            ("Side margin %".into(), config.side_padding.to_string()),
            ("Line spacing".into(), config.line_spacing.to_string()),
            ("Paragraph spacing".into(), config.paragraph_spacing.to_string()),
            ("Sidebar by default".into(), onoff(config.show_sidebar)),
            ("Status bar by default".into(), onoff(config.show_status)),
            ("Status · theme".into(), onoff(config.status.theme)),
            ("Status · view".into(), onoff(config.status.view)),
            ("Status · position".into(), onoff(config.status.position)),
            ("Status · percent".into(), onoff(config.status.percent)),
            ("Status · gauge".into(), onoff(config.status.gauge)),
            (
                "Image max px".into(),
                if config.image_max_px == 0 {
                    "off".into()
                } else {
                    config.image_max_px.to_string()
                },
            ),
            ("Code wrap".into(), onoff(config.code_wrap)),
            ("Chapter lock".into(), onoff(config.chapter_lock)),
        ],
        SettingsTab::General => vec![("Mouse".into(), onoff(config.mouse_enabled))],
        SettingsTab::Library => vec![],
    }
}

/// Rects from the last render, used for mouse hit-testing.
#[derive(Default)]
pub struct LayoutRects {
    pub sidebar: Option<Rect>,
    pub content: Option<Rect>,
}

/// A reading position, for the navigation (back/forward) history.
#[derive(Clone, Copy)]
struct Pos {
    section: usize,
    scroll: usize,
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
    /// Keep scrolling within the current chapter (set each render from config).
    pub chapter_lock: bool,
    /// Cached (outline index, line) for the current section's entries, recomputed
    /// on re-wrap; drives the TOC scroll-spy cheaply.
    heading_lines: Vec<(usize, usize)>,
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
    history_pos: Option<usize>,
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

        let first = doc.load_section(0).unwrap_or_default().blocks;
        let mut cache = HashMap::new();
        cache.insert(0usize, first.clone());

        let mut reader = Self {
            doc,
            outline,
            section: 0,
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
            chapter_lock: false,
            heading_lines: Vec::new(),
            image_cache: LruCache::new(NonZeroUsize::new(IMAGE_CACHE_CAP).unwrap()),
            section_images: HashMap::new(),
            image_rows_estimate: Vec::new(),
            images_key: (usize::MAX, 0, 0, 0),
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
                },
                &self.image_rows_estimate,
            );
            self.wrap_width = width;
            self.wrap_theme = self.code_theme.clone();
            self.wrap_line_spacing = self.line_spacing;
            self.wrap_para_spacing = self.paragraph_spacing;
            self.wrap_code_wrap = self.code_wrap;
            self.wrap_code_hscroll = self.code_hscroll;
            self.wrap_images_key = self.images_key;
            self.recompute_heading_lines();
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

    /// Collect any finished background image builds, and — when the section or
    /// size changes — estimate each image's rows (cheaply, for reflow) and
    /// dispatch the protocol builds to the worker. Never blocks on encoding.
    pub fn sync_images(
        &mut self,
        builder: &ImageBuilder,
        picker: &Picker,
        avail: u16,
        max_rows: u16,
        max_px: u16,
    ) {
        // Tell the worker where we are so it can drop builds for far-away
        // sections (avoids a fast-scroll backlog delaying the current one).
        builder.set_current(self.section);

        // 1. Move finished builds into the cache; evictions free the terminal image.
        for done in builder.poll() {
            self.img_requested.remove(&done.key);
            if done.stale {
                continue; // skipped as far-away; re-requested if it's needed again
            }
            match done.plan {
                Some(plan) => {
                    if let Some((_, evicted)) = self.image_cache.push(done.key, plan) {
                        if let Some(id) = evicted.image_id() {
                            self.pending_deletes.push(id);
                        }
                    }
                }
                None => {
                    self.img_failed.insert(done.key);
                }
            }
        }

        // 2. On section/size change, remap the current section and dispatch any
        //    builds it still needs.
        let key = (self.section, avail, max_rows, max_px);
        if self.images_key != key {
            self.images_key = key;
            self.remap_section_images(builder, picker, avail, max_rows, max_px);
        }

        // 3. Keep the visible section's images most-recently-used so they aren't
        //    evicted while on screen.
        let keys: Vec<ImgKey> = self.section_images.values().copied().collect();
        for k in keys {
            self.image_cache.get(&k);
        }

        // 4. Pre-build neighbouring sections' images once the current one is ready.
        if !self.images_pending() {
            self.prefetch_neighbor_images(builder, avail, max_rows, max_px);
        }
    }

    /// Map the current section's images to cache keys, estimate their rows for
    /// reflow, and request builds for any not already cached/in-flight/failed.
    fn remap_section_images(
        &mut self,
        builder: &ImageBuilder,
        picker: &Picker,
        avail: u16,
        max_rows: u16,
        max_px: u16,
    ) {
        let fs = picker.font_size();
        let (fw, fh) = (fs.width, fs.height);
        let mut section_images = HashMap::new();
        let mut estimates = Vec::new();
        let mut requests: Vec<(ImgKey, Vec<u8>)> = Vec::new();
        let mut idx = 0;
        for block in &self.blocks {
            if let Block::Image { data, .. } = block {
                let key = ImgKey { section: self.section, idx, avail, max_rows, max_px };
                let rows = if let Some(plan) = self.image_cache.peek(&key) {
                    plan.rows
                } else if data.is_empty() {
                    0
                } else {
                    media::image_dimensions(data)
                        .map(|(w, h)| media::target_cells(w, h, fw, fh, avail, max_rows, max_px).1)
                        .unwrap_or(0)
                };
                estimates.push(rows);
                section_images.insert(idx, key);
                if rows > 0
                    && !self.image_cache.contains(&key)
                    && !self.img_requested.contains(&key)
                    && !self.img_failed.contains(&key)
                {
                    requests.push((key, data.clone()));
                }
                idx += 1;
            }
        }
        self.section_images = section_images;
        self.image_rows_estimate = estimates;
        for (k, bytes) in requests {
            self.img_requested.insert(k);
            builder.request(k, bytes);
        }
    }

    /// Build the adjacent sections' images ahead of time (from already-prefetched
    /// blocks) so crossing a chapter boundary is instant. Never forces a load.
    fn prefetch_neighbor_images(
        &mut self,
        builder: &ImageBuilder,
        avail: u16,
        max_rows: u16,
        max_px: u16,
    ) {
        let n = self.doc.section_count();
        let neighbors = [self.section + 1, self.section.wrapping_sub(1)];
        let mut requests: Vec<(ImgKey, Vec<u8>)> = Vec::new();
        for &sec in &neighbors {
            if sec >= n || sec == self.section {
                continue;
            }
            let Some(blocks) = self.cache.get(&sec) else {
                continue;
            };
            let mut idx = 0;
            for block in blocks {
                if let Block::Image { data, .. } = block {
                    if !data.is_empty() {
                        let key = ImgKey { section: sec, idx, avail, max_rows, max_px };
                        if !self.image_cache.contains(&key)
                            && !self.img_requested.contains(&key)
                            && !self.img_failed.contains(&key)
                        {
                            requests.push((key, data.clone()));
                        }
                    }
                    idx += 1;
                }
            }
        }
        for (k, bytes) in requests {
            self.img_requested.insert(k);
            builder.request(k, bytes);
        }
    }

    /// Look up a built plan for the current section's image `idx`.
    pub fn image_plan(&self, idx: usize) -> Option<&ImagePlan> {
        let key = self.section_images.get(&idx)?;
        self.image_cache.peek(key)
    }

    /// Drain terminal image ids that should be deleted (evicted from cache).
    pub fn take_image_deletes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending_deletes)
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
        self.flash = Some(format!("✓ copied {n} line{} of code", if n == 1 { "" } else { "s" }));
        Some(n)
    }

    /// Whether a smooth scroll is currently in progress (so heavy image
    /// transmits can be deferred until it settles).
    pub fn is_scrolling(&self) -> bool {
        self.scroll_pending != 0
    }

    /// Are any of the current section's images still building (so the loop
    /// should keep redrawing until they pop in)?
    pub fn images_pending(&self) -> bool {
        self.image_rows_estimate.iter().enumerate().any(|(i, &rows)| {
            rows > 0
                && self
                    .section_images
                    .get(&i)
                    .is_some_and(|k| !self.image_cache.contains(k) && !self.img_failed.contains(k))
        })
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
            if active != self.last_active {
                self.last_active = active;
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
        let mut best: Option<(usize, usize)> = None; // (line, outline index)
        for &(oi, line) in &self.heading_lines {
            // Greatest line at/above the viewport top; on ties keep the earlier
            // entry (strictly greater to replace).
            if line <= self.scroll && best.is_none_or(|(bl, _)| line > bl) {
                best = Some((line, oi));
            }
        }
        best.map(|(_, oi)| oi)
            .or_else(|| self.heading_lines.first().map(|&(oi, _)| oi))
    }

    /// Position of `active_outline` within the visible (collapsed-aware) list.
    pub fn active_outline_row(&self) -> Option<usize> {
        let active = self.active_outline()?;
        self.outline_visible().iter().position(|&oi| oi == active)
    }

    /// Outline indices currently visible (respecting collapsed parents).
    pub fn outline_visible(&self) -> Vec<usize> {
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
        if let Some(oi) = self.selected_outline() {
            if let Some(item) = self.outline.get(oi).cloned() {
                self.jump_to(item.section, item.locator.as_deref());
            }
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
            if depth > 0 {
                if let Some(pi) = (0..oi).rev().find(|&j| self.outline[j].depth < depth) {
                    if let Some(pos) = self.outline_visible().iter().position(|&x| x == pi) {
                        self.sidebar_sel = pos;
                    }
                }
            }
        }
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

    pub fn start_search(&mut self) {
        self.searching = true;
        self.search_input.clear();
        self.history_pos = None;
    }

    pub fn search_count(&self) -> usize {
        self.search_matches.len()
    }

    /// Cycle the search mode (plain → regex → fuzzy) while typing a query.
    pub fn cycle_search_mode(&mut self) {
        self.search_mode = self.search_mode.next();
    }

    /// Recall the previous (`-1`) or next (`+1`) query from history into the
    /// prompt.
    pub fn search_history_recall(&mut self, dir: i32) {
        if self.search_history.is_empty() {
            return;
        }
        let len = self.search_history.len();
        let pos = match (self.history_pos, dir) {
            (None, -1) => len - 1,
            (Some(p), -1) => p.saturating_sub(1),
            (Some(p), 1) if p + 1 < len => p + 1,
            (Some(_), 1) => {
                // Past the newest → back to a fresh, empty prompt.
                self.history_pos = None;
                self.search_input.clear();
                return;
            }
            _ => return,
        };
        self.history_pos = Some(pos);
        self.search_input = self.search_history[pos].clone();
    }

    /// Run the typed query across the whole book in the current mode, recording
    /// matching (section, line) positions and jumping to the first.
    pub fn run_search(&mut self) {
        self.searching = false;
        let query = self.search_input.trim().to_string();
        self.search_matches.clear();
        self.search_idx = 0;
        self.history_pos = None;
        if query.is_empty() {
            self.search_matcher = None;
            return;
        }

        // Record in history (dedup, most-recent last, bounded).
        self.search_history.retain(|q| q != &query);
        self.search_history.push(query.clone());
        if self.search_history.len() > 50 {
            self.search_history.remove(0);
        }

        let matcher = Matcher::new(self.search_mode, &query);
        if matcher.is_valid() {
            let width = self.last_measure.max(1);
            for s in 0..self.doc.section_count() {
                let blocks = self.fetch_blocks(s);
                let lines = wrap_blocks(
                    &blocks,
                    &WrapOpts {
                        width,
                        code_theme: &self.code_theme,
                        line_spacing: self.line_spacing,
                        para_spacing: self.paragraph_spacing,
                        // Search always wraps code so no matches are hidden off-screen.
                        code_wrap: true,
                        code_hscroll: 0,
                    },
                    &[],
                );
                for (li, line) in lines.iter().enumerate() {
                    if matcher.matches(&line.text()) {
                        self.search_matches.push((s, li));
                    }
                }
            }
        }
        self.search_matcher = Some(matcher);
        if !self.search_matches.is_empty() {
            self.goto_match(0);
        }
    }

    pub fn search_next(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        let i = (self.search_idx + 1) % self.search_matches.len();
        self.goto_match(i);
    }

    pub fn search_prev(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        let n = self.search_matches.len();
        let i = (self.search_idx + n - 1) % n;
        self.goto_match(i);
    }

    fn goto_match(&mut self, i: usize) {
        let Some(&(section, line)) = self.search_matches.get(i) else {
            return;
        };
        self.search_idx = i;
        if section != self.section {
            self.load(section);
        }
        self.scroll = line;
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

pub struct App {
    pub mode: Mode,
    pub config: Config,
    pub reader: Option<Reader>,
    pub last_layout: LayoutRects,
    pub pending: Pending,
    pub should_quit: bool,
    /// Open settings popup, if any.
    pub settings: Option<Settings>,
    /// Open annotations overlay, if any.
    pub annot: Option<AnnotState>,
    /// Active note-entry buffer, if typing a note.
    pub note_input: Option<String>,
    /// Open image viewer overlay, if any.
    pub image_view: Option<ImageView>,
    /// Detected terminal image protocol (None if unsupported / headless).
    pub picker: Option<Picker>,
    /// Background builder for inline-image protocols.
    pub image_builder: Option<ImageBuilder>,
    /// Start of the current reading session, for time tracking.
    session_start: Option<Instant>,
    store: Option<Store>,
    /// Canonical path of the open book; key for persistence.
    book_path: String,
    // Library view state.
    pub lib_section: LibrarySection,
    pub lib_books: Vec<BookRow>,
    pub lib_sel: usize,
    pub lib_filter: String,
    pub lib_filtering: bool,
}

/// Build a reader for `path`, applying global config and any saved per-book
/// overrides (theme, view mode, resume position).
fn build_reader(path: &str, store: &Option<Store>) -> Result<(Reader, Config, String)> {
    let doc = EpubDocument::open(path)?;
    let mut reader = Reader::new(Box::new(doc))?;
    let mut config = Config::load();
    let book_path = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    if let Some(store) = store {
        if let Some(p) = store.load_progress(&book_path) {
            config.view_mode = p.view_mode;
            if let Some(t) = theme::by_name(&p.theme) {
                config.theme = t;
            }
            reader.load(p.section);
            reader.pending_frac = Some(p.frac);
        }
    }
    Ok((reader, config, book_path))
}

impl App {
    pub fn open_book(path: &str) -> Result<Self> {
        let store = Store::open_default().ok();
        let (reader, config, book_path) = build_reader(path, &store)?;
        if let Some(s) = &store {
            s.mark_opened(&book_path);
        }
        Ok(Self {
            mode: Mode::Reader,
            config,
            reader: Some(reader),
            last_layout: LayoutRects::default(),
            pending: Pending::default(),
            should_quit: false,
            settings: None,
            annot: None,
            note_input: None,
            image_view: None,
            picker: None,
            image_builder: None,
            session_start: Some(Instant::now()),
            store,
            book_path,
            lib_section: LibrarySection::All,
            lib_books: Vec::new(),
            lib_sel: 0,
            lib_filter: String::new(),
            lib_filtering: false,
        })
    }

    pub fn library() -> Self {
        let config = Config::load();
        let store = Store::open_default().ok();
        if let Some(s) = &store {
            library::scan(&config.library_paths, s);
        }
        let mut app = Self {
            mode: Mode::Library,
            config,
            reader: None,
            last_layout: LayoutRects::default(),
            pending: Pending::default(),
            should_quit: false,
            settings: None,
            annot: None,
            note_input: None,
            image_view: None,
            picker: None,
            image_builder: None,
            session_start: None,
            store,
            book_path: String::new(),
            lib_section: LibrarySection::All,
            lib_books: Vec::new(),
            lib_sel: 0,
            lib_filter: String::new(),
            lib_filtering: false,
        };
        app.refresh_library();
        app
    }

    fn refresh_library(&mut self) {
        let Some(store) = &self.store else {
            self.lib_books.clear();
            return;
        };
        let f = self.lib_filter.to_lowercase();
        self.lib_books = if f.is_empty() {
            store.list_books(self.lib_section)
        } else {
            // Library-wide search: title/author substring OR full-text match.
            let fts: HashSet<String> = store.fts_paths(&self.lib_filter).into_iter().collect();
            store
                .all_books()
                .into_iter()
                .filter(|b| {
                    b.title.to_lowercase().contains(&f)
                        || b.author.to_lowercase().contains(&f)
                        || fts.contains(&b.path)
                })
                .collect()
        };
        if self.lib_sel >= self.lib_books.len() {
            self.lib_sel = self.lib_books.len().saturating_sub(1);
        }
    }

    fn lib_move(&mut self, delta: isize) {
        if self.lib_books.is_empty() {
            return;
        }
        let last = self.lib_books.len() as isize - 1;
        self.lib_sel = (self.lib_sel as isize + delta).clamp(0, last) as usize;
    }

    fn lib_favorite(&mut self) {
        if let (Some(store), Some(book)) = (&self.store, self.lib_books.get(self.lib_sel)) {
            store.set_favorite(&book.path, !book.favorite);
        }
        self.refresh_library();
    }

    fn cycle_section(&mut self) {
        self.lib_section = match self.lib_section {
            LibrarySection::Recent => LibrarySection::All,
            LibrarySection::All => LibrarySection::Favorites,
            LibrarySection::Favorites => LibrarySection::Reading,
            LibrarySection::Reading => LibrarySection::Duplicates,
            LibrarySection::Duplicates => LibrarySection::Recent,
        };
        self.lib_sel = 0;
        self.refresh_library();
    }

    fn open_selected(&mut self) {
        let Some(path) = self.lib_books.get(self.lib_sel).map(|b| b.path.clone()) else {
            return;
        };
        self.flush_reading_time();
        if let Ok((reader, config, book_path)) = build_reader(&path, &self.store) {
            self.reader = Some(reader);
            self.config = config;
            self.book_path = book_path;
            self.mode = Mode::Reader;
            self.session_start = Some(Instant::now());
            if let Some(s) = &self.store {
                s.mark_opened(&self.book_path);
            }
        }
    }

    /// Persist the current reading position (best-effort).
    pub fn save_progress(&self) {
        if let (Some(store), Some(reader)) = (&self.store, &self.reader) {
            if !self.book_path.is_empty() {
                let _ = store.save_progress(
                    &self.book_path,
                    reader.section,
                    reader.within_frac(),
                    self.config.view_mode,
                    self.config.theme.name,
                );
            }
        }
    }

    /// Accumulate elapsed reading time into the open book and reset the clock.
    fn flush_reading_time(&mut self) {
        if let (Some(start), Some(store)) = (self.session_start, &self.store) {
            let secs = start.elapsed().as_secs() as i64;
            if secs > 0 && !self.book_path.is_empty() {
                store.add_read_time(&self.book_path, secs);
            }
        }
        if self.session_start.is_some() {
            self.session_start = Some(Instant::now());
        }
    }

    /// Save progress + reading time on quit.
    pub fn on_exit(&mut self) {
        self.flush_reading_time();
        self.save_progress();
    }

    pub fn total_read_seconds(&self) -> i64 {
        self.store.as_ref().map(|s| s.total_read_seconds()).unwrap_or(0)
    }

    /// Terminal image ids to delete (evicted from the reader's cache).
    pub fn take_image_deletes(&mut self) -> Vec<u32> {
        self.reader
            .as_mut()
            .map(|r| r.take_image_deletes())
            .unwrap_or_default()
    }

    /// Text queued for the system clipboard (OSC 52), if any.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.reader.as_mut().and_then(|r| r.take_clipboard())
    }

    /// Is a smooth scroll in progress, or are inline images still building (so
    /// the loop should keep drawing until things settle)?
    pub fn animating(&self) -> bool {
        let Some(r) = self.reader.as_ref() else {
            return false;
        };
        r.scroll_pending != 0 || (self.mode == Mode::Reader && r.images_pending())
    }

    /// Advance one frame of smooth scrolling; returns whether anything moved.
    pub fn step_scroll(&mut self) -> bool {
        if self.mode != Mode::Reader {
            return false;
        }
        self.reader.as_mut().is_some_and(|r| r.step_scroll())
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.settings.is_some() {
            self.settings_key(key);
            return;
        }
        if self.note_input.is_some() {
            self.note_key(key);
            return;
        }
        if self.image_view.is_some() {
            self.image_key(key);
            return;
        }
        if self.annot.is_some() {
            self.annot_key(key);
            return;
        }
        if self.mode == Mode::Reader && key.code == KeyCode::Char('i') {
            self.open_images();
            return;
        }
        if self.mode == Mode::Reader && self.reader.as_ref().is_some_and(|r| r.searching) {
            self.search_key(key);
            return;
        }
        if key.code == KeyCode::Char(';') {
            let tab = match self.mode {
                Mode::Reader => SettingsTab::Reading,
                Mode::Library => SettingsTab::Library,
            };
            self.settings = Some(Settings { tab, row: 0 });
            return;
        }
        match self.mode {
            Mode::Reader => {
                // Clear any transient flash message on the next keypress.
                if let Some(r) = self.reader.as_mut() {
                    r.flash = None;
                }
                let action = input::map_key(key, &mut self.pending);
                self.apply(action);
                // Returning to the library (Back) should reflect the latest state.
                if self.mode == Mode::Library {
                    self.refresh_library();
                }
            }
            Mode::Library => self.library_key(key),
        }
    }

    /// Open the image viewer on the current section's images.
    fn open_images(&mut self) {
        let (Some(picker), Some(reader)) = (self.picker.as_ref(), self.reader.as_mut()) else {
            return;
        };
        let images = reader.doc.section_images(reader.section);
        self.image_view = ImageView::new(picker, &images);
    }

    fn image_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') => self.image_view = None,
            KeyCode::Char('n') | KeyCode::Char('l') | KeyCode::Right | KeyCode::Char('j') => {
                if let Some(v) = self.image_view.as_mut() {
                    v.next();
                }
            }
            KeyCode::Char('N') | KeyCode::Char('h') | KeyCode::Left | KeyCode::Char('k') => {
                if let Some(v) = self.image_view.as_mut() {
                    v.prev();
                }
            }
            _ => {}
        }
    }

    fn note_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.note_input = None,
            KeyCode::Enter => {
                if let Some(text) = self.note_input.take() {
                    if let (Some(store), Some(r)) = (&self.store, &self.reader) {
                        if !self.book_path.is_empty() {
                            store.add_annotation(
                                &self.book_path,
                                r.section,
                                &r.current_quote(),
                                text.trim(),
                            );
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(s) = self.note_input.as_mut() {
                    s.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(s) = self.note_input.as_mut() {
                    s.push(c);
                }
            }
            _ => {}
        }
    }

    fn annot_key(&mut self, key: KeyEvent) {
        let Some(a) = self.annot.as_ref() else {
            return;
        };
        let (len, sel) = (a.items.len(), a.sel);
        match key.code {
            KeyCode::Esc | KeyCode::Char('\'') | KeyCode::Char('q') => self.annot = None,
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(a) = self.annot.as_mut() {
                    if len > 0 {
                        a.sel = (sel + 1).min(len - 1);
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(a) = self.annot.as_mut() {
                    a.sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Enter | KeyCode::Char('l') => {
                let target = self
                    .annot
                    .as_ref()
                    .and_then(|a| a.items.get(a.sel))
                    .map(|i| (i.section, i.quote.clone()));
                if let Some((section, quote)) = target {
                    if let Some(r) = self.reader.as_mut() {
                        r.jump_to(section, Some(&quote));
                    }
                    self.annot = None;
                }
            }
            KeyCode::Char('d') => {
                let id = self.annot.as_ref().and_then(|a| a.items.get(a.sel)).map(|i| i.id);
                if let (Some(id), Some(store)) = (id, &self.store) {
                    store.delete_annotation(id);
                    let items = store.list_annotations(&self.book_path);
                    if let Some(a) = self.annot.as_mut() {
                        a.items = items;
                        if a.sel >= a.items.len() {
                            a.sel = a.items.len().saturating_sub(1);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn search_key(&mut self, key: KeyEvent) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                reader.searching = false;
                reader.search_input.clear();
            }
            KeyCode::Enter => reader.run_search(),
            KeyCode::Tab => reader.cycle_search_mode(),
            KeyCode::Up => reader.search_history_recall(-1),
            KeyCode::Down => reader.search_history_recall(1),
            KeyCode::Backspace => {
                reader.history_pos = None;
                reader.search_input.pop();
            }
            KeyCode::Char(c) => {
                reader.history_pos = None;
                reader.search_input.push(c);
            }
            _ => {}
        }
    }

    fn settings_key(&mut self, key: KeyEvent) {
        let Some(s) = self.settings.as_ref() else {
            return;
        };
        let (tab, row) = (s.tab, s.row);
        match key.code {
            KeyCode::Esc | KeyCode::Char(';') | KeyCode::Char('q') => {
                self.settings = None;
                self.config.save();
            }
            KeyCode::Tab => {
                if let Some(s) = self.settings.as_mut() {
                    s.tab = tab.next();
                    s.row = 0;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let n = settings_rows(&self.config, tab).len();
                if let Some(s) = self.settings.as_mut() {
                    if n > 0 {
                        s.row = (row + 1).min(n - 1);
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(s) = self.settings.as_mut() {
                    s.row = row.saturating_sub(1);
                }
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.settings_change(1),
            KeyCode::Char('h') | KeyCode::Left => self.settings_change(-1),
            _ => {}
        }
    }

    fn settings_change(&mut self, delta: i32) {
        use crate::config::{MAX_LINE_SPACING, MAX_SIDE_PADDING};
        let Some(s) = self.settings.as_ref() else {
            return;
        };
        let c = &mut self.config;
        match (s.tab, s.row) {
            (SettingsTab::Reading, 0) => {
                c.theme = if delta > 0 { c.theme.next() } else { c.theme.prev() }
            }
            (SettingsTab::Reading, 1) => {
                c.view_mode = if delta > 0 {
                    c.view_mode.next()
                } else {
                    c.view_mode.prev()
                }
            }
            (SettingsTab::Reading, 2) => {
                c.side_padding =
                    (c.side_padding as i32 + delta).clamp(0, MAX_SIDE_PADDING as i32) as u16
            }
            (SettingsTab::Reading, 3) => {
                c.line_spacing = (c.line_spacing as i32 + delta).clamp(0, MAX_LINE_SPACING as i32) as u8
            }
            (SettingsTab::Reading, 4) => {
                c.paragraph_spacing = (c.paragraph_spacing as i32 + delta).clamp(0, 3) as u8
            }
            (SettingsTab::Reading, 5) => c.show_sidebar = !c.show_sidebar,
            (SettingsTab::Reading, 6) => c.show_status = !c.show_status,
            (SettingsTab::Reading, 7) => c.status.theme = !c.status.theme,
            (SettingsTab::Reading, 8) => c.status.view = !c.status.view,
            (SettingsTab::Reading, 9) => c.status.position = !c.status.position,
            (SettingsTab::Reading, 10) => c.status.percent = !c.status.percent,
            (SettingsTab::Reading, 11) => c.status.gauge = !c.status.gauge,
            (SettingsTab::Reading, 12) => {
                // 0 = off (uncapped); otherwise step in 128px increments.
                c.image_max_px =
                    (c.image_max_px as i32 + delta * 128).clamp(0, crate::config::MAX_IMAGE_PX as i32)
                        as u16
            }
            (SettingsTab::Reading, 13) => c.code_wrap = !c.code_wrap,
            (SettingsTab::Reading, 14) => c.chapter_lock = !c.chapter_lock,
            (SettingsTab::General, 0) => c.mouse_enabled = !c.mouse_enabled,
            _ => {}
        }
    }

    fn library_key(&mut self, key: KeyEvent) {
        if self.lib_filtering {
            match key.code {
                KeyCode::Esc => {
                    self.lib_filter.clear();
                    self.lib_filtering = false;
                    self.refresh_library();
                }
                KeyCode::Enter => self.lib_filtering = false,
                KeyCode::Backspace => {
                    self.lib_filter.pop();
                    self.refresh_library();
                }
                KeyCode::Char(c) => {
                    self.lib_filter.push(c);
                    self.refresh_library();
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_quit = true,
            KeyCode::Esc => {
                if self.lib_filter.is_empty() {
                    self.should_quit = true;
                } else {
                    self.lib_filter.clear();
                    self.refresh_library();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.lib_move(1),
            KeyCode::Char('k') | KeyCode::Up => self.lib_move(-1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char('o') => self.open_selected(),
            KeyCode::Char('f') => self.lib_favorite(),
            KeyCode::Char('/') => self.lib_filtering = true,
            KeyCode::Tab => self.cycle_section(),
            KeyCode::Char('g') => self.lib_sel = 0,
            KeyCode::Char('G') => self.lib_sel = self.lib_books.len().saturating_sub(1),
            _ => {}
        }
    }

    fn apply(&mut self, action: Action) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        let before = reader.section;
        let mut save = false;
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back => {
                // Accumulate reading time for the session before leaving.
                if let (Some(start), Some(store)) = (self.session_start, &self.store) {
                    let secs = start.elapsed().as_secs() as i64;
                    if secs > 0 && !self.book_path.is_empty() {
                        store.add_read_time(&self.book_path, secs);
                    }
                }
                self.session_start = Some(Instant::now());
                self.mode = Mode::Library;
                save = true;
            }
            Action::Down(n) => match reader.focus {
                Focus::Content => reader.queue_scroll(n as isize),
                Focus::Sidebar => reader.sidebar_move(n as isize),
            },
            Action::Up(n) => match reader.focus {
                Focus::Content => reader.queue_scroll(-(n as isize)),
                Focus::Sidebar => reader.sidebar_move(-(n as isize)),
            },
            Action::HalfDown => reader.scroll_down(reader.page_lines.max(2) / 2),
            Action::HalfUp => reader.scroll_up(reader.page_lines.max(2) / 2),
            Action::PageDown => reader.scroll_down(reader.page_lines.max(1)),
            Action::PageUp => reader.scroll_up(reader.page_lines.max(1)),
            Action::Top => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_sel = 0;
                } else {
                    reader.scroll = 0;
                }
            }
            Action::Bottom => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_sel = reader.outline.len().saturating_sub(1);
                } else {
                    reader.scroll = reader.max_scroll();
                }
            }
            Action::ToggleStatus => self.config.show_status = !self.config.show_status,
            Action::CycleView => {
                self.config.view_mode = self.config.view_mode.next();
                save = true;
            }
            Action::CycleTheme => {
                self.config.theme = self.config.theme.next();
                save = true;
            }
            Action::ToggleFocus => self.config.focus_mode = !self.config.focus_mode,
            // `]` widens the text (less margin), `[` narrows it (more margin).
            Action::WidthUp => {
                self.config.side_padding = self.config.side_padding.saturating_sub(1);
            }
            Action::WidthDown => {
                self.config.side_padding =
                    (self.config.side_padding + 1).min(crate::config::MAX_SIDE_PADDING);
            }
            Action::LineSpacingDown => {
                self.config.line_spacing = self.config.line_spacing.saturating_sub(1);
            }
            Action::LineSpacingUp => {
                self.config.line_spacing =
                    (self.config.line_spacing + 1).min(crate::config::MAX_LINE_SPACING);
            }
            Action::ToggleSidebar => {
                self.config.show_sidebar = !self.config.show_sidebar;
                if !self.config.show_sidebar {
                    reader.focus = Focus::Content;
                }
            }
            Action::FocusToggle => {
                // Tab moves focus into the sidebar (showing it first if hidden),
                // then back to the content. Entering the sidebar, start the
                // cursor at the entry tracking the current reading position.
                if !self.config.show_sidebar {
                    self.config.show_sidebar = true;
                    reader.focus = Focus::Sidebar;
                    reader.sidebar_sel = reader.active_outline_row().unwrap_or(0);
                    reader.center_sidebar();
                } else if reader.focus == Focus::Content {
                    reader.focus = Focus::Sidebar;
                    reader.sidebar_sel = reader.active_outline_row().unwrap_or(0);
                    reader.center_sidebar();
                } else {
                    reader.focus = Focus::Content;
                }
            }
            Action::Activate => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_activate();
                }
            }
            Action::Expand => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_expand();
                }
            }
            Action::Collapse => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_collapse();
                }
            }
            Action::HistBack => reader.history_back(),
            Action::HistForward => reader.history_forward(),
            Action::Search => reader.start_search(),
            Action::SearchNext => reader.search_next(),
            Action::SearchPrev => reader.search_prev(),
            Action::AddBookmark => {
                if let Some(store) = &self.store {
                    if !self.book_path.is_empty() {
                        store.add_annotation(
                            &self.book_path,
                            reader.section,
                            &reader.current_quote(),
                            "",
                        );
                    }
                }
            }
            Action::AddNote => self.note_input = Some(String::new()),
            Action::OpenAnnotations => {
                if let Some(store) = &self.store {
                    let items = store.list_annotations(&self.book_path);
                    self.annot = Some(AnnotState { items, sel: 0 });
                }
            }
            Action::CopyCode => {
                reader.copy_visible_code();
            }
            Action::ToggleCodeWrap => {
                self.config.code_wrap = !self.config.code_wrap;
                reader.code_hscroll = 0;
                reader.flash = Some(
                    if self.config.code_wrap { "code: wrap" } else { "code: no-wrap (< > to pan)" }
                        .to_string(),
                );
                save = true;
            }
            // Horizontal panning only applies to non-wrapped code.
            Action::PanLeft => {
                reader.code_hscroll = reader.code_hscroll.saturating_sub(8);
            }
            Action::PanRight => {
                if !self.config.code_wrap {
                    reader.code_hscroll = (reader.code_hscroll + 8).min(400);
                }
            }
            Action::ToggleChapterLock => {
                self.config.chapter_lock = !self.config.chapter_lock;
                reader.flash = Some(
                    if self.config.chapter_lock { "chapter lock: on" } else { "chapter lock: off" }
                        .to_string(),
                );
                save = true;
            }
            Action::NextChapter => reader.next_chapter(),
            Action::PrevChapter => reader.prev_chapter(),
            Action::None => {}
        }

        // Persist on chapter change or a settings change (cheap).
        if save || reader.section != before {
            if let Some(store) = &self.store {
                if !self.book_path.is_empty() {
                    let _ = store.save_progress(
                        &self.book_path,
                        reader.section,
                        reader.within_frac(),
                        self.config.view_mode,
                        self.config.theme.name,
                    );
                }
            }
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if !self.config.mouse_enabled || self.mode != Mode::Reader {
            return;
        }
        // The wheel scrolls whichever pane the cursor is over: the TOC view
        // (without changing the selection) or the content.
        let over_sidebar = self
            .last_layout
            .sidebar
            .is_some_and(|sb| sb.contains((m.column, m.row).into()));
        match m.kind {
            MouseEventKind::ScrollDown => {
                if let Some(r) = self.reader.as_mut() {
                    if over_sidebar {
                        r.sidebar_wheel(3);
                    } else {
                        r.queue_scroll(3);
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(r) = self.reader.as_mut() {
                    if over_sidebar {
                        r.sidebar_wheel(-3);
                    } else {
                        r.queue_scroll(-3);
                    }
                }
            }
            MouseEventKind::Down(_) => self.mouse_click(m.column, m.row),
            _ => {}
        }
    }

    /// Click on a sidebar row selects and jumps to that TOC entry.
    fn mouse_click(&mut self, col: u16, row: u16) {
        let Some(sb) = self.last_layout.sidebar else {
            return;
        };
        let in_x = col >= sb.x && col < sb.x + sb.width;
        // Account for the sidebar's top/bottom border.
        let first = sb.y + 1;
        let last = sb.y + sb.height.saturating_sub(1);
        if !in_x || row < first || row >= last {
            return;
        }
        if let Some(r) = self.reader.as_mut() {
            // Screen row → list index, accounting for the scrolled viewport.
            let idx = r.sidebar_offset + (row - first) as usize;
            let vis = r.outline_visible();
            if let Some(&oi) = vis.get(idx) {
                r.sidebar_sel = idx;
                r.focus = Focus::Sidebar;
                if let Some(item) = r.outline.get(oi).cloned() {
                    r.jump_to(item.section, item.locator.as_deref());
                }
            }
        }
    }
}
