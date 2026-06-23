//! Add-to-collection picker popup (`c` in the library): tick/untick the
//! selected book's collections, or type a new one. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;

pub fn render(f: &mut Frame, app: &App) {
    let Some(p) = &app.shelf_picker else {
        return;
    };
    let theme = app.config.theme;
    // One row per shelf + the "new" row, plus border / title / hint chrome.
    let rows_n = p.shelves.len() as u16 + 1;
    let area = super::centered(f.area(), 56, rows_n + 4);

    f.render_widget(Clear, area);

    let bg = theme.paper();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Add to collection ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // book title
        Constraint::Min(0),    // shelves + new row
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(Line::styled(
            super::truncate(&p.title, inner.width as usize),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )),
        rows[0],
    );

    let mut lines: Vec<Line> = Vec::new();
    for (i, (name, member)) in p.shelves.iter().enumerate() {
        let selected = i == p.sel && p.new_name.is_none();
        let marker = if selected { "▸ " } else { "  " };
        let check = if *member { "[✓] " } else { "[ ] " };
        let style = if selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if *member {
            Style::default().fg(theme.fg)
        } else {
            Style::default().fg(theme.muted)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{check}{name}"),
            style,
        )));
    }

    // The "new collection" row — a label, or a live text input when creating.
    let new_selected = p.sel == p.new_row();
    if let Some(buf) = &p.new_name {
        lines.push(Line::from(vec![
            Span::styled("▸ ＋ ", Style::default().fg(theme.accent)),
            Span::styled(buf.clone(), Style::default().fg(theme.heading)),
            Span::styled("▏", Style::default().fg(theme.accent)),
        ]));
    } else {
        let marker = if new_selected { "▸ " } else { "  " };
        let style = if new_selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}＋ New collection…"),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), rows[1]);
    // Shortcuts live in the bottom status bar (see view::status).
}
