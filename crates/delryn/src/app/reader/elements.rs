//! Jump-by-type navigation over the "rich elements" of a section — code blocks,
//! tables, math, figures, and footnotes.
//!
//! The wrapped lines carry a [`LineKind`] tag; `element_starts` reduces them to
//! the first line of each element run, and `w`/`b` ([`next_element`](Reader::next_element)
//! / [`prev_element`](Reader::prev_element)) step the reading position between
//! those anchors, flashing "`kind N/M`". `copy_visible_code` grabs the code block
//! currently in view. Reflow-only; operates on the current section's `lines`.

use super::*;

impl Reader {
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
    pub(super) fn element_starts(&self) -> Vec<(usize, &'static str)> {
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
}
