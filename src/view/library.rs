//! Library view — sections sidebar + a sortable book table, a cover grid, and a
//! detail pane. See `DESIGN.md` §5.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui_image::{Resize, StatefulImage};

use crate::app::{App, LibPane, LibView, SortKey};
use crate::config::LibLayout;
use crate::store::{BookRow, LibrarySection};
use crate::theme::Theme;

/// Minimum body width before the detail pane is shown.
const DETAIL_MIN_WIDTH: u16 = 90;

/// Title rows under each grid cover.
const LABEL_H: u16 = 2;
/// Cover protocols built per frame, so a screenful pops in over a few frames.
const GRID_BUILD_PER_FRAME: usize = 2;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let area = f.area();
    if theme.bg.is_some() {
        f.render_widget(Block::default().style(base(theme)), area);
    }

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let body = rows[0];

    let grid = app.config.library_layout == LibLayout::Grid;
    let show_sidebar = app.lib_show_sidebar;
    // Detail pane: only for the list views, when wanted and there's room (the
    // grid is itself a cover view, so it takes the full width).
    let show_detail = !grid && app.lib_detail && body.width >= DETAIL_MIN_WIDTH;
    // Clamp pane widths so the list always keeps a usable middle.
    let cap = (body.width / 3).max(1);
    let sidebar_w = app.lib_sidebar_w.min(cap);
    let detail_w = app.lib_detail_w.min(cap);

    let mut constraints = Vec::new();
    if show_sidebar {
        constraints.push(Constraint::Length(sidebar_w));
    }
    constraints.push(Constraint::Min(10));
    if show_detail {
        constraints.push(Constraint::Length(detail_w));
    }
    let cols = Layout::horizontal(constraints).split(body);

    let mut i = 0;
    if show_sidebar {
        render_sections(f, cols[i], app, theme, app.lib_pane == LibPane::Sidebar);
        i += 1;
    }
    let list_area = cols[i];
    i += 1;
    if grid {
        render_grid(f, list_area, app, theme, app.lib_pane == LibPane::List);
    } else {
        render_books(f, list_area, app, theme, app.lib_pane == LibPane::List);
    }
    if show_detail {
        render_detail(f, cols[i], app, theme, app.lib_pane == LibPane::Detail);
    }
    render_status(f, rows[1], app, theme);
}

