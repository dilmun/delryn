//! Library view — sections sidebar + a list of books. Grid/cover view and
//! richer columns come later. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::App;
use crate::store::LibrarySection;
use crate::theme::Theme;

const SECTIONS: [LibrarySection; 4] = [
    LibrarySection::Recent,
    LibrarySection::All,
    LibrarySection::Favorites,
    LibrarySection::Reading,
];

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

fn render_sections(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let items: Vec<ListItem> = SECTIONS
        .iter()
        .map(|s| {
            let here = *s == app.lib_section;
            let mut style = Style::default().fg(if here { theme.accent } else { theme.fg });
            if let Some(bg) = theme.bg {
                style = style.bg(bg);
            }
            if here {
                style = style.add_modifier(Modifier::BOLD);
            }
            let marker = if here { "▸ " } else { "  " };
            ListItem::new(Line::from(Span::styled(format!("{marker}{}", s.label()), style)))
        })
        .collect();

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
    let meta_w = 34usize.min(inner_w / 2);
    let title_w = inner_w.saturating_sub(meta_w + 2).max(8);

    let items: Vec<ListItem> = app
        .lib_books
        .iter()
        .map(|b| {
            let star = if b.favorite { "★ " } else { "  " };
            let title = truncate(&b.title, title_w);
            let year = b.year.map(|y| y.to_string()).unwrap_or_else(|| "—".into());
            let meta = format!(
                "{:<18}  {:>4}  {:>3}%  {:>6}",
                truncate(&b.author, 18),
                year,
                b.pct,
                fmt_size(b.size),
            );
            Line::from(vec![
                Span::styled(star, Style::default().fg(theme.marker)),
                Span::styled(format!("{title:<title_w$}"), Style::default().fg(theme.fg)),
                Span::raw("  "),
                Span::styled(meta, Style::default().fg(theme.muted)),
            ])
            .into()
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

fn render_status(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let left = if app.lib_filtering || !app.lib_filter.is_empty() {
        format!(" /{}", app.lib_filter)
    } else {
        let read = app.total_read_seconds();
        format!(
            " {} books · {} · {}h{}m read",
            app.lib_books.len(),
            app.lib_section.label(),
            read / 3600,
            (read % 3600) / 60,
        )
    };
    let right = "Tab section  / filter  f fav  ⏎ open  q quit ";
    let width = area.width as usize;
    let pad = width.saturating_sub(left.chars().count() + right.chars().count());
    let line = format!("{left}{}{right}", " ".repeat(pad));
    let style = Style::default().fg(theme.status_fg).bg(theme.status_bg);
    f.render_widget(Paragraph::new(Line::raw(line)).style(style), area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
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
