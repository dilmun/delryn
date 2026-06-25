//! Settings popup (`;`), scoped to the current mode — Reading settings in the
//! reader, Library settings in the library — so the two never mix. Options are
//! grouped into tabs (Tab / Shift-Tab to switch); the body scrolls when a tab is
//! taller than the window. Edits the live config. See `DESIGN.md` §7.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::app::{App, Mode, SettingRow, settings_tabs, tab_rows};

/// Column where each option's value is shown (label left, value right).
const VALUE_COL: usize = 30;

pub fn render(f: &mut Frame, app: &App) {
    let Some(state) = &app.settings else {
        return;
    };
    let theme = app.config.theme;
    let area = super::centered(f.area(), 64, 26);
    f.render_widget(Clear, area);

    let bg = theme.paper();
    let scope = match state.scope {
        Mode::Reader => "Reading",
        Mode::Library => "Library",
    };
    let tabs = settings_tabs(state.scope);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" {scope} Settings "),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                " Tab section · ↑↓ move · ←→ change · q close ",
                Style::default().fg(theme.muted),
            ))
            .alignment(Alignment::Center),
        )
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1), // tab bar
        Constraint::Length(1), // divider rule
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // body
    ])
    .split(inner);

    render_tab_bar(f, chunks[0], &tabs, state.tab, theme);

    // Divider under the tabs.
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(chunks[1].width as usize),
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        )),
        chunks[1],
    );

    render_body(f, chunks[3], app, state.scope, state.tab, state.row, theme);
}

/// The pill-style tab strip, the active tab filled with the accent.
fn render_tab_bar(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    tabs: &[crate::app::SettingTab],
    active: usize,
    theme: crate::theme::Theme,
) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, t) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if i == active {
            Style::default()
                .fg(theme.on_accent())
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        spans.push(Span::styled(format!(" {} ", t.title), style));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        area,
    );
}

/// The active tab's options, scrolled to keep the cursor visible, with a
/// scrollbar when the tab is taller than the body.
fn render_body(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    app: &App,
    scope: Mode,
    tab: usize,
    sel_row: usize,
    theme: crate::theme::Theme,
) {
    let rows = tab_rows(scope, tab);
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    for (i, row) in rows.iter().enumerate() {
        match row {
            SettingRow::Section(title) => {
                if i > 0 {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::styled(
                    format!("  {title}"),
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::BOLD | Modifier::DIM),
                ));
            }
            SettingRow::Item(item) => {
                let selected = i == sel_row;
                if selected {
                    sel_line = lines.len();
                }
                let marker = if selected { "  ▸ " } else { "    " };
                let label = item.label();
                let label_style = if selected {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.fg)
                };
                let pad = VALUE_COL.saturating_sub(label.chars().count() + 4);
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker}{label}"), label_style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(item.value(&app.config), Style::default().fg(theme.heading)),
                ]));
            }
        }
    }

    let h = area.height as usize;
    let total = lines.len();
    let max_off = total.saturating_sub(h);
    // Center the cursor in the body (clamped at the ends), like the book list.
    let offset = sel_line.saturating_sub(h / 2).min(max_off);
    let visible: Vec<Line> = lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(visible), area);

    // A slim scrollbar only when the tab overflows the body.
    if total > h {
        let mut sb = ScrollbarState::new(total).position(offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(theme.accent))
                .track_style(Style::default().fg(theme.muted).add_modifier(Modifier::DIM)),
            area,
            &mut sb,
        );
    }
}