/// Cover-grid view: cards of cover thumbnails (built lazily) reflowing to width,
/// with the title under each and the selection framed in the accent colour.
fn render_grid(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
    let block = pane_block("Books", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let area = inner;
    if app.lib_books.is_empty() {
        let msg = if app.config.library_paths.is_empty() {
            "No library configured.\n\nAdd a folder:  delryn --add <dir>"
        } else {
            "No books in this section."
        };
        f.render_widget(Paragraph::new(msg).style(base(theme)), area);
        return;
    }

    // Card size from the configured grid size; cell pitch adds a 1-cell gutter.
    let (cover_w, cover_h) = app.config.library_grid_size.card();
    let cell_w = cover_w + 1;
    let cell_h = cover_h + LABEL_H + 1;

    let cols = (area.width / cell_w).max(1) as usize;
    let rows_screen = (area.height / cell_h).max(1) as usize;
    let len = app.lib_books.len();
    let sel = app.lib_sel.min(len - 1);

    // Center the card block within the pane so the leftover width/height becomes
    // balanced margins instead of one big gap on the right / bottom. The trailing
    // gutter (cell pitch − card) is trimmed off each edge before centering.
    let block_w = (cols as u16 * cell_w).saturating_sub(1).min(area.width);
    let block_h = (rows_screen as u16 * cell_h).saturating_sub(1).min(area.height);
    let x0 = area.x + (area.width - block_w) / 2;
    let y0 = area.y + (area.height - block_h) / 2;

    // Keep the selected row centered in the viewport (clamped at the ends),
    // like the sidebar — rather than pinning it to the bottom edge.
    let sel_row = sel / cols;
    let total_rows = len.div_ceil(cols);
    let max_top = total_rows.saturating_sub(rows_screen);
    let top_row = sel_row.saturating_sub(rows_screen / 2).min(max_top);
    let start = top_row * cols;
    let end = ((top_row + rows_screen) * cols).min(len);

    // Snapshot the visible cells (ends the immutable borrow before building).
    let visible: Vec<(usize, String, String, bool, bool)> = (start..end)
        .map(|i| {
            let b = &app.lib_books[i];
            let marked = app.lib_marked.contains(&b.path);
            (i, b.path.clone(), b.title.clone(), b.favorite, marked)
        })
        .collect();
    let paths: Vec<String> = visible.iter().map(|(_, p, _, _, _)| p.clone()).collect();

    app.lib_grid_cols = cols;
    app.ensure_grid_covers(&paths, GRID_BUILD_PER_FRAME);
    let font = super::image_font(app);
    let mut book_hits: Vec<(usize, Rect)> = Vec::with_capacity(visible.len());

    for (i, path, title, fav, marked) in &visible {
        let pos = i - start;
        let (r, c) = ((pos / cols) as u16, (pos % cols) as u16);
        let x = x0 + c * cell_w;
        let y = y0 + r * cell_h;
        let card = Rect { x, y, width: cover_w, height: cover_h };
        // Title sits on a single row so the selection highlight covers only the
        // text, not the blank gutter row below it; LABEL_H still spaces the cells.
        let label = Rect { x, y: y + cover_h, width: cover_w, height: 1 };
        // Whole cell (cover + title) is the click target for this book.
        book_hits.push((*i, Rect { x, y, width: cover_w, height: cover_h + LABEL_H }));
        let selected = *i == sel;

        // Card frame — rounded; accent border for the cursor, marker colour for a
        // multi-select mark, else quiet. The file format sits in the top-left.
        let border = if selected {
            theme.accent
        } else if *marked {
            theme.marker
        } else {
            theme.muted
        };
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_uppercase)
            .unwrap_or_default();
        let mut frame = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .style(base(theme));
        if !ext.is_empty() {
            frame = frame.title(Line::from(vec![
                Span::styled("─", Style::default().fg(border)),
                Span::styled(
                    ext,
                    Style::default()
                        .fg(if selected { theme.accent } else { theme.fg })
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        let inner = frame.inner(card);
        f.render_widget(frame, card);

        match app.lib_grid_covers.get_mut(path) {
            Some(Some(cover)) => {
                let rect = super::cover_image_rect(inner, font, cover.dims);
                let w = StatefulImage::default().resize(Resize::Scale(None));
                f.render_stateful_widget(w, rect, &mut cover.proto);
            }
            _ => {
                let star = if *fav { "★\n" } else { "" };
                f.render_widget(
                    Paragraph::new(format!("{star}\nno\ncover"))
                        .alignment(Alignment::Center)
                        .style(Style::default().fg(theme.muted)),
                    inner,
                );
            }
        }

        let style = if selected {
            Style::default().fg(theme.bg.unwrap_or(Color::Black)).bg(theme.accent)
        } else {
            Style::default().fg(theme.fg)
        };
        let check = if *marked { "✓ " } else { "" };
        let star = if *fav { "★ " } else { "" };
        f.render_widget(
            Paragraph::new(super::truncate(
                &format!("{check}{star}{title}"),
                cover_w as usize,
            ))
            .style(style),
            label,
        );
    }
    app.mouse.books = book_hits;
}

/// Right-hand pane: the selected book's cover (via the image protocol) plus its
/// full metadata.
fn render_detail(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
    let block = pane_block("Details", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Gather the selected book's fields (immutable) before the mutable cover render.
    let Some(b) = app.lib_books.get(app.lib_sel) else {
        return;
    };
    let title = b.title.clone();
    let subtitle = b.subtitle.clone();
    let author = b.author.clone();
    let series = series_suffix(b).trim_start().to_string();
    let year = b.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into());
    let publisher = b.publisher.clone();
    let isbn = b.isbn.clone();
    let language = b.language.clone();
    let size = fmt_size(b.size);
    let pct = b.pct;
    let fav = b.favorite;
    let converted = b.converted;

    let parts = Layout::vertical([Constraint::Min(2), Constraint::Length(13)]).split(inner);

    // Cover (or a fallback box when there's none / no graphics protocol).
    let font = super::image_font(app);
    if let Some(cover) = app.lib_cover.as_mut() {
        let rect = super::cover_image_rect(parts[0], font, cover.dims);
        let img = StatefulImage::default().resize(Resize::Scale(None));
        f.render_stateful_widget(img, rect, &mut cover.proto);
    } else {
        let ph = Paragraph::new("\n  (no cover)")
            .style(Style::default().fg(theme.muted))
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme.muted)));
        f.render_widget(ph, parts[0]);
    }

    // Metadata.
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                if fav { "★ " } else { "" },
                Style::default().fg(theme.marker),
            ),
            Span::styled(title, Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)),
        ]),
        Line::styled(author, Style::default().fg(theme.muted)),
    ];
    if !subtitle.is_empty() {
        lines.push(Line::styled(
            subtitle,
            Style::default().fg(theme.muted).add_modifier(Modifier::ITALIC),
        ));
    }
    lines.push(Line::raw(""));
    if !series.is_empty() {
        lines.push(meta_kv("Series", &series, theme));
    }
    lines.push(meta_kv("Year", &year, theme));
    if !publisher.is_empty() {
        lines.push(meta_kv("Publisher", &publisher, theme));
    }
    if !language.is_empty() {
        lines.push(meta_kv("Language", &language, theme));
    }
    if !isbn.is_empty() {
        lines.push(meta_kv("ISBN", &isbn, theme));
    }
    lines.push(meta_kv("Size", &size, theme));
    lines.push(Line::from(vec![
        Span::styled("Source: ", Style::default().fg(theme.muted)),
        Span::styled(
            if converted { "Converted EPUB" } else { "Original EPUB" },
            Style::default().fg(if converted { theme.marker } else { theme.fg }),
        ),
    ]));
    lines.push(meta_kv("Progress", &format!("{pct}%"), theme));
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).style(base(theme)),
        parts[1],
    );
}

