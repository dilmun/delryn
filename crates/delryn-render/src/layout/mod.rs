//! Reflow: turn a section's structured blocks into styled display lines for a
//! given content width. We wrap here (rather than letting the widget do it) so
//! scroll position, total line count, and progress % are exact and stable
//! across resizes. The view layer maps these to ratatui styles. See
//! `DESIGN.md` §2.1, §4.
//!
//! This module is the public surface and the orchestrator: [`wrap_blocks`]
//! dispatches each block to a per-kind emit helper. The four wrapping
//! algorithms live in submodules — [`blocks`] (per-kind emit), [`spans`]
//! (styled-span word-wrap), [`table`] (column fitting), and [`code`] (gutter +
//! pack/pan).

mod blocks;
mod code;
mod spans;
mod table;

use delryn_model::{Anchor, Block, Inline};

use blocks::{
    emit_callout, emit_footnote, emit_heading, emit_image, emit_math, emit_para, emit_rule,
};
use code::emit_code;
use table::wrap_table;

/// An RGB foreground colour (from syntax highlighting / themes).
pub type Rgb = (u8, u8, u8);

/// A styled run of text within a display line.
#[derive(Clone)]
pub struct Run {
    pub text: String,
    pub style: Inline,
    /// Explicit foreground colour, if any (syntax highlighting).
    pub fg: Option<Rgb>,
    /// Navigation target carried from the source span (footnote ref / cross-ref /
    /// link), so the reader's link cursor can locate and follow it. `None` for
    /// ordinary text and all non-prose runs (code, table, prefixes…).
    pub anchor: Option<Anchor>,
}

/// What a display line represents, so the view can style it by theme.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Body,
    Heading(u8),
    Quote,
    /// A code line belonging to the code block with this section-local index.
    Code(usize),
    Rule,
    /// A reserved row for the inline image with this section-local index.
    Image(usize),
    /// A footnote-definition line (rendered muted, set apart from the body),
    /// tagged with the footnote's section-local index so the reader can map a
    /// reference to its definition's first line.
    Footnote(usize),
    /// A display-math line (centred, rendered to Unicode).
    Math,
    /// A table row (header, rule, or body), so tables are jump-navigable.
    /// `shaded` marks alternating body rows for zebra striping (header/rule are
    /// never shaded).
    Table {
        shaded: bool,
    },
}

/// One wrapped, styled display line.
#[derive(Clone)]
pub struct DisplayLine {
    pub runs: Vec<Run>,
    pub kind: LineKind,
}

impl DisplayLine {
    fn blank() -> DisplayLine {
        DisplayLine {
            runs: Vec::new(),
            kind: LineKind::Body,
        }
    }

    /// Plain concatenated text (for search / jump-target matching).
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// Options controlling a reflow pass.
pub struct WrapOpts<'a> {
    pub width: usize,
    /// syntect theme used to highlight code.
    pub code_theme: &'a str,
    /// Extra blank lines between wrapped text lines.
    pub line_spacing: u8,
    /// Blank lines between blocks.
    pub para_spacing: u8,
    /// Soft-wrap code to the column (true) vs. keep lines intact and pan
    /// horizontally by `code_hscroll` (false).
    pub code_wrap: bool,
    pub code_hscroll: usize,
    /// Word-wrap table cells to their column (true) vs. truncate with `…` (false).
    pub table_wrap: bool,
    /// Fully justify body paragraphs to the column (true) vs. ragged-right (false).
    pub justify: bool,
    /// Collapse converter spacing artifacts in body text (see [`spans`]).
    pub tidy_spacing: bool,
}

impl Default for WrapOpts<'_> {
    fn default() -> Self {
        WrapOpts {
            width: 72,
            code_theme: "base16-ocean.dark",
            line_spacing: 0,
            para_spacing: 1,
            code_wrap: true,
            code_hscroll: 0,
            table_wrap: true,
            justify: false,
            tidy_spacing: true,
        }
    }
}

