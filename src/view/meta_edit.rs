//! Tabbed metadata editor (`e`): Details · Cover · Collections · Online · File.
//! A scalable, form-style popup with navigate/edit two-mode fields, an Open
//! Library lookup, cover search, and template-driven file renaming. See
//! `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use ratatui_image::{Resize, StatefulImage};

use crate::app::{
    App, DEFAULT_RENAME_TEMPLATE, EditMode, EditTab, FILE_NAME, FILE_TEMPLATE, META_FIELDS, MetaEdit,
};
use crate::theme::Theme;

/// Left column width for field labels.
const LABEL_W: usize = 14;

/// Base fg/bg style for the popup background.
fn base(theme: Theme) -> Style {
    let s = Style::default().fg(theme.fg);
    match theme.bg {
        Some(bg) => s.bg(bg),
        None => s,
    }
}

pub fn render(f: &mut Frame, app: &mut App) {
    if app.meta_edit.is_none() {
        return;
    }
    let theme = app.config.theme;
    let bg = theme.bg.unwrap_or(Color::Black);
    let area = scaled(f.area());

    f.render_widget(Clear, area);
    let title = {
        let ed = app.meta_edit.as_ref().unwrap();
        format!(
            " Edit · {} ",
            super::truncate(&ed.book_title, area.width.saturating_sub(12) as usize)
        )
    };
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

    render_tabs(f, rows[0], app.meta_edit.as_ref().unwrap(), theme, bg);
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
    match app.meta_edit.as_ref().unwrap().tab {
        EditTab::Details => render_details(f, body, app.meta_edit.as_ref().unwrap(), theme),
        EditTab::Collections => {
            render_collections(f, body, app.meta_edit.as_ref().unwrap(), theme)
        }
        EditTab::Online => render_online(f, body, app.meta_edit.as_ref().unwrap(), theme),
        EditTab::File => render_file(f, body, app.meta_edit.as_ref().unwrap(), theme),
        EditTab::Cover => render_cover(f, body, app, theme),
    }

    let (status, hint_text) = {
        let ed = app.meta_edit.as_ref().unwrap();
        (ed.status.clone().unwrap_or_default(), hint(ed))
    };
    f.render_widget(
        Paragraph::new(Line::styled(
            format!("  {status}"),
            Style::default().fg(theme.heading).add_modifier(Modifier::ITALIC),
        )),
        rows[3],
    );
    f.render_widget(
        Paragraph::new(Line::styled(hint_text, Style::default().fg(theme.muted))),
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

fn render_details(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    // marker (3) + label (LABEL_W) + value + the " ↵" hint (3).
    let value_w = (area.width as usize).saturating_sub(LABEL_W + 6).max(8);
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
        ));
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

/// A flat search row: ` search   <query/cursor>`, distinguished by a shaded
/// label rather than a box. A block cursor shows while editing.
fn search_bar(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let s = ed.search();
    let focused = s.editing;
    let lab = if focused { theme.accent } else { theme.muted };
    let mut spans = vec![Span::styled(
        " search   ",
        Style::default().fg(lab).add_modifier(Modifier::BOLD),
    )];
    if focused {
        let w = area.width.saturating_sub(10) as usize;
        spans.extend(field_cursor_spans(&s.q, ed.cursor, w, theme));
    } else if s.q.is_empty() {
        spans.push(Span::styled("type to search…", Style::default().fg(theme.muted)));
    } else {
        spans.push(Span::styled(s.q.clone(), Style::default().fg(theme.fg)));
    }
    if !focused && s.fetching {
        spans.push(Span::styled("   · searching…", Style::default().fg(theme.muted)));
    }
    let line = Rect { height: 1, ..area };
    f.render_widget(Paragraph::new(Line::from(spans)), line);
}

/// Results as a list; `cover` shows a cover availability mark, else year/series.
fn results_list(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme, cover: bool) {
    let bg = theme.bg.unwrap_or(Color::Black);
    let mut lines: Vec<Line> = Vec::new();
    let search = ed.search();
    if search.results.is_empty() {
        let msg = if search.fetching {
            "  searching…"
        } else {
            "  Press / (or just type) to search."
        };
        lines.push(Line::styled(msg, Style::default().fg(theme.muted)));
    }
    for (i, c) in search.results.iter().enumerate().take(area.height as usize) {
        let selected = i == search.row && !search.editing;
        let marker = if selected { "▸ " } else { "  " };
        let tail = if cover {
            if c.cover_url().is_some() { "  ✓" } else { "  ✗" }.to_string()
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
    f.render_widget(Paragraph::new(lines), area);
}

/// Online tab: search bar + a results list; Enter applies the metadata.
fn render_online(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);
    search_bar(f, rows[0], ed, theme);
    results_list(f, rows[1], ed, theme, false);
}

/// Cover tab: search bar on top, results list on the left, and a big preview of
/// the highlighted result's cover on the right. Takes `&mut App` to render the
/// preview image protocol.
fn render_cover(f: &mut Frame, area: Rect, app: &mut App, theme: Theme) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);
    let cols = Layout::horizontal([Constraint::Min(20), Constraint::Length(24)]).split(rows[1]);
    {
        let ed = app.meta_edit.as_ref().unwrap();
        search_bar(f, rows[0], ed, theme);
        results_list(f, cols[0], ed, theme, true);
    }
    // Preview pane (mutable: the image protocol updates on render).
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.muted))
        .title(Span::styled("Preview", Style::default().fg(theme.muted)))
        .style(base(theme));
    let pinner = block.inner(cols[1]);
    f.render_widget(block, cols[1]);
    let font = super::image_font(app);
    if let Some(cover) = app.edit_cover.as_mut() {
        let rect = super::cover_image_rect(pinner, font, cover.dims);
        f.render_stateful_widget(StatefulImage::default().resize(Resize::Scale(None)), rect, &mut cover.proto);
    } else {
        let msg = if app.preview_pending() {
            "\n  loading…"
        } else {
            "\n  no cover"
        };
        f.render_widget(
            Paragraph::new(msg).style(Style::default().fg(theme.muted)),
            pinner,
        );
    }
}

