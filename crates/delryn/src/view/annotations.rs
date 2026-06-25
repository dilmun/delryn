//! Bookmarks/notes overlay and the note-entry prompt. See `DESIGN.md` §(annotations).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, Prompt, PromptKind};

pub fn render(f: &mut Frame, app: &App) {
    if let Some(prompt) = &app.prompt {
        render_prompt(f, app, prompt);
    }
    if app.annot.is_some() {
        render_overlay(f, app);
    }
}

fn render_prompt(f: &mut Frame, app: &App, prompt: &Prompt) {
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
    let label = match prompt.kind {
        PromptKind::Note => "note",
        PromptKind::Name(_) => "name",
        PromptKind::Folder(_) => "folder",
    };
    f.render_widget(
        Paragraph::new(Line::raw(format!("{label}: {}", prompt.buffer))).style(style),
        row,
    );
}

fn render_overlay(f: &mut Frame, app: &App) {
    let Some(state) = &app.annot else {
        return;
    };
    let theme = app.config.theme;
    let area = super::centered(f.area(), 72, 18);
    f.render_widget(Clear, area);

    let bg = theme.paper();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Bookmarks & Notes ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Min(0)]).split(inner);

    if state.items.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                "  No bookmarks yet — press m to bookmark, M to note.",
                Style::default().fg(theme.muted),
            )),
            rows[0],
        );
    } else {
        // Build the rendered rows, inserting a non-selectable header whenever the
        // folder changes (items arrive folder-grouped from the store). `row_of`
        // maps each item index to its rendered row so the cursor lands right.
        let mut list_items: Vec<ListItem> = Vec::new();
        let mut row_of: Vec<usize> = Vec::with_capacity(state.items.len());
        let mut current_folder: Option<&str> = None;
        for a in &state.items {
            if current_folder != Some(a.folder.as_str()) {
                current_folder = Some(a.folder.as_str());
                let title = if a.folder.is_empty() {
                    "Bookmarks"
                } else {
                    a.folder.as_str()
                };
                list_items.push(
                    Line::from(Span::styled(
                        format!("▾ {title}"),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .into(),
                );
            }
            row_of.push(list_items.len());
            let marker = if a.note.is_empty() { "•" } else { "✎" };
            // A custom name wins over the auto-captured quote as the label.
            let label = if a.name.is_empty() { &a.quote } else { &a.name };
            let body = if a.note.is_empty() {
                label.clone()
            } else {
                format!("{label}  — {}", a.note)
            };
            list_items.push(
                Line::from(vec![
                    Span::styled(format!("  {marker} "), Style::default().fg(theme.marker)),
                    Span::styled(
                        format!("§{} ", a.section + 1),
                        Style::default().fg(theme.muted),
                    ),
                    Span::styled(body, Style::default().fg(theme.fg)),
                ])
                .into(),
            );
        }
        let highlight = Style::default()
            .fg(bg)
            .bg(theme.accent)
            .add_modifier(Modifier::BOLD);
        let list = List::new(list_items).highlight_style(highlight);
        let mut st = ListState::default();
        let sel = state.sel.min(state.items.len() - 1);
        st.select(Some(row_of[sel]));
        f.render_stateful_widget(list, rows[0], &mut st);
    }
    // Shortcuts live in the bottom status bar (see view::status).
}
