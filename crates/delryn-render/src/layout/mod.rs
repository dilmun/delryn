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
mod width;

use delryn_model::{Anchor, Block, Inline};

use blocks::{
    emit_callout, emit_footnote, emit_heading, emit_image, emit_math, emit_para, emit_rule,
};
use code::emit_code;
use table::wrap_table;
use width::{display_width, truncate_to_width};

/// An RGB foreground colour (from syntax highlighting / themes).
pub type Rgb = (u8, u8, u8);

/// A styled run of text within a display line.
#[derive(Clone, Default)]
pub struct Run {
    pub text: String,
    pub style: Inline,
    /// Explicit foreground colour, if any (syntax highlighting).
    pub fg: Option<Rgb>,
    /// Navigation target carried from the source span (footnote ref / cross-ref /
    /// link), so the reader's link cursor can locate and follow it. `None` for
    /// ordinary text and all non-prose runs (code, table, prefixes…).
    pub anchor: Option<Anchor>,
    /// The section-local id of an atomic inline-math image occupying this run's
    /// cells. `Some` marks a placeholder run of `text` blank spaces the reader
    /// paints a small equation raster over (see the reader's inline-math draw pass);
    /// `None` for all ordinary text.
    pub math: Option<usize>,
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
    /// Show the line-number gutter in code blocks.
    pub code_line_numbers: bool,
    /// Show a language tag at the top of each code block (skipped for plain text).
    pub code_label: bool,
    /// Collapse code blocks longer than `code_fold_threshold` lines to a preview.
    pub code_fold: bool,
    /// Line count above which a code block folds (when `code_fold` is on).
    pub code_fold_threshold: usize,
    /// Section-local code-block indices whose fold state is *flipped* from the
    /// `code_fold` default — the per-block `F` overrides. A block folds when
    /// `code_fold != code_fold_flip.contains(idx)` and it exceeds the threshold.
    pub code_fold_flip: &'a [usize],
    /// Word-wrap table cells to their column (true) vs. truncate with `…` (false).
    pub table_wrap: bool,
    /// Fully justify body paragraphs to the column (true) vs. ragged-right (false).
    pub justify: bool,
    /// Collapse converter spacing artifacts in body text (see [`spans`]).
    pub tidy_spacing: bool,
    /// Reserved cell width per **inline-math** id (section-local), from the reader's
    /// rendered inline equations. A `SpanMath::Raster` span with a non-zero width
    /// here becomes an atomic image run of that width; empty (the default) → inline
    /// math falls back to its Unicode text (e.g. following continuous sections).
    pub inline_math_cols: &'a [u16],
    /// Reserved cell **height** per inline-math id (parallel to `inline_math_cols`). An
    /// atom with a height > 1 (a fraction) makes its wrapped line reserve a blank spacer
    /// row below, so the raster hangs into empty space rather than over the next line.
    /// Empty / a `0` entry ⇒ one row.
    pub inline_math_rows: &'a [u16],
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
            code_line_numbers: true,
            code_label: false,
            code_fold: false,
            code_fold_threshold: 20,
            code_fold_flip: &[],
            table_wrap: true,
            justify: false,
            tidy_spacing: true,
            inline_math_cols: &[],
            inline_math_rows: &[],
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
    // Set when a block consumed the *next* block as its trailing label — an equation
    // number rendered on the equation's own row, not stranded on a line below.
    let mut skip_next = false;

    for (bi, block) in blocks.iter().enumerate() {
        if std::mem::take(&mut skip_next) {
            continue;
        }
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
            Block::Image {
                alt, caption, math, ..
            } => {
                // A math equation raster directly followed by a lone equation-number
                // paragraph ("Eq. 4") renders that number right-aligned on the
                // equation's last row instead of stranded on its own line below.
                let number = if *math {
                    blocks.get(bi + 1).and_then(equation_number_para)
                } else {
                    None
                };
                emit_image(
                    alt,
                    caption,
                    img_idx,
                    image_rows,
                    width,
                    number.as_deref(),
                    &mut out,
                );
                img_idx += 1;
                skip_next = number.is_some();
            }
            Block::Code { lang, lines } => {
                emit_code(lang.as_deref(), lines, code_idx, width, opts, &mut out);
                code_idx += 1;
            }
            Block::Math { item } => {
                let number = blocks.get(bi + 1).and_then(equation_number_para);
                emit_math(&item.text, width, number.as_deref(), &mut out);
                skip_next = number.is_some();
            }
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

/// The equation-number label of `block` when it is a paragraph that is *solely* a
/// number ("Eq. 4", "Equation 3.2") — the InDesign/Packt caption naming a display
/// equation. Whitespace-normalised. `None` for anything else, so ordinary prose
/// after an equation (even one opening "Eq. 4 shows…") is never consumed.
fn equation_number_para(block: &Block) -> Option<String> {
    let Block::Para {
        spans,
        marker: None,
        ..
    } = block
    else {
        return None;
    };
    let text = spans
        .iter()
        .map(|s| s.text.as_str())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    is_equation_number(&text).then_some(text)
}

/// Whether `text` is exactly an equation-number label: `Eq`/`Eqn`/`Equation`, an
/// optional dot, then a number (`4`, `3.2`, `12`, `(7)`). A strict full-string match
/// so a sentence beginning "Eq. 4 …" is never mistaken for a bare label.
fn is_equation_number(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("equation")
        .or_else(|| lower.strip_prefix("eqn"))
        .or_else(|| lower.strip_prefix("eq"))
    else {
        return false;
    };
    let rest = rest.trim_start_matches(['.', ' ']);
    rest.starts_with(|c: char| c.is_ascii_digit())
        && rest
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '–' | '(' | ')'))
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
    // Split on whitespace, hard-breaking any word wider than the column (by
    // display width) so the greedy packer never has to overflow.
    let mut words: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let mut rest = word;
        while display_width(rest) > width {
            let (mut head, _) = truncate_to_width(rest, width);
            if head.is_empty() {
                // A single glyph is wider than the whole column (e.g. a 2-cell
                // CJK char at width 1); emit it alone so we always make progress.
                head.push(rest.chars().next().unwrap());
            }
            let cut = head.len();
            words.push(head);
            rest = &rest[cut..];
        }
        if !rest.is_empty() {
            words.push(rest.to_string());
        }
    }
    if words.is_empty() {
        return vec![String::new()];
    }
    let widths: Vec<usize> = words.iter().map(display_width).collect();
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
