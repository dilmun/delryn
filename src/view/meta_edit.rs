//! Tabbed metadata editor (`e`): Details · Cover · Collections · Online · File.
//! A scalable, form-style popup with navigate/edit two-mode fields, an Open
//! Library lookup, cover search, and template-driven file renaming. See
//! `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::{
    App, EditMode, EditTab, FILE_NAME, FILE_RENAME_ROW, FILE_TEMPLATE, META_FIELDS, MetaEdit,
    ONLINE_AUTHOR, ONLINE_RESULTS_START, ONLINE_SEARCH_ROW, ONLINE_TITLE,
};
use crate::theme::Theme;

/// Left column width for field labels.
const LABEL_W: usize = 14;

pub fn render(f: &mut Frame, app: &App) {
    let Some(ed) = &app.meta_edit else {
        return;
    };
    let theme = app.config.theme;
    let bg = theme.bg.unwrap_or(Color::Black);
    let area = scaled(f.area());

    f.render_widget(Clear, area);
    let title = format!(
        " Edit · {} ",
        super::truncate(&ed.book_title, area.width.saturating_sub(12) as usize)
    );
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
        Constraint::Length(1), // rule
        Constraint::Min(0),    // body
        Constraint::Length(1), // status
        Constraint::Length(1), // hint
    ])
    .split(inner);

    render_tabs(f, rows[0], ed, theme, bg);
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        )),
        rows[1],
    );

    let body = rows[2];
    match ed.tab {
        EditTab::Details => render_details(f, body, ed, theme, bg),
        EditTab::Cover => render_cover(f, body, ed, theme, bg),
        EditTab::Collections => render_collections(f, body, ed, theme),
        EditTab::Online => render_online(f, body, ed, theme, bg),
        EditTab::File => render_file(f, body, ed, theme, bg),
    }

    let status = ed.status.as_deref().unwrap_or("");
    f.render_widget(
        Paragraph::new(Line::styled(
            format!("  {status}"),
            Style::default().fg(theme.heading).add_modifier(Modifier::ITALIC),
        )),
        rows[3],
    );
    f.render_widget(
        Paragraph::new(Line::styled(hint(ed), Style::default().fg(theme.muted))),
        rows[4],
    );
}

fn render_tabs(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color) {
    let mut spans = Vec::new();
    for (i, t) in EditTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
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
    let value_w = (area.width as usize).saturating_sub(LABEL_W + 4).max(8);
    let mut lines: Vec<Line> = Vec::new();
    for (i, label) in META_FIELDS.iter().enumerate() {
        let focused = i == ed.row;
        let editing = focused && ed.mode == EditMode::Edit;
        let value = ed.values.get(i).map(String::as_str).unwrap_or("");
        lines.push(form_field(
            label,
            value,
            focused,
            editing,
            ed.cursor,
            ed.field_invalid(i),
            ed.changed(i),
            value_w,
            theme,
            bg,
        ));
        lines.push(Line::raw(""));
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
        lines.push(Line::from(Span::styled(format!("{marker}＋ New collection…"), style)));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_online(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color) {
    render_search(f, area, ed, theme, bg, false);
}

fn render_cover(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color) {
    render_search(f, area, ed, theme, bg, true);
}

/// Shared Online/Cover layout: two query fields, a Search action, and results.
/// `cover` switches the result line + the action's meaning to covers.
fn render_search(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color, cover: bool) {
    let rows = Layout::vertical([
        Constraint::Length(2), // query fields
        Constraint::Length(1), // search action
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // results
    ])
    .split(area);
    let value_w = (area.width as usize).saturating_sub(LABEL_W + 4).max(8);

    let q = vec![
        query_line("Title", &ed.q_title, ed.online_row == ONLINE_TITLE, ed, value_w, theme, bg),
        query_line("Author", &ed.q_author, ed.online_row == ONLINE_AUTHOR, ed, value_w, theme, bg),
    ];
    f.render_widget(Paragraph::new(q), rows[0]);

    let search_sel = ed.online_row == ONLINE_SEARCH_ROW;
    let style = if search_sel {
        Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.accent)
    };
    let label = if ed.fetching { " Searching… " } else { " ⌕ Search " };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!("  {label}"), style))),
        rows[1],
    );

    let cap = rows[3].height as usize;
    let mut lines: Vec<Line> = Vec::new();
    if ed.results.is_empty() {
        let msg = if cover {
            "  Search, then ⏎ on a result to use its cover."
        } else {
            "  Search, then ⏎ on a result to apply its metadata."
        };
        lines.push(Line::styled(msg, Style::default().fg(theme.muted)));
    }
    for (i, c) in ed.results.iter().enumerate().take(cap) {
        let selected = ed.online_row == ONLINE_RESULTS_START + i;
        let marker = if selected { "▸ " } else { "  " };
        let tail = if cover {
            if c.cover_url().is_some() { "  ✓ cover" } else { "  ✗ no cover" }.to_string()
        } else {
            let series = match (&c.series, c.series_index) {
                (Some(s), Some(n)) => format!("  · {s} #{n}"),
                (Some(s), None) => format!("  · {s}"),
                _ => String::new(),
            };
            format!("{}{series}", c.year.map(|y| format!(" ({y})")).unwrap_or_default())
        };
        let text = format!("{} — {}{tail}", c.title, c.author_line());
        let style = if selected {
            Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}", super::truncate(&text, area.width.saturating_sub(3) as usize)),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), rows[3]);
}