fn render_file(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let rows = Layout::vertical([
        Constraint::Length(1), // template
        Constraint::Length(1), // resulting name
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // current + placeholder legend
    ])
    .split(area);
    let value_w = (area.width as usize).saturating_sub(LABEL_W + 6).max(8);

    let tmpl = ed.file_row == FILE_TEMPLATE;
    f.render_widget(
        Paragraph::new(form_field(
            "Template",
            &ed.rename_template,
            tmpl,
            tmpl && ed.mode == EditMode::Edit,
            ed.cursor,
            false,
            ed.rename_template != DEFAULT_RENAME_TEMPLATE,
            value_w,
            theme,
        )),
        rows[0],
    );
    let namef = ed.file_row == FILE_NAME;
    let current = std::path::Path::new(&ed.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    f.render_widget(
        Paragraph::new(form_field(
            "New name",
            &ed.rename_name,
            namef,
            namef && ed.mode == EditMode::Edit,
            ed.cursor,
            false,
            ed.rename_name != current,
            value_w,
            theme,
        )),
        rows[1],
    );

    let info = vec![
        Line::from(vec![
            Span::styled(format!(" {:<pad$}", "current", pad = LABEL_W + 2), Style::default().fg(theme.muted)),
            Span::styled(current.to_string(), Style::default().fg(theme.fg)),
        ]),
        Line::raw(""),
        Line::styled(
            "   %T title   %A author   %Y year   %S series   %I #   %P publisher   %E ext",
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        ),
    ];
    f.render_widget(Paragraph::new(info).wrap(Wrap { trim: true }), rows[3]);
}

/// A value with a block cursor at `caret`, windowed to `valw` cells so the caret
/// stays visible (a leading … marks scrolled-off text). No background fill — the
/// field is distinguished by label shading, not a recessed box.
fn field_cursor_spans(val: &str, caret: usize, valw: usize, theme: Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = val.chars().collect();
    let len = chars.len();
    let caret = caret.min(len);
    let win = valw.max(2);
    let start = (caret + 1).saturating_sub(win);
    let text = Style::default().fg(theme.heading).add_modifier(Modifier::BOLD);
    let cursor = Style::default()
        .fg(theme.bg.unwrap_or(Color::Black))
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if start > 0 {
        spans.push(Span::styled("…", Style::default().fg(theme.muted)));
    }
    let end = (start + win).min(len);
    for (idx, ch) in chars.iter().enumerate().take(end).skip(start) {
        let st = if idx == caret { cursor } else { text };
        spans.push(Span::styled(ch.to_string(), st));
    }
    if caret >= len {
        spans.push(Span::styled(" ".to_string(), cursor)); // caret past the last char
    }
    spans
}

/// A labelled form row: ` ▸ Label        value ↵`. The field is distinguished by
/// shading the **label** (bold; accent when focused, dim otherwise), not by a
/// background box. A leading marker flags focus (▸) or an unsaved change (•);
/// invalid values turn red; a block cursor shows while editing.
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
) -> Line<'static> {
    let (marker, marker_col) = if focused {
        ("▸", theme.accent)
    } else if changed {
        ("•", theme.marker)
    } else {
        (" ", theme.muted)
    };
    let label_style = if invalid {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(if focused { theme.accent } else { theme.muted })
            .add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![
        Span::styled(format!(" {marker} "), Style::default().fg(marker_col)),
        Span::styled(format!("{label:<LABEL_W$}"), label_style),
    ];
    if editing {
        spans.extend(field_cursor_spans(value, cursor, value_w, theme));
    } else {
        let c = if invalid { Color::Red } else { theme.fg };
        let shown = super::truncate(value, value_w);
        if shown.is_empty() {
            spans.push(Span::styled("—", Style::default().fg(theme.muted)));
        } else {
            let st = if focused {
                Style::default().fg(c).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(c)
            };
            spans.push(Span::styled(shown, st));
        }
        if focused {
            spans.push(Span::styled("  ↵", Style::default().fg(theme.accent)));
        }
    }
    Line::from(spans)
}

fn hint(ed: &MetaEdit) -> &'static str {
    if ed.search().editing {
        return "type to search   ←→ move   ^U clear   ⏎ run   Esc done";
    }
    if ed.mode == EditMode::Edit || ed.new_shelf.is_some() {
        return "type to edit   ←→ move   ^U clear   ⏎/Esc done";
    }
    match ed.tab {
        EditTab::Details => "Tab tab · j/k move · ⏎ edit · r/R reset · ^S save · Esc cancel",
        EditTab::Cover => "Tab tab · / search · j/k pick · ⏎ use cover · ^S save · Esc",
        EditTab::Collections => "Tab tab · j/k move · ⏎ toggle/new · ^S save · Esc cancel",
        EditTab::Online => "Tab tab · / search · j/k pick · ⏎ apply · ^S save · Esc",
        EditTab::File => "Tab tab · j/k move · ⏎ edit · ^S rename + save · Esc cancel",
    }
}

/// A centered rect scaled to the terminal (≈72% × 70%, clamped).
fn scaled(area: Rect) -> Rect {
    let w = (area.width * 72 / 100).clamp(40, 96).min(area.width.saturating_sub(2).max(1));
    let h = (area.height * 70 / 100).clamp(14, 34).min(area.height.saturating_sub(2).max(1));
    super::centered(area, w, h)
}
