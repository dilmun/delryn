//! Per-block-kind emit helpers — one `emit_*` per content [`Block`] variant,
//! dispatched from [`super::wrap_blocks`]; each turns one block into display
//! lines (code and tables live in their own modules).

use delryn_model::{Block, CalloutKind, Inline, Span};

use super::spans::{InlineMathDims, Prefix, ProseFit, tidy_spacing, wrap_spans};
use super::width::display_width;
use super::{DisplayLine, LineKind, Run, WrapOpts, wrap_blocks};

/// A horizontal rule spanning the full column.
pub(super) fn emit_rule(width: usize, out: &mut Vec<DisplayLine>) {
    out.push(DisplayLine {
        runs: vec![Run {
            text: "─".repeat(width),
            style: Inline::default(),
            fg: None,
            anchor: None,
            math: None,
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
    wrap_spans(
        spans,
        width,
        Prefix {
            first: "",
            cont: "",
        },
        LineKind::Heading(level),
        ProseFit {
            justify: false,
            hyphenate: opts.hyphenate,
        },
        InlineMathDims {
            cols: opts.inline_math_cols,
            rows: opts.inline_math_rows,
        },
        out,
    );
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
        let pad = " ".repeat(display_width(m));
        (format!("{ind}{m}"), format!("{ind}{pad}"), LineKind::Body)
    } else {
        (ind.clone(), ind.clone(), LineKind::Body)
    };
    let tidied = opts.tidy_spacing.then(|| tidy_spacing(spans)).flatten();
    let spans = tidied.as_deref().unwrap_or(spans);
    wrap_spans(
        spans,
        width,
        Prefix {
            first: &first_prefix,
            cont: &cont_prefix,
        },
        kind,
        ProseFit {
            justify: opts.justify,
            hyphenate: opts.hyphenate,
        },
        InlineMathDims {
            cols: opts.inline_math_cols,
            rows: opts.inline_math_rows,
        },
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
    number: Option<&str>,
    out: &mut Vec<DisplayLine>,
) {
    // The reader pre-computes each image's height; reserve exactly that many
    // blank rows. Zero rows → no image support → show a text placeholder.
    let rows = image_rows.get(img_idx).copied().unwrap_or(0);
    if rows > 0 {
        for r in 0..rows {
            let mut line = DisplayLine {
                runs: Vec::new(),
                kind: LineKind::Image(img_idx),
            };
            // An equation number rides right-aligned on the equation's last row, as
            // in standard math typesetting. The raster is centred and narrower than
            // the column, so a label at the right margin clears it.
            if r + 1 == rows
                && let Some(num) = number
            {
                push_right_aligned(&mut line, num, width);
            }
            out.push(line);
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
                math: None,
            }],
            kind: LineKind::Body,
        });
        // No image protocol: still surface the equation number so it isn't lost.
        if let Some(num) = number {
            let mut line = DisplayLine {
                runs: Vec::new(),
                kind: LineKind::Body,
            };
            push_right_aligned(&mut line, num, width);
            out.push(line);
        }
    }
    // A blank line sets the caption off from the picture instead of letting it
    // hug the bottom edge (only when there's actually an image above it).
    if rows > 0 && !caption.is_empty() {
        out.push(DisplayLine {
            runs: Vec::new(),
            kind: LineKind::Body,
        });
    }
    // Figure caption (italic), wrapped and centred beneath the image — images
    // are centred in the column (see the reader view), so the caption sits on
    // the same axis rather than hugging the left margin.
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
                math: None,
            })
            .collect();
        let mut cap = Vec::new();
        // Captions never carry inline-math atoms (the reader only reserves widths
        // for body/heading spans), so pass no widths — any math stays Unicode.
        let no_prefix = Prefix {
            first: "",
            cont: "",
        };
        wrap_spans(
            &italic,
            width,
            no_prefix,
            LineKind::Body,
            ProseFit::default(),
            InlineMathDims {
                cols: &[],
                rows: &[],
            },
            &mut cap,
        );
        for mut line in cap {
            center_line(&mut line, width);
            out.push(line);
        }
    }
}

/// Right-align `text` within `width` on an otherwise-empty display line — leading
/// pad, then the text ending at the right margin. Used for equation numbers.
fn push_right_aligned(line: &mut DisplayLine, text: &str, width: usize) {
    let pad = width.saturating_sub(display_width(text));
    line.runs.push(Run {
        text: format!("{}{text}", " ".repeat(pad)),
        style: Inline::default(),
        fg: None,
        anchor: None,
        math: None,
    });
}

