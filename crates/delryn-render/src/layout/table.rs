//! Table layout: fit columns to the pane (max-min fair), then render aligned
//! rows — header (bold) + rule + zebra-striped body — wrapping or truncating
//! cells per the `wrap` flag.

use delryn_model::{Inline, Span, TableCell};

use super::width::{display_width, truncate_to_width};
use super::{DisplayLine, LineKind, Run, wrap_text};

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

/// Plain concatenated text of a table cell (for width measurement / rendering).
fn table_cell_text(cell: &[Span]) -> String {
    // Cells often hold `<p>`/whitespace, so the raw text carries newlines and
    // runs of spaces. Collapse to a single line — otherwise an embedded newline
    // breaks the row mid-render and the column separators stop lining up.
    let raw: String = cell.iter().map(|s| s.text.as_str()).collect();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate (with `…`) or right-pad `s` to exactly `w` display columns.
fn fit(s: &str, w: usize) -> String {
    let n = display_width(s);
    if n == w {
        s.to_string()
    } else if n < w {
        format!("{s}{}", " ".repeat(w - n))
    } else if w == 0 {
        String::new()
    } else if w == 1 {
        "…".to_string()
    } else {
        // Truncate to (w-1) columns + a 1-column ellipsis; if a wide glyph
        // straddled the cut, pad the shortfall so the cell still fills exactly `w`.
        let (mut t, tw) = truncate_to_width(s, w - 1);
        t.push('…');
        let used = tw + 1;
        if used < w {
            t.push_str(&" ".repeat(w - used));
        }
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
pub(super) fn wrap_table(
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
            col_w[i] = col_w[i].max(display_width(table_cell_text(c)));
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
    use super::super::{DisplayLine, LineKind, WrapOpts, wrap_blocks};
    use super::fit_columns;
    use delryn_model::{Block, Span};

    fn texts(lines: &[DisplayLine]) -> Vec<String> {
        lines.iter().map(DisplayLine::text).collect()
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
}
