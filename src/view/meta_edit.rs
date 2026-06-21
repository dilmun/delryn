//! Tabbed metadata editor (`e`): Details / Collections / Online. Scales to the
//! terminal, navigate/edit two-mode fields, and an Open Library lookup that
//! applies official metadata + cover. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{
    App, EditMode, EditTab, META_FIELDS, MetaEdit, ONLINE_AUTHOR, ONLINE_RESULTS_START,
    ONLINE_SEARCH_ROW, ONLINE_TITLE,
};
use crate::theme::Theme;

pub fn render(f: &mut Frame, app: &App) {
    let Some(ed) = &app.meta_edit else {
        return;
    };
    let theme = app.config.theme;
    let bg = theme.bg.unwrap_or(Color::Black);
    let area = scaled(f.area());

    f.render_widget(Clear, area);
    let title = format!(" Edit · {} ", truncate(&ed.book_title, area.width.saturating_sub(12) as usize));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().fg(theme.fg).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1), // tab strip
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // tab body
        Constraint::Length(1), // status
        Constraint::Length(1), // hint
    ])
    .split(inner);

    render_tabs(f, rows[0], ed, theme, bg);
    match ed.tab {
        EditTab::Details => render_details(f, rows[2], ed, theme, bg),
        EditTab::Collections => render_collections(f, rows[2], ed, theme),
        EditTab::Online => render_online(f, rows[2], ed, theme, bg),
    }
    if let Some(s) = &ed.status {
        f.render_widget(
            Paragraph::new(Line::styled(
                format!("  {s}"),
                Style::default().fg(theme.heading).add_modifier(Modifier::ITALIC),
            )),
            rows[3],
        );
    }
    f.render_widget(
        Paragraph::new(Line::styled(hint(ed), Style::default().fg(theme.muted))),
        rows[4],
    );
}