/// A `key: value` metadata line.
fn meta_kv(key: &str, val: &str, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key}: "), Style::default().fg(theme.muted)),
        Span::styled(val.to_string(), Style::default().fg(theme.fg)),
    ])
}

fn base(theme: Theme) -> Style {
    let s = Style::default().fg(theme.fg);
    match theme.bg {
        Some(bg) => s.bg(bg),
        None => s,
    }
}

/// A bordered pane block whose border + title turn accent when the pane is
/// focused, else muted.
fn pane_block(title: &str, focused: bool, theme: Theme) -> Block<'static> {
    let border = if focused { theme.accent } else { theme.muted };
    let mut title_style = Style::default().fg(if focused { theme.accent } else { theme.muted });
    if focused {
        title_style = title_style.add_modifier(Modifier::BOLD);
    }
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(title.to_string(), title_style))
        .style(base(theme))
}

/// One sidebar row (a fixed section or a collection). The active entry gets a
/// solid cursor highlight when the sidebar is focused, else just a marker.
fn section_item(label: &str, here: bool, focused: bool, theme: Theme) -> ListItem<'static> {
    let style = if here && focused {
        Style::default()
            .fg(theme.bg.unwrap_or(Color::Black))
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else if here {
        let mut s = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
        if let Some(bg) = theme.bg {
            s = s.bg(bg);
        }
        s
    } else {
        let mut s = Style::default().fg(theme.fg);
        if let Some(bg) = theme.bg {
            s = s.bg(bg);
        }
        s
    };
    let marker = if here { "▸ " } else { "  " };
    ListItem::new(Line::from(Span::styled(format!("{marker}{label}"), style)))
}

fn render_sections(f: &mut Frame, area: Rect, app: &App, theme: Theme, focused: bool) {
    let mut items: Vec<ListItem> = LibrarySection::ALL
        .iter()
        .map(|s| {
            let here = matches!(&app.lib_view, LibView::Section(cur) if cur == s);
            section_item(s.label(), here, focused, theme)
        })
        .collect();

    // User collections, below a divider, each with its book count.
    if !app.lib_shelves.is_empty() {
        let mut header = Style::default().fg(theme.muted).add_modifier(Modifier::DIM);
        if let Some(bg) = theme.bg {
            header = header.bg(bg);
        }
        items.push(ListItem::new(Line::from(Span::styled("  Collections", header))));
        for (name, count) in &app.lib_shelves {
            let here = matches!(&app.lib_view, LibView::Shelf(cur) if cur == name);
            items.push(section_item(&format!("{name}  ({count})"), here, focused, theme));
        }
    }

    f.render_widget(List::new(items).block(pane_block("Library", focused, theme)), area);
}

