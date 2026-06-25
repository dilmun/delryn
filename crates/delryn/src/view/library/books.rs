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
    let cols = columns(compact, area.width, &app.config);
    // The `s` sort cycle follows exactly the columns drawn here, so it skips any
    // the user hid or that collapsed on this width.
    app.lib_sort_cycle = cols.iter().filter_map(|c| c.sort_key()).collect();

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
    Type,
    Source,
    Pct,
    Size,
    Status,
}

impl Col {
    fn width(self) -> Constraint {
        match self {
            Col::Star => Constraint::Length(1),
            Col::Title => Constraint::Min(10),
            Col::Author => Constraint::Length(20),
            // Year widens by the 2-cell sort indicator so the arrow has room and
            // the right-aligned label stays put (see `header_row`).
            Col::Year => Constraint::Length(6),
            Col::Type => Constraint::Length(5),
            Col::Source => Constraint::Length(9),
            Col::Pct => Constraint::Length(4),
            Col::Size => Constraint::Length(7),
            Col::Status => Constraint::Length(9),
        }
    }

    /// Visibility key (matches [`config::LIB_COLUMNS`]); `None` for the always-on
    /// star + title columns.
    fn key(self) -> Option<&'static str> {
        Some(match self {
            Col::Author => "author",
            Col::Year => "year",
            Col::Type => "type",
            Col::Source => "source",
            Col::Pct => "progress",
            Col::Size => "size",
            Col::Status => "status",
            Col::Star | Col::Title => return None,
        })
    }

    /// The sort key this column sorts by, for the `s` cycle; `None` for the
    /// non-sortable lead star column.
    fn sort_key(self) -> Option<SortKey> {
        Some(match self {
            Col::Title => SortKey::Title,
            Col::Author => SortKey::Author,
            Col::Year => SortKey::Year,
            Col::Type => SortKey::Type,
            Col::Source => SortKey::Source,
            Col::Pct => SortKey::Progress,
            Col::Size => SortKey::Size,
            Col::Status => SortKey::Status,
            Col::Star => return None,
        })
    }
}

/// The columns to show at pane `width`. Compact mode is fixed (star · title · %);
/// the rich list keeps star + title and adds each optional column only when the
/// user has it enabled *and* it still fits — so columns drop one-by-one as the
/// window narrows (widest thresholds go first).
fn columns(compact: bool, width: u16, config: &Config) -> Vec<Col> {
    if compact {
        return vec![Col::Star, Col::Title, Col::Pct];
    }
    let mut cols = vec![Col::Star, Col::Title];
    for (col, min) in [
        (Col::Author, 58u16),
        (Col::Year, 44),
        (Col::Type, 64),
        (Col::Source, 94),
        (Col::Pct, 36),
        (Col::Size, 78),
        (Col::Status, 60),
    ] {
        if col.key().is_some_and(|k| config.column_on(k)) && width >= min {
            cols.push(col);
        }
    }
    cols
}

/// The sort keys for the columns visible under `compact`/`width` (in display
/// order) — the `s` cycle for callers that don't build the table directly (the
/// grid and the pre-render fallback). The book table sets the cycle from its own
/// rendered columns so width-collapsed ones are skipped.
pub(crate) fn sort_cycle(config: &Config, compact: bool, width: u16) -> Vec<SortKey> {
    columns(compact, width, config)
        .iter()
        .filter_map(|c| c.sort_key())
        .collect()
}

