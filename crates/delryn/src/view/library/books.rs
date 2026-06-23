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
        f.render_widget(Paragraph::new(msg).style(base(theme)), area);
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
        rows.push(book_row(b, compact, grouped, marked, theme));
    }

    let widths: Vec<Constraint> = if compact {
        vec![
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(4),
        ]
    } else {
        vec![
            Constraint::Length(1),  // favorite star
            Constraint::Min(10),    // title (+ series)
            Constraint::Length(20), // author
            Constraint::Length(4),  // year
            Constraint::Length(9),  // source (Original / Converted)
            Constraint::Length(4),  // %
            Constraint::Length(7),  // size
        ]
    };

    // Solid highlight bar when the list is focused; a quieter accent-text
    // selection when the keyboard is elsewhere.
    let highlight = if focused {
        Style::default()
            .fg(theme.bg.unwrap_or(Color::Black))
            .bg(theme.accent)
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
        .style(base(theme));
    if !compact {
        table = table.header(header_row(app, theme));
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

/// The sortable column header, marking the active sort column with an arrow.
fn header_row(app: &App, theme: Theme) -> Row<'static> {
    let cell = |key: SortKey, text: &str, right: bool| -> Cell<'static> {
        let active = app.lib_sort == key;
        let label = if active {
            format!("{text} {}", if app.lib_sort_desc { "↓" } else { "↑" })
        } else {
            text.to_string()
        };
        let style = if active {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD)
        };
        let line = Line::from(Span::styled(label, style));
        Cell::from(if right {
            line.alignment(Alignment::Right)
        } else {
            line
        })
    };
    let plain = |text: &str| {
        Cell::from(Line::from(Span::styled(
            text.to_string(),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        )))
    };
    Row::new(vec![
        Cell::from(""),
        cell(SortKey::Title, "Title", false),
        cell(SortKey::Author, "Author", false),
        cell(SortKey::Year, "Year", true),
        plain("Source"),
        cell(SortKey::Progress, "%", true),
        cell(SortKey::Size, "Size", true),
    ])
}

/// A book row: rich (all columns) or compact (star · title · %). In `grouped`
/// (Series) view the title is indented under its header and prefixed with #idx.
fn book_row(b: &BookRow, compact: bool, grouped: bool, marked: bool, theme: Theme) -> Row<'static> {
    // The 1-cell lead column shows a multi-select check, else the favorite star.
    let star = if marked {
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
    };
    let title = title_cell(b, grouped, theme);
    let num = |s: String| {
        Cell::from(
            Line::from(Span::styled(s, Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        )
    };
    if compact {
        Row::new(vec![star, title, num(format!("{}%", b.pct))])
    } else {
        let author = Cell::from(Span::styled(
            b.author.clone(),
            Style::default().fg(theme.muted),
        ));
        let year = num(b.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into()));
        Row::new(vec![
            star,
            title,
            author,
            year,
            source_cell(b.converted, theme),
            num(format!("{}%", b.pct)),
            num(fmt_size(b.size)),
        ])
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
    let mut spans = vec![Span::styled(b.title.clone(), Style::default().fg(theme.fg))];
    let suffix = series_suffix(b);
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(theme.muted)));
    }
    Cell::from(Line::from(spans))
}
