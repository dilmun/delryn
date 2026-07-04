//! Library book list: sortable table with series grouping.

use super::*;

pub(crate) fn render_books(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
    let block = pane_block("Books", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let area = inner;
    if app.library.books.is_empty() {
        let msg = if app.config.library_paths.is_empty() {
            "No library configured.\n\nAdd a folder:  delryn --add <dir>\nthen run:      delryn"
        } else {
            "No books in this section."
        };
        f.render_widget(Paragraph::new(msg).style(theme.text_style()), area);
        return;
    }

    let compact = app.config.library_layout == LibLayout::Compact;
    let sel = app.library.sel.min(app.library.books.len() - 1);
    // The Series view groups books under series headers.
    let grouped = matches!(app.library.view, LibView::Section(LibrarySection::Series));
    let mut counts: HashMap<&str, usize> = HashMap::new();
    if grouped {
        for b in &app.library.books {
            *counts.entry(b.series.as_str()).or_insert(0) += 1;
        }
    }

    // Responsive columns: drop the least-important ones as the pane narrows so a
    // fixed column never overlaps the title (shared idea with the pane collapse).
    let cols = columns(compact, area.width, &app.config);
    // The `s` sort cycle follows exactly the columns drawn here, so it skips any
    // the user hid or that collapsed on this width.
    app.last_layout.sort_cycle = cols.iter().filter_map(|c| c.sort_key()).collect();

    // Build rows, interleaving series headers; track the selected book's row
    // index (it shifts down past the headers above it).
    let mut rows: Vec<Row> = Vec::new();
    let mut sel_row = 0;
    let mut last_series: Option<&str> = None;
    // (book index, row position) for each book row, for mouse hit-testing.
    let mut row_meta: Vec<(usize, usize)> = Vec::with_capacity(app.library.books.len());
    for (i, b) in app.library.books.iter().enumerate() {
        if grouped && last_series != Some(b.series.as_str()) {
            let n = counts.get(b.series.as_str()).copied().unwrap_or(0);
            rows.push(series_header_row(&b.series, n, theme));
            last_series = Some(b.series.as_str());
        }
        if i == sel {
            sel_row = rows.len();
        }
        row_meta.push((i, rows.len()));
        let marked = app.library.marked.contains(&b.path);
        rows.push(book_row(b, &cols, grouped, marked, theme));
    }

    let widths: Vec<Constraint> = cols.iter().map(|c| c.width()).collect();

    // Solid highlight bar when the list is focused; a quieter accent-text
    // selection when the keyboard is elsewhere.
    let highlight = if focused {
        theme.style(Role::Selection).remove_modifier(Modifier::BOLD)
    } else {
        theme.style(Role::AccentStrong)
    };

    // Scroll the cursor *into view* rather than always re-centring it: keep the
    // last offset, nudging it only when the selection would fall off the top or
    // bottom edge. So a click selects the book in place (no snap-to-centre) and the
    // wheel scrolls freely — the selection stays where it is on screen.
    let header_h: u16 = if compact { 0 } else { 1 };
    let view_rows = (area.height as usize)
        .saturating_sub(header_h as usize)
        .max(1);
    app.last_layout.visible_rows = view_rows;
    let max_off = rows.len().saturating_sub(view_rows);
    let mut off = app.library.list_offset.min(max_off);
    if sel_row < off {
        off = sel_row;
    } else if sel_row >= off + view_rows {
        off = sel_row + 1 - view_rows;
    }
    let centered_off = off.min(max_off);
    app.library.list_offset = centered_off;

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
    // Reserve one column each side so the rounded selection caps have room.
    let table_area = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    f.render_stateful_widget(table, table_area, &mut state);

    // Map each on-screen book row to its index for click hit-testing, using the
    // scroll offset the table settled on and the header line (non-compact only).
    let offset = state.offset();

    // Round the ends of the focused selection bar (caps sit in the reserved
    // margins). Only the solid highlight gets them — not the quiet text selection.
    if focused && sel_row >= offset {
        let sy = table_area.y + header_h + (sel_row - offset) as u16;
        if sy < table_area.y + table_area.height {
            crate::view::round_bar(
                f,
                Rect {
                    x: table_area.x,
                    y: sy,
                    width: table_area.width,
                    height: 1,
                },
                theme,
            );
        }
    }
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
        Cell::from(Line::from(Span::styled(label, theme.style(Role::Heading)))),
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
    Tags,
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
            Col::Tags => Constraint::Length(18),
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
            Col::Tags => "tags",
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
            Col::Tags => SortKey::Tags,
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
        (Col::Tags, 100),
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
        let active = app.library.sort == key;
        // The direction indicator is a fixed 2-cell slot (space + arrow). On
        // right-aligned columns it must be reserved even when inactive, else
        // adding the arrow would shift the right-pinned title left on toggle.
        let label = if active {
            format!("{text} {}", if app.library.sort_desc { "↓" } else { "↑" })
        } else if right {
            format!("{text}  ")
        } else {
            text.to_string()
        };
        let color = if active {
            theme.color(Role::Accent)
        } else {
            theme.color(Role::Muted)
        };
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
        Col::Tags => sort(SortKey::Tags, "Tags", false),
    });
    Row::new(cells.collect::<Vec<_>>())
}

