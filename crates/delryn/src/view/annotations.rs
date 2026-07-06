//! The annotations overlay — a folder-grouped, searchable, jump-able list of
//! bookmarks (⚑) and notes (✎) — plus the bottom-row rename / move-to-folder /
//! note-commentary prompt. See `DESIGN.md` §(annotations).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph,
};

use crate::app::{AnnotTab, App, Overlay, Prompt, PromptKind};
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &App) {
    if let Overlay::Prompt(prompt) = &app.overlay {
        render_prompt(f, app, prompt);
    }
    if matches!(app.overlay, Overlay::Annot(_)) {
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
    let style = theme.style(Role::StatusBar);
    let label = match prompt.kind {
        PromptKind::Name(_) => "name",
        PromptKind::Folder(_) => "folder",
        PromptKind::NewNote { .. } | PromptKind::EditNote(_) => "note",
    };
    f.render_widget(
        Paragraph::new(Line::raw(format!("{label}: {}▏", prompt.input.text()))).style(style),
        row,
    );
}

fn render_overlay(f: &mut Frame, app: &App) {
    let Overlay::Annot(state) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let items = state.filtered();
    let bg = theme.paper();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.style(Role::BorderFocus))
        .padding(Padding::horizontal(1))
        .title(Span::styled(" Annotations ", theme.style(Role::Title)))
        .title_bottom(Line::from(Span::styled(
            " ↑↓ move · ⏎ jump · ⇥ tab · / find · r name · F folder · e note · d del ",
            theme.style(Role::Muted),
        )))
        .style(theme.style(Role::Body).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // A tab bar on top (Bookmarks | Notes, active one highlighted), an optional
    // filter row, then the list.
    let show_filter = state.filtering || !state.filter.is_empty();
    let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
    if show_filter {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    let rows = Layout::vertical(constraints).split(inner);

    // Tab bar.
    let tab_span = |tab: AnnotTab, icon: &str| {
        let label = format!(" {icon} {} ({}) ", tab.label(), state.count(tab));
        let style = if state.tab == tab {
            theme.style(Role::Selection)
        } else {
            theme.style(Role::Muted)
        };
        Span::styled(label, style)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            tab_span(AnnotTab::Bookmarks, "⚑"),
            Span::raw("  "),
            tab_span(AnnotTab::Notes, "✎"),
        ])),
        rows[0],
    );
    // A thin rule under the tabs.
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(inner.width as usize),
            theme.style(Role::Muted),
        )),
        rows[1],
    );
    if show_filter {
        let caret = if state.filtering { "▏" } else { "" };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("find: ", theme.style(Role::Muted)),
                Span::styled(format!("{}{caret}", state.filter), theme.style(Role::Body)),
            ])),
            rows[2],
        );
    }
    let list_area = rows[rows.len() - 1];

    if items.is_empty() {
        let (body, muted) = (theme.style(Role::Body), theme.style(Role::Muted));
        let msg = if !state.filter.is_empty() {
            vec![
                Line::raw(""),
                Line::styled("  Nothing matches your search.", muted),
            ]
        } else if state.tab == AnnotTab::Notes {
            vec![
                Line::raw(""),
                Line::styled("  No notes yet.", body),
                Line::styled("  Press a in the reader to write one.", muted),
            ]
        } else {
            vec![
                Line::raw(""),
                Line::styled("  No bookmarks yet.", body),
                Line::styled("  Press m in the reader to drop one.", muted),
            ]
        };
        f.render_widget(Paragraph::new(msg), list_area);
        return;
    }

    // Build the rendered rows, inserting a non-selectable header whenever the
    // folder changes (items arrive folder-grouped from the store). `row_of` maps
    // each item index to its rendered row so the cursor lands on the right line.
    let mut list_items: Vec<ListItem> = Vec::new();
    let mut row_of: Vec<usize> = Vec::with_capacity(items.len());
    let mut current_folder: Option<&str> = None;
    for a in &items {
        if current_folder != Some(a.folder.as_str()) {
            current_folder = Some(a.folder.as_str());
            let title = if a.folder.is_empty() {
                "Ungrouped"
            } else {
                a.folder.as_str()
            };
            let count = items.iter().filter(|x| x.folder == a.folder).count();
            list_items.push(
                Line::from(vec![
                    Span::styled(format!("▾ {title}"), theme.style(Role::AccentStrong)),
                    Span::styled(format!("  {count}"), theme.style(Role::Muted)),
                ])
                .into(),
            );
        }
        row_of.push(list_items.len());
        // A note shows its commentary (falling back to the quote); a bookmark its
        // name or quote. A pen marks a note, a flag a bookmark — matching the gutter.
        let (icon, primary): (&str, &str) = if a.is_note() {
            ("✎", pick(&a.name, &a.note, &a.quote))
        } else {
            ("⚑", pick(&a.name, &a.quote, &a.quote))
        };
        let mut spans = vec![
            Span::styled(format!(" {icon} "), theme.style(Role::AccentStrong)),
            Span::styled(format!("§{} ", a.section + 1), theme.style(Role::Muted)),
            Span::styled(primary.to_string(), theme.style(Role::Body)),
        ];
        // For a note, dim-trail the anchored line so the location is still visible.
        if a.is_note() && !a.quote.is_empty() && a.name.is_empty() {
            spans.push(Span::styled(
                format!("  — {}", trim_to(&a.quote, 28)),
                theme.style(Role::Muted),
            ));
        }
        list_items.push(Line::from(spans).into());
    }
    let list = List::new(list_items).highlight_style(theme.style(Role::Selection));
    let mut st = ListState::default();
    let sel = state.sel.min(items.len() - 1);
    st.select(Some(row_of[sel]));
    crate::view::round_list(f, list_area, list, &mut st, theme);
}

/// The first non-empty of three candidate labels.
fn pick<'a>(a: &'a str, b: &'a str, c: &'a str) -> &'a str {
    if !a.is_empty() {
        a
    } else if !b.is_empty() {
        b
    } else {
        c
    }
}

/// Truncate `s` to `max` characters, adding an ellipsis when it was cut.
fn trim_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
