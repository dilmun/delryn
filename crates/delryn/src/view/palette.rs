//! Command-palette overlay: a query line over a fuzzy-filtered command list.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::Role;

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
        .border_style(theme.style(Role::BorderFocus))
        .title(Span::styled(" Commands ", theme.style(Role::Title)))
        .style(theme.text_style());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);

    // Query line: a ">" prompt + the editable text with a caret.
    let mut q = vec![Span::styled(" > ", theme.style(Role::AccentStrong))];
    q.extend(super::field_spans(
        p.input.text(),
        p.input.cursor(),
        inner.width.saturating_sub(4) as usize,
        theme,
    ));
    f.render_widget(Paragraph::new(Line::from(q)), rows[0]);

    // Filtered command list; the selection gets an accent highlight.
    let hi = theme.style(Role::Selection);
    let lines: Vec<Line> = matches
        .iter()
        .take(rows[1].height as usize)
        .enumerate()
        .map(|(i, it)| {
            let style = if i == p.sel {
                hi
            } else {
                theme.style(Role::Body)
            };
            Line::from(Span::styled(format!(" {}", it.label), style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rows[1]);
}
