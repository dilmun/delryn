//! Command-palette overlay: a query line over a fuzzy-filtered command list.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &mut App) {
    let Overlay::Palette(p) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let bold = app.config.bold_borders;
    let matches = p.filtered();
    let sel = p.sel;
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let block = super::overlay_frame(theme, bold)
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
    let list_area = rows[1];
    let h = list_area.height as usize;
    let offset = sel
        .saturating_sub(h / 2)
        .min(matches.len().saturating_sub(h));
    let mut lines: Vec<Line> = Vec::with_capacity(h.min(matches.len()));
    let mut hits: Vec<(usize, Rect)> = Vec::new();
    for (i, it) in matches.iter().enumerate().skip(offset).take(h) {
        let sy = list_area.y + (i - offset) as u16;
        hits.push((
            i,
            Rect {
                x: list_area.x,
                y: sy,
                width: list_area.width,
                height: 1,
            },
        ));
        if i == sel {
            lines.push(crate::view::rounded_line(
                format!(" {}", it.label),
                list_area.width,
                theme,
            ));
        } else {
            lines.push(Line::from(Span::styled(
                format!(" {}", it.label),
                theme.style(Role::Body),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), list_area);
    app.mouse.overlay_rows = hits;
}
