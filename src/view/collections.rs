//! Manage-collections popup (`C`): create / rename / delete collections.
//! Shortcuts live in the bottom status bar.

use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, CollInput};
use crate::theme::Theme;

pub fn render(f: &mut Frame, app: &App) {
    let Some(m) = &app.coll_manager else {
        return;
    };
    let theme = app.config.theme;
    let bg = theme.bg.unwrap_or(Color::Black);
    let area = super::centered(f.area(), 56, 22);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Manage Collections ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let renaming_sel = m.input.as_ref().is_some_and(|i| i.rename_from.is_some());
    let creating = m.input.as_ref().is_some_and(|i| i.rename_from.is_none());

    let mut lines: Vec<Line> = Vec::new();
    if m.items.is_empty() && !creating {
        lines.push(Line::styled(
            "  No collections yet — ＋ New collection.",
            Style::default().fg(theme.muted),
        ));
    }
    for (i, (name, count)) in m.items.iter().enumerate() {
        let selected = i == m.sel;
        if selected && renaming_sel {
            lines.push(input_line(m.input.as_ref().unwrap(), theme, bg));
            continue;
        }
        let marker = if selected { "▸ " } else { "  " };
        let name_style = if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let mut spans = vec![
            Span::styled(format!("{marker}{name}"), name_style),
            Span::styled(format!("  ({count})"), Style::default().fg(theme.muted)),
        ];
        if selected && m.confirm_delete {
            spans.push(Span::styled(
                "   delete? press d again",
                Style::default().fg(theme.marker).add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
    }

    // The trailing "＋ New collection" row (index == items.len()).
    let new_sel = m.sel == m.items.len();
    if new_sel && creating {
        lines.push(input_line(m.input.as_ref().unwrap(), theme, bg));
    } else {
        let marker = if new_sel { "▸ " } else { "  " };
        let style = if new_sel {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        lines.push(Line::from(Span::styled(format!("{marker}＋ New collection"), style)));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// A name being typed (create or rename), with a block cursor at the caret.
fn input_line(input: &CollInput, theme: Theme, bg: Color) -> Line<'static> {
    let chars: Vec<char> = input.buf.chars().collect();
    let cur = input.cursor.min(chars.len());
    let text = Style::default().fg(theme.heading).add_modifier(Modifier::BOLD);
    let cursor = Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled("▸ ", Style::default().fg(theme.accent))];
    spans.push(Span::styled(chars[..cur].iter().collect::<String>(), text));
    let at = chars.get(cur).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
    spans.push(Span::styled(at, cursor));
    if cur < chars.len() {
        spans.push(Span::styled(chars[cur + 1..].iter().collect::<String>(), text));
    }
    Line::from(spans)
}
