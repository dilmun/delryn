//! Metadata-edit form popup (`e` in the library): a small centered form over
//! the selected book's title/author/year/series/publisher. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, META_FIELDS};

pub fn render(f: &mut Frame, app: &App) {
    let Some(ed) = &app.meta_edit else {
        return;
    };
    let theme = app.config.theme;
    let height = META_FIELDS.len() as u16 + 4; // border + hint + spacers
    let area = super::centered(f.area(), 60, height);

    f.render_widget(Clear, area);

    let bg = theme.bg.unwrap_or(ratatui::style::Color::Black);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Edit metadata ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Min(0),    // fields
        Constraint::Length(1), // hint
    ])
    .split(inner);

    let mut lines: Vec<Line> = Vec::new();
    for (i, label) in META_FIELDS.iter().enumerate() {
        let selected = i == ed.row;
        let value = ed.values.get(i).map(String::as_str).unwrap_or("");
        let marker = if selected { "▸ " } else { "  " };
        let label_style = if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let pad = 12usize.saturating_sub(label.chars().count() + 2);
        let mut spans = vec![
            Span::styled(format!("{marker}{label}"), label_style),
            Span::raw(" ".repeat(pad)),
            Span::styled(value.to_string(), Style::default().fg(theme.heading)),
        ];
        // A block cursor on the focused field signals where typing lands.
        if selected {
            spans.push(Span::styled(
                "▏",
                Style::default().fg(theme.accent).add_modifier(Modifier::SLOW_BLINK),
            ));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), rows[0]);

    f.render_widget(
        Paragraph::new(Line::styled(
            "↑↓ field   type to edit   ⏎ save   Esc cancel",
            Style::default().fg(theme.muted),
        )),
        rows[1],
    );
}
