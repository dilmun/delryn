//! Visual (vim-style) text selection and the shared section-text resolver.
//!
//! Two jobs live here:
//! - [`Selection`] — a caret + anchor over the current section's display lines,
//!   driven by vim motions (`h`/`l`/`w`/`b`/`0`/`$`/`j`/`k`), yielding the selected
//!   text (to copy or to anchor a highlight/note).
//! - [`flat_index`] / [`resolve_spans`] — flatten a section's wrapped lines into a
//!   whitespace-normalized string and re-find a stored quote's exact cells after a
//!   reflow, so a sub-line highlight re-washes the same characters at any width.

use crate::layout::DisplayLine;

use super::Reader;

/// Reader-facing visual-selection controls (vim `V`). Kept here beside the
/// [`Selection`] they drive; they read/write the reader's private `select`,
/// `lines`, and `scroll`, so they live in this child module.
impl Reader {
    /// Whether a visual selection is active (keys are then motions, not commands).
    pub fn selection_active(&self) -> bool {
        self.select.is_some()
    }

    /// Enter visual mode with a collapsed selection at the first non-blank visible
    /// line. No-op for paged (image) documents, which have no character grid.
    pub fn start_selection(&mut self) {
        if self.is_paged_image() || self.lines.is_empty() {
            return;
        }
        let line = (self.scroll..self.lines.len())
            .find(|&i| !self.lines[i].text().trim().is_empty())
            .unwrap_or_else(|| self.scroll.min(self.lines.len() - 1));
        self.select = Some(Selection::at(line, 0));
    }

    /// Leave visual mode, discarding the selection.
    pub fn cancel_selection(&mut self) {
        self.select = None;
    }

    /// The selected text (whitespace-normalized), or empty if none.
    pub fn selection_text(&self) -> String {
        self.select.map(|s| s.text(&self.lines)).unwrap_or_default()
    }

    /// The selection's `[start, end)` column span on a display line, for washing.
    pub fn selection_span_on(&self, line: usize) -> Option<(usize, usize)> {
        self.select.and_then(|s| s.span_on(line, &self.lines))
    }

    /// The caret cell `(line, col)`, drawn as a block so the user sees the head.
    pub fn selection_caret(&self) -> Option<(usize, usize)> {
        self.select.map(|s| (s.head.line, s.head.col))
    }

    /// Copy the selection to the clipboard and leave visual mode. Returns whether
    /// anything was copied.
    pub fn copy_selection(&mut self) -> bool {
        let text = self.selection_text();
        self.select = None;
        if text.is_empty() {
            return false;
        }
        let n = text.chars().count();
        self.pending_clipboard = Some(text);
        self.flash = Some(format!(
            "✓ copied {n} char{}",
            if n == 1 { "" } else { "s" }
        ));
        true
    }

    /// Apply a caret motion, then scroll so the caret stays on screen.
    fn move_head(&mut self, motion: impl FnOnce(&mut Selection, &[DisplayLine])) {
        if let Some(sel) = self.select.as_mut() {
            sel.clamp(&self.lines);
            motion(sel, &self.lines);
        }
        self.follow_caret();
    }

    pub fn selection_left(&mut self) {
        self.move_head(Selection::left);
    }
    pub fn selection_right(&mut self) {
        self.move_head(Selection::right);
    }
    pub fn selection_up(&mut self) {
        self.move_head(Selection::up);
    }
    pub fn selection_down(&mut self) {
        self.move_head(Selection::down);
    }
    pub fn selection_word_forward(&mut self) {
        self.move_head(Selection::word_forward);
    }
    pub fn selection_word_back(&mut self) {
        self.move_head(Selection::word_back);
    }
    pub fn selection_line_start(&mut self) {
        self.move_head(|s, _| s.line_start());
    }
    pub fn selection_line_end(&mut self) {
        self.move_head(Selection::line_end);
    }

