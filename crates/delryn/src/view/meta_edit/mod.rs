//! Tabbed metadata editor (`e`): Details · Cover · Lookup. A scalable, form-style
//! popup with navigate/edit two-mode fields (which scroll horizontally to keep
//! the caret visible), an Open Library metadata lookup, and a cover search. Every
//! transient line (search progress, results, hints) is collapsed into one footer
//! row. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};

use ratatui_image::{Resize, StatefulImage};

use crate::app::{App, EditMode, EditTab, LOOKUP_FIELDS, META_FIELDS, MetaEdit, Overlay};
use crate::theme::Theme;

/// Left column width for field labels.
const LABEL_W: usize = 14;

// Sub-views; `render` orchestrates them. Shared helpers (`base`, `form_field`,
// `scaled`, `footer_line`) stay here and are called from the children.
mod hits;
mod online;

pub fn render(f: &mut Frame, app: &mut App) {
    if !matches!(app.overlay, Overlay::MetaEdit(_)) {
        return;
    }
    let theme = app.config.theme;
    let bg = theme.paper();
    let area = scaled(f.area());

    f.render_widget(Clear, area);
    let title = {
        let Overlay::MetaEdit(ed) = &app.overlay else {
            return;
        };
        // Progress counter when stepping through a multi-book edit queue.
        let progress = if app.edit_total > 1 {
            format!(
                " ({}/{})",
                app.edit_total - app.edit_queue.len(),
                app.edit_total
            )
        } else {
            String::new()
        };
        format!(
            " ✎ Edit{progress} · {} ",
            super::truncate(&ed.book_title, area.width.saturating_sub(20) as usize)
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // tab strip
        Constraint::Length(1), // rule
        Constraint::Min(0),    // body
        Constraint::Length(1), // status (transient feedback)
    ])
    .split(inner);

    if let Overlay::MetaEdit(ed) = &app.overlay {
        render_tabs(f, rows[0], ed, theme, bg);
    }
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        )),
        rows[1],
    );

    // Body — each arm takes a fresh short borrow so the Cover tab can also touch
    // app.edit_cover (the preview image protocol) mutably.
    let body = rows[2];
    let tab = match &app.overlay {
        Overlay::MetaEdit(e) => e.tab,
        _ => return,
    };
    match tab {
        EditTab::Details => {
            if let Overlay::MetaEdit(ed) = &app.overlay {
                render_details(f, body, ed, theme);
            }
        }
        EditTab::Online => {
            if let Overlay::MetaEdit(ed) = &app.overlay {
                online::render_online(f, body, ed, theme);
            }
        }
        EditTab::Cover => online::render_cover(f, body, app, theme),
    }

    // One transient line at the foot: search progress / results / errors, else a
    // quiet hint for the lookup tabs. The shortcut legend lives in the app status
    // bar (see view::status), not here.
    if let Overlay::MetaEdit(ed) = &app.overlay {
        let footer = footer_line(ed, theme);
        f.render_widget(Paragraph::new(footer), rows[3]);
    }

    hits::record_hits(app, rows[0], body);

    // The metadata-diff overlay sits on top of the editor when open.
    render_diff(f, app, theme);
}

/// The metadata-diff overlay: current vs the picked candidate, one row per field,
/// with a tick on the fields chosen to apply.
fn render_diff(f: &mut Frame, app: &App, theme: Theme) {
    let Overlay::MetaEdit(ed) = &app.overlay else {
        return;
    };
    let Some(diff) = ed.diff.as_ref() else {
        return;
    };
    let bg = theme.paper();
    let area = super::centered(f.area(), 84, diff.rows.len() as u16 + 4);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Apply remote metadata ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(Span::styled(
            " space toggle · a all · ⏎ apply ticked · Esc cancel ",
            Style::default().fg(theme.muted),
        )))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // tick(4) + field(12) + the rest split between current and remote.
    let field_w = 12usize;
    let col = (inner.width as usize).saturating_sub(4 + field_w + 2) / 2;
    let items: Vec<ListItem> = diff
        .rows
        .iter()
        .map(|r| {
            let tick = if r.remote.is_empty() {
                "   "
            } else if r.apply {
                "[x]"
            } else {
                "[ ]"
            };
            let current = super::truncate(&ed.values[r.field], col.max(1));
            let remote = super::truncate(&r.remote, col.max(1));
            Line::from(vec![
                Span::styled(
                    format!("{tick} "),
                    Style::default().fg(if r.apply { theme.accent } else { theme.muted }),
                ),
                Span::styled(
                    format!("{:field_w$}", META_FIELDS[r.field]),
                    Style::default().fg(theme.heading),
                ),
                Span::styled(
                    format!("{current:col$}  "),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(
                    remote,
                    Style::default().fg(if r.remote.is_empty() {
                        theme.muted
                    } else {
                        theme.fg
                    }),
                ),
            ])
            .into()
        })
        .collect();
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme.accent)
            .fg(theme.on_accent())
            .add_modifier(Modifier::BOLD),
    );
    let mut st = ListState::default();
    st.select(Some(diff.row.min(diff.rows.len().saturating_sub(1))));
    f.render_stateful_widget(list, inner, &mut st);
}

