//! Command-palette overlay: a query line over a fuzzy-filtered command list.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::{App, Overlay};

pub fn render(f: &mut Frame, app: &App) {
    let Overlay::Palette(p) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let matches = p.filtered();
    let visible = matches.len().min(10) as u16;
    let area = super::centered(f.area(), 56, visible + 3);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Commands ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(theme.text_style());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);

    // Query line: a ">" prompt + the editable text with a caret.
    let mut q = vec![Span::styled(
        " > ",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
    q.extend(super::field_spans(
        &p.query,
        p.cursor,
        inner.width.saturating_sub(4) as usize,
        theme,
    ));
    f.render_widget(Paragraph::new(Line::from(q)), rows[0]);

    // Filtered command list; the selection gets an accent highlight.
    let hi = Style::default()
        .fg(theme.on_accent())
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let lines: Vec<Line> = matches
        .iter()
        .take(rows[1].height as usize)
        .enumerate()
        .map(|(i, it)| {
            let style = if i == p.sel {
                hi
            } else {
                Style::default().fg(theme.fg)
            };
            Line::from(Span::styled(format!(" {}", it.label), style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rows[1]);
}