fn render_file(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, bg: Color) {
    let rows = Layout::vertical([
        Constraint::Length(2), // template
        Constraint::Length(2), // resulting name
        Constraint::Length(1), // rename action
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // legend / current
    ])
    .split(area);
    let value_w = (area.width as usize).saturating_sub(LABEL_W + 4).max(8);

    let tmpl = ed.file_row == FILE_TEMPLATE;
    f.render_widget(
        Paragraph::new(form_field(
            "Template",
            &ed.rename_template,
            tmpl,
            tmpl && ed.mode == EditMode::Edit,
            ed.cursor,
            false,
            false,
            value_w,
            theme,
            bg,
        )),
        rows[0],
    );
    let namef = ed.file_row == FILE_NAME;
    f.render_widget(
        Paragraph::new(form_field(
            "New name",
            &ed.rename_name,
            namef,
            namef && ed.mode == EditMode::Edit,
            ed.cursor,
            false,
            false,
            value_w,
            theme,
            bg,
        )),
        rows[1],
    );

    let act_sel = ed.file_row == FILE_RENAME_ROW;
    let style = if act_sel {
        Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.accent)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled("  ⤳ Rename file ", style))),
        rows[2],
    );

    let current = std::path::Path::new(&ed.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let legend = vec![
        Line::from(vec![
            Span::styled("current: ", Style::default().fg(theme.muted)),
            Span::styled(current.to_string(), Style::default().fg(theme.fg)),
        ]),
        Line::raw(""),
        Line::styled(
            "%T title  %A author  %Y year  %S series  %I #  %P publisher  %E ext",
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        ),
    ];
    f.render_widget(Paragraph::new(legend).wrap(Wrap { trim: true }), rows[4]);
}

/// A labelled form field: `▸ Label   value________` with the focused field
/// underlined and a block cursor while editing; red when invalid, • when changed.
#[allow(clippy::too_many_arguments)]
fn form_field(
    label: &str,
    value: &str,
    focused: bool,
    editing: bool,
    cursor: usize,
    invalid: bool,
    changed: bool,
    value_w: usize,
    theme: Theme,
    bg: Color,
) -> Line<'static> {
    let marker = if editing {
        "✎ "
    } else if focused {
        "▸ "
    } else {
        "  "
    };
    let dot = if changed { "•" } else { " " };
    let label_style = if invalid {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if focused {
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.fg)
    };
    let pad = LABEL_W.saturating_sub(label.chars().count() + 3);
    let mut spans = vec![
        Span::styled(format!("{marker}{dot} {label}"), label_style),
        Span::raw(" ".repeat(pad)),
    ];
    spans.extend(input_spans(value, focused, editing, cursor, value_w, invalid, theme, bg));
    Line::from(spans)
}

fn query_line(
    label: &str,
    value: &str,
    focused: bool,
    ed: &MetaEdit,
    value_w: usize,
    theme: Theme,
    bg: Color,
) -> Line<'static> {
    form_field(
        label,
        value,
        focused,
        focused && ed.mode == EditMode::Edit,
        ed.cursor,
        false,
        false,
        value_w,
        theme,
        bg,
    )
}

/// The value portion of a field: an underlined "input line" when focused, with a
/// block cursor while editing.
#[allow(clippy::too_many_arguments)]
fn input_spans(
    value: &str,
    focused: bool,
    editing: bool,
    cursor: usize,
    width: usize,
    invalid: bool,
    theme: Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    let fg = if invalid {
        Color::Red
    } else if focused {
        theme.heading
    } else {
        theme.muted
    };
    let mut base = Style::default().fg(fg);
    if focused {
        base = base.add_modifier(Modifier::UNDERLINED);
    }
    if !focused {
        return vec![Span::styled(super::truncate(value, width), base)];
    }
    let chars: Vec<char> = value.chars().collect();
    let cur = cursor.min(chars.len());
    let mut spans = Vec::new();
    if editing {
        let before: String = chars[..cur].iter().collect();
        let at = if cur < chars.len() {
            chars[cur].to_string()
        } else {
            " ".to_string()
        };
        let after: String = if cur < chars.len() {
            chars[cur + 1..].iter().collect()
        } else {
            String::new()
        };
        spans.push(Span::styled(before, base));
        spans.push(Span::styled(at, Style::default().fg(bg).bg(theme.accent)));
        spans.push(Span::styled(after, base));
        let used = chars.len().max(cur + 1);
        if width > used {
            spans.push(Span::styled(" ".repeat(width - used), base));
        }
    } else {
        spans.push(Span::styled(chars.iter().collect::<String>(), base));
        if width > chars.len() {
            spans.push(Span::styled(" ".repeat(width - chars.len()), base));
        }
    }
    spans
}

fn hint(ed: &MetaEdit) -> &'static str {
    if ed.mode == EditMode::Edit || ed.new_shelf.is_some() {
        return "type to edit   ←→ move   ^U clear   ⏎/Esc done";
    }
    match ed.tab {
        EditTab::Details => "Tab tab · j/k move · ⏎ edit · r/R reset · ^S save · Esc cancel",
        EditTab::Cover => "Tab tab · j/k move · ⏎ search/use cover · ^S save · Esc cancel",
        EditTab::Collections => "Tab tab · j/k move · ⏎ toggle/new · ^S save · Esc cancel",
        EditTab::Online => "Tab tab · j/k move · ⏎ search/apply · ^S save · Esc cancel",
        EditTab::File => "Tab tab · j/k move · ⏎ edit/rename · ^S save · Esc cancel",
    }
}

/// A centered rect scaled to the terminal (≈72% × 70%, clamped).
fn scaled(area: Rect) -> Rect {
    let w = (area.width * 72 / 100).clamp(40, 96).min(area.width.saturating_sub(2).max(1));
    let h = (area.height * 70 / 100).clamp(14, 34).min(area.height.saturating_sub(2).max(1));
    super::centered(area, w, h)
}
