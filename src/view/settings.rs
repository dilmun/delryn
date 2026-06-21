//! Mode-scoped settings popup (`;`): tabbed General / Reading / Library,
//! navigable, edits the live config. See `DESIGN.md` §7.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, SettingsTab, settings_rows};

const TABS: [SettingsTab; 3] = [
    SettingsTab::General,
    SettingsTab::Reading,
    SettingsTab::Library,
];

pub fn render(f: &mut Frame, app: &App) {
    let Some(state) = &app.settings else {
        return;
    };
    let theme = app.config.theme;
    let area = super::centered(f.area(), 60, 20);

    f.render_widget(Clear, area);

    let bg = theme.bg.unwrap_or(ratatui::style::Color::Black);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Settings ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // tabs
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // body
        Constraint::Length(1), // hint
    ])
    .split(inner);

    // Tab strip.
    let mut tab_spans = Vec::new();
    for (i, t) in TABS.iter().enumerate() {
        if i > 0 {
            tab_spans.push(Span::raw("  "));
        }
        let active = *t == state.tab;
        let style = if active {
            Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        tab_spans.push(Span::styled(format!(" {} ", t.label()), style));
    }
    f.render_widget(Paragraph::new(Line::from(tab_spans)), rows[0]);

    // Rows.
    let items = settings_rows(&app.config, state.tab);
    let mut lines: Vec<Line> = Vec::new();
    if items.is_empty() {
        lines.push(Line::styled(
            "  (coming soon)",
            Style::default().fg(theme.muted),
        ));
    }
    for (i, (label, value)) in items.iter().enumerate() {
        let selected = i == state.row;
        let marker = if selected { "▸ " } else { "  " };
        let label_style = if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let pad = 26usize.saturating_sub(label.chars().count() + 2);
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{label}"), label_style),
            Span::raw(" ".repeat(pad)),
            Span::styled(value.clone(), Style::default().fg(theme.heading)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), rows[2]);

    f.render_widget(
        Paragraph::new(Line::styled(
            "↑↓ move   ←→ change   Tab switch   Esc close",
            Style::default().fg(theme.muted),
        )),
        rows[3],
    );
}