fn render_tabs(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color) {
    let mut spans = Vec::new();
    for (i, t) in EditTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let active = *t == ed.tab;
        let style = if active {
            Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        spans.push(Span::styled(format!(" {} ", t.label()), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_details(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color) {
    let mut lines: Vec<Line> = Vec::new();
    for (i, label) in META_FIELDS.iter().enumerate() {
        let selected = i == ed.row;
        let editing = selected && ed.mode == EditMode::Edit;
        let value = ed.values.get(i).map(String::as_str).unwrap_or("");
        let invalid = ed.field_invalid(i);

        let marker = if editing {
            "✎ "
        } else if selected {
            "▸ "
        } else {
            "  "
        };
        let dot = if ed.changed(i) { "•" } else { " " };
        let label_style = if invalid {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let pad = 12usize.saturating_sub(label.chars().count() + 3);
        let mut spans = vec![
            Span::styled(format!("{marker}{dot} {label}"), label_style),
            Span::raw(" ".repeat(pad)),
        ];
        let vstyle = if invalid {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(theme.heading)
        };
        spans.extend(value_spans(value, ed.cursor, editing, vstyle, theme, bg));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_collections(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let mut lines: Vec<Line> = Vec::new();
    if ed.shelves.is_empty() && ed.new_shelf.is_none() {
        lines.push(Line::styled(
            "  No collections yet — pick “New collection”.",
            Style::default().fg(theme.muted),
        ));
    }
    for (i, (name, member)) in ed.shelves.iter().enumerate() {
        let selected = i == ed.shelf_sel && ed.new_shelf.is_none();
        let marker = if selected { "▸ " } else { "  " };
        let check = if *member { "[✓] " } else { "[ ] " };
        let style = if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else if *member {
            Style::default().fg(theme.fg)
        } else {
            Style::default().fg(theme.muted)
        };
        lines.push(Line::from(Span::styled(format!("{marker}{check}{name}"), style)));
    }
    // The "new collection" row, or a live text input when creating one.
    if let Some(buf) = &ed.new_shelf {
        lines.push(Line::from(vec![
            Span::styled("▸ ＋ ", Style::default().fg(theme.accent)),
            Span::styled(buf.clone(), Style::default().fg(theme.heading)),
            Span::styled("█", Style::default().fg(theme.accent)),
        ]));
    } else {
        let selected = ed.shelf_sel == ed.new_shelf_row();
        let marker = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}＋ New collection…"),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_online(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color) {
    let rows = Layout::vertical([
        Constraint::Length(2), // query fields
        Constraint::Length(1), // search button
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // results
    ])
    .split(area);

    // Query fields.
    let q = vec![
        query_line("Title ", &ed.q_title, ed.online_row == ONLINE_TITLE, ed, theme, bg),
        query_line("Author", &ed.q_author, ed.online_row == ONLINE_AUTHOR, ed, theme, bg),
    ];
    f.render_widget(Paragraph::new(q), rows[0]);

    // Search action.
    let search_sel = ed.online_row == ONLINE_SEARCH_ROW;
    let search_style = if search_sel {
        Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.accent)
    };
    let label = if ed.fetching { " Searching… " } else { " ⌕ Search " };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!("  {label}"), search_style))),
        rows[1],
    );

    // Results.
    let cap = rows[3].height as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (i, c) in ed.results.iter().enumerate().take(cap) {
        let selected = ed.online_row == ONLINE_RESULTS_START + i;
        let marker = if selected { "▸ " } else { "  " };
        let series = match (&c.series, c.series_index) {
            (Some(s), Some(n)) => format!("  · {s} #{n}"),
            (Some(s), None) => format!("  · {s}"),
            _ => String::new(),
        };
        let year = c.year.map(|y| format!(" ({y})")).unwrap_or_default();
        let meta = format!("{} — {}{year}{series}", c.title, c.author_line());
        let style = if selected {
            Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}", truncate(&meta, area.width.saturating_sub(3) as usize)),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), rows[3]);
}

fn query_line(
    label: &str,
    value: &str,
    selected: bool,
    ed: &MetaEdit,
    theme: Theme,
    bg: Color,
) -> Line<'static> {
    let editing = selected && ed.mode == EditMode::Edit;
    let marker = if editing {
        "✎ "
    } else if selected {
        "▸ "
    } else {
        "  "
    };
    let lstyle = if selected {
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg)
    };
    let mut spans = vec![
        Span::styled(format!("{marker}{label}  "), lstyle),
    ];
    spans.extend(value_spans(
        value,
        ed.cursor,
        editing,
        Style::default().fg(theme.heading),
        theme,
        bg,
    ));
    Line::from(spans)
}

/// Render a value, drawing a block cursor at `cursor` when actively editing.
fn value_spans(
    value: &str,
    cursor: usize,
    editing: bool,
    base: Style,
    theme: Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    if !editing {
        return vec![Span::styled(value.to_string(), base)];
    }
    let chars: Vec<char> = value.chars().collect();
    let cur = cursor.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let (at, after) = if cur < chars.len() {
        (chars[cur].to_string(), chars[cur + 1..].iter().collect())
    } else {
        (" ".to_string(), String::new())
    };
    let cursor_style = Style::default().fg(bg).bg(theme.accent);
    vec![
        Span::styled(before, base),
        Span::styled(at, cursor_style),
        Span::styled(after, base),
    ]
}

fn hint(ed: &MetaEdit) -> &'static str {
    if ed.mode == EditMode::Edit || ed.new_shelf.is_some() {
        "type to edit   ←→ move   ^U clear   ⏎/Esc done"
    } else if ed.has_invalid() {
        "fix the red field   Tab tab   j/k move   ^S save   Esc cancel"
    } else {
        match ed.tab {
            EditTab::Details => "Tab tab · j/k move · ⏎ edit · r/R reset · ^S save · Esc cancel",
            EditTab::Collections => "Tab tab · j/k move · ⏎ toggle/new · ^S save · Esc cancel",
            EditTab::Online => "Tab tab · j/k move · ⏎ edit/search/apply · ^S save · Esc cancel",
        }
    }
}

/// A centered rect scaled to the terminal (≈72% × 70%, clamped) so the editor
/// grows on big screens and stays usable on small ones.
fn scaled(area: Rect) -> Rect {
    let w = (area.width * 72 / 100).clamp(40, 96).min(area.width.saturating_sub(2).max(1));
    let h = (area.height * 70 / 100).clamp(14, 34).min(area.height.saturating_sub(2).max(1));
    super::centered(area, w, h)
}

fn truncate(s: &str, max: usize) -> String {
    super::truncate(s, max)
}
