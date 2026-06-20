//! Bookmarks/notes overlay and the note-entry prompt. See `DESIGN.md` §(annotations).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::App;

pub fn render(f: &mut Frame, app: &App) {
    if let Some(text) = &app.note_input {
        render_note_prompt(f, app, text);
    }
    if app.annot.is_some() {
        render_overlay(f, app);
    }
}

fn render_note_prompt(f: &mut Frame, app: &App, text: &str) {
    let theme = app.config.theme;
    let area = f.area();
    let row = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };
    f.render_widget(Clear, row);
    let style = Style::default().fg(theme.status_fg).bg(theme.status_bg);
    f.render_widget(
        Paragraph::new(Line::raw(format!("note: {text}"))).style(style),
        row,
    );
}

fn render_overlay(f: &mut Frame, app: &App) {
    let Some(state) = &app.annot else {
        return;
    };
    let theme = app.config.theme;
    let area = centered(f.area(), 72, 18);
    f.render_widget(Clear, area);

    let bg = theme.bg.unwrap_or(Color::Black);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Bookmarks & Notes ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);

    if state.items.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                "  No bookmarks yet — press m to bookmark, M to note.",
                Style::default().fg(theme.muted),
            )),
            rows[0],
        );
    } else {
        let items: Vec<ListItem> = state
            .items
            .iter()
            .map(|a| {
                let marker = if a.note.is_empty() { "•" } else { "✎" };
                let body = if a.note.is_empty() {
                    a.quote.clone()
                } else {
                    format!("{}  — {}", a.quote, a.note)
                };
                Line::from(vec![
                    Span::styled(format!("{marker} "), Style::default().fg(theme.marker)),
                    Span::styled(format!("§{} ", a.section + 1), Style::default().fg(theme.muted)),
                    Span::styled(body, Style::default().fg(theme.fg)),
                ])
                .into()
            })
            .collect();
        let highlight = Style::default()
            .fg(bg)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD);
        let list = List::new(items).highlight_style(highlight);
        let mut st = ListState::default();
        st.select(Some(state.sel.min(state.items.len().saturating_sub(1))));
        f.render_stateful_widget(list, rows[0], &mut st);
    }

    f.render_widget(
        Paragraph::new(Line::styled(
            "↑↓ move   ⏎ jump   d delete   Esc close",
            Style::default().fg(theme.muted),
        )),
        rows[1],
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(1);
    let h = h.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}
