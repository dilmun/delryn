//! Library book list: sortable table with series grouping.

use super::*;

pub(crate) fn render_books(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
    let block = pane_block("Books", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let area = inner;
    if app.lib_books.is_empty() {
        let msg = if app.config.library_paths.is_empty() {
            "No library configured.\n\nAdd a folder:  delryn --add <dir>\nthen run:      delryn"
        } else {
            "No books in this section."
        };
        f.render_widget(Paragraph::new(msg).style(theme.text_style()), area);
        return;
    }

    let compact = app.config.library_layout == LibLayout::Compact;
    let sel = app.lib_sel.min(app.lib_books.len() - 1);
    // The Series view groups books under series headers.
    let grouped = matches!(app.lib_view, LibView::Section(LibrarySection::Series));
    let mut counts: HashMap<&str, usize> = HashMap::new();
    if grouped {
        for b in &app.lib_books {
            *counts.entry(b.series.as_str()).or_insert(0) += 1;
        }
    }

    // Responsive columns: drop the least-important ones as the pane narrows so a
    // fixed column never overlaps the title (shared idea with the pane collapse).
    let cols = columns(compact, area.width);

    // Build rows, interleaving series headers; track the selected book's row
    // index (it shifts down past the headers above it).
    let mut rows: Vec<Row> = Vec::new();
    let mut sel_row = 0;
    let mut last_series: Option<&str> = None;
    // (book index, row position) for each book row, for mouse hit-testing.
    let mut row_meta: Vec<(usize, usize)> = Vec::with_capacity(app.lib_books.len());
    for (i, b) in app.lib_books.iter().enumerate() {
        if grouped && last_series != Some(b.series.as_str()) {
            let n = counts.get(b.series.as_str()).copied().unwrap_or(0);
            rows.push(series_header_row(&b.series, n, theme));
            last_series = Some(b.series.as_str());
        }
        if i == sel {
            sel_row = rows.len();
        }
        row_meta.push((i, rows.len()));
        let marked = app.lib_marked.contains(&b.path);
        rows.push(book_row(b, &cols, grouped, marked, theme));
    }

    let widths: Vec<Constraint> = cols.iter().map(|c| c.width()).collect();

    // Solid highlight bar when the list is focused; a quieter accent-text
    // selection when the keyboard is elsewhere.
    let highlight = if focused {
        Style::default().fg(theme.on_accent()).bg(theme.accent)
    } else {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    };

    // Center the selected row in the viewport (clamped at the ends), like the
    // sidebar — instead of letting it stick to whichever edge we scroll toward.
    let header_h: u16 = if compact { 0 } else { 1 };
    let view_rows = (area.height as usize)
        .saturating_sub(header_h as usize)
        .max(1);
    let max_off = rows.len().saturating_sub(view_rows);
    let centered_off = sel_row.saturating_sub(view_rows / 2).min(max_off);

    let mut table = Table::new(rows, widths)
        .column_spacing(1)
        .row_highlight_style(highlight)
        .style(theme.text_style());
    if !compact {
        table = table.header(header_row(&cols, app, theme));
    }
    let mut state = TableState::new()
        .with_offset(centered_off)
        .with_selected(Some(sel_row));
    f.render_stateful_widget(table, area, &mut state);

    // Map each on-screen book row to its index for click hit-testing, using the
    // scroll offset the table settled on and the header line (non-compact only).
    let offset = state.offset();
    let mut hits: Vec<(usize, Rect)> = Vec::with_capacity(row_meta.len());
    for (idx, pos) in row_meta {
        if pos < offset {
            continue;
        }
        let sy = area.y + header_h + (pos - offset) as u16;
        if sy >= area.y + area.height {
            continue;
        }
        hits.push((
            idx,
            Rect {
                x: area.x,
                y: sy,
                width: area.width,
                height: 1,
            },
        ));
    }
    app.mouse.books = hits;
}

/// A series group header row (spans the title column).
fn series_header_row(series: &str, count: usize, theme: Theme) -> Row<'static> {
    let label = if series.is_empty() {
        "(no series)".to_string()
    } else {
        format!("▾ {series}  ({count})")
    };
    Row::new(vec![
        Cell::from(""),
        Cell::from(Line::from(Span::styled(
            label,
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ))),
    ])
}

/// A library table column. The set shown adapts to the pane width (see
/// [`columns`]) so the title is never overlapped — columns drop one-by-one.
#[derive(Clone, Copy, PartialEq)]
enum Col {
    Star,
    Title,
    Author,
    Year,
    Source,
    Pct,
    Size,
}

impl Col {
    fn width(self) -> Constraint {
        match self {
            Col::Star => Constraint::Length(1),
            Col::Title => Constraint::Min(10),
            Col::Author => Constraint::Length(20),
            Col::Year => Constraint::Length(4),
            Col::Source => Constraint::Length(9),
            Col::Pct => Constraint::Length(4),
            Col::Size => Constraint::Length(7),
        }
    }
}