/// A book row for the active `cols`. In `grouped` (Series) view the title is
/// indented under its header and prefixed with #idx.
fn book_row(b: &BookRow, cols: &[Col], grouped: bool, marked: bool, theme: Theme) -> Row<'static> {
    let num = |s: String| {
        Cell::from(
            Line::from(Span::styled(s, theme.style(Role::Muted))).alignment(Alignment::Right),
        )
    };
    let cells = cols.iter().map(|c| match c {
        Col::Star => star_cell(b, marked, theme),
        Col::Title => title_cell(b, grouped, theme),
        Col::Author => Cell::from(Span::styled(b.author.clone(), theme.style(Role::Muted))),
        Col::Year => num(b.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into())),
        Col::Type => type_cell(&b.path, theme),
        Col::Source => source_cell(b.converted, theme),
        Col::Pct => num(format!("{}%", b.pct)),
        Col::Size => num(fmt_size(b.size)),
        Col::Status => status_cell(b, theme),
        Col::Tags => tags_cell(&b.tags, theme),
    });
    let row = Row::new(cells.collect::<Vec<_>>());
    // A multi-selected (not cursor) row gets a faint surface tint so the selection
    // reads at a glance, beyond the ✓ in the narrow lead column. The cursor row's
    // own highlight layers on top of this.
    match (marked, theme.code_surface()) {
        (true, Some(bg)) => row.style(Style::default().bg(bg)),
        _ => row,
    }
}

/// The Tags cell: the book's tags, comma-separated and muted (the table clips to
/// the column width). Empty when untagged.
fn tags_cell(tags: &str, theme: Theme) -> Cell<'static> {
    Cell::from(Span::styled(tags.to_string(), theme.style(Role::Muted)))
}

/// The reading-status cell: the effective status label, with manual overrides
/// (paused / dropped / reference) tinted to stand out from the derived ones.
fn status_cell(b: &BookRow, theme: Theme) -> Cell<'static> {
    let st = delryn_model::ReadingStatus::effective(b.pct, &b.status);
    let color = if st.is_manual() {
        theme.color(Role::Marker)
    } else {
        theme.color(Role::Muted)
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
        Cell::from(Span::styled("✓", theme.style(Role::AccentStrong)))
    } else if b.favorite {
        Cell::from(Span::styled(
            "★",
            Style::default().fg(theme.color(Role::Marker)),
        ))
    } else {
        Cell::from(" ")
    }
}

/// The Source cell: "Original" (publisher file) vs "Converted" (repackaged, e.g.
/// by calibre). Converted is flagged in the marker colour so it stands out.
fn source_cell(converted: bool, theme: Theme) -> Cell<'static> {
    let (label, color) = if converted {
        ("Converted", theme.color(Role::Marker))
    } else {
        ("Original", theme.color(Role::Muted))
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
            Span::styled(format!("   {idx}"), theme.style(Role::Muted)),
            Span::styled(b.title.clone(), theme.style(Role::Body)),
        ]));
    }
    let mut spans = vec![Span::styled(b.title.clone(), theme.style(Role::Body))];
    let suffix = series_suffix(b);
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, theme.style(Role::Muted)));
    }
    Cell::from(Line::from(spans))
}

/// The Type cell: the file format (`EPUB`, `PDF`, `MOBI`, `AZW3`). The readable
/// default (EPUB) is dimmed; the not-yet-readable formats use the marker colour
/// so they stand out. This replaces the old leading title badge.
fn type_cell(path: &str, theme: Theme) -> Cell<'static> {
    let fmt = crate::document::BookFormat::from_path(path);
    let (label, color) = match fmt {
        crate::document::BookFormat::Epub => ("EPUB", theme.color(Role::Muted)),
        crate::document::BookFormat::Unknown => ("—", theme.color(Role::Muted)),
        other => (other.label(), theme.color(Role::Marker)),
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