    /// Scroll the minimum needed to keep the caret's line within the viewport.
    fn follow_caret(&mut self) {
        let Some(sel) = self.select else { return };
        let head = sel.head.line;
        let page = self.page_lines.max(1);
        if head < self.scroll {
            self.scroll = head;
        } else if head >= self.scroll + page {
            self.scroll = head + 1 - page;
        }
        let max = self.lines.len().saturating_sub(page);
        self.scroll = self.scroll.min(max);
    }
}

/// A caret position: a display-line index and a character column within it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    pub line: usize,
    pub col: usize,
}

impl Caret {
    /// Line-major ordering, so a selection can be normalized to (start, end).
    fn key(self) -> (usize, usize) {
        (self.line, self.col)
    }
}

/// A live visual selection: `anchor` is fixed where the user pressed `V`, `head`
/// moves with the motions. The selected range is the inclusive span between them.
#[derive(Clone, Copy)]
pub struct Selection {
    pub anchor: Caret,
    pub head: Caret,
}

/// Character count of display line `li` (0 if out of range).
fn line_len(lines: &[DisplayLine], li: usize) -> usize {
    lines.get(li).map(|l| l.text().chars().count()).unwrap_or(0)
}

/// The highest landable column on a line — its last character (0 on an empty line).
fn max_col(lines: &[DisplayLine], li: usize) -> usize {
    line_len(lines, li).saturating_sub(1)
}

impl Selection {
    /// Start a collapsed selection at `(line, col)`.
    pub fn at(line: usize, col: usize) -> Selection {
        let c = Caret { line, col };
        Selection { anchor: c, head: c }
    }