/// The sortable column header for the active `cols`, marking the sort column.
fn header_row(cols: &[Col], app: &App, theme: Theme) -> Row<'static> {
    let sort = |key: SortKey, text: &str, right: bool| -> Cell<'static> {
        let active = app.lib_sort == key;
        // The direction indicator is a fixed 2-cell slot (space + arrow). On
        // right-aligned columns it must be reserved even when inactive, else
        // adding the arrow would shift the right-pinned title left on toggle.
        let label = if active {
            format!("{text} {}", if app.lib_sort_desc { "↓" } else { "↑" })
        } else if right {
            format!("{text}  ")
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
        Col::Type => sort(SortKey::Type, "Type", false),
        Col::Source => sort(SortKey::Source, "Source", false),
        Col::Pct => sort(SortKey::Progress, "%", true),
        Col::Size => sort(SortKey::Size, "Size", true),
        Col::Status => sort(SortKey::Status, "Status", false),
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
        Col::Type => type_cell(&b.path, theme),
        Col::Source => source_cell(b.converted, theme),
        Col::Pct => num(format!("{}%", b.pct)),
        Col::Size => num(fmt_size(b.size)),
        Col::Status => status_cell(b, theme),
    });
    Row::new(cells.collect::<Vec<_>>())
}

/// The reading-status cell: the effective status label, with manual overrides
/// (paused / dropped / reference) tinted to stand out from the derived ones.
fn status_cell(b: &BookRow, theme: Theme) -> Cell<'static> {
    let st = delryn_model::ReadingStatus::effective(b.pct, &b.status);
    let color = if st.is_manual() {
        theme.marker
    } else {
        theme.muted
    };
    Cell::from(Span::styled(
        st.label().to_string(),
        Style::default().fg(color),
    ))
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
    let mut spans = vec![Span::styled(b.title.clone(), Style::default().fg(theme.fg))];
    let suffix = series_suffix(b);
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(theme.muted)));
    }
    Cell::from(Line::from(spans))
}

/// The Type cell: the file format (`EPUB`, `PDF`, `MOBI`, `AZW3`). The readable
/// default (EPUB) is dimmed; the not-yet-readable formats use the marker colour
/// so they stand out. This replaces the old leading title badge.
fn type_cell(path: &str, theme: Theme) -> Cell<'static> {
    let fmt = crate::document::BookFormat::from_path(path);
    let (label, color) = match fmt {
        crate::document::BookFormat::Epub => ("EPUB", theme.muted),
        crate::document::BookFormat::Unknown => ("—", theme.muted),
        other => (other.label(), theme.marker),
    };
    Cell::from(Span::styled(label.to_string(), Style::default().fg(color)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_cycle_includes_title_then_enabled_columns() {
        let cfg = Config::default();
        let cycle = sort_cycle(&cfg, false, u16::MAX);
        assert_eq!(
            cycle.first(),
            Some(&SortKey::Title),
            "title is always first"
        );
        // Default config enables every optional column.
        for k in [
            SortKey::Author,
            SortKey::Year,
            SortKey::Type,
            SortKey::Source,
            SortKey::Progress,
            SortKey::Size,
            SortKey::Status,
        ] {
            assert!(cycle.contains(&k), "{k:?} should be in the cycle");
        }
    }

    #[test]
    fn sort_cycle_skips_hidden_columns() {
        let mut cfg = Config::default();
        cfg.toggle_column("author"); // hide Author
        cfg.toggle_column("size"); // hide Size
        let cycle = sort_cycle(&cfg, false, u16::MAX);
        assert!(
            !cycle.contains(&SortKey::Author),
            "hidden column is skipped"
        );
        assert!(!cycle.contains(&SortKey::Size), "hidden column is skipped");
        assert!(cycle.contains(&SortKey::Year), "visible column stays");
        assert!(cycle.contains(&SortKey::Title), "title always present");
    }

    #[test]
    fn sort_cycle_drops_columns_that_do_not_fit() {
        let cfg = Config::default();
        // A narrow pane keeps Title but collapses the wide optional columns.
        let narrow = sort_cycle(&cfg, false, 40);
        assert_eq!(narrow.first(), Some(&SortKey::Title));
        assert!(
            !narrow.contains(&SortKey::Source),
            "Source needs a wide pane"
        );
        // Compact layout is fixed to title + progress.
        let compact = sort_cycle(&cfg, true, u16::MAX);
        assert_eq!(compact, vec![SortKey::Title, SortKey::Progress]);
    }
}
