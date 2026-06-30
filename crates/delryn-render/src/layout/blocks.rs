//! Per-block-kind emit helpers — one `emit_*` per content [`Block`] variant,
//! dispatched from [`super::wrap_blocks`]; each turns one block into display
//! lines (code and tables live in their own modules).

use delryn_model::{Block, CalloutKind, Inline, Span};

use super::spans::{tidy_spacing, wrap_spans};
use super::{DisplayLine, LineKind, Run, WrapOpts, wrap_blocks};

/// A horizontal rule spanning the full column.
pub(super) fn emit_rule(width: usize, out: &mut Vec<DisplayLine>) {
    out.push(DisplayLine {
        runs: vec![Run {
            text: "─".repeat(width),
            style: Inline::default(),
            fg: None,
            anchor: None,
        }],
        kind: LineKind::Rule,
    });
}

/// A heading, wrapped to the column (no prefix, never justified).
pub(super) fn emit_heading(
    level: u8,
    spans: &[Span],
    width: usize,
    opts: &WrapOpts,
    out: &mut Vec<DisplayLine>,
) {
    let tidied = opts.tidy_spacing.then(|| tidy_spacing(spans)).flatten();
    let spans = tidied.as_deref().unwrap_or(spans);
    wrap_spans(spans, width, "", "", LineKind::Heading(level), false, out);
}

/// A paragraph — possibly a quote, a list item (`marker`), or nested (`indent`).
pub(super) fn emit_para(
    spans: &[Span],
    indent: u8,
    quote: bool,
    marker: Option<&str>,
    width: usize,
    opts: &WrapOpts,
    out: &mut Vec<DisplayLine>,
) {
    let ind = "  ".repeat(indent as usize);
    let (first_prefix, cont_prefix, kind) = if quote {
        (format!("{ind}▎ "), format!("{ind}▎ "), LineKind::Quote)
    } else if let Some(m) = marker {
        let pad = " ".repeat(m.chars().count());
        (format!("{ind}{m}"), format!("{ind}{pad}"), LineKind::Body)
    } else {
        (ind.clone(), ind.clone(), LineKind::Body)
    };
    let tidied = opts.tidy_spacing.then(|| tidy_spacing(spans)).flatten();
    let spans = tidied.as_deref().unwrap_or(spans);
    wrap_spans(
        spans,
        width,
        &first_prefix,
        &cont_prefix,
        kind,
        opts.justify,
        out,
    );
}

/// A figure image: reserve its pre-computed rows (or a text placeholder when
/// there's no image support), then wrap the italic caption beneath it.
pub(super) fn emit_image(
    alt: &str,
    caption: &[Span],
    img_idx: usize,
    image_rows: &[u16],
    width: usize,
    out: &mut Vec<DisplayLine>,
) {
    // The reader pre-computes each image's height; reserve exactly that many
    // blank rows. Zero rows → no image support → show a text placeholder.
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
        wrap_spans(&italic, width, "", "", LineKind::Body, false, out);
    }
}

/// Display math: `tex` is already Unicode (the parser resolved LaTeX/MathML);
/// just centre each line.
pub(super) fn emit_math(tex: &str, width: usize, out: &mut Vec<DisplayLine>) {
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

/// A callout/admonition: a themed glyph + label/title header, then its inner
/// blocks rendered inside the left border.
pub(super) fn emit_callout(
    kind: &CalloutKind,
    title: Option<&str>,
    blocks: &[Block],
    opts: &WrapOpts,
    out: &mut Vec<DisplayLine>,
) {
    // A themed glyph leads the header, then the label/title — a clean icon in
    // place of the publisher's raster admonition icon.
    let head = format!(
        "{} {}",
        kind.glyph(),
        title
            .map(str::to_string)
            .unwrap_or_else(|| kind.label().to_string())
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
    wrap_nested(blocks, opts, "▌ ", LineKind::Quote, out);
}

/// A footnote/endnote definition: render "[label] body" by wrapping the body
/// indented by the label's width, then dropping the bold label onto line one.
pub(super) fn emit_footnote(
    label: &str,
    blocks: &[Block],
    footnote_idx: usize,
    opts: &WrapOpts,
    out: &mut Vec<DisplayLine>,
) {
    let tag = format!("[{label}] ");
    let pad = " ".repeat(tag.chars().count());
    let start = out.len();
    wrap_nested(blocks, opts, &pad, LineKind::Footnote(footnote_idx), out);
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
        justify: false,
        tidy_spacing: opts.tidy_spacing,
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

#[cfg(test)]
mod tests {
    use super::super::{DisplayLine, LineKind, WrapOpts, wrap_blocks};
    use delryn_model::{Block, CalloutKind, Span};

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
            width: delryn_model::ImageWidth::Auto,
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
            id: "fn1".into(),
            label: "1".into(),
            blocks: vec![para("the cited source")],
        };
        let joined = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[])).join("\n");
        assert!(joined.contains("[1]"));
        assert!(joined.contains("the cited source"));
    }
}
