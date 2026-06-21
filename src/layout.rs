//! Reflow: turn a section's structured blocks into styled display lines for a
//! given content width. We wrap here (rather than letting the widget do it) so
//! scroll position, total line count, and progress % are exact and stable
//! across resizes. The view layer maps these to ratatui styles. See
//! `DESIGN.md` §2.1, §4.

use crate::document::{Block, Inline, Span};
use crate::highlight::highlight_code;

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

/// Wrap a section's blocks to `width` columns, returning styled display lines.
/// `code_theme` is the syntect theme used for code highlighting; `line_spacing`
/// inserts extra blank lines between text lines and `para_spacing` sets the gap
/// between blocks.
pub fn wrap_blocks(
    blocks: &[Block],
    width: usize,
    code_theme: &str,
    line_spacing: u8,
    para_spacing: u8,
    image_rows: &[u16],
) -> Vec<DisplayLine> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut prev_item = false;
    let mut first = true;
    let mut img_idx = 0usize;
    let mut code_idx = 0usize;

    for block in blocks {
        let is_item = matches!(block, Block::Para { marker: Some(_), .. });

        // Spacing between blocks: blank line(s), except between consecutive list
        // items and around explicit blanks.
        if !first && !matches!(block, Block::Blank) && !(is_item && prev_item) {
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
            Block::Image { alt, .. } => {
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
                img_idx += 1;
            }
            Block::Code { lang, lines } => {
                let gutter_w = lines.len().max(1).to_string().len();
                let highlighted = highlight_code(lines, lang.as_deref(), code_theme);
                for (i, runs) in highlighted.into_iter().enumerate() {
                    let num = format!("{:>gutter_w$} │ ", i + 1);
                    let avail = width.saturating_sub(num.chars().count()).max(1);
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
                }
                code_idx += 1;
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
            && matches!(line.kind, LineKind::Body | LineKind::Heading(_) | LineKind::Quote);
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
        let prefix = if first_line { first_prefix } else { cont_prefix };
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