/// Left-pad a display line so its content is centred within `width` (the wrapper
/// already kept each line ≤ `width`). Used for figure captions.
fn center_line(line: &mut DisplayLine, width: usize) {
    let w: usize = line.runs.iter().map(|r| display_width(&r.text)).sum();
    let pad = width.saturating_sub(w) / 2;
    if pad > 0 {
        line.runs.insert(
            0,
            Run {
                text: " ".repeat(pad),
                style: Inline::default(),
                fg: None,
                anchor: None,
                math: None,
            },
        );
    }
}

/// Display math: `tex` is already Unicode (the parser resolved LaTeX/MathML);
/// just centre each line.
pub(super) fn emit_math(tex: &str, width: usize, number: Option<&str>, out: &mut Vec<DisplayLine>) {
    let raw: Vec<&str> = tex.lines().collect();
    let last = raw.len().saturating_sub(1);
    // The equation number sits flush-right on the equation's last line when it fits
    // beside the centred equation; a wide equation drops the number to its own
    // right-aligned line below so it is never lost.
    let fits_last = !raw.is_empty()
        && number.is_some_and(|num| {
            let text = raw[last].trim_end();
            let pad = width.saturating_sub(display_width(text)) / 2;
            pad + display_width(text) + 1 + display_width(num) <= width
        });
    for (i, line) in raw.iter().enumerate() {
        let text = line.trim_end();
        let pad = width.saturating_sub(display_width(text)) / 2;
        let mut s = format!("{}{text}", " ".repeat(pad));
        if i == last
            && fits_last
            && let Some(num) = number
        {
            let gap = width.saturating_sub(display_width(&s) + display_width(num));
            s.push_str(&" ".repeat(gap));
            s.push_str(num);
        }
        out.push(DisplayLine {
            runs: vec![Run {
                text: s,
                style: Inline {
                    italic: true,
                    ..Inline::default()
                },
                fg: None,
                anchor: None,
                math: None,
            }],
            kind: LineKind::Math,
        });
    }
    if let Some(num) = number.filter(|_| !fits_last) {
        let mut line = DisplayLine {
            runs: Vec::new(),
            kind: LineKind::Math,
        };
        push_right_aligned(&mut line, num, width);
        out.push(line);
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
                math: None,
            },
            Run {
                text: head,
                style: Inline {
                    bold: true,
                    ..Inline::default()
                },
                fg: None,
                anchor: None,
                math: None,
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
    let pad = " ".repeat(display_width(&tag));
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
        math: None,
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
        width: opts.width.saturating_sub(display_width(border)).max(1),
        code_theme: opts.code_theme,
        line_spacing: 0,
        para_spacing: opts.para_spacing,
        code_wrap: true,
        code_hscroll: 0,
        code_line_numbers: opts.code_line_numbers,
        code_label: opts.code_label,
        // Nested code (callouts/footnotes) keeps its own index space, which the
        // reader's per-block `F`/viewer `O` don't address — so never fold it.
        code_fold: false,
        code_fold_threshold: 0,
        code_fold_flip: &[],
        table_wrap: opts.table_wrap,
        // A callout / footnote body is a narrow inset column. It isn't justified,
        // so it has no gaps to close, and hyphens in it would read as content.
        justify: false,
        hyphenate: false,
        tidy_spacing: opts.tidy_spacing,
        // Inline math shares one id space across nested blocks (the reader's
        // `convert_inline_math`/`remap_inline_math` recurse into callout/footnote bodies),
        // so thread the section's reservations through — a note's fraction renders as a
        // raster like top-level inline math instead of degrading to its Unicode floor.
        inline_math_cols: opts.inline_math_cols,
        inline_math_rows: opts.inline_math_rows,
    };
    for line in wrap_blocks(blocks, &inner, &[]) {
        let mut runs = Vec::with_capacity(line.runs.len() + 1);
        runs.push(Run {
            text: border.to_string(),
            style: Inline::default(),
            fg: None,
            anchor: None,
            math: None,
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
        // The parser resolves LaTeX/MathML to a Unicode floor; layout only centres it.
        let block = Block::Math {
            item: delryn_model::MathItem {
                display: true,
                typeset: None,
                picture: None,
                text: "α + β".to_string(),
            },
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

    fn display_math(text: &str) -> Block {
        Block::Math {
            item: delryn_model::MathItem {
                display: true,
                typeset: None,
                picture: None,
                text: text.to_string(),
            },
        }
    }

    #[test]
    fn equation_number_rides_on_the_last_image_row() {
        // A math equation raster (2 reserved rows) followed by a lone "Eq. 4"
        // paragraph: the number rides right-aligned on the equation's last row, not
        // stranded on its own line below.
        let eq = Block::Image {
            src: "eq.png".into(),
            alt: "a·b".into(),
            data: Vec::new(),
            caption: Vec::new(),
            math: true,
            width: delryn_model::ImageWidth::Auto,
            ink: None,
        };
        let opts = WrapOpts {
            width: 40,
            ..Default::default()
        };
        let lines = wrap_blocks(&[eq, para("Eq. 4")], &opts, &[2]);
        let img: Vec<&DisplayLine> = lines
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Image(_)))
            .collect();
        assert_eq!(img.len(), 2, "two reserved image rows");
        let last = img[1].text();
        assert!(
            last.trim_end().ends_with("Eq. 4"),
            "number rides the last row: {last:?}"
        );
        assert!(last.starts_with(' '), "right-aligned: {last:?}");
        assert!(
            !lines
                .iter()
                .any(|l| matches!(l.kind, LineKind::Body) && l.text().trim() == "Eq. 4"),
            "not also stranded as a separate line",
        );
    }

    #[test]
    fn equation_number_flushes_right_on_the_math_last_line() {
        let opts = WrapOpts {
            width: 40,
            ..Default::default()
        };
        let lines = wrap_blocks(&[display_math("x = 1"), para("Eq. 5")], &opts, &[]);
        let math: Vec<String> = lines
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Math))
            .map(DisplayLine::text)
            .collect();
        assert_eq!(math.len(), 1, "equation + number share one line: {math:?}");
        assert!(math[0].contains("x = 1"), "{math:?}");
        assert!(
            math[0].trim_end().ends_with("Eq. 5"),
            "number flush-right: {math:?}"
        );
        assert!(
            !texts(&lines).iter().any(|l| l.trim() == "Eq. 5"),
            "not a separate line",
        );
    }

    #[test]
    fn prose_after_an_equation_is_not_consumed_as_a_number() {
        // "Eq. 4 shows…" opens with a label but is a sentence — it must render
        // normally, never be swallowed as the equation's number.
        let opts = WrapOpts {
            width: 60,
            ..Default::default()
        };
        let prose = "Eq. 4 shows the inner product of two vectors.";
        let lines = wrap_blocks(&[display_math("x = 1"), para(prose)], &opts, &[]);
        assert!(
            texts(&lines).join("\n").contains(prose),
            "prose paragraph still rendered in full",
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
            ink: None,
        };
        // No image protocol (image_rows empty) → text placeholder + caption.
        let lines = texts(&wrap_blocks(&[block], &WrapOpts::default(), &[]));
        let joined = lines.join("\n");
        assert!(joined.contains("[image: diagram]"));
        assert!(joined.contains("Figure 1: the pipeline"));
    }

    #[test]
    fn image_caption_is_centred_under_the_image() {
        // A short caption narrower than the column is padded on the left so it
        // sits centred (images are centred in the column), not flush-left.
        let block = Block::Image {
            src: "x.png".into(),
            alt: String::new(),
            data: Vec::new(),
            caption: vec![Span::plain("Figure 1")],
            math: false,
            width: delryn_model::ImageWidth::Auto,
            ink: None,
        };
        let opts = WrapOpts {
            width: 40,
            ..Default::default()
        };
        let caption = texts(&wrap_blocks(&[block], &opts, &[]))
            .into_iter()
            .find(|l| l.contains("Figure 1"))
            .expect("caption line");
        assert!(
            caption.starts_with(' ') && caption.trim() == "Figure 1",
            "caption centred: {caption:?}"
        );
        let lead = caption.len() - caption.trim_start().len();
        assert_eq!(lead, (40 - "Figure 1".len()) / 2, "centred padding");
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

#[cfg(test)]
mod heading_spacing_tests {
    use super::super::*;
    use delryn_model::Span;

    fn opts(para_spacing: u8) -> WrapOpts<'static> {
        WrapOpts {
            width: 40,
            para_spacing,
            ..Default::default()
        }
    }

    fn para(t: &str) -> Block {
        Block::Para {
            spans: vec![Span::plain(t)],
            indent: 0,
            quote: false,
            marker: None,
        }
    }

    fn heading(level: u8, t: &str) -> Block {
        Block::Heading {
            level,
            spans: vec![Span::plain(t)],
        }
    }

    /// Blank rows immediately before the line whose text is `needle`.
    fn blanks_before(lines: &[DisplayLine], needle: &str) -> usize {
        let at = lines
            .iter()
            .position(|l| l.text().contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not laid out"));
        lines[..at]
            .iter()
            .rev()
            .take_while(|l| l.text().trim().is_empty())
            .count()
    }

    /// A heading is spaced typographically: the air goes above it, and the text it
    /// introduces follows on the very next row, so the two read as one unit rather
    /// than as a heading floating between two paragraphs.
    #[test]
    fn a_heading_sits_directly_on_its_own_text() {
        let blocks = vec![
            para("Body before."),
            heading(2, "Related Work"),
            para("Body after."),
        ];
        let lines = wrap_blocks(&blocks, &opts(1), &[]);
        assert!(blanks_before(&lines, "Related Work") >= 2, "air above");
        assert_eq!(
            blanks_before(&lines, "Body after."),
            0,
            "its first line follows immediately"
        );
    }

    /// The air above scales with the reader's paragraph-spacing setting rather than
    /// hard-coding a gap, so a dense setting stays dense. Below is closed at every
    /// setting — the heading belongs to the text under it.
    #[test]
    fn heading_spacing_follows_the_paragraph_setting() {
        let blocks = vec![
            para("Body before."),
            heading(2, "Related Work"),
            para("Body after."),
        ];
        for spacing in [1u8, 2, 3, 4] {
            let lines = wrap_blocks(&blocks, &opts(spacing), &[]);
            assert_eq!(
                blanks_before(&lines, "Related Work"),
                spacing as usize + 1,
                "one row more than the body gap at spacing {spacing}"
            );
            assert_eq!(
                blanks_before(&lines, "Body after."),
                0,
                "no gap below at spacing {spacing}"
            );
        }
    }

    /// The trim is for section headings. A chapter title opens a section rather
    /// than labelling one inside it, and keeps the reader's full gap below.
    #[test]
    fn a_chapter_title_keeps_its_full_gap() {
        let blocks = vec![
            para("Body before."),
            heading(1, "Chapter One"),
            para("Body after."),
        ];
        for spacing in [2u8, 3, 4] {
            let lines = wrap_blocks(&blocks, &opts(spacing), &[]);
            assert_eq!(
                blanks_before(&lines, "Body after."),
                spacing as usize,
                "a title is untrimmed at spacing {spacing}"
            );
        }
    }

    /// The tiers are legible from the spacing, not only from the ink: a subheading
    /// rising out of body text is approached with one row less than a section
    /// heading is, so the reader can see which one outranks the other.
    #[test]
    fn a_subheading_gets_a_shorter_run_up_than_a_section() {
        let after_body = |level| {
            let blocks = vec![para("Body."), heading(level, "Marker"), para("After.")];
            blanks_before(&wrap_blocks(&blocks, &opts(1), &[]), "Marker")
        };
        let section = after_body(2);
        let subsection = after_body(3);
        assert_eq!(
            subsection,
            section - 1,
            "a subheading takes one row less than its parent tier \
             (section {section}, subsection {subsection})"
        );
        assert!(subsection >= 1, "but still parts from the paragraph above");
    }

    /// Back-to-back headings (a section immediately followed by its subsection)
    /// are already a unit, so the subsection gets no extra row driving them apart.
    #[test]
    fn consecutive_headings_stay_together() {
        let blocks = vec![
            para("Body."),
            heading(2, "Related Work"),
            heading(3, "Zero-Shot"),
            para("After."),
        ];
        let lines = wrap_blocks(&blocks, &opts(1), &[]);
        assert_eq!(
            blanks_before(&lines, "Zero-Shot"),
            1,
            "a subsection keeps the plain body gap under its parent heading"
        );
    }

    /// A heading opening a section gets no leading blank — no gap at the top.
    #[test]
    fn a_leading_heading_adds_no_blank_above() {
        let lines = wrap_blocks(&[heading(1, "Title"), para("Body.")], &opts(1), &[]);
        assert!(
            !lines[0].text().trim().is_empty(),
            "the first line is the heading itself, not padding"
        );
    }
}
