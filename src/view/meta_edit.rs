//! Tabbed metadata editor (`e`): Details · Cover · Lookup. A scalable, form-style
//! popup with navigate/edit two-mode fields (which scroll horizontally to keep
//! the caret visible), an Open Library metadata lookup, and a cover search. Every
//! transient line (search progress, results, hints) is collapsed into one footer
//! row. See `DESIGN.md` §5.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use ratatui_image::{Resize, StatefulImage};

use crate::app::{App, EditMode, EditTab, META_FIELDS, MetaEdit};
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
            " ✎ Edit · {} ",
            super::truncate(&ed.book_title, area.width.saturating_sub(14) as usize)
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
        Constraint::Length(1), // status (transient feedback)
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
        EditTab::Online => render_online(f, body, app.meta_edit.as_ref().unwrap(), theme),
        EditTab::Cover => render_cover(f, body, app, theme),
    }

    // One transient line at the foot: search progress / results / errors, else a
    // quiet hint for the lookup tabs. The shortcut legend lives in the app status
    // bar (see view::status), not here.
    let footer = footer_line(app.meta_edit.as_ref().unwrap(), theme);
    f.render_widget(Paragraph::new(footer), rows[3]);

    record_hits(app, rows[0], body);
}

/// Capture the editor's clickable regions for mouse hit-testing, mirroring the
/// tab strip / body layouts above (kept here so the geometry stays in one file).
fn record_hits(app: &mut App, tab_strip: Rect, body: Rect) {
    let (tab, results_len) = match app.meta_edit.as_ref() {
        Some(e) => (e.tab, e.search().results.len()),
        None => return,
    };

    // Tab strip: " N label " cells separated by a single space (see render_tabs).
    let mut tabs = Vec::new();
    let mut tx = tab_strip.x;
    for (i, t) in EditTab::ALL.iter().enumerate() {
        if i > 0 {
            tx += 1;
        }
        let w = t.label().chars().count() as u16 + 4; // " N label "
        tabs.push((*t, Rect { x: tx, y: tab_strip.y, width: w, height: 1 }));
        tx += w;
    }
    app.mouse.edit_tabs = tabs;

    let row = |i: u16| Rect { x: body.x, y: body.y + i, width: body.width, height: 1 };
    let in_body = |y: u16| y < body.y + body.height;
    let value_start = body.x + 3 + LABEL_W as u16; // marker (3) + label column
    let mut fields = Vec::new();
    let mut results = Vec::new();
    let mut search = None;
    match tab {
        EditTab::Details => {
            // Mirror render_details: a section header (+ a gap before later
            // groups) precedes each group's fields, shifting their rows down.
            let mut line = 0u16;
            for (gi, (_, group)) in DETAILS_GROUPS.iter().enumerate() {
                if gi > 0 {
                    line += 1; // blank between groups
                }
                line += 1; // section header
                for &fi in *group {
                    if in_body(body.y + line) {
                        fields.push((fi, value_start, row(line)));
                    }
                    line += 1;
                }
            }
        }
        EditTab::Online | EditTab::Cover => {
            search = Some(row(0)); // search bar occupies the first body row
            let rw = if tab == EditTab::Cover {
                body.width.saturating_sub(38) // results sit left of the preview
            } else {
                body.width
            };
            for i in 0..results_len as u16 {
                let y = body.y + 2 + i; // one search row + one blank
                if !in_body(y) {
                    break;
                }
                results.push((i as usize, Rect { x: body.x, y, width: rw, height: 1 }));
            }
        }
    }
    app.mouse.edit_fields = fields;
    app.mouse.edit_results = results;
    app.mouse.edit_search = search;
}