/// The columns to show at pane `width`. Compact mode is fixed (star · title · %);
/// the rich list keeps star + title and adds the rest only while they fit, so
/// columns drop one-by-one as the window narrows (widest thresholds go first).
fn columns(compact: bool, width: u16) -> Vec<Col> {
    if compact {
        return vec![Col::Star, Col::Title, Col::Pct];
    }
    let mut cols = vec![Col::Star, Col::Title];
    for (col, min) in [
        (Col::Author, 58u16),
        (Col::Year, 44),
        (Col::Source, 94),
        (Col::Pct, 36),
        (Col::Size, 78),
    ] {
        if width >= min {
            cols.push(col);
        }
    }
    cols
}

/// The sortable column header for the active `cols`, marking the sort column.
fn header_row(cols: &[Col], app: &App, theme: Theme) -> Row<'static> {
    let sort = |key: SortKey, text: &str, right: bool| -> Cell<'static> {
        let active = app.lib_sort == key;
        let label = if active {
            format!("{text} {}", if app.lib_sort_desc { "↓" } else { "↑" })
        } else {
            text.to_string()
        };
        let color = if active { theme.accent } else { theme.muted };
        let line = Line::from(Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        Cell::from(if right {
            line.alignment(Alignment::Right)
        } else {
            line
        })
    };
    let cells = cols.iter().map(|c| match c {
        Col::Star => Cell::from(""),
        Col::Title => sort(SortKey::Title, "Title", false),
        Col::Author => sort(SortKey::Author, "Author", false),
        Col::Year => sort(SortKey::Year, "Year", true),
        Col::Source => Cell::from(Line::from(Span::styled(
            "Source",
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        ))),
        Col::Pct => sort(SortKey::Progress, "%", true),
        Col::Size => sort(SortKey::Size, "Size", true),
    });
    Row::new(cells.collect::<Vec<_>>())
}

/// A book row for the active `cols`. In `grouped` (Series) view the title is
/// indented under its header and prefixed with #idx.
fn book_row(b: &BookRow, cols: &[Col], grouped: bool, marked: bool, theme: Theme) -> Row<'static> {
    let num = |s: String| {
        Cell::from(
            Line::from(Span::styled(s, Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        )
    };
    let cells = cols.iter().map(|c| match c {
        Col::Star => star_cell(b, marked, theme),
        Col::Title => title_cell(b, grouped, theme),
        Col::Author => Cell::from(Span::styled(
            b.author.clone(),
            Style::default().fg(theme.muted),
        )),
        Col::Year => num(b.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into())),
        Col::Source => source_cell(b.converted, theme),
        Col::Pct => num(format!("{}%", b.pct)),
        Col::Size => num(fmt_size(b.size)),
    });
    Row::new(cells.collect::<Vec<_>>())
}

/// The 1-cell lead column: a multi-select check, else the favorite star, else
/// blank.
fn star_cell(b: &BookRow, marked: bool, theme: Theme) -> Cell<'static> {
    if marked {
        Cell::from(Span::styled(
            "✓",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
    } else if b.favorite {
        Cell::from(Span::styled("★", Style::default().fg(theme.marker)))
    } else {
        Cell::from(" ")
    }
}

/// The Source cell: "Original" (publisher file) vs "Converted" (repackaged, e.g.
/// by calibre). Converted is flagged in the marker colour so it stands out.
fn source_cell(converted: bool, theme: Theme) -> Cell<'static> {
    let (label, color) = if converted {
        ("Converted", theme.marker)
    } else {
        ("Original", theme.muted)
    };
    Cell::from(Span::styled(label, Style::default().fg(color)))
}

/// Title cell. Normally `Title  Series #n` (dimmed suffix); in a grouped Series
/// view, indented under the header and prefixed with the position (`#2 Title`).
fn title_cell(b: &BookRow, grouped: bool, theme: Theme) -> Cell<'static> {
    if grouped {
        let idx = b
            .series_index
            .map(|i| format!("#{} ", fmt_idx(i)))
            .unwrap_or_default();
        return Cell::from(Line::from(vec![
            Span::styled(format!("   {idx}"), Style::default().fg(theme.muted)),
            Span::styled(b.title.clone(), Style::default().fg(theme.fg)),
        ]));
    }
    let mut spans = Vec::new();
    if let Some(badge) = format_badge(&b.path, theme) {
        spans.push(badge);
    }
    spans.push(Span::styled(b.title.clone(), Style::default().fg(theme.fg)));
    let suffix = series_suffix(b);
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(theme.muted)));
    }
    Cell::from(Line::from(spans))
}

/// A leading format badge (`PDF `, `MOBI `, …) for non-EPUB library entries, so
/// the not-yet-readable formats are visually distinct. EPUB — the readable
/// default — gets no badge to keep the common case clean.
fn format_badge(path: &str, theme: Theme) -> Option<Span<'static>> {
    let fmt = crate::document::BookFormat::from_path(path);
    if matches!(
        fmt,
        crate::document::BookFormat::Epub | crate::document::BookFormat::Unknown
    ) {
        return None;
    }
    Some(Span::styled(
        format!("{} ", fmt.label()),
        Style::default()
            .fg(theme.marker)
            .add_modifier(Modifier::BOLD),
    ))
}
