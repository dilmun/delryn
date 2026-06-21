//! Library view — sections sidebar + a list of books. Grid/cover view and
//! richer columns come later. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, LibView};
use crate::store::LibrarySection;
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

/// One sidebar row (a fixed section or a collection), highlighted when active.
fn section_item(label: &str, here: bool, theme: Theme) -> ListItem<'static> {
    let mut style = Style::default().fg(if here { theme.accent } else { theme.fg });
    if let Some(bg) = theme.bg {
        style = style.bg(bg);
    }
    if here {
        style = style.add_modifier(Modifier::BOLD);
    }
    let marker = if here { "▸ " } else { "  " };
    ListItem::new(Line::from(Span::styled(format!("{marker}{label}"), style)))
}

fn render_sections(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let mut items: Vec<ListItem> = LibrarySection::ALL
        .iter()
        .map(|s| {
            let here = matches!(&app.lib_view, LibView::Section(cur) if cur == s);
            section_item(s.label(), here, theme)
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
            items.push(section_item(&format!("{name}  ({count})"), here, theme));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.muted))
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

    let inner_w = area.width.saturating_sub(1) as usize;
    let compact = app.config.library_compact;
    let items: Vec<ListItem> = app
        .lib_books
        .iter()
        .map(|b| {
            if compact {
                compact_row(b, inner_w, theme)
            } else {
                rich_row(b, inner_w, theme)
            }
        })
        .collect();

    let highlight = Style::default()
        .fg(theme.bg.unwrap_or(Color::Black))
        .bg(theme.accent);
    let list = List::new(items)
        .block(Block::default().style(base(theme)))
        .highlight_style(highlight);
    let mut state = ListState::default();
    state.select(Some(app.lib_sel.min(app.lib_books.len().saturating_sub(1))));
    f.render_stateful_widget(list, area, &mut state);
}

/// Full row: star · title (+ series suffix) · author · year · % · size.
fn rich_row(b: &crate::store::BookRow, inner_w: usize, theme: Theme) -> ListItem<'static> {
    let meta_w = 34usize.min(inner_w / 2);
    let title_w = inner_w.saturating_sub(meta_w + 2).max(8);
    let star = if b.favorite { "★ " } else { "  " };
    // Calibre-style series suffix (` Foundation #2`), dimmed, capped to half the
    // title cell so it never crowds out the title itself.
    let suffix = super::truncate(&series_suffix(b), title_w / 2);
    let suffix_w = suffix.chars().count();
    let title = super::truncate(&b.title, title_w.saturating_sub(suffix_w).max(4));
    let pad = title_w.saturating_sub(title.chars().count() + suffix_w);
    let year = b.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into());
    let meta = format!(
        "{:<18}  {:>4}  {:>3}%  {:>6}",
        super::truncate(&b.author, 18),
        year,
        b.pct,
        fmt_size(b.size),
    );
    ListItem::new(Line::from(vec![
        Span::styled(star, Style::default().fg(theme.marker)),
        Span::styled(title, Style::default().fg(theme.fg)),
        Span::styled(suffix, Style::default().fg(theme.muted)),
        Span::raw(" ".repeat(pad)),
        Span::raw("  "),
        Span::styled(meta, Style::default().fg(theme.muted)),
    ]))
}

/// Dense row: star · title (+ series suffix) · right-aligned %. Fits more books.
fn compact_row(b: &crate::store::BookRow, inner_w: usize, theme: Theme) -> ListItem<'static> {
    let pct = format!("{:>3}%", b.pct);
    let star = if b.favorite { "★ " } else { "  " };
    // Leave room for the star (2) and " 100%" (pct width + a gap).
    let title_w = inner_w.saturating_sub(2 + pct.chars().count() + 1).max(8);
    let suffix = super::truncate(&series_suffix(b), title_w / 2);
    let suffix_w = suffix.chars().count();
    let title = super::truncate(&b.title, title_w.saturating_sub(suffix_w).max(4));
    let pad = title_w.saturating_sub(title.chars().count() + suffix_w) + 1;
    ListItem::new(Line::from(vec![
        Span::styled(star, Style::default().fg(theme.marker)),
        Span::styled(title, Style::default().fg(theme.fg)),
        Span::styled(suffix, Style::default().fg(theme.muted)),
        Span::raw(" ".repeat(pad)),
        Span::styled(pct, Style::default().fg(theme.muted)),
    ]))
}

fn render_status(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let left = if app.lib_filtering || !app.lib_filter.is_empty() {
        format!(" /{}", app.lib_filter)
    } else {
        let read = app.total_read_seconds();
        format!(
            " {} books · {} · {}h{}m read",
            app.lib_books.len(),
            app.lib_view.label(),
            read / 3600,
            (read % 3600) / 60,
        )
    };
    let right = "Tab view  / filter  f fav  e edit  c shelf  v dense  ⏎ open  q quit ";
    let width = area.width as usize;
    let pad = width.saturating_sub(left.chars().count() + right.chars().count());
    let line = format!("{left}{}{right}", " ".repeat(pad));
    let style = Style::default().fg(theme.status_fg).bg(theme.status_bg);
    f.render_widget(Paragraph::new(Line::raw(line)).style(style), area);
}

/// `  Foundation #2` for a series book, else empty. The leading spaces separate
/// it from the title.
fn series_suffix(b: &crate::store::BookRow) -> String {
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
