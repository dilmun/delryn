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
    let area = super::overlay_rect(f.area(), app.overlay_large);
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

    // Filtered command list; the selection gets a rounded accent bar. Scroll the
    // window to keep the selection visible when the list is taller than the pane.
    let h = rows[1].height as usize;
    let offset = p
        .sel
        .saturating_sub(h / 2)
        .min(matches.len().saturating_sub(h));
    let lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .skip(offset)
        .take(h)
        .map(|(i, it)| {
            if i == p.sel {
                crate::view::rounded_line(format!(" {}", it.label), rows[1].width, theme)
            } else {
                Line::from(Span::styled(
                    format!(" {}", it.label),
                    theme.style(Role::Body),
                ))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), rows[1]);
}
