//! Add-to-collection picker popup (`c` in the library): tick/untick the
//! selected book's collections, or type a new one. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &App) {
    let Overlay::ShelfPicker(p) = &app.overlay else {
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
        .border_type(BorderType::Rounded)
        .border_style(theme.style(Role::BorderFocus))
        .title(Span::styled(
            " Add to collection ",
            theme.style(Role::Title),
        ))
        .style(theme.style(Role::Body).bg(bg));
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
            theme.style(Role::Muted).add_modifier(Modifier::ITALIC),
        )),
        rows[0],
    );

    let mut lines: Vec<Line> = Vec::new();
    for (i, (name, member)) in p.shelves.iter().enumerate() {
        let selected = i == p.sel && p.new_name.is_none();
        let marker = if selected { "▸ " } else { "  " };
        let check = if *member { "[✓] " } else { "[ ] " };
        let style = if selected {
            theme.style(Role::AccentStrong)
        } else if *member {
            theme.style(Role::Body)
        } else {
            theme.style(Role::Muted)
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
            Span::styled("▸ ＋ ", theme.style(Role::Accent)),
            Span::styled(buf.clone(), Style::default().fg(theme.color(Role::Heading))),
            Span::styled("▏", theme.style(Role::Accent)),
        ]));
    } else {
        let marker = if new_selected { "▸ " } else { "  " };
        let style = if new_selected {
            theme.style(Role::AccentStrong)
        } else {
            theme.style(Role::Muted)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}＋ New collection…"),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), rows[1]);
    // Shortcuts live in the bottom status bar (see view::status).
}