fn render_books(f: &mut Frame, area: Rect, app: &mut App, theme: Theme, focused: bool) {
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
        vec![Constraint::Length(1), Constraint::Min(10), Constraint::Length(4)]
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
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
    };

    // Center the selected row in the viewport (clamped at the ends), like the
    // sidebar — instead of letting it stick to whichever edge we scroll toward.
    let header_h: u16 = if compact { 0 } else { 1 };
    let view_rows = (area.height as usize).saturating_sub(header_h as usize).max(1);
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
        hits.push((idx, Rect { x: area.x, y: sy, width: area.width, height: 1 }));
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
            Style::default().fg(theme.heading).add_modifier(Modifier::BOLD),
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
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted).add_modifier(Modifier::BOLD)
        };
        let line = Line::from(Span::styled(label, style));
        Cell::from(if right { line.alignment(Alignment::Right) } else { line })
    };
    let plain = |text: &str| {
        Cell::from(Line::from(Span::styled(
            text.to_string(),
            Style::default().fg(theme.muted).add_modifier(Modifier::BOLD),
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
        Cell::from(Span::styled("✓", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)))
    } else if b.favorite {
        Cell::from(Span::styled("★", Style::default().fg(theme.marker)))
    } else {
        Cell::from(" ")
    };
    let title = title_cell(b, grouped, theme);
    let num = |s: String| {
        Cell::from(Line::from(Span::styled(s, Style::default().fg(theme.muted))).alignment(Alignment::Right))
    };
    if compact {
        Row::new(vec![star, title, num(format!("{}%", b.pct))])
    } else {
        let author = Cell::from(Span::styled(b.author.clone(), Style::default().fg(theme.muted)));
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

fn render_status(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let marked = app.lib_marked.len();
    let visual = app.lib_visual.is_some();
    let state = if let Some(flash) = &app.lib_flash {
        flash.clone()
    } else if visual {
        format!("VISUAL · {marked} selected")
    } else if app.lib_filtering || !app.lib_filter.is_empty() {
        format!("/{}", app.lib_filter)
    } else {
        let read = app.total_read_seconds();
        let sort = if app.lib_sort == SortKey::Default {
            String::new()
        } else {
            format!(
                " · sort {} {}",
                app.lib_sort.label(),
                if app.lib_sort_desc { "↓" } else { "↑" }
            )
        };
        let pos = if app.lib_books.is_empty() {
            String::new()
        } else {
            format!("{}/{} · ", app.lib_sel.min(app.lib_books.len() - 1) + 1, app.lib_books.len())
        };
        let size = if app.is_grid() {
            format!(" · {} covers", app.config.library_grid_size.label())
        } else {
            String::new()
        };
        format!(
            "{pos}{} · {}h{}m read{sort}{size}",
            app.lib_view.label(),
            read / 3600,
            (read % 3600) / 60,
        )
    };
    // Visual mode gets range + bulk keys; grid (no side panes) gets size keys.
    let keys = if visual {
        "j/k extend · e rename · f favorite · V/Esc cancel"
    } else if app.is_grid() {
        "Tab pane · hjkl move · ⏎ open · V select · e edit · c shelf · s sort · v view · +/- size · q"
    } else {
        "Tab pane · hjkl move · ⏎ open · V select · e edit · c shelf · s sort · v view · [] size · q"
    };
    super::status::bar(f, area, theme, &state, keys);
}

/// `  Foundation #2` for a series book, else empty. The leading spaces separate
/// it from the title.
fn series_suffix(b: &BookRow) -> String {
    if b.series.is_empty() {
        return String::new();
    }
    match b.series_index {
        Some(i) => format!("  {} #{}", b.series, fmt_idx(i)),
        None => format!("  {}", b.series),
    }
}

/// Series index without a trailing `.0` (`2.0` → "2", `2.5` → "2.5").
fn fmt_idx(i: f32) -> String {
    if (i.fract()).abs() < f32::EPSILON {
        format!("{}", i as i64)
    } else {
        format!("{i}")
    }
}

fn fmt_size(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        format!("{kb:.0}K")
    } else {
        format!("{:.1}M", kb / 1024.0)
    }
}
