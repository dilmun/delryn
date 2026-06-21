//! Metadata-edit form popup (`e` in the library): an editable form over the
//! selected book's title/author/year/series/publisher with an in-field text
//! cursor, numeric validation, and reset-to-EPUB. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, META_FIELDS, MetaEdit};

pub fn render(f: &mut Frame, app: &App) {
    let Some(ed) = &app.meta_edit else {
        return;
    };
    let theme = app.config.theme;
    let height = META_FIELDS.len() as u16 + 4; // border + hint + spacers
    let area = super::centered(f.area(), 60, height);

    f.render_widget(Clear, area);

    let bg = theme.bg.unwrap_or(Color::Black);
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
        lines.push(field_line(ed, i, label, theme, bg));
    }
    f.render_widget(Paragraph::new(lines), rows[0]);

    f.render_widget(
        Paragraph::new(Line::styled(hint(ed), Style::default().fg(theme.muted))),
        rows[1],
    );
}

/// One labelled field row: marker · label · value (with cursor when focused).
fn field_line(ed: &MetaEdit, i: usize, label: &str, theme: crate::theme::Theme, bg: Color) -> Line<'static> {
    let selected = i == ed.row;
    let value = ed.values.get(i).map(String::as_str).unwrap_or("");
    let invalid = ed.field_invalid(i);
    let changed = ed.changed(i);

    let marker = if selected { "▸ " } else { "  " };
    // A small dot flags a field edited away from the EPUB's value.
    let dot = if changed { "•" } else { " " };
    let label_style = if invalid {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg)
    };
    let pad = 12usize.saturating_sub(label.chars().count() + 3);

    let mut spans = vec![
        Span::styled(format!("{marker}{dot} {label}"), label_style),
        Span::raw(" ".repeat(pad)),
    ];
    let value_style = if invalid {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(theme.heading)
    };
    spans.extend(value_spans(value, ed.cursor, selected, value_style, theme, bg));
    Line::from(spans)
}

/// Render a field value, drawing a block cursor at `cursor` when focused.
fn value_spans(
    value: &str,
    cursor: usize,
    focused: bool,
    base: Style,
    theme: crate::theme::Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    if !focused {
        return vec![Span::styled(value.to_string(), base)];
    }
    let chars: Vec<char> = value.chars().collect();
    let cur = cursor.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let (at, after) = if cur < chars.len() {
        (chars[cur].to_string(), chars[cur + 1..].iter().collect())
    } else {
        (" ".to_string(), String::new())
    };
    let cursor_style = Style::default().fg(bg).bg(theme.accent);
    vec![
        Span::styled(before, base),
        Span::styled(at, cursor_style),
        Span::styled(after, base),
    ]
}

fn hint(ed: &MetaEdit) -> &'static str {
    if ed.has_invalid() {
        "fix the red field   ↑↓ field   ←→ move   ^R reset   Esc cancel"
    } else {
        "↑↓ field   ←→ move   ^R reset field   ^U clear   ⏎ save   Esc cancel"
    }
}
