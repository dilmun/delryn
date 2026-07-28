//! Reader in-book search: query prompt + history, match running, and
//! next/prev navigation between matches.

use super::*;
use crate::search::Matcher;

impl Reader {
    pub fn start_search(&mut self) {
        self.search.searching = true;
        self.search.input.clear();
        self.search.history_pos = None;
    }

    pub fn search_count(&self) -> usize {
        self.search.matches.len()
    }

    /// Cycle the search mode (plain → regex → fuzzy) while typing a query.
    pub fn cycle_search_mode(&mut self) {
        self.search.mode = self.search.mode.next();
    }

    /// Recall the previous (`-1`) or next (`+1`) query from history into the
    /// prompt.
    pub fn search_history_recall(&mut self, dir: i32) {
        if self.search.history.is_empty() {
            return;
        }
        let len = self.search.history.len();
        let pos = match (self.search.history_pos, dir) {
            (None, -1) => len - 1,
            (Some(p), -1) => p.saturating_sub(1),
            (Some(p), 1) if p + 1 < len => p + 1,
            (Some(_), 1) => {
                // Past the newest → back to a fresh, empty prompt.
                self.search.history_pos = None;
                self.search.input.clear();
                return;
            }
            _ => return,
        };
        self.search.history_pos = Some(pos);
        self.search.input = self.search.history[pos].clone();
    }

    /// Drop the active search highlight, keeping the query in history so `/` + Up
    /// recalls it. Returns whether anything was lit — the caller uses that to make
    /// Esc peel one layer at a time.
    pub fn clear_search_highlight(&mut self) -> bool {
        if self.search.matcher.is_none() {
            return false;
        }
        self.search.matcher = None;
        self.search.matches.clear();
        self.search.idx = 0;
        true
    }

    /// Run the typed query across the whole book in the current mode, recording
    /// matching (section, line) positions and jumping to the first.
    pub fn run_search(&mut self) {
        self.search.searching = false;
        let query = self.search.input.trim().to_string();
        self.search.matches.clear();
        self.search.idx = 0;
        self.search.history_pos = None;
        if query.is_empty() {
            self.search.matcher = None;
            return;
        }

        // Record in history (dedup, most-recent last, bounded).
        self.search.history.retain(|q| q != &query);
        self.search.history.push(query.clone());
        if self.search.history.len() > 50 {
            self.search.history.remove(0);
        }

        let matcher = Matcher::new(self.search.mode, &query);
        if matcher.is_valid() {
            let width = self.last_measure.max(1);
            for s in 0..self.doc.section_count() {
                let blocks = self.fetch_blocks(s);
                // Fold must mirror the display so a match's line index lands where it
                // shows: the per-block overrides are the current section's, so only it
                // gets them. (A match in a folded block's hidden tail isn't listed —
                // `Z` unfolds to search the whole block.)
                let flip: &[usize] = if s == self.section {
                    &self.code_fold_flip
                } else {
                    &[]
                };
                // Mirror the display's inline-math atoms for the current section so a
                // match's column/line lands where it shows (equations become blank
                // atom cells, so they're not text-matched — same as the display).
                // Other sections aren't reserved yet, so they wrap their Unicode
                // fallback (searchable, at the cost of a rare off-by-one on jump).
                let (inline_cols, inline_rows): (&[u16], &[u16]) = if s == self.section {
                    (&self.images.inline_cols, &self.images.inline_rows)
                } else {
                    (&[], &[])
                };
                let lines = wrap_blocks(
                    &blocks,
                    &WrapOpts {
                        width,
                        code_theme: &self.code_theme,
                        line_spacing: self.line_spacing,
                        para_spacing: self.paragraph_spacing,
                        // Search always wraps code/tables so no matches are hidden off-screen.
                        code_wrap: true,
                        code_hscroll: 0,
                        // Gutter/label must mirror the display so match line indices align.
                        code_line_numbers: self.code_line_numbers,
                        code_label: self.code_label,
                        code_fold: self.code_fold,
                        code_fold_threshold: self.code_fold_threshold,
                        code_fold_flip: flip,
                        table_wrap: true,
                        // Never justify (keeps single spaces so phrase matches work);
                        // tidy must match the display so positions line up.
                        justify: false,
                        // Hyphenation *does* move line breaks, so it has to match
                        // the display or every match index below it is off by the
                        // lines the two wraps disagree on.
                        hyphenate: self.hyphenate,
                        tidy_spacing: self.tidy_spacing,
                        inline_math_cols: inline_cols,
                        inline_math_rows: inline_rows,
                    },
                    &[],
                );
                for (li, line) in lines.iter().enumerate() {
                    if matcher.matches(&line.text()) {
                        self.search.matches.push((s, li));
                    }
                }
            }
        }
        self.search.matcher = Some(matcher);
        if !self.search.matches.is_empty() {
            self.goto_match(0);
        }
    }

    pub fn search_next(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let i = (self.search.idx + 1) % self.search.matches.len();
        self.goto_match(i);
    }

    pub fn search_prev(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        let n = self.search.matches.len();
        let i = (self.search.idx + n - 1) % n;
        self.goto_match(i);
    }

    fn goto_match(&mut self, i: usize) {
        let Some(&(section, line)) = self.search.matches.get(i) else {
            return;
        };
        self.search.idx = i;
        if section != self.section {
            self.load(section);
        }
        self.scroll = line;
        self.focus = Focus::Content;
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{para, reader_with};

    /// Esc had no effect on a search highlight: it mapped to "clear the link
    /// cursor" and stopped there, so matches stayed lit and the only way out was
    /// running a different search. It peels the highlight once the cursor is gone.
    #[test]
    fn clearing_the_highlight_reports_whether_anything_was_lit() {
        let mut r = reader_with(vec![para()]);
        assert!(
            !r.clear_search_highlight(),
            "nothing lit yet, so Esc must fall through to whatever is next"
        );

        r.search.input = "lorem".into();
        r.run_search();
        assert!(r.search.matcher.is_some(), "the search lit some matches");

        assert!(r.clear_search_highlight(), "Esc consumes the highlight");
        assert!(r.search.matcher.is_none(), "and the highlight is gone");
        assert!(r.search.matches.is_empty());
        assert_eq!(r.search.idx, 0);
        // A second Esc has nothing left to take, so it falls through again.
        assert!(!r.clear_search_highlight());
    }

    /// The query survives in history — Esc dismisses the highlight, it doesn't
    /// forget what you searched for.
    #[test]
    fn clearing_the_highlight_keeps_the_query_recallable() {
        let mut r = reader_with(vec![para()]);
        r.search.input = "ipsum".into();
        r.run_search();
        r.clear_search_highlight();
        assert!(
            r.search.history.iter().any(|q| q == "ipsum"),
            "history should still hold the query for `/` + Up"
        );
    }
}
