//! Bulk-rename popup: one filename template applied to every marked book, with
//! a live `old → new` preview per book. Shortcuts live in the bottom status bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, fill_template};

pub fn render(f: &mut Frame, app: &App) {
    let Some(br) = &app.bulk_rename else {
        return;
    };
    let theme = app.config.theme;
    let bg = theme.paper();
    // ^F expands to (near) full screen for a wider, taller before/after view.
    let area = if br.full {
        let a = f.area();
        super::centered(a, a.width.saturating_sub(4), a.height.saturating_sub(2))
    } else {
        super::centered(f.area(), 84, 26)
    };
    f.render_widget(Clear, area);

    let n = br.targets.len();
    let books = if n == 1 { "book" } else { "books" };
    let title = if br.full {
        format!(" Rename · {n} {books}  (^F exit full screen) ")
    } else {
        format!(" Rename · {n} {books} ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
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
        Constraint::Length(1), // template field
        Constraint::Length(1), // placeholder legend
        Constraint::Length(1), // rule
        Constraint::Min(0),    // preview list
    ])
    .split(inner);

    // Template field — flat, label-shaded, with a block cursor that scrolls
    // horizontally so the caret stays visible for long templates.
    let mut spans = vec![Span::styled(
        " template   ",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
    let w = rows[0].width.saturating_sub(12) as usize; // " template   " = 12 cells
    spans.extend(super::field_spans(&br.template, br.cursor, w, theme));
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    f.render_widget(
        Paragraph::new(Line::styled(
            "   %T title   %A author   %Y year   %S series   %I #   %P publisher   %E ext",
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        )),
        rows[1],
    );
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        )),
        rows[2],
    );

    // Live preview: old → new for each book (clipped to the available rows).
    render_preview(f, rows[3], br, theme);
}

fn render_preview(
    f: &mut Frame,
    area: Rect,
    br: &crate::app::BulkRename,
    theme: crate::theme::Theme,
) {
    let col = (area.width.saturating_sub(7) / 2) as usize;
    let cap = area.height as usize;
    let mut lines: Vec<Line> = Vec::new();
    for t in br.targets.iter().take(cap.saturating_sub(1).max(1)) {
        let new = fill_template(&br.template, &t.values, &t.ext);
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {}", super::truncate(&t.old_name, col)),
                Style::default().fg(theme.muted),
            ),
            Span::styled("  →  ", Style::default().fg(theme.accent)),
            Span::styled(super::truncate(&new, col), Style::default().fg(theme.fg)),
        ]));
    }
    let shown = lines.len();
    if br.targets.len() > shown {
        lines.push(Line::styled(
            format!("   … and {} more", br.targets.len() - shown),
            Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}
