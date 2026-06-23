//! Reflow: turn a section's structured blocks into styled display lines for a
//! given content width. We wrap here (rather than letting the widget do it) so
//! scroll position, total line count, and progress % are exact and stable
//! across resizes. The view layer maps these to ratatui styles. See
//! `DESIGN.md` §2.1, §4.

use crate::highlight::highlight_code;
use delryn_model::{Block, Inline, Span, TableCell, math};

/// An RGB foreground colour (from syntax highlighting / themes).
pub type Rgb = (u8, u8, u8);

/// A styled run of text within a display line.
#[derive(Clone)]
pub struct Run {
    pub text: String,
    pub style: Inline,
    /// Explicit foreground colour, if any (syntax highlighting).
    pub fg: Option<Rgb>,
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
                            }];
                            full.append(&mut line_runs);
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
                        }];
                        full.extend(shift_runs(runs, opts.code_hscroll, avail));
                        out.push(DisplayLine {
                            runs: full,
                            kind: LineKind::Code(code_idx),
                        });
                    }
                }
                code_idx += 1;
            }
            Block::Math { tex } => {
                // Render TeX-ish source to Unicode and centre each line. (Rich
                // inline detection + navigation land in the math task.)
                let uni = math::latex_to_unicode(tex);
                for line in uni.lines() {
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
                        }],
                        kind: LineKind::Body,
                    });
                }
            }
            Block::Table { header, rows } => wrap_table(header.as_deref(), rows, width, &mut out),
            Block::Callout {
                kind,
                title,
                blocks,
            } => {
                let head = title.clone().unwrap_or_else(|| kind.label().to_string());
                out.push(DisplayLine {
                    runs: vec![
                        Run {
                            text: "▌ ".to_string(),
                            style: Inline::default(),
                            fg: None,
                        },
                        Run {
                            text: head,
                            style: Inline {
                                bold: true,
                                ..Inline::default()
                            },
                            fg: None,
                        },
                    ],
                    kind: LineKind::Quote,
                });
                wrap_nested(blocks, opts, "▌ ", LineKind::Quote, &mut out);
            }
            Block::Footnote { label, blocks } => {
                out.push(DisplayLine {
                    runs: vec![Run {
                        text: format!("[{label}]"),
                        style: Inline {
                            bold: true,
                            ..Inline::default()
                        },
                        fg: None,
                    }],
                    kind: LineKind::Body,
                });
                wrap_nested(blocks, opts, "  ", LineKind::Body, &mut out);
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
    let words: Vec<(&str, Inline)> = spans
        .iter()
        .flat_map(|s| s.text.split_whitespace().map(move |w| (w, s.style)))
        .collect();
    if words.is_empty() {
        return;
    }

    let mut i = 0;
    let mut first_line = true;
    while i < words.len() {
        let prefix = if first_line {
            first_prefix
        } else {
            cont_prefix
        };
        let avail = width.saturating_sub(prefix.chars().count()).max(1);

        let mut runs: Vec<Run> = Vec::new();
        if !prefix.is_empty() {
            runs.push(Run {
                text: prefix.to_string(),
                style: Inline::default(),
                fg: None,
            });
        }

        let mut len = 0usize;
        let mut placed = 0usize;
        while i < words.len() {
            let (word, style) = words[i];
            let wlen = word.chars().count();
            let need = if placed == 0 { wlen } else { 1 + wlen };
            if placed > 0 && len + need > avail {
                break;
            }
            if placed > 0 {
                runs.push(Run {
                    text: " ".to_string(),
                    style: Inline::default(),
                    fg: None,
                });
                len += 1;
            }
            runs.push(Run {
                text: word.to_string(),
                style,
                fg: None,
            });
            len += wlen;
            placed += 1;
            i += 1;
        }

        out.push(DisplayLine { runs, kind });
        first_line = false;
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
    };
    for line in wrap_blocks(blocks, &inner, &[]) {
        let mut runs = Vec::with_capacity(line.runs.len() + 1);
        runs.push(Run {
            text: border.to_string(),
            style: Inline::default(),
            fg: None,
        });
        runs.extend(line.runs);
        out.push(DisplayLine { runs, kind });
    }
}

/// Plain concatenated text of a table cell (for width measurement / rendering).
fn table_cell_text(cell: &[Span]) -> String {
    cell.iter().map(|s| s.text.as_str()).collect()
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

/// One table row: each cell fitted to its column width, joined by " │ ".
fn table_row(cells: &[TableCell], col_w: &[usize], bold: bool) -> DisplayLine {
    let text = col_w
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let raw = cells.get(i).map(|c| table_cell_text(c)).unwrap_or_default();
            fit(&raw, *w)
        })
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
        }],
        kind: LineKind::Body,
    }
}

/// Render a table to aligned text rows (header bold + a rule), fitting `width`.
fn wrap_table(
    header: Option<&[TableCell]>,
    rows: &[Vec<TableCell>],
    width: usize,
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

    // Fit to the pane: reserve " │ " (3 cols) between columns, then shrink
    // proportionally if the natural widths overflow.
    let budget = width.saturating_sub(3 * ncols.saturating_sub(1)).max(ncols);
    let natural: usize = col_w.iter().sum();
    if natural > budget {
        let scale = budget as f64 / natural as f64;
        for w in &mut col_w {
            *w = ((*w as f64 * scale).floor() as usize).max(1);
        }
    }

    if let Some(h) = header {
        out.push(table_row(h, &col_w, true));
        out.push(DisplayLine {
            runs: vec![Run {
                text: col_w
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─"),
                style: Inline::default(),
                fg: None,
            }],
            kind: LineKind::Body,
        });
    }
    for r in rows {
        out.push(table_row(r, &col_w, false));
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
    fn math_renders_unicode_centered() {
        let block = Block::Math {
            tex: r"\alpha + \beta".to_string(),
        };
        let lines = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[]));
        let joined = lines.join("\n");
        // latex_to_unicode maps \alpha/\beta to their glyphs.
        assert!(joined.contains('α') && joined.contains('β'), "{joined:?}");
    }

    #[test]
    fn image_caption_renders_below_placeholder() {
        let block = Block::Image {
            src: "x.png".into(),
            alt: "diagram".into(),
            data: Vec::new(),
            caption: vec![Span::plain("Figure 1: the pipeline")],
        };
        // No image protocol (image_rows empty) → text placeholder + caption.
        let lines = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[]));
        let joined = lines.join("\n");
        assert!(joined.contains("[image: diagram]"));
        assert!(joined.contains("Figure 1: the pipeline"));
    }

    #[test]
    fn footnote_renders_label_and_body() {
        let block = Block::Footnote {
            label: "1".into(),
            blocks: vec![para("the cited source")],
        };
        let joined = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[])).join("\n");
        assert!(joined.contains("[1]"));
        assert!(joined.contains("the cited source"));
    }
}
