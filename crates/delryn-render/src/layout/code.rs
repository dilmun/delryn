//! Code-block layout: a line-number gutter, then either soft-wrap to the column
//! or keep lines intact and pan horizontally by `code_hscroll`. Each row is
//! padded to width so the code panel renders as a clean rectangle.

use delryn_model::Inline;

use crate::highlight::highlight_code;

use super::width::display_width;
use super::{DisplayLine, LineKind, Run, WrapOpts};

/// A code block → gutter-numbered, highlighted display lines, soft-wrapped or
/// panned per `opts`.
pub(super) fn emit_code(
    lang: Option<&str>,
    lines: &[String],
    code_idx: usize,
    width: usize,
    opts: &WrapOpts,
    out: &mut Vec<DisplayLine>,
) {
    let (highlighted, lang_name) = highlight_code(lines, lang, opts.code_theme);
    if opts.code_label
        && let Some(name) = &lang_name
    {
        emit_label(name, width, code_idx, out);
    }
    // Fold a long block to a short head preview: the global `code_fold` default,
    // flipped for this block by a per-`F` override. Short blocks never fold.
    let total = highlighted.len();
    let folded = opts.code_fold_threshold > 0
        && total > opts.code_fold_threshold
        && (opts.code_fold ^ opts.code_fold_flip.contains(&code_idx));
    let shown = if folded {
        FOLD_PREVIEW.min(opts.code_fold_threshold).min(total)
    } else {
        total
    };
    // The gutter is optional; when off, code fills the full column with no numbers.
    // Its width follows the *full* line count so the numbers keep their column
    // whether or not the block is folded.
    let gutter_w = if opts.code_line_numbers {
        total.max(1).to_string().len()
    } else {
        0
    };
    let cont = if opts.code_line_numbers {
        format!("{:>gutter_w$}   ", "")
    } else {
        String::new()
    };
    for (i, runs) in highlighted.into_iter().enumerate().take(shown) {
        let num = if opts.code_line_numbers {
            format!("{:>gutter_w$} │ ", i + 1)
        } else {
            String::new()
        };
        let avail = width.saturating_sub(display_width(&num)).max(1);
        if opts.code_wrap {
            emit_wrapped_line(runs, &num, &cont, avail, width, code_idx, out);
        } else {
            emit_panned_line(runs, num, opts.code_hscroll, avail, width, code_idx, out);
        }
    }
    if folded {
        emit_fold_marker(total - shown, gutter_w, width, code_idx, out);
    }
}

/// Head lines a folded block previews before its fold marker.
const FOLD_PREVIEW: usize = 10;

/// The summary row shown under a folded block's preview: the hidden-line count and
/// the keys that expand it. A `Code` line, so it sits inside the code panel and
/// inherits its surface; muted italic to read as chrome, not code.
fn emit_fold_marker(
    hidden: usize,
    gutter_w: usize,
    width: usize,
    code_idx: usize,
    out: &mut Vec<DisplayLine>,
) {
    let gutter = if gutter_w > 0 {
        format!("{:>gutter_w$} │ ", "⋯")
    } else {
        "⋯ ".to_string()
    };
    let plural = if hidden == 1 { "" } else { "s" };
    let mut runs = vec![Run {
        text: format!("{gutter}{hidden} more line{plural} · F expand · O viewer"),
        style: Inline {
            italic: true,
            code: true,
            ..Inline::default()
        },
        fg: None,
        anchor: None,
    }];
    pad_to_width(&mut runs, width);
    out.push(DisplayLine {
        runs,
        kind: LineKind::Code(code_idx),
    });
}

/// A dim, right-aligned language tag at the top of the code panel. A `Code` line,
/// so it inherits the muted code colour and the surface background.
fn emit_label(name: &str, width: usize, code_idx: usize, out: &mut Vec<DisplayLine>) {
    let tag = format!("{name} ");
    let pad = width.saturating_sub(display_width(&tag));
    let mut runs = Vec::new();
    if pad > 0 {
        runs.push(Run {
            text: " ".repeat(pad),
            style: Inline::default(),
            fg: None,
            anchor: None,
        });
    }
    runs.push(Run {
        text: tag,
        style: Inline {
            italic: true,
            code: true,
            ..Inline::default()
        },
        fg: None,
        anchor: None,
    });
    pad_to_width(&mut runs, width);
    out.push(DisplayLine {
        runs,
        kind: LineKind::Code(code_idx),
    });
}

