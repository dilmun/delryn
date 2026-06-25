//! Reflow: turn a section's structured blocks into styled display lines for a
//! given content width. We wrap here (rather than letting the widget do it) so
//! scroll position, total line count, and progress % are exact and stable
//! across resizes. The view layer maps these to ratatui styles. See
//! `DESIGN.md` §2.1, §4.

use crate::highlight::highlight_code;
use delryn_model::{Anchor, Block, Inline, Span, TableCell};

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
        }
    }
}

/// Wrap a section's blocks to styled display lines per `opts`. `image_rows` gives
/// the reserved row count for each image block (0 → text placeholder).
pub fn wrap_blocks(blocks: &[Block], opts: &WrapOpts, image_rows: &[u16]) -> Vec<DisplayLine> {
    let width = opts.width.max(1);
    let code_theme = opts.code_theme;
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
            Block::Rule => out.push(DisplayLine {
                runs: vec![Run {
                    text: "─".repeat(width),
                    style: Inline::default(),
                    fg: None,
                    anchor: None,
                }],
                kind: LineKind::Rule,
            }),
            Block::Heading { level, spans } => {
                wrap_spans(spans, width, "", "", LineKind::Heading(*level), &mut out)
            }
            Block::Para {
                spans,
                indent,
                quote,
                marker,
            } => {
                let ind = "  ".repeat(*indent as usize);
                let (first_prefix, cont_prefix, kind) = if *quote {
                    (format!("{ind}▎ "), format!("{ind}▎ "), LineKind::Quote)
                } else if let Some(m) = marker {
                    let pad = " ".repeat(m.chars().count());
                    (format!("{ind}{m}"), format!("{ind}{pad}"), LineKind::Body)
                } else {
                    (ind.clone(), ind.clone(), LineKind::Body)
                };
                wrap_spans(spans, width, &first_prefix, &cont_prefix, kind, &mut out);
            }
            Block::Image { alt, caption, .. } => {
                // The reader pre-computes each image's height; reserve exactly
                // that many blank rows. Zero rows → no image support → show a
                // text placeholder instead.
                let rows = image_rows.get(img_idx).copied().unwrap_or(0);
                if rows > 0 {
                    for _ in 0..rows {
                        out.push(DisplayLine {
                            runs: Vec::new(),
                            kind: LineKind::Image(img_idx),
                        });
                    }
                } else {
                    let label = if alt.is_empty() {
                        "[image]".to_string()
                    } else {
                        format!("[image: {alt}]")
                    };
                    out.push(DisplayLine {
                        runs: vec![Run {
                            text: label,
                            style: Inline::default(),
                            fg: None,
                            anchor: None,
                        }],
                        kind: LineKind::Body,
                    });
                }
                // Figure caption (italic), wrapped under the image.
                if !caption.is_empty() {
                    let italic: Vec<Span> = caption
                        .iter()
                        .map(|s| Span {
                            text: s.text.clone(),
                            style: Inline {
                                italic: true,
                                ..s.style
                            },
                            anchor: s.anchor.clone(),
                        })
                        .collect();
                    wrap_spans(&italic, width, "", "", LineKind::Body, &mut out);
                }
                img_idx += 1;
            }
            Block::Code { lang, lines } => {
                let gutter_w = lines.len().max(1).to_string().len();
                let highlighted = highlight_code(lines, lang.as_deref(), code_theme);
                for (i, runs) in highlighted.into_iter().enumerate() {
                    let num = format!("{:>gutter_w$} │ ", i + 1);
                    let avail = width.saturating_sub(num.chars().count()).max(1);
                    if opts.code_wrap {
                        // Soft-wrap: one source line spills onto several rows.
                        for (j, mut line_runs) in pack_runs(runs, avail).into_iter().enumerate() {
                            let gutter = if j == 0 {
                                num.clone()
                            } else {
                                format!("{:>gutter_w$}   ", "")
                            };
                            let mut full = vec![Run {
                                text: gutter,
                                style: Inline::default(),
                                fg: None,
                                anchor: None,
                            }];
                            full.append(&mut line_runs);
                            pad_to_width(&mut full, width);
                            out.push(DisplayLine {
                                runs: full,
                                kind: LineKind::Code(code_idx),
                            });
                        }
                    } else {
                        // No-wrap: one row per source line, panned by code_hscroll.
                        let mut full = vec![Run {
                            text: num,
                            style: Inline::default(),
                            fg: None,
                            anchor: None,
                        }];
                        full.extend(shift_runs(runs, opts.code_hscroll, avail));
                        pad_to_width(&mut full, width);
                        out.push(DisplayLine {
                            runs: full,
                            kind: LineKind::Code(code_idx),
                        });
                    }
                }
                code_idx += 1;
            }
            Block::Math { tex } => {
                // `tex` is already Unicode (the parser resolved LaTeX/MathML);
                // just centre each line.
                for line in tex.lines() {
                    let text = line.trim_end();
                    let pad = width.saturating_sub(text.chars().count()) / 2;
                    out.push(DisplayLine {
                        runs: vec![Run {
                            text: format!("{}{text}", " ".repeat(pad)),
                            style: Inline {
                                italic: true,
                                ..Inline::default()
                            },
                            fg: None,
                            anchor: None,
                        }],
                        kind: LineKind::Math,
                    });
                }
            }
            Block::Table { header, rows } => {
                wrap_table(header.as_deref(), rows, width, opts.table_wrap, &mut out)
            }
            Block::Callout {
                kind,
                title,
                blocks,
            } => {
                // A themed glyph leads the header, then the label/title — a clean
                // icon in place of the publisher's raster admonition icon.
                let head = format!(
                    "{} {}",
                    kind.glyph(),
                    title.clone().unwrap_or_else(|| kind.label().to_string())
                );
                out.push(DisplayLine {
                    runs: vec![
                        Run {
                            text: "▌ ".to_string(),
                            style: Inline::default(),
                            fg: None,
                            anchor: None,
                        },
                        Run {
                            text: head,
                            style: Inline {
                                bold: true,
                                ..Inline::default()
                            },
                            fg: None,
                            anchor: None,
                        },
                    ],
                    kind: LineKind::Quote,
                });
                wrap_nested(blocks, opts, "▌ ", LineKind::Quote, &mut out);
            }
            Block::Footnote { label, blocks, .. } => {
                // Render "[label] body" on one line: wrap the body indented by the
                // label's width, then drop the bold label onto its first line.
                let tag = format!("[{label}] ");
                let pad = " ".repeat(tag.chars().count());
                let start = out.len();
                wrap_nested(
                    blocks,
                    opts,
                    &pad,
                    LineKind::Footnote(footnote_idx),
                    &mut out,
                );
                let label_run = Run {
                    text: tag,
                    style: Inline {
                        bold: true,
                        ..Inline::default()
                    },
                    fg: None,
                    anchor: None,
                };
                match out.get_mut(start).and_then(|l| l.runs.first_mut()) {
                    // Replace the first body line's indent prefix with the label.
                    Some(prefix) => *prefix = label_run,
                    // Empty body → just the label.
                    None => out.push(DisplayLine {
                        runs: vec![label_run],
                        kind: LineKind::Footnote(footnote_idx),
                    }),
                }
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

/// Word-wrap styled spans into display lines, with a prefix on the first line
/// and a (usually padding) prefix on continuations.
fn wrap_spans(
    spans: &[Span],
    width: usize,
    first_prefix: &str,
    cont_prefix: &str,
    kind: LineKind,
    out: &mut Vec<DisplayLine>,
) {
    // Flatten the spans to (char, style) preserving adjacency, then split into
    // words on *actual* whitespace. A word is a run of non-whitespace chars that
    // may span several source spans — so adjacent spans with no whitespace
    // between them join with no inserted space. This matters for math, which
    // some publishers (InDesign MathTools) emit as one span per glyph
    // (`𝔼`,`(`,`X`,`)`): the old per-span split turned that into `𝔼 ( X )`.
    let chars = spans
        .iter()
        .flat_map(|s| s.text.chars().map(move |c| (c, s.style, s.anchor.clone())));
    let mut words: Vec<Vec<(char, Inline, Option<Anchor>)>> = Vec::new();
    let mut cur: Vec<(char, Inline, Option<Anchor>)> = Vec::new();
    for (c, st, an) in chars {
        if c.is_whitespace() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push((c, st, an));
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    if words.is_empty() {
        return;
    }

    // Pack words into lines with the shared greedy wrapper; the available width
    // shrinks by the first/continuation prefix.
    let prefix_for = |line: usize| if line == 0 { first_prefix } else { cont_prefix };
    let widths: Vec<usize> = words.iter().map(Vec::len).collect();
    let counts = pack_words(&widths, |line| {
        width
            .saturating_sub(prefix_for(line).chars().count())
            .max(1)
    });

    let mut i = 0;
    for (line, &count) in counts.iter().enumerate() {
        let prefix = prefix_for(line);
        let mut runs: Vec<Run> = Vec::new();
        if !prefix.is_empty() {
            runs.push(Run {
                text: prefix.to_string(),
                style: Inline::default(),
                fg: None,
                anchor: None,
            });
        }
        for k in 0..count {
            if k > 0 {
                runs.push(Run {
                    text: " ".to_string(),
                    style: Inline::default(),
                    fg: None,
                    anchor: None,
                });
            }
            push_word_runs(&words[i], &mut runs);
            i += 1;
        }
        out.push(DisplayLine { runs, kind });
    }
}

/// Pad a line's runs with trailing spaces so it spans `width` columns. Used for
/// code blocks so their surface panel fills the column as a clean rectangle (the
/// filler inherits the line's `kind` background at render time).
fn pad_to_width(runs: &mut Vec<Run>, width: usize) {
    let len: usize = runs.iter().map(|r| r.text.chars().count()).sum();
    if len < width {
        runs.push(Run {
            text: " ".repeat(width - len),
            style: Inline::default(),
            fg: None,
            anchor: None,
        });
    }
}

/// Append one word's chars as runs, coalescing consecutive chars of equal style
/// *and* anchor (a word may mix styles — a bold glyph next to a normal one with
/// no whitespace between source spans — or carry a navigation anchor on part of
/// it, e.g. a footnote-ref superscript).
fn push_word_runs(word: &[(char, Inline, Option<Anchor>)], runs: &mut Vec<Run>) {
    let mut buf = String::new();
    let mut cur: Option<(Inline, Option<Anchor>)> = None;
    for (c, st, an) in word {
        let key = (*st, an.clone());
        if cur.as_ref() == Some(&key) {
            buf.push(*c);
        } else {
            if let Some((s, a)) = cur.take() {
                runs.push(Run {
                    text: std::mem::take(&mut buf),
                    style: s,
                    fg: None,
                    anchor: a,
                });
            }
            buf.push(*c);
            cur = Some(key);
        }
    }
    if let Some((s, a)) = cur {
        runs.push(Run {
            text: buf,
            style: s,
            fg: None,
            anchor: a,
        });
    }
}

/// Take a horizontal window of styled runs: skip the first `skip` columns, then
/// keep up to `avail` columns, preserving each run's style. For no-wrap code
/// panning.
fn shift_runs(runs: Vec<Run>, skip: usize, avail: usize) -> Vec<Run> {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let mut taken = 0usize;
    for run in runs {
        if taken >= avail {
            break;
        }
        let mut text = String::new();
        for c in run.text.chars() {
            if skipped < skip {
                skipped += 1;
                continue;
            }
            if taken >= avail {
                break;
            }
            text.push(c);
            taken += 1;
        }
        if !text.is_empty() {
            out.push(Run {
                text,
                style: run.style,
                fg: run.fg,
                anchor: None,
            });
        }
    }
    out
}

/// Soft-wrap styled runs to `avail` columns, splitting runs at character
/// boundaries and preserving each run's style/colour. Used for code lines.
fn pack_runs(runs: Vec<Run>, avail: usize) -> Vec<Vec<Run>> {
    let mut out: Vec<Vec<Run>> = Vec::new();
    let mut cur: Vec<Run> = Vec::new();
    let mut len = 0usize;

    for run in runs {
        let chars: Vec<char> = run.text.chars().collect();
        let mut idx = 0;
        while idx < chars.len() {
            if len >= avail {
                out.push(std::mem::take(&mut cur));
                len = 0;
            }
            let take = (avail - len).min(chars.len() - idx);
            cur.push(Run {
                text: chars[idx..idx + take].iter().collect(),
                style: run.style,
                fg: run.fg,
                anchor: None,
            });
            len += take;
            idx += take;
        }
    }

    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

/// Render inner blocks of a callout/footnote at reduced width, each line
/// prefixed with `border` and retagged as `kind` — so nested code/images degrade
/// to text and never disturb the top-level image/code indices.
fn wrap_nested(
    blocks: &[Block],
    opts: &WrapOpts,
    border: &str,
    kind: LineKind,
    out: &mut Vec<DisplayLine>,
) {
    let inner = WrapOpts {
        width: opts.width.saturating_sub(border.chars().count()).max(1),
        code_theme: opts.code_theme,
        line_spacing: 0,
        para_spacing: opts.para_spacing,
        code_wrap: true,
        code_hscroll: 0,
        table_wrap: opts.table_wrap,
    };
    for line in wrap_blocks(blocks, &inner, &[]) {
        let mut runs = Vec::with_capacity(line.runs.len() + 1);
        runs.push(Run {
            text: border.to_string(),
            style: Inline::default(),
            fg: None,
            anchor: None,
        });
        runs.extend(line.runs);
        out.push(DisplayLine { runs, kind });
    }
}

/// Plain concatenated text of a table cell (for width measurement / rendering).
/// Allocate `budget` columns across table columns of the given natural widths,
/// max-min fair: narrow columns keep their full width, and wide columns split
/// whatever's left. This keeps a short column (e.g. a yes/no flag) readable
/// instead of letting a proportional shrink starve it to one character while a
/// long prose column hogs the space.
fn fit_columns(natural: &[usize], budget: usize) -> Vec<usize> {
    let mut out = vec![0usize; natural.len()];
    let mut remaining = budget;
    let mut pending: Vec<usize> = (0..natural.len()).collect();
    while !pending.is_empty() {
        let share = (remaining / pending.len()).max(1);
        // Columns that fit within an equal share take their natural width; the
        // freed space then redistributes among the still-too-wide columns.
        let fits: Vec<usize> = pending
            .iter()
            .copied()
            .filter(|&i| natural[i] <= share)
            .collect();
        if fits.is_empty() {
            for &i in &pending {
                out[i] = share;
            }
            break;
        }
        for &i in &fits {
            out[i] = natural[i];
            remaining = remaining.saturating_sub(natural[i]);
        }
        pending.retain(|i| !fits.contains(i));
    }
    out
}

fn table_cell_text(cell: &[Span]) -> String {
    // Cells often hold `<p>`/whitespace, so the raw text carries newlines and
    // runs of spaces. Collapse to a single line — otherwise an embedded newline
    // breaks the row mid-render and the column separators stop lining up.
    let raw: String = cell.iter().map(|s| s.text.as_str()).collect();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate (with `…`) or right-pad `s` to exactly `w` display columns.
fn fit(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n == w {
        s.to_string()
    } else if n < w {
        format!("{s}{}", " ".repeat(w - n))
    } else if w <= 1 {
        "…".repeat(w)
    } else {
        let mut t: String = s.chars().take(w - 1).collect();
        t.push('…');
        t
    }
}

/// One logical table row → display lines. When `wrap`, each cell word-wraps to
/// its column width and the row is as tall as its tallest cell; otherwise each
/// cell is truncated to a single line. Either way the " │ " separators stay
/// aligned on every line (blank where a cell ran out). `shaded` zebra-stripes
/// the whole logical row (all its wrapped lines share the band).
fn table_row(
    cells: &[TableCell],
    col_w: &[usize],
    bold: bool,
    wrap: bool,
    shaded: bool,
) -> Vec<DisplayLine> {
    let wrapped: Vec<Vec<String>> = col_w
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let raw = cells.get(i).map(|c| table_cell_text(c)).unwrap_or_default();
            if wrap {
                wrap_text(&raw, *w)
            } else {
                vec![fit(&raw, *w)]
            }
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
    (0..height)
        .map(|line| {
            let text = col_w
                .iter()
                .enumerate()
                .map(|(i, w)| fit(wrapped[i].get(line).map_or("", String::as_str), *w))
                .collect::<Vec<_>>()
                .join(" │ ");
            DisplayLine {
                runs: vec![Run {
                    text,
                    style: Inline {
                        bold,
                        ..Inline::default()
                    },
                    fg: None,
                    anchor: None,
                }],
                kind: LineKind::Table { shaded },
            }
        })
        .collect()
}

/// Render a table to aligned text rows (header bold + a rule), fitting `width`.
fn wrap_table(
    header: Option<&[TableCell]>,
    rows: &[Vec<TableCell>],
    width: usize,
    wrap: bool,
    out: &mut Vec<DisplayLine>,
) {
    let mut ncols = header.map_or(0, <[_]>::len);
    for r in rows {
        ncols = ncols.max(r.len());
    }
    if ncols == 0 {
        return;
    }

    // Natural column widths from the widest cell in each column.
    let mut col_w = vec![0usize; ncols];
    let mut note = |cells: &[TableCell]| {
        for (i, c) in cells.iter().enumerate() {
            col_w[i] = col_w[i].max(table_cell_text(c).chars().count());
        }
    };
    if let Some(h) = header {
        note(h);
    }
    for r in rows {
        note(r);
    }

    // Fit to the pane: reserve " │ " (3 cols) between columns, then allocate the
    // remaining budget max-min fair if the natural widths overflow.
    let budget = width.saturating_sub(3 * ncols.saturating_sub(1)).max(ncols);
    if col_w.iter().sum::<usize>() > budget {
        col_w = fit_columns(&col_w, budget);
    }

    if let Some(h) = header {
        out.extend(table_row(h, &col_w, true, wrap, false));
        out.push(DisplayLine {
            runs: vec![Run {
                text: col_w
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─"),
                style: Inline::default(),
                fg: None,
                anchor: None,
            }],
            kind: LineKind::Table { shaded: false },
        });
    }
    // Zebra-stripe body rows (every other logical row) for readability.
    for (i, r) in rows.iter().enumerate() {
        out.extend(table_row(r, &col_w, false, wrap, i % 2 == 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delryn_model::CalloutKind;

    fn texts(lines: &[DisplayLine]) -> Vec<String> {
        lines.iter().map(DisplayLine::text).collect()
    }

    fn para(s: &str) -> Block {
        Block::Para {
            spans: vec![Span::plain(s)],
            indent: 0,
            quote: false,
            marker: None,
        }
    }

    #[test]
    fn callout_renders_header_and_bordered_body() {
        let block = Block::Callout {
            kind: CalloutKind::Warning,
            title: None,
            blocks: vec![para("be careful here")],
        };
        let lines = wrap_blocks(&[block], &WrapOpts::default(), &[]);
        let joined = texts(&lines).join("\n");
        assert!(joined.contains("WARNING"), "header label: {joined:?}");
        assert!(
            joined.contains(CalloutKind::Warning.glyph()),
            "themed glyph in header: {joined:?}"
        );
        assert!(joined.contains("be careful"), "bordered body: {joined:?}");
        assert!(
            lines.iter().any(|l| l.text().starts_with('▌')),
            "left border on callout lines"
        );
        // A custom title overrides the kind label.
        let titled = Block::Callout {
            kind: CalloutKind::Note,
            title: Some("Heads up".into()),
            blocks: vec![para("x")],
        };
        let t = texts(&wrap_blocks(&[titled], &WrapOpts::default(), &[])).join("\n");
        assert!(t.contains("Heads up") && !t.contains("NOTE"));
    }

    #[test]
    fn code_lines_are_padded_to_width_for_a_clean_panel() {
        let block = Block::Code {
            lang: Some("text".into()),
            lines: vec!["x = 1".into()],
        };
        let opts = WrapOpts {
            width: 40,
            ..Default::default()
        };
        let lines = wrap_blocks(&[block], &opts, &[]);
        let code: Vec<&DisplayLine> = lines
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Code(_)))
            .collect();
        assert!(!code.is_empty(), "produced a code line");
        for l in code {
            assert_eq!(
                l.text().chars().count(),
                40,
                "code line padded to width: {:?}",
                l.text()
            );
        }
    }

    #[test]
    fn table_aligns_header_rule_and_rows() {
        let cell = |s: &str| vec![Span::plain(s)];
        let block = Block::Table {
            header: Some(vec![cell("Name"), cell("Qty")]),
            rows: vec![vec![cell("Apples"), cell("12")]],
        };
        let lines = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[]));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Name") && l.contains("Qty"))
        );
        assert!(lines.iter().any(|l| l.contains('┼')), "header rule");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Apples") && l.contains("12"))
        );
    }

    #[test]
    fn table_cell_newlines_dont_break_alignment() {
        // Cells holding `<p>`/whitespace carry newlines; rows must stay one line
        // each with the column separators in a fixed position.
        let cell = |s: &str| vec![Span::plain(s)];
        let block = Block::Table {
            header: Some(vec![cell("\nName\n"), cell(" Qty ")]),
            rows: vec![vec![cell("Ap\nples"), cell("12")]],
        };
        let table: Vec<String> = wrap_blocks(&[block], &WrapOpts::default(), &[])
            .into_iter()
            .filter(|l| matches!(l.kind, LineKind::Table { .. }))
            .map(|l| l.text())
            .collect();
        let sep = |t: &str| -> Vec<usize> {
            t.chars()
                .enumerate()
                .filter(|(_, c)| *c == '│' || *c == '┼')
                .map(|(i, _)| i)
                .collect()
        };
        let positions: Vec<Vec<usize>> = table.iter().map(|t| sep(t)).collect();
        for t in &table {
            assert!(!t.contains('\n'), "row is one line: {t:?}");
        }
        assert!(
            positions.windows(2).all(|w| w[0] == w[1]),
            "separators aligned across rows: {positions:?}"
        );
    }

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

    #[test]
    fn table_cells_wrap_to_aligned_lines_instead_of_truncating() {
        let cell = |s: &str| vec![Span::plain(s)];
        let block = Block::Table {
            header: Some(vec![cell("Name"), cell("Notes")]),
            rows: vec![vec![
                cell("Apples"),
                cell("a long description that must wrap across several lines"),
            ]],
        };
        let opts = WrapOpts {
            width: 30,
            ..WrapOpts::default()
        };
        let table: Vec<String> = wrap_blocks(&[block], &opts, &[])
            .into_iter()
            .filter(|l| matches!(l.kind, LineKind::Table { .. }))
            .map(|l| l.text())
            .collect();
        // header + rule + a data row that wrapped to several lines.
        assert!(table.len() > 3, "data row wrapped: {table:?}");
        // Wraps rather than truncating, and never exceeds the width.
        assert!(
            !table.iter().any(|t| t.contains('…')),
            "no truncation: {table:?}"
        );
        for t in &table {
            assert!(t.chars().count() <= 30, "within width: {t:?}");
        }
        assert!(
            table.concat().contains("description") && table.concat().contains("lines"),
            "full text preserved: {table:?}"
        );
        // Column separators stay aligned on every wrapped line.
        let sep = |t: &str| -> Vec<usize> {
            t.chars()
                .enumerate()
                .filter(|(_, c)| *c == '│' || *c == '┼')
                .map(|(i, _)| i)
                .collect()
        };
        let positions: Vec<Vec<usize>> = table.iter().map(|t| sep(t)).collect();
        assert!(
            positions.windows(2).all(|w| w[0] == w[1]),
            "separators aligned: {positions:?}"
        );
    }

    #[test]
    fn table_truncates_when_wrap_is_disabled() {
        let cell = |s: &str| vec![Span::plain(s)];
        let block = Block::Table {
            header: Some(vec![cell("Name"), cell("Notes")]),
            rows: vec![vec![
                cell("Apples"),
                cell("a long description that would otherwise wrap"),
            ]],
        };
        let opts = WrapOpts {
            width: 30,
            table_wrap: false,
            ..WrapOpts::default()
        };
        let table: Vec<String> = wrap_blocks(&[block], &opts, &[])
            .into_iter()
            .filter(|l| matches!(l.kind, LineKind::Table { .. }))
            .map(|l| l.text())
            .collect();
        // Wrap off → header + rule + exactly one line per row, truncated with `…`.
        assert_eq!(table.len(), 3, "one line per row: {table:?}");
        assert!(
            table.iter().any(|t| t.contains('…')),
            "truncated: {table:?}"
        );
    }

    #[test]
    fn table_body_rows_zebra_stripe() {
        let cell = |s: &str| vec![Span::plain(s)];
        let block = Block::Table {
            header: Some(vec![cell("A"), cell("B")]),
            rows: vec![
                vec![cell("1"), cell("2")],
                vec![cell("3"), cell("4")],
                vec![cell("5"), cell("6")],
            ],
        };
        let shaded: Vec<bool> = wrap_blocks(&[block], &WrapOpts::default(), &[])
            .into_iter()
            .filter_map(|l| match l.kind {
                LineKind::Table { shaded } => Some(shaded),
                _ => None,
            })
            .collect();
        // header, rule, then body rows alternating (1st body unshaded).
        assert_eq!(
            shaded,
            vec![false, false, false, true, false],
            "header/rule never shaded; body rows alternate"
        );
    }

    #[test]
    fn anchors_survive_wrapping_onto_their_run() {
        // A footnote-ref span must reach the view as a Run carrying its Anchor,
        // so the reader's link cursor can locate and follow it.
        let spans = vec![
            Span::plain("see "),
            Span {
                text: "7".to_string(),
                style: Inline::default(),
                anchor: Some(Anchor::Footnote("fn7".to_string())),
            },
            Span::plain(" for details"),
        ];
        let block = Block::Para {
            spans,
            indent: 0,
            quote: false,
            marker: None,
        };
        let lines = wrap_blocks(&[block], &WrapOpts::default(), &[]);
        let anchored: Vec<(&str, &Anchor)> = lines
            .iter()
            .flat_map(|l| &l.runs)
            .filter_map(|r| r.anchor.as_ref().map(|a| (r.text.as_str(), a)))
            .collect();
        assert_eq!(anchored.len(), 1, "exactly the ref run is anchored");
        assert_eq!(anchored[0].0, "7");
        assert_eq!(anchored[0].1, &Anchor::Footnote("fn7".to_string()));
    }

    #[test]
    fn footnote_definitions_are_indexed_per_section() {
        let cell = |s: &str| vec![Span::plain(s)];
        let foot = |id: &str, label: &str, body: &str| Block::Footnote {
            id: id.to_string(),
            label: label.to_string(),
            blocks: vec![Block::Para {
                spans: cell(body),
                indent: 0,
                quote: false,
                marker: None,
            }],
        };
        let lines = wrap_blocks(
            &[foot("fn1", "1", "first"), foot("fn2", "2", "second")],
            &WrapOpts::default(),
            &[],
        );
        // Each definition's lines carry its own section-local index.
        assert!(
            lines
                .iter()
                .any(|l| matches!(l.kind, LineKind::Footnote(0)))
        );
        assert!(
            lines
                .iter()
                .any(|l| matches!(l.kind, LineKind::Footnote(1)))
        );
    }

    #[test]
    fn fit_columns_keeps_narrow_columns_readable() {
        // A short flag column shouldn't be starved by a long prose column.
        let w = fit_columns(&[5, 9, 80], 40);
        assert_eq!(w[0], 5, "narrow kept");
        assert_eq!(w[1], 9, "flag column kept");
        assert!(
            w[2] >= 20 && w.iter().sum::<usize>() <= 40,
            "rest to prose: {w:?}"
        );
    }

    #[test]
    fn math_centres_prerendered_unicode() {
        // The parser resolves LaTeX/MathML to Unicode; layout only centres it.
        let block = Block::Math {
            tex: "α + β".to_string(),
        };
        let lines = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[]));
        let joined = lines.join("\n");
        assert!(joined.contains("α + β"), "{joined:?}");
        // Centred: leading padding before the equation.
        assert!(
            lines.iter().any(|l| l.starts_with(' ')),
            "centred: {lines:?}"
        );
    }

    #[test]
    fn image_caption_renders_below_placeholder() {
        let block = Block::Image {
            src: "x.png".into(),
            alt: "diagram".into(),
            data: Vec::new(),
            caption: vec![Span::plain("Figure 1: the pipeline")],
            math: false,
        };
        // No image protocol (image_rows empty) → text placeholder + caption.
        let lines = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[]));
        let joined = lines.join("\n");
        assert!(joined.contains("[image: diagram]"));
        assert!(joined.contains("Figure 1: the pipeline"));
    }

    #[test]
    fn adjacent_spans_join_without_inserted_spaces() {
        // Per-glyph spans (as InDesign MathTools emits for math) with explicit
        // space spans only where the publisher intended them. The wrapper must
        // join touching glyphs with no space, and honour the real spaces.
        let glyphs = ["𝔼", "(", "X", ")", " ", "=", " ", "∑", "x"];
        let spans: Vec<Span> = glyphs.iter().map(|g| Span::plain(*g)).collect();
        let block = Block::Para {
            spans,
            indent: 0,
            quote: false,
            marker: None,
        };
        let joined = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[])).join("\n");
        assert!(joined.contains("𝔼(X) = ∑x"), "{joined:?}");
        assert!(
            !joined.contains("𝔼 ( X )"),
            "no per-glyph spaces: {joined:?}"
        );
    }

    #[test]
    fn whitespace_runs_collapse_to_single_space() {
        let block = Block::Para {
            spans: vec![Span::plain("a   b\tc")],
            indent: 0,
            quote: false,
            marker: None,
        };
        let joined = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[])).join("\n");
        assert!(joined.contains("a b c"), "{joined:?}");
    }

    #[test]
    fn footnote_renders_label_and_body() {
        let block = Block::Footnote {
            id: "fn1".into(),
            label: "1".into(),
            blocks: vec![para("the cited source")],
        };
        let joined = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[])).join("\n");
        assert!(joined.contains("[1]"));
        assert!(joined.contains("the cited source"));
    }
}
