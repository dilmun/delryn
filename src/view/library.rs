//! Library view — sections sidebar + a sortable book table. Grid/cover view
//! comes later. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState};

use crate::app::{App, Focus, LibView, SortKey};
use crate::store::{BookRow, LibrarySection};
use crate::theme::Theme;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let area = f.area();
    if theme.bg.is_some() {
        f.render_widget(Block::default().style(base(theme)), area);
    }

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let body = rows[0];
    let cols = Layout::horizontal([Constraint::Length(24), Constraint::Min(0)]).split(body);

    render_sections(f, cols[0], app, theme);
    render_books(f, cols[1], app, theme);
    render_status(f, rows[1], app, theme);
}

fn base(theme: Theme) -> Style {
    let s = Style::default().fg(theme.fg);
    match theme.bg {
        Some(bg) => s.bg(bg),
        None => s,
    }
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

fn render_sections(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let focused = app.lib_focus == Focus::Sidebar;
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

    // The focused pane gets an accent border to show where the keyboard is.
    let border = if focused { theme.accent } else { theme.muted };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            "Library",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .style(base(theme));
    f.render_widget(List::new(items).block(block), area);
}

fn render_books(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    if app.lib_books.is_empty() {
        let msg = if app.config.library_paths.is_empty() {
            "No library configured.\n\nAdd a folder:  delryn --add <dir>\nthen run:      delryn"
        } else {
            "No books in this section."
        };
        let p = Paragraph::new(msg).style(base(theme));
        f.render_widget(p, area);
        return;
    }

    let compact = app.config.library_compact;
    let rows: Vec<Row> = app.lib_books.iter().map(|b| book_row(b, compact, theme)).collect();
    let widths: Vec<Constraint> = if compact {
        vec![Constraint::Length(1), Constraint::Min(10), Constraint::Length(4)]
    } else {
        vec![
            Constraint::Length(1),  // favorite star
            Constraint::Min(10),    // title (+ series)
            Constraint::Length(20), // author
            Constraint::Length(4),  // year
            Constraint::Length(4),  // %
            Constraint::Length(7),  // size
        ]
    };

    // Solid highlight bar when the list is focused; a quieter accent-text
    // selection when the keyboard is over in the sidebar.
    let highlight = if app.lib_focus == Focus::Content {
        Style::default()
            .fg(theme.bg.unwrap_or(Color::Black))
            .bg(theme.accent)
    } else {
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
    };

    let mut table = Table::new(rows, widths)
        .column_spacing(1)
        .row_highlight_style(highlight)
        .block(Block::default().style(base(theme)));
    if !compact {
        table = table.header(header_row(app, theme));
    }
    let mut state = TableState::new()
        .with_selected(Some(app.lib_sel.min(app.lib_books.len().saturating_sub(1))));
    f.render_stateful_widget(table, area, &mut state);
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
    Row::new(vec![
        Cell::from(""),
        cell(SortKey::Title, "Title", false),
        cell(SortKey::Author, "Author", false),
        cell(SortKey::Year, "Year", true),
        cell(SortKey::Progress, "%", true),
        cell(SortKey::Size, "Size", true),
    ])
}

/// A book row: rich (all columns) or compact (star · title · %).
fn book_row(b: &BookRow, compact: bool, theme: Theme) -> Row<'static> {
    let star = if b.favorite {
        Cell::from(Span::styled("★", Style::default().fg(theme.marker)))
    } else {
        Cell::from(" ")
    };
    let title = title_cell(b, theme);
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
            num(format!("{}%", b.pct)),
            num(fmt_size(b.size)),
        ])
    }
}

/// Title cell with a dimmed Calibre-style series suffix when present.
fn title_cell(b: &BookRow, theme: Theme) -> Cell<'static> {
    let mut spans = vec![Span::styled(b.title.clone(), Style::default().fg(theme.fg))];
    let suffix = series_suffix(b);
    if !suffix.is_empty() {
        spans.push(Span::styled(suffix, Style::default().fg(theme.muted)));
    }
    Cell::from(Line::from(spans))
}

fn render_status(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let left = if app.lib_filtering || !app.lib_filter.is_empty() {
        format!(" /{}", app.lib_filter)
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
        format!(
            " {} books · {} · {}h{}m read{sort}",
            app.lib_books.len(),
            app.lib_view.label(),
            read / 3600,
            (read % 3600) / 60,
        )
    };
    let right = "Tab focus  j/k move  ⏎ open  s sort  e edit  c shelf  v dense  q quit ";
    let width = area.width as usize;
    let pad = width.saturating_sub(left.chars().count() + right.chars().count());
    let line = format!("{left}{}{right}", " ".repeat(pad));
    let style = Style::default().fg(theme.status_fg).bg(theme.status_bg);
    f.render_widget(Paragraph::new(Line::raw(line)).style(style), area);
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
