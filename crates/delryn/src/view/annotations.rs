//! The bookmarks overlay (a folder-grouped, jump-able list) and the bottom-row
//! rename / move-to-folder prompt. Notes are a separate Phase 4 concern.
//! See `DESIGN.md` §(annotations).

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph,
};

use crate::app::{App, Overlay, Prompt, PromptKind};
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
    let area = super::centered(f.area(), 74, 20);
    f.render_widget(Clear, area);

    let bg = theme.paper();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.style(Role::BorderFocus))
        .padding(Padding::horizontal(1))
        .title(Span::styled(" Bookmarks ", theme.style(Role::Title)))
        .title_bottom(Line::from(Span::styled(
            " ↑↓ move · ⏎ jump · r name · f folder · d delete ",
            theme.style(Role::Muted),
        )))
        .style(theme.style(Role::Body).bg(bg));
    // A count badge, right-aligned in the title bar.
    if !state.items.is_empty() {
        let n = state.items.len();
        let unit = if n == 1 { "mark" } else { "marks" };
        block = block.title(
            Line::from(Span::styled(
                format!(" {n} {unit} "),
                theme.style(Role::Muted),
            ))
            .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Min(0)]).split(inner);

    if state.items.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::styled("  No bookmarks yet.", theme.style(Role::Body)),
                Line::styled(
                    "  Press m in the reader to drop one at your place.",
                    theme.style(Role::Muted),
                ),
            ]),
            rows[0],
        );
        return;
    }

    // Build the rendered rows, inserting a non-selectable header whenever the
    // folder changes (items arrive folder-grouped from the store). `row_of` maps
    // each item index to its rendered row so the cursor lands on the right line.
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
            let count = state.items.iter().filter(|x| x.folder == a.folder).count();
            list_items.push(
                Line::from(vec![
                    Span::styled(format!("▾ {title}"), theme.style(Role::AccentStrong)),
                    Span::styled(format!("  {count}"), theme.style(Role::Muted)),
                ])
                .into(),
            );
        }
        row_of.push(list_items.len());
        // A custom name wins over the auto-captured quote as the label.
        let label = if a.name.is_empty() { &a.quote } else { &a.name };
        list_items.push(
            Line::from(vec![
                Span::styled(format!("   §{} ", a.section + 1), theme.style(Role::Muted)),
                Span::styled(label.clone(), theme.style(Role::Body)),
            ])
            .into(),
        );
    }
    let list = List::new(list_items).highlight_style(theme.style(Role::Selection));
    let mut st = ListState::default();
    let sel = state.sel.min(state.items.len() - 1);
    st.select(Some(row_of[sel]));
    crate::view::round_list(f, rows[0], list, &mut st, theme);
}