    /// The selection normalized to `(start, end)` in reading order.
    pub fn ordered(self) -> (Caret, Caret) {
        if self.anchor.key() <= self.head.key() {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The `[start, end)` column span this selection covers on display line `line`,
    /// or `None` if the line is outside it. The head character is included.
    pub fn span_on(self, line: usize, lines: &[DisplayLine]) -> Option<(usize, usize)> {
        let (a, b) = self.ordered();
        if line < a.line || line > b.line {
            return None;
        }
        let len = line_len(lines, line);
        let start = if line == a.line { a.col } else { 0 };
        let end = if line == b.line { b.col + 1 } else { len }.min(len);
        (start < end).then_some((start, end))
    }

    /// The selected text, whitespace-normalized (lines joined by single spaces) —
    /// used to copy, and to anchor a highlight/note so it survives reflow.
    pub fn text(self, lines: &[DisplayLine]) -> String {
        let (a, b) = self.ordered();
        let end_line = b.line.min(lines.len().saturating_sub(1));
        if lines.is_empty() || a.line > end_line {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        for (rel, l) in lines[a.line..=end_line].iter().enumerate() {
            let li = a.line + rel;
            let chars: Vec<char> = l.text().chars().collect();
            let start = if li == a.line { a.col } else { 0 }.min(chars.len());
            let end = if li == b.line {
                (b.col + 1).min(chars.len())
            } else {
                chars.len()
            };
            if start < end {
                parts.push(chars[start..end].iter().collect());
            }
        }
        parts
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    // --- motions (move the head; the anchor stays put) ---

    /// One character left, crossing to the previous line's end at column 0.
    pub fn left(&mut self, lines: &[DisplayLine]) {
        if self.head.col > 0 {
            self.head.col -= 1;
        } else if self.head.line > 0 {
            self.head.line -= 1;
            self.head.col = max_col(lines, self.head.line);
        }
    }

    /// One character right, crossing to the next line's start.
    pub fn right(&mut self, lines: &[DisplayLine]) {
        if self.head.col < max_col(lines, self.head.line) {
            self.head.col += 1;
        } else if self.head.line + 1 < lines.len() {
            self.head.line += 1;
            self.head.col = 0;
        }
    }

    /// Up a line, keeping the column (clamped to the shorter line).
    pub fn up(&mut self, lines: &[DisplayLine]) {
        if self.head.line > 0 {
            self.head.line -= 1;
            self.head.col = self.head.col.min(max_col(lines, self.head.line));
        }
    }

    /// Down a line, keeping the column (clamped to the shorter line).
    pub fn down(&mut self, lines: &[DisplayLine]) {
        if self.head.line + 1 < lines.len() {
            self.head.line += 1;
            self.head.col = self.head.col.min(max_col(lines, self.head.line));
        }
    }

    /// To the first / last character of the current line.
    pub fn line_start(&mut self) {
        self.head.col = 0;
    }
    pub fn line_end(&mut self, lines: &[DisplayLine]) {
        self.head.col = max_col(lines, self.head.line);
    }

    /// Forward to the start of the next word (crossing lines).
    pub fn word_forward(&mut self, lines: &[DisplayLine]) {
        let chars: Vec<char> = current_chars(lines, self.head.line);
        let mut col = self.head.col;
        // Skip the rest of the current word, then any spaces.
        while col < chars.len() && !chars[col].is_whitespace() {
            col += 1;
        }
        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        if col < chars.len() {
            self.head.col = col;
        } else if self.head.line + 1 < lines.len() {
            self.head.line += 1;
            self.head.col = 0;
        } else {
            self.head.col = max_col(lines, self.head.line);
        }
    }

    /// Back to the start of the current or previous word (crossing lines).
    pub fn word_back(&mut self, lines: &[DisplayLine]) {
        if self.head.col == 0 {
            if self.head.line > 0 {
                self.head.line -= 1;
                self.head.col = max_col(lines, self.head.line);
            }
            return;
        }
        let chars: Vec<char> = current_chars(lines, self.head.line);
        let mut col = self.head.col;
        // Step back over spaces, then to the start of the word.
        col = col.saturating_sub(1);
        while col > 0 && chars.get(col).is_some_and(|c| c.is_whitespace()) {
            col -= 1;
        }
        while col > 0 && chars.get(col - 1).is_some_and(|c| !c.is_whitespace()) {
            col -= 1;
        }
        self.head.col = col;
    }

    /// Clamp the caret/anchor to a (possibly re-wrapped) `lines`, so an image load
    /// that re-flows mid-selection can't leave the caret out of bounds.
    pub fn clamp(&mut self, lines: &[DisplayLine]) {
        let last = lines.len().saturating_sub(1);
        for c in [&mut self.anchor, &mut self.head] {
            c.line = c.line.min(last);
            c.col = c.col.min(max_col(lines, c.line));
        }
    }
}

/// The chars of display line `li` (empty if out of range).
fn current_chars(lines: &[DisplayLine], li: usize) -> Vec<char> {
    lines
        .get(li)
        .map(|l| l.text().chars().collect())
        .unwrap_or_default()
}

/// Flatten a section's display lines into one whitespace-normalized string plus a
/// parallel map from each string character to its `(line, col)` cell (`None` for a
/// synthetic separator space — a collapsed run of whitespace or a line break).
/// Normalizing makes the string reflow-stable: the same words in the same order
/// flatten identically regardless of where the wrap falls, so a quote captured at
/// one width still matches at another.
pub fn flat_index(lines: &[DisplayLine]) -> (Vec<char>, Vec<Option<(usize, usize)>>) {
    let mut chars: Vec<char> = Vec::new();
    let mut map: Vec<Option<(usize, usize)>> = Vec::new();
    let mut pending_space = false;
    for (li, l) in lines.iter().enumerate() {
        for (ci, ch) in l.text().chars().enumerate() {
            if ch.is_whitespace() {
                pending_space = true;
                continue;
            }
            if pending_space && !chars.is_empty() {
                chars.push(' ');
                map.push(None);
            }
            pending_space = false;
            chars.push(ch);
            map.push(Some((li, ci)));
        }
        // A line break is whitespace between this line and the next.
        pending_space = true;
    }
    (chars, map)
}

/// Whitespace-normalize a quote the way [`flat_index`] normalizes the section, so
/// the two match. Case is preserved — highlights need the exact text.
fn normalize(quote: &str) -> Vec<char> {
    quote
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect()
}

/// Locate `quote` within the section (first occurrence) and return the cells it
/// covers, grouped into one `[start, end)` column span per display line, in line
/// order. Empty if the quote isn't found (e.g. a hyphenated word that broke
/// differently across the reflow) — the highlight then simply doesn't render.
pub fn resolve_spans(quote: &str, lines: &[DisplayLine]) -> Vec<(usize, (usize, usize))> {
    let needle = normalize(quote);
    if needle.is_empty() {
        return Vec::new();
    }
    let (flat, map) = flat_index(lines);
    let Some(at) = find_subslice(&flat, &needle) else {
        return Vec::new();
    };
    let mut spans: Vec<(usize, (usize, usize))> = Vec::new();
    for cell in map[at..at + needle.len()].iter().flatten() {
        let (line, col) = *cell;
        match spans.last_mut() {
            Some((l, s)) if *l == line => {
                s.0 = s.0.min(col);
                s.1 = s.1.max(col + 1);
            }
            _ => spans.push((line, (col, col + 1))),
        }
    }
    spans
}

/// First index where `needle` occurs in `hay` (naive scan — sections are short).
fn find_subslice(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LineKind, Run};
    use delryn_model::Inline;

    fn line(text: &str) -> DisplayLine {
        DisplayLine {
            runs: vec![Run {
                text: text.into(),
                style: Inline::default(),
                fg: None,
                anchor: None,
            }],
            kind: LineKind::Body,
        }
    }

    #[test]
    fn resolve_spans_finds_a_sub_line_phrase() {
        let lines = vec![line("the quick brown fox"), line("jumps over the dog")];
        // "quick brown" is on line 0, cols 4..15.
        let spans = resolve_spans("quick brown", &lines);
        assert_eq!(spans, vec![(0, (4, 15))]);
    }

    #[test]
    fn resolve_spans_crosses_a_wrap_and_survives_rewrap() {
        // A phrase split across two display lines by the wrap.
        let narrow = vec![line("the quick brown"), line("fox jumps")];
        let spans = resolve_spans("brown fox", &narrow);
        // "brown" on line 0 (cols 10..15), "fox" on line 1 (cols 0..3).
        assert_eq!(spans, vec![(0, (10, 15)), (1, (0, 3))]);

        // Same words wrapped differently → still found (reflow-stable), now one line.
        let wide = vec![line("the quick brown fox jumps")];
        let spans = resolve_spans("brown fox", &wide);
        assert_eq!(spans, vec![(0, (10, 19))]);
    }

    #[test]
    fn selection_text_is_normalized_across_lines() {
        let lines = vec![line("the quick brown"), line("fox jumps")];
        let mut sel = Selection::at(0, 4); // 'q' of quick
        sel.head = Caret { line: 1, col: 2 }; // 'x' of fox
        assert_eq!(sel.text(&lines), "quick brown fox");
    }

    #[test]
    fn motions_move_and_cross_lines() {
        let lines = vec![line("abc"), line("de")];
        let mut sel = Selection::at(0, 0);
        sel.right(&lines);
        sel.right(&lines);
        assert_eq!((sel.head.line, sel.head.col), (0, 2)); // 'c'
        sel.right(&lines); // past line 0 end → line 1 col 0
        assert_eq!((sel.head.line, sel.head.col), (1, 0));
        sel.down(&lines); // already last line — no move
        assert_eq!((sel.head.line, sel.head.col), (1, 0));
        sel.line_end(&lines);
        assert_eq!(sel.head.col, 1); // 'e'
        sel.left(&lines);
        sel.left(&lines); // col0 → cross up to end of line 0
        assert_eq!((sel.head.line, sel.head.col), (0, 2));
    }
}