/// Wrap a section's blocks to styled display lines per `opts`. `image_rows` gives
/// the reserved row count for each image block (0 → text placeholder).
///
/// Thin dispatch: per block, insert inter-block spacing, then hand the block to
/// its per-kind emit helper. A final pass injects optional inter-line spacing.
pub fn wrap_blocks(blocks: &[Block], opts: &WrapOpts, image_rows: &[u16]) -> Vec<DisplayLine> {
    let width = opts.width.max(1);
    let line_spacing = opts.line_spacing;
    let para_spacing = opts.para_spacing;
    let mut out = Vec::new();
    let mut prev_item = false;
    let mut first = true;
    let mut img_idx = 0usize;
    let mut code_idx = 0usize;
    let mut footnote_idx = 0usize;

    for block in blocks {
        let is_item = matches!(
            block,
            Block::Para {
                marker: Some(_),
                ..
            }
        );

        // Spacing between blocks: blank line(s), except between consecutive list
        // items and around explicit blanks.
        let consecutive_items = is_item && prev_item;
        if !(first || matches!(block, Block::Blank) || consecutive_items) {
            for _ in 0..para_spacing {
                out.push(DisplayLine::blank());
            }
        }

        match block {
            Block::Blank => out.push(DisplayLine::blank()),
            Block::Rule => emit_rule(width, &mut out),
            Block::Heading { level, spans } => emit_heading(*level, spans, width, opts, &mut out),
            Block::Para {
                spans,
                indent,
                quote,
                marker,
            } => emit_para(
                spans,
                *indent,
                *quote,
                marker.as_deref(),
                width,
                opts,
                &mut out,
            ),
            Block::Image { alt, caption, .. } => {
                emit_image(alt, caption, img_idx, image_rows, width, &mut out);
                img_idx += 1;
            }
            Block::Code { lang, lines } => {
                emit_code(lang.as_deref(), lines, code_idx, width, opts, &mut out);
                code_idx += 1;
            }
            Block::Math { tex } => emit_math(tex, width, &mut out),
            Block::Table { header, rows } => {
                wrap_table(header.as_deref(), rows, width, opts.table_wrap, &mut out)
            }
            Block::Callout {
                kind,
                title,
                blocks: inner,
            } => emit_callout(kind, title.as_deref(), inner, opts, &mut out),
            Block::Footnote {
                label,
                blocks: inner,
                ..
            } => {
                emit_footnote(label, inner, footnote_idx, opts, &mut out);
                footnote_idx += 1;
            }
        }

        prev_item = is_item;
        first = false;
    }

    if line_spacing == 0 {
        return out;
    }
    // Insert extra blank lines between text lines (not code/rules).
    let mut spaced = Vec::with_capacity(out.len() * (1 + line_spacing as usize));
    for line in out {
        let spread = !line.runs.is_empty()
            && matches!(
                line.kind,
                LineKind::Body | LineKind::Heading(_) | LineKind::Quote
            );
        spaced.push(line);
        if spread {
            for _ in 0..line_spacing {
                spaced.push(DisplayLine::blank());
            }
        }
    }
    spaced
}

/// Greedy line packing — the one word-wrap algorithm shared by every wrapper.
/// Given each word's display width and the available width per line (line index
/// → columns), return how many words land on each line, joining adjacent words
/// with one space. At least one word is placed per line: a word wider than the
/// line takes its own line and overflows (callers that must not overflow, like
/// fixed-width table cells, hard-break such words *before* calling).
fn pack_words(widths: &[usize], avail: impl Fn(usize) -> usize) -> Vec<usize> {
    let mut counts = Vec::new();
    let mut i = 0;
    while i < widths.len() {
        let cap = avail(counts.len()).max(1);
        let (mut used, mut n) = (0usize, 0usize);
        while i < widths.len() {
            let need = if n == 0 { widths[i] } else { 1 + widths[i] };
            if n > 0 && used + need > cap {
                break;
            }
            used += need;
            n += 1;
            i += 1;
        }
        counts.push(n);
    }
    counts
}

/// Word-wrap plain text to `width` columns, hard-breaking any word longer than
/// the column so no line overflows. Always returns at least one (possibly empty)
/// line. The shared plain-text wrapper — table cells today, reusable by any
/// fixed-width text (status lines, popups, captions…).
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    // Split on whitespace, hard-breaking any word wider than the column so the
    // greedy packer never has to overflow.
    let mut words: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let mut chars: Vec<char> = word.chars().collect();
        while chars.len() > width {
            words.push(chars.drain(..width).collect());
        }
        if !chars.is_empty() {
            words.push(chars.into_iter().collect());
        }
    }
    if words.is_empty() {
        return vec![String::new()];
    }
    let widths: Vec<usize> = words.iter().map(|w| w.chars().count()).collect();
    let mut lines = Vec::new();
    let mut i = 0;
    for n in pack_words(&widths, |_| width) {
        lines.push(words[i..i + n].join(" "));
        i += n;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::wrap_text;

    #[test]
    fn wrap_text_wraps_on_words_and_hard_breaks_long_tokens() {
        // Greedy word wrap to the column.
        assert_eq!(
            wrap_text("the quick brown fox", 9),
            ["the quick", "brown fox"]
        );
        // A token longer than the column is hard-broken, never overflowed.
        assert_eq!(
            wrap_text("supercalifragilistic", 6),
            ["superc", "alifra", "gilist", "ic"]
        );
        // No line ever exceeds the width.
        for line in wrap_text("a verylongunbreakable word here", 7) {
            assert!(line.chars().count() <= 7, "within width: {line:?}");
        }
        // Always at least one line.
        assert_eq!(wrap_text("", 5), [""]);
    }
}
