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
    let area = super::centered(f.area(), 78, 26);
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
                " ↑↓ move · space toggle · a auto · u none · n keep group · d delete · Esc ",
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
            lines.push(member_line(m, cursor_row == Some((gi, mi)), theme));
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
/// and the file name.
fn member_line(
    m: &crate::app::DupMember,
    focused: bool,
    theme: crate::theme::Theme,
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
        Span::styled(m.file.clone(), name_style),
    ])
}