/// Soft-wrap one source line: it spills onto several rows, the gutter (`first`)
/// shown only on the first, continuation rows using the blank `cont` gutter.
fn emit_wrapped_line(
    runs: Vec<Run>,
    first: &str,
    cont: &str,
    avail: usize,
    width: usize,
    code_idx: usize,
    out: &mut Vec<DisplayLine>,
) {
    for (j, mut line_runs) in pack_runs(runs, avail).into_iter().enumerate() {
        let gutter = if j == 0 { first } else { cont }.to_string();
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
}

/// No-wrap: one row per source line, panned by `hscroll`.
fn emit_panned_line(
    runs: Vec<Run>,
    num: String,
    hscroll: usize,
    avail: usize,
    width: usize,
    code_idx: usize,
    out: &mut Vec<DisplayLine>,
) {
    let mut full = vec![Run {
        text: num,
        style: Inline::default(),
        fg: None,
        anchor: None,
    }];
    full.extend(shift_runs(runs, hscroll, avail));
    pad_to_width(&mut full, width);
    out.push(DisplayLine {
        runs: full,
        kind: LineKind::Code(code_idx),
    });
}

/// Pad a line's runs with trailing spaces so it spans `width` columns. Used for
/// code blocks so their surface panel fills the column as a clean rectangle (the
/// filler inherits the line's `kind` background at render time).
fn pad_to_width(runs: &mut Vec<Run>, width: usize) {
    let len: usize = runs.iter().map(|r| display_width(&r.text)).sum();
    if len < width {
        runs.push(Run {
            text: " ".repeat(width - len),
            style: Inline::default(),
            fg: None,
            anchor: None,
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

#[cfg(test)]
mod tests {
    use super::super::{LineKind, WrapOpts, wrap_blocks};
    use delryn_model::Block;

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
        let code: Vec<&_> = lines
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
    fn language_label_row_shown_when_enabled() {
        let block = Block::Code {
            lang: Some("rust".into()),
            lines: vec!["fn main() {}".into()],
        };
        let opts = WrapOpts {
            width: 40,
            code_label: true,
            ..Default::default()
        };
        let text = wrap_blocks(&[block], &opts, &[])
            .iter()
            .map(|l| l.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Rust"), "a language tag is present: {text:?}");
    }

    #[test]
    fn long_block_folds_to_a_preview_plus_marker() {
        let lines: Vec<String> = (0..30).map(|i| format!("line_{i} = {i}")).collect();
        let block = Block::Code {
            lang: Some("text".into()),
            lines,
        };
        let opts = WrapOpts {
            width: 60,
            code_fold: true,
            code_fold_threshold: 20,
            ..Default::default()
        };
        let code: Vec<String> = wrap_blocks(&[block], &opts, &[])
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Code(_)))
            .map(|l| l.text())
            .collect();
        // 10 preview lines + 1 fold marker (short lines don't wrap).
        assert_eq!(code.len(), 11, "preview + marker: {code:?}");
        let marker = code.last().unwrap();
        assert!(
            marker.contains("20 more lines"),
            "marker names the 30-10 hidden lines: {marker:?}"
        );
    }

    #[test]
    fn per_block_flip_expands_a_folded_block() {
        let lines: Vec<String> = (0..30).map(|i| format!("x{i}")).collect();
        let block = Block::Code {
            lang: Some("text".into()),
            lines,
        };
        let flip = [0usize];
        let opts = WrapOpts {
            width: 40,
            code_fold: true,
            code_fold_threshold: 20,
            code_fold_flip: &flip,
            ..Default::default()
        };
        let code = wrap_blocks(&[block], &opts, &[])
            .into_iter()
            .filter(|l| matches!(l.kind, LineKind::Code(_)))
            .count();
        assert_eq!(code, 30, "the flipped block shows every line, no marker");
    }

    #[test]
    fn no_gutter_when_line_numbers_off() {
        let block = Block::Code {
            lang: Some("text".into()),
            lines: vec!["x = 1".into()],
        };
        let opts = WrapOpts {
            width: 40,
            code_line_numbers: false,
            ..Default::default()
        };
        let code: Vec<String> = wrap_blocks(&[block], &opts, &[])
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Code(_)))
            .map(|l| l.text())
            .collect();
        assert!(!code.is_empty());
        assert!(
            code.iter().all(|t| !t.contains('│')),
            "no gutter bar when off: {code:?}"
        );
    }
}
