//! Bulk-rename popup: one filename template applied to every marked book, with
//! a live `old → new` preview per book. Shortcuts live in the bottom status bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, Overlay, fill_template};
use crate::theme::Role;

pub fn render(f: &mut Frame, app: &App) {
    let Overlay::BulkRename(br) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let bold = app.config.bold_borders;
    let bg = theme.paper();
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let n = br.targets.len();
    let books = if n == 1 { "book" } else { "books" };
    let title = format!(" Rename · {n} {books} ");
    let block = super::overlay_frame(theme, bold)
        .title(Span::styled(title, theme.style(Role::Title)))
        .style(theme.style(Role::Body).bg(bg));
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
        theme.style(Role::AccentStrong),
    )];
    let w = rows[0].width.saturating_sub(12) as usize; // " template   " = 12 cells
    spans.extend(super::field_spans(
        br.input.text(),
        br.input.cursor(),
        w,
        theme,
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), rows[0]);

    f.render_widget(
        Paragraph::new(Line::styled(
            "   %T title   %A author   %Y year   %S series   %I #   %P publisher   %E ext",
            theme.style(Role::Hint),
        )),
        rows[1],
    );
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(inner.width as usize),
            theme.style(Role::Hint),
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
        let new = fill_template(br.input.text(), &t.values, &t.ext);
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {}", super::truncate(&t.old_name, col)),
                theme.style(Role::Muted),
            ),
            Span::styled("  →  ", theme.style(Role::Accent)),
            Span::styled(super::truncate(&new, col), theme.style(Role::Body)),
        ]));
    }
    let shown = lines.len();
    if br.targets.len() > shown {
        lines.push(Line::styled(
            format!("   … and {} more", br.targets.len() - shown),
            theme.style(Role::Hint),
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}