/// The single foot-of-popup line: the tab's transient status (search progress,
/// result counts, errors) when present, otherwise a quiet search hint on the
/// lookup tabs. The status is shown only on the tab it belongs to, so a
/// Cover/Lookup "searching…" never leaks onto Details.
fn footer_line(ed: &MetaEdit, theme: Theme) -> Line<'static> {
    let on_this_tab = ed.status_tab == Some(ed.tab);
    if let Some(status) = ed.status.as_ref().filter(|_| on_this_tab) {
        return Line::styled(
            format!("  {status}"),
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::ITALIC),
        );
    }
    let hint = match ed.tab {
        EditTab::Online => "  edit Title / Author, then ⏎ to search",
        EditTab::Cover => "  type or / to search for a cover",
        EditTab::Details => "",
    };
    Line::styled(
        hint,
        Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
    )
}

fn render_tabs(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color) {
    // Numbered pills (1–4 jump to a tab): active is an accent block, inactive
    // shows its number in accent as a shortcut hint.
    let mut spans = Vec::new();
    for (i, t) in EditTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let num = i + 1;
        if *t == ed.tab {
            spans.push(Span::styled(
                format!(" {num} {} ", t.label()),
                Style::default()
                    .fg(bg)
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!(" {num} "),
                Style::default().fg(theme.accent),
            ));
            spans.push(Span::styled(
                format!("{} ", t.label()),
                Style::default().fg(theme.muted),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Details fields grouped into labelled sections (the field indices keep their
/// `META_FIELDS` order, so navigation by `ed.row` still runs top-to-bottom).
const DETAILS_GROUPS: &[(&str, &[usize])] = &[
    ("Book", &[0, 1, 2, 3, 4]),    // Title, Author, Year, Series, Series #
    ("Publishing", &[5, 6, 7, 8]), // Publisher, Subtitle, ISBN, Language
];

fn render_details(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    // marker (3) + label (LABEL_W) + value + the " ↵" hint (3).
    let value_w = (area.width as usize).saturating_sub(LABEL_W + 6).max(8);
    let mut lines: Vec<Line> = Vec::new();
    for (gi, (section, fields)) in DETAILS_GROUPS.iter().enumerate() {
        if gi > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            format!(" {section}"),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD | Modifier::DIM),
        ));
        for &i in *fields {
            let focused = i == ed.row;
            let editing = focused && ed.mode == EditMode::Edit;
            let value = ed.values.get(i).map(String::as_str).unwrap_or("");
            lines.push(form_field(
                META_FIELDS[i],
                value,
                FieldState {
                    focused,
                    editing,
                    cursor: ed.cursor,
                    invalid: ed.field_invalid(i),
                    changed: ed.changed(i),
                },
                value_w,
                theme,
            ));
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The interactive state of a form field, for rendering.
#[derive(Clone, Copy)]
struct FieldState {
    focused: bool,
    editing: bool,
    cursor: usize,
    invalid: bool,
    changed: bool,
}

/// A labelled form row: ` ▸ Label        value ↵`. The field is distinguished by
/// shading the **label** (bold; accent when focused, dim otherwise), not by a
/// background box. A leading marker flags focus (▸) or an unsaved change (•);
/// invalid values turn red; a block cursor shows while editing.
fn form_field(
    label: &str,
    value: &str,
    st: FieldState,
    value_w: usize,
    theme: Theme,
) -> Line<'static> {
    let (marker, marker_col) = if st.focused {
        ("▸", theme.accent)
    } else if st.changed {
        ("•", theme.marker)
    } else {
        (" ", theme.muted)
    };
    let label_style = if st.invalid {
        Style::default()
            .fg(theme.danger)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(if st.focused {
                theme.accent
            } else {
                theme.muted
            })
            .add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![
        Span::styled(format!(" {marker} "), Style::default().fg(marker_col)),
        Span::styled(format!("{label:<LABEL_W$}"), label_style),
    ];
    if st.editing {
        spans.extend(super::field_spans(value, st.cursor, value_w, theme));
    } else {
        let c = if st.invalid { theme.danger } else { theme.fg };
        let shown = super::truncate(value, value_w);
        if shown.is_empty() {
            spans.push(Span::styled("—", Style::default().fg(theme.muted)));
        } else {
            let vs = if st.focused {
                Style::default().fg(c).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(c)
            };
            spans.push(Span::styled(shown, vs));
        }
        if st.focused {
            spans.push(Span::styled("  ↵", Style::default().fg(theme.accent)));
        }
    }
    Line::from(spans)
}

/// A centered rect scaled to the terminal (≈72% × 70%, clamped).
fn scaled(area: Rect) -> Rect {
    let w = (area.width * 72 / 100)
        .clamp(40, 96)
        .min(area.width.saturating_sub(2).max(1));
    let h = (area.height * 70 / 100)
        .clamp(14, 34)
        .min(area.height.saturating_sub(2).max(1));
    super::centered(area, w, h)
}
