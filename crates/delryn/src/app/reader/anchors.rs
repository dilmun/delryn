//! In-book reference navigation: the inline **link cursor** plus footnote /
//! cross-reference / citation following.
//!
//! Built on the per-section anchor index that [`recompute_anchors`](Reader::recompute_anchors)
//! rebuilds after each re-wrap. The cursor (`e`/`E`) steps the anchors in reading
//! order; activating one follows it — a footnote reference jumps to its definition
//! (same section, then a cross-section endnote scan), a link is copied, and a
//! cross-ref / citation resolves to its target section + line via the book-wide
//! id index. Reflow-only; keyed to the anchor section.

use super::*;

impl Reader {
    /// Rebuild the inline-anchor index and footnote definition map from the
    /// freshly wrapped lines. Done once per re-wrap.
    pub(super) fn recompute_anchors(&mut self) {
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
}