/// The single foot-of-popup line: a transient status message (search progress,
/// result counts, errors) when present, otherwise a quiet search hint on the
/// lookup tabs. This is where every "searching…" / help string now lives.
fn footer_line(ed: &MetaEdit, theme: Theme) -> Line<'static> {
    if let Some(status) = &ed.status {
        return Line::styled(
            format!("  {status}"),
            Style::default().fg(theme.heading).add_modifier(Modifier::ITALIC),
        );
    }
    let hint = match ed.tab {
        EditTab::Online => "  type or / to search by title",
        EditTab::Cover => "  type or / to search for a cover",
        EditTab::Details => "",
    };
    Line::styled(hint, Style::default().fg(theme.muted).add_modifier(Modifier::DIM))
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
                Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(format!(" {num} "), Style::default().fg(theme.accent)));
            spans.push(Span::styled(format!("{} ", t.label()), Style::default().fg(theme.muted)));
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
            Style::default().fg(theme.muted).add_modifier(Modifier::BOLD | Modifier::DIM),
        ));
        for &i in *fields {
            let focused = i == ed.row;
            let editing = focused && ed.mode == EditMode::Edit;
            let value = ed.values.get(i).map(String::as_str).unwrap_or("");
            lines.push(form_field(
                META_FIELDS[i],
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
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// A flat search row: ` search   <query/cursor>`, distinguished by a shaded
/// label rather than a box. A block cursor shows while editing; the value scrolls
/// horizontally so the caret stays visible. Progress/results live in the popup
/// footer, not here.
fn search_bar(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let s = ed.search();
    let focused = s.editing;
    let lab = if focused { theme.accent } else { theme.muted };
    let w = area.width.saturating_sub(10) as usize;
    let mut spans = vec![Span::styled(
        " search   ",
        Style::default().fg(lab).add_modifier(Modifier::BOLD),
    )];
    if focused {
        spans.extend(super::field_spans(&s.q, ed.cursor, w, theme));
    } else if s.q.is_empty() {
        spans.push(Span::styled("type to search…", Style::default().fg(theme.muted)));
    } else {
        spans.push(Span::styled(super::truncate(&s.q, w), Style::default().fg(theme.fg)));
    }
    let line = Rect { height: 1, ..area };
    f.render_widget(Paragraph::new(Line::from(spans)), line);
}

/// Results as a list; `cover` shows a cover availability mark, else year/series.
/// Online tab: the metadata-candidate list (title — author, year · series).
fn results_list(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let bg = theme.bg.unwrap_or(Color::Black);
    let mut lines: Vec<Line> = Vec::new();
    let search = ed.search();
    for (i, c) in search.results.iter().enumerate().take(area.height as usize) {
        let selected = i == search.row && !search.editing;
        let marker = if selected { "▸ " } else { "  " };
        let series = match (&c.series, c.series_index) {
            (Some(s), Some(n)) => format!("  · {s} #{n}"),
            (Some(s), None) => format!("  · {s}"),
            _ => String::new(),
        };
        let tail = format!("{}{series}", c.year.map(|y| format!(" ({y})")).unwrap_or_default());
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

/// Cover tab: the source-labelled cover-candidate list (Google Books, Open
/// Library, etc.). The highlighted row drives the live preview.
fn cover_list(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let bg = theme.bg.unwrap_or(Color::Black);
    let s = &ed.cover_search;
    let mut lines: Vec<Line> = Vec::new();
    for (i, h) in ed.cover_hits.iter().enumerate().take(area.height as usize) {
        let selected = i == s.row && !s.editing;
        let marker = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::default().fg(bg).bg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}", super::truncate(&h.source, area.width.saturating_sub(3) as usize)),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Online tab: search bar + a results list; Enter applies the metadata.
fn render_online(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);
    search_bar(f, rows[0], ed, theme);
    results_list(f, rows[1], ed, theme);
}

/// Cover tab: search bar on top, results list on the left, and a wide preview of
/// the highlighted result's cover on the right. Takes `&mut App` to render the
/// preview image protocol.
fn render_cover(f: &mut Frame, area: Rect, app: &mut App, theme: Theme) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);
    // Wider preview column (the list keeps the rest) so the cover renders large.
    let cols = Layout::horizontal([Constraint::Min(20), Constraint::Length(38)]).split(rows[1]);
    {
        let ed = app.meta_edit.as_ref().unwrap();
        search_bar(f, rows[0], ed, theme);
        cover_list(f, cols[0], ed, theme);
    }
    let pane = cols[1];
    let font = super::image_font(app);
    let border = Style::default().fg(theme.muted);
    if let Some(cover) = app.edit_cover.as_mut() {
        // Fit the cover into the pane (less a 1-cell border), then draw a rounded
        // box that hugs exactly that image — no letterbox, no empty slack.
        let inner_max = Rect {
            x: pane.x + 1,
            y: pane.y + 1,
            width: pane.width.saturating_sub(2),
            height: pane.height.saturating_sub(2),
        };
        let img = super::cover_image_rect(inner_max, font, cover.dims);
        let frame = Rect {
            x: img.x.saturating_sub(1),
            y: img.y.saturating_sub(1),
            width: img.width + 2,
            height: img.height + 2,
        };
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border)
                .style(base(theme)),
            frame,
        );
        f.render_stateful_widget(
            StatefulImage::default().resize(Resize::Scale(None)),
            img,
            &mut cover.proto,
        );
    } else {
        // No cover yet: a rounded placeholder box with a status line.
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(Span::styled("Preview", Style::default().fg(theme.muted)))
            .style(base(theme));
        let pinner = block.inner(pane);
        f.render_widget(block, pane);
        let msg = if app.preview_pending() {
            "\n  loading…"
        } else {
            "\n  no cover"
        };
        f.render_widget(Paragraph::new(msg).style(Style::default().fg(theme.muted)), pinner);
    }
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
        spans.extend(super::field_spans(value, cursor, value_w, theme));
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

/// A centered rect scaled to the terminal (≈72% × 70%, clamped).
fn scaled(area: Rect) -> Rect {
    let w = (area.width * 72 / 100).clamp(40, 96).min(area.width.saturating_sub(2).max(1));
    let h = (area.height * 70 / 100).clamp(14, 34).min(area.height.saturating_sub(2).max(1));
    super::centered(area, w, h)
}
