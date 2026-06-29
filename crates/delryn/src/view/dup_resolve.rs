//! Duplicate-resolution overlay: groups of duplicate copies, each row a checkbox
//! (checked = will be deleted). The smart auto-select pre-checks the worse copies;
//! the reader toggles manually, then deletes all checked at once.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::app::App;
use crate::view::library::fmt_size;

pub fn render(f: &mut Frame, app: &App) {
    let Some(dr) = &app.dup_resolve else {
        return;
    };
    let theme = app.config.theme;
    let area = if dr.fullscreen {
        f.area()
    } else {
        super::centered(f.area(), 78, 26)
    };
    f.render_widget(Clear, area);

    let to_delete = dr.checked_count();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(
                " Resolve Duplicates — {} group{} · {to_delete} to delete ",
                dr.groups.len(),
                if dr.groups.len() == 1 { "" } else { "s" },
            ),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                " ↑↓ · space · a auto · u none · n keep · o prefs · f screen · d delete · Esc ",
                Style::default().fg(theme.muted),
            ))
            .alignment(Alignment::Center),
        )
        .style(Style::default().fg(theme.fg).bg(theme.paper()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    f.render_widget(
        Paragraph::new(Line::styled(
            "Checked copies are deleted; one is kept per group. ✓ = keep, ✗ = delete.",
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        )),
        chunks[0],
    );

    // Build display lines (group headers + member rows), tracking the cursor line.
    // Leave a column for the scrollbar so the path tail isn't clipped.
    let body_width = (chunks[1].width as usize).saturating_sub(1);
    let rows = dr.rows();
    let cursor_row = rows.get(dr.cursor).copied();
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    for (gi, g) in dr.groups.iter().enumerate() {
        if gi > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            format!("{}  ({} copies)", g.label, g.members.len()),
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ));
        for (mi, m) in g.members.iter().enumerate() {
            if cursor_row == Some((gi, mi)) {
                sel_line = lines.len();
            }
            lines.push(member_line(
                m,
                cursor_row == Some((gi, mi)),
                theme,
                body_width,
            ));
        }
    }

    let body = chunks[1];
    let h = body.height as usize;
    let total = lines.len();
    let offset = sel_line.saturating_sub(h / 2).min(total.saturating_sub(h));
    let visible: Vec<Line> = lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(visible), body);

    if total > h {
        let mut sb = ScrollbarState::new(total).position(offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(theme.accent)),
            body,
            &mut sb,
        );
    }
}

/// One copy row: cursor marker, keep/delete checkbox, distinguishing attributes,
/// and the file's path (directory kept, an over-long filename trimmed to fit
/// `width`; the whole path shows when the overlay is full-screen).
fn member_line(
    m: &crate::app::DupMember,
    focused: bool,
    theme: crate::theme::Theme,
    width: usize,
) -> Line<'static> {
    let marker = if focused { "▸ " } else { "  " };
    let (box_txt, box_color) = if m.checked {
        ("[✗] delete", theme.marker)
    } else {
        ("[✓] keep  ", theme.accent)
    };

    // Distinguishing attributes: format · size · converted · read%.
    let mut attrs = format!("{} · {}", m.format, fmt_size(m.size));
    if m.converted {
        attrs.push_str(" · converted");
    }
    if m.rating > 0 {
        attrs.push_str(&format!(" · ★{}", m.rating));
    }
    if m.favorite {
        attrs.push_str(" · ♥");
    }
    if m.pct > 0 {
        attrs.push_str(&format!(" · {}%", m.pct));
    }

    // The path takes whatever width is left after the marker, box, and attributes.
    let prefix = marker.chars().count() + box_txt.chars().count() + 2 + attrs.chars().count() + 3;
    let budget = width.saturating_sub(prefix).max(12);
    let location = elide_path(&m.path, budget);

    let name_style = if m.checked {
        Style::default().fg(theme.muted)
    } else {
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
    };
    Line::from(vec![
        Span::styled(
            format!("{marker}{box_txt}  "),
            Style::default().fg(box_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{attrs}   "), Style::default().fg(theme.muted)),
        Span::styled(location, name_style),
    ])
}

/// Fit a file path into `max` columns, keeping the **directory** visible and
/// trimming an over-long **filename** on the right (`a/b/c/longfilename…`) — the
/// location is what tells copies apart, and titles baked into filenames are the
/// usual offenders. If the directory alone won't fit, fall back to left-eliding
/// the whole path so at least the filename shows.
fn elide_path(path: &str, max: usize) -> String {
    let total = path.chars().count();
    if total <= max {
        return path.to_string();
    }
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let dir_len = dir.chars().count();
    // Keep the whole directory when it leaves room for a few filename chars + "…".
    if dir_len + 8 <= max {
        let head_budget = max - dir_len - 1; // reserve a column for the ellipsis
        let head: String = file.chars().take(head_budget).collect();
        format!("{dir}{head}…")
    } else {
        // Directory too long to show whole — keep the path tail (incl. filename).
        let tail: String = path.chars().skip(total - max.saturating_sub(1)).collect();
        format!("…{tail}")
    }
}
