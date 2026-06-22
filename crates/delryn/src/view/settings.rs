//! Settings popup (`;`), scoped to the current mode — Reading settings in the
//! reader, Library settings in the library — so the two never mix. Navigable,
//! edits the live config. See `DESIGN.md` §7.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Mode, SettingRow, settings_rows};

pub fn render(f: &mut Frame, app: &App) {
    let Some(state) = &app.settings else {
        return;
    };
    let theme = app.config.theme;
    let area = super::centered(f.area(), 64, 28);

    f.render_widget(Clear, area);

    let bg = theme.bg.unwrap_or(ratatui::style::Color::Black);
    let scope = match state.scope {
        Mode::Reader => "Reading",
        Mode::Library => "Library",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" {scope} Settings "),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // body
    ])
    .split(inner);

    // Rows: section headers (non-selectable) interleaved with settings.
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in settings_rows(state.scope).iter().enumerate() {
        match row {
            SettingRow::Section(title) => {
                if i > 0 {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::styled(
                    format!("  {title}"),
                    Style::default().fg(theme.muted).add_modifier(Modifier::BOLD | Modifier::DIM),
                ));
            }
            SettingRow::Item(item) => {
                let selected = i == state.row;
                let marker = if selected { "  ▸ " } else { "    " };
                let label = item.label();
                let label_style = if selected {
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg)
                };
                let pad = 28usize.saturating_sub(label.chars().count() + 4);
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker}{label}"), label_style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(item.value(&app.config), Style::default().fg(theme.heading)),
                ]));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), rows[1]);
    // Shortcuts live in the bottom status bar (see view::status).
}
