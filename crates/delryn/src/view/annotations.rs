//! The annotations overlay — a folder-grouped, searchable, jump-able list of
//! bookmarks (⚑), notes (✎), and highlights (a coloured ▌) — plus the bottom-row
//! rename / move-to-folder / note-commentary prompt. See `DESIGN.md` §(annotations).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Padding, Paragraph};

use crate::HighlightColor;
use crate::app::{AnnotTab, App, Overlay, Prompt, PromptKind};
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &mut App) {
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

fn render_overlay(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let bold = app.config.bold_borders;
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let Overlay::Annot(state) = &app.overlay else {
        return;
    };
    let items = state.filtered();
    let active_tab = state.tab;
    let sel = state.sel;
    let filtering = state.filtering;
    let filter = state.filter.clone();
    let bg = theme.paper();
    let block = super::overlay_frame(theme, bold)
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
    let show_filter = filtering || !filter.is_empty();
    let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
    if show_filter {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));
    let rows = Layout::vertical(constraints).split(inner);

    // Tab bar — one labelled pill per tab, built in a loop so the click rects line
    // up exactly with what's drawn (a 2-cell gap between pills). Labels use only
    // width-1 glyphs, so the char count is the cell width.
    let mut tab_spans: Vec<Span> = Vec::new();
    let mut tab_hits: Vec<(usize, Rect)> = Vec::new();
    let mut tx = rows[0].x;
    for (i, &t) in AnnotTab::ALL.iter().enumerate() {
        if i > 0 {
            tab_spans.push(Span::raw("  "));
            tx += 2;
        }
        let label = format!(" {} {} ({}) ", tab_icon(t), t.label(), state.count(t));
        let w = label.chars().count() as u16;
        let style = if active_tab == t {
            theme.style(Role::Selection)
        } else {
            theme.style(Role::Muted)
        };
        tab_spans.push(Span::styled(label, style));
        tab_hits.push((
            i,
            Rect {
                x: tx,
                y: rows[0].y,
                width: w,
                height: 1,
            },
        ));
        tx += w;
    }
    f.render_widget(Paragraph::new(Line::from(tab_spans)), rows[0]);
    // A thin rule under the tabs.
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(inner.width as usize),
            theme.style(Role::Muted),
        )),
        rows[1],
    );
    if show_filter {
        let caret = if filtering { "▏" } else { "" };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("find: ", theme.style(Role::Muted)),
                Span::styled(format!("{filter}{caret}"), theme.style(Role::Body)),
            ])),
            rows[2],
        );
    }
    let list_area = rows[rows.len() - 1];

    if items.is_empty() {
        app.mouse.overlay_tabs = tab_hits;
        let (body, muted) = (theme.style(Role::Body), theme.style(Role::Muted));
        let msg = if !filter.is_empty() {
            vec![
                Line::raw(""),
                Line::styled("  Nothing matches your search.", muted),
            ]
        } else if active_tab == AnnotTab::Notes {
            vec![
                Line::raw(""),
                Line::styled("  No notes yet.", body),
                Line::styled("  Press a in the reader to write one.", muted),
            ]
        } else if active_tab == AnnotTab::Highlights {
            vec![
                Line::raw(""),
                Line::styled("  No highlights yet.", body),
                Line::styled("  Press H in the reader to mark a line.", muted),
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
        // A note shows its commentary (falling back to the quote); a bookmark or
        // highlight its name or quote. The icon matches the gutter: a pen for a
        // note, a flag for a bookmark, a coloured bar (in its own hue) for a
        // highlight.
        let icon_span = if a.is_highlight() {
            Span::styled(
                " ▌ ",
                Style::default().fg(HighlightColor::from_index(a.color).bg()),
            )
        } else if a.is_note() {
            Span::styled(" ✎ ", theme.style(Role::AccentStrong))
        } else {
            Span::styled(" ⚑ ", theme.style(Role::AccentStrong))
        };
        let primary = if a.is_note() {
            pick(&a.name, &a.note, &a.quote)
        } else {
            pick(&a.name, &a.quote, &a.quote)
        };
        let mut spans = vec![
            icon_span,
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
    let sel = sel.min(items.len() - 1);
    st.select(Some(row_of[sel]));
    crate::view::round_list(f, list_area, list, &mut st, theme);

    // Map each item to its on-screen rect for click hit-testing, using the offset
    // `round_list` settled on — it insets the list one column each side (rounded
    // caps), so mirror that inset here. Folder-header rows carry no item and so
    // aren't recorded.
    let off = st.offset();
    let inset_x = list_area.x + 1;
    let inset_w = list_area.width.saturating_sub(2);
    let mut row_hits: Vec<(usize, Rect)> = Vec::with_capacity(row_of.len());
    for (i, &rrow) in row_of.iter().enumerate() {
        if rrow < off {
            continue;
        }
        let sy = list_area.y + (rrow - off) as u16;
        if sy >= list_area.y + list_area.height {
            continue;
        }
        row_hits.push((
            i,
            Rect {
                x: inset_x,
                y: sy,
                width: inset_w,
                height: 1,
            },
        ));
    }
    app.mouse.overlay_tabs = tab_hits;
    app.mouse.overlay_rows = row_hits;
}

/// The tab-bar glyph for each annotation kind (matches the list-row icons).
fn tab_icon(t: AnnotTab) -> &'static str {
    match t {
        AnnotTab::Bookmarks => "⚑",
        AnnotTab::Notes => "✎",
        AnnotTab::Highlights => "▌",
    }
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
