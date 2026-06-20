//! Reflow: turn a section's structured blocks into styled display lines for a
//! given content width. We wrap here (rather than letting the widget do it) so
//! scroll position, total line count, and progress % are exact and stable
//! across resizes. The view layer maps these to ratatui styles. See
//! `DESIGN.md` §2.1, §4.

use crate::document::{Block, Inline, Span};

/// A styled run of text within a display line.
#[derive(Clone)]
pub struct Run {
    pub text: String,
    pub style: Inline,
}

/// What a display line represents, so the view can style it by theme.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Body,
    Heading(u8),
    Quote,
    Code,
    Rule,
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
pub fn wrap_blocks(blocks: &[Block], width: usize) -> Vec<DisplayLine> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut prev_item = false;
    let mut first = true;

    for block in blocks {
        let is_item = matches!(block, Block::Para { marker: Some(_), .. });

        // Spacing between blocks: blank line, except between consecutive list
        // items and around explicit blanks.
        if !first && !matches!(block, Block::Blank) && !(is_item && prev_item) {
            out.push(DisplayLine::blank());
        }

        match block {
            Block::Blank => out.push(DisplayLine::blank()),
            Block::Rule => out.push(DisplayLine {
                runs: vec![Run {
                    text: "─".repeat(width),
                    style: Inline::default(),
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
            Block::Code { lines, .. } => {
                for line in lines {
                    out.push(DisplayLine {
                        runs: vec![Run {
                            text: line.clone(),
                            style: Inline {
                                code: true,
                                ..Inline::default()
                            },
                        }],
                        kind: LineKind::Code,
                    });
                }
            }
        }

        prev_item = is_item;
        first = false;
    }

    out
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
                });
                len += 1;
            }
            runs.push(Run {
                text: word.to_string(),
                style,
            });
            len += wlen;
            placed += 1;
            i += 1;
        }

        out.push(DisplayLine { runs, kind });
        first_line = false;
    }
}
