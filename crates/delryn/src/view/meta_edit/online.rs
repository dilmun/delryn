//! Metadata editor: the Lookup (Online) and Cover tab bodies — search bar,
//! results list, cover candidates, and the cover preview.

use super::*;

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
        spans.extend(crate::view::field_spans(&s.q, ed.cursor, w, theme));
    } else if s.q.is_empty() {
        spans.push(Span::styled(
            "type to search…",
            Style::default().fg(theme.muted),
        ));
    } else {
        spans.push(Span::styled(
            crate::view::truncate(&s.q, w),
            Style::default().fg(theme.fg),
        ));
    }
    let line = Rect { height: 1, ..area };
    f.render_widget(Paragraph::new(Line::from(spans)), line);
}

/// The Lookup metadata-candidate list (title — author, year · series). A row is
/// selected only when the keyboard focus has moved past the seed fields into the
/// results (and not while a field is being edited).
fn results_list(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let bg = theme.bg.unwrap_or(Color::Black);
    let mut lines: Vec<Line> = Vec::new();
    let sel = if !ed.lookup.editing && ed.lookup.focus >= LOOKUP_FIELDS {
        Some(ed.lookup.focus - LOOKUP_FIELDS)
    } else {
        None
    };
    for (i, c) in ed
        .online
        .results
        .iter()
        .enumerate()
        .take(area.height as usize)
    {
        let selected = sel == Some(i);
        let marker = if selected { "▸ " } else { "  " };
        let series = match (&c.series, c.series_index) {
            (Some(s), Some(n)) => format!("  · {s} #{n}"),
            (Some(s), None) => format!("  · {s}"),
            _ => String::new(),
        };
        let tail = format!(
            "{}{series}",
            c.year.map(|y| format!(" ({y})")).unwrap_or_default()
        );
        let text = format!("{} — {}{tail}", c.title, c.author_line());
        let style = if selected {
            Style::default()
                .fg(bg)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!(
                "{marker}{}",
                crate::view::truncate(&text, area.width.saturating_sub(3) as usize)
            ),
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
            Style::default()
                .fg(bg)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!(
                "{marker}{}",
                crate::view::truncate(&h.source, area.width.saturating_sub(3) as usize)
            ),
            style,
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Lookup tab: a read-only composed query, the editable Title/Author/Year seed
/// fields it's derived from, then the results list. Enter applies the metadata.
pub(crate) fn render_online(f: &mut Frame, area: Rect, ed: &MetaEdit, theme: Theme) {
    let rows = Layout::vertical([
        Constraint::Length(1),                    // composed query (read-only)
        Constraint::Length(1),                    // gap
        Constraint::Length(LOOKUP_FIELDS as u16), // seed fields
        Constraint::Length(1),                    // rule
        Constraint::Min(0),                       // results
    ])
    .split(area);

    // Read-only composed query — label aligned with the seed fields below.
    let q = ed.lookup.query();
    let query_line = Line::from(vec![
        Span::styled(
            format!("   {:<LABEL_W$}", "query"),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        ),
        if q.is_empty() {
            Span::styled(
                "— fill the fields below",
                Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
            )
        } else {
            Span::styled(
                q,
                Style::default()
                    .fg(theme.heading)
                    .add_modifier(Modifier::BOLD),
            )
        },
    ]);
    f.render_widget(Paragraph::new(query_line), rows[0]);

    // Editable seed fields (reuse the Details form-field renderer).
    const LABELS: [&str; LOOKUP_FIELDS] = ["Title", "Author"];
    let value_w = (rows[2].width as usize).saturating_sub(LABEL_W + 6).max(8);
    let fields: Vec<Line> = (0..LOOKUP_FIELDS)
        .map(|i| {
            let focused = ed.lookup.focus == i;
            let editing = focused && ed.lookup.editing;
            form_field(
                LABELS[i],
                ed.lookup.field(i),
                focused,
                editing,
                ed.lookup.cursor,
                false,
                false,
                value_w,
                theme,
            )
        })
        .collect();
    f.render_widget(Paragraph::new(fields), rows[2]);

    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(rows[3].width as usize),
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        )),
        rows[3],
    );

    results_list(f, rows[4], ed, theme);
}

/// Cover tab: search bar on top, results list on the left, and a wide preview of
/// the highlighted result's cover on the right. Takes `&mut App` to render the
/// preview image protocol.
pub(crate) fn render_cover(f: &mut Frame, area: Rect, app: &mut App, theme: Theme) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);
    // Wider preview column (the list keeps the rest) so the cover renders large.
    let cols = Layout::horizontal([Constraint::Min(20), Constraint::Length(38)]).split(rows[1]);
    {
        let ed = app.meta_edit.as_ref().unwrap();
        search_bar(f, rows[0], ed, theme);
        cover_list(f, cols[0], ed, theme);
    }
    let pane = cols[1];
    let font = crate::view::image_font(app);
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
        let img = crate::view::cover_image_rect(inner_max, font, cover.dims);
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
        f.render_widget(
            Paragraph::new(msg).style(Style::default().fg(theme.muted)),
            pinner,
        );
    }
}
