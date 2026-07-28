//! Duplicate-resolution overlay: groups of duplicate copies in an aligned table
//! (keep/delete · format · size · source · read-flags · path) under a fixed column
//! header. The smart auto-select pre-checks the worse copies; the reader toggles
//! them, previews/reveals a copy, then deletes all checked at once.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::app::{App, Overlay};
use crate::theme::Role;
use crate::view::library::fmt_size;

pub fn render(f: &mut Frame, app: &mut App) {
    let Overlay::DupResolve(dr) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let bold = app.config.bold_borders;
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let to_delete = dr.checked_count();
    let block = super::overlay_frame(theme, bold)
        .title(Span::styled(
            format!(
                " Resolve Duplicates — {} group{} · {to_delete} to delete ",
                dr.groups.len(),
                if dr.groups.len() == 1 { "" } else { "s" },
            ),
            theme.style(Role::Title),
        ))
        .title_bottom(
            Line::from(Span::styled(
                " ↑↓ space · p preview · r open location · d delete · n ignore · I ignored · o prefs · f full · q ",
                theme.style(Role::Muted),
            ))
            .alignment(Alignment::Center),
        )
        .style(theme.style(Role::Body).bg(theme.paper()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner);
    // Fixed column header above the scrolling body.
    f.render_widget(Paragraph::new(column_header(theme)), chunks[0]);

    // Build display lines (group headers + member rows), tracking the cursor line.
    // Leave a column for the scrollbar so the path tail isn't clipped.
    let body_width = (chunks[1].width as usize).saturating_sub(1);
    let rows = dr.rows();
    let cursor_row = rows.get(dr.cursor).copied();
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    // (cursor index into `rows`, line index) for each copy row, for hit-testing.
    let mut member_lines: Vec<(usize, usize)> = Vec::new();
    let mut ci = 0usize;
    for (gi, g) in dr.groups.iter().enumerate() {
        // A blank line separates one duplicate group's rows from the next — no
        // per-group title, so the table stays clean.
        if gi > 0 {
            lines.push(Line::raw(""));
        }
        for (mi, m) in g.members.iter().enumerate() {
            if cursor_row == Some((gi, mi)) {
                sel_line = lines.len();
            }
            member_lines.push((ci, lines.len()));
            ci += 1;
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
    let max_off = total.saturating_sub(h);
    let offset = sel_line.saturating_sub(h / 2).min(max_off);
    let visible: Vec<Line> = lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(visible), body);

    // `content_length` counts scroll positions, not lines — see `view::settings`.
    if total > h {
        let mut sb = ScrollbarState::new(max_off + 1).position(offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(theme.style(Role::Accent)),
            body,
            &mut sb,
        );
    }

    // Screen rect per visible copy row (the scrollbar column is left clickable too).
    let mut hits: Vec<(usize, Rect)> = Vec::with_capacity(member_lines.len());
    for (idx, li) in member_lines {
        if li < offset {
            continue;
        }
        let sy = body.y + (li - offset) as u16;
        if sy >= body.y + body.height {
            continue;
        }
        hits.push((
            idx,
            Rect {
                x: body.x,
                y: sy,
                width: body.width,
                height: 1,
            },
        ));
    }
    app.mouse.overlay_rows = hits;
}

// Fixed column widths (chars) for the member table — shared by the rows and the
// header so they line up. The name column takes the remaining width.
const W_CHECK: usize = 8; // "[✓] keep" / "[✗] del"
const W_FMT: usize = 4; // "EPUB" / "PDF"
const W_SIZE: usize = 7; // right-aligned "131.0M"
const W_SOURCE: usize = 9; // "converted" / "original"

/// The fixed table header drawn above the scrolling rows.
fn column_header(theme: crate::theme::Theme) -> Line<'static> {
    let text = format!(
        "  {:<W_CHECK$} {:<W_FMT$} {:>W_SIZE$}  {:<W_SOURCE$} NAME",
        "KEEP", "FMT", "SIZE", "SOURCE",
    );
    Line::styled(text, theme.style(Role::Hint).add_modifier(Modifier::BOLD))
}

/// One copy row as aligned columns — cursor marker, keep/delete, format, size,
/// source (original/converted), and the file path (directory kept, an over-long
/// filename trimmed to fit; whole path full-screen).
fn member_line(
    m: &crate::app::DupMember,
    focused: bool,
    theme: crate::theme::Theme,
    width: usize,
) -> Line<'static> {
    let marker = if focused { "▸ " } else { "  " };
    let (check, check_color) = if m.checked {
        ("[✗] del", theme.color(Role::Marker))
    } else {
        ("[✓] keep", theme.color(Role::Accent))
    };
    let source = if m.converted { "converted" } else { "original" };

    let check_cell = format!("{check:<W_CHECK$} ");
    let attrs = format!(
        "{:<W_FMT$} {:>W_SIZE$}  {source:<W_SOURCE$} ",
        m.format,
        fmt_size(m.size),
    );

    // The name column takes whatever's left after the marker + fixed columns.
    let prefix = marker.chars().count() + check_cell.chars().count() + attrs.chars().count();

    // Focused → a full-width rounded selection bar (uniform accent fill; the
    // per-column colours stay visible on the unfocused rows). Reserve two extra
    // cells for the rounded caps.
    if focused {
        let budget = width.saturating_sub(prefix + 2).max(12);
        let location = elide_path(&m.path, budget);
        return crate::view::rounded_line(
            format!("{marker}{check_cell}{attrs}{location}"),
            width as u16,
            theme,
        );
    }

    let budget = width.saturating_sub(prefix).max(12);
    let location = elide_path(&m.path, budget);
    let name_style = if m.checked {
        theme.style(Role::Muted)
    } else {
        theme.style(Role::Body).add_modifier(Modifier::BOLD)
    };
    Line::from(vec![
        Span::styled(marker.to_string(), theme.style(Role::Accent)),
        Span::styled(
            check_cell,
            Style::default()
                .fg(check_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(attrs, theme.style(Role::Muted)),
        Span::styled(location, name_style),
    ])
}

/// The ignored-groups manager: one ignored duplicate group per row (its member
/// filenames), with keys to restore one or all.
pub fn render_ignored(f: &mut Frame, app: &mut App) {
    let Overlay::IgnoredView(v) = &app.overlay else {
        return;
    };
    let theme = app.config.theme;
    let bold = app.config.bold_borders;
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let block = super::overlay_frame(theme, bold)
        .title(Span::styled(
            format!(" Ignored Duplicate Groups — {} ", v.signatures.len()),
            theme.style(Role::Title),
        ))
        .title_bottom(
            Line::from(Span::styled(
                " ↑↓ move · u/⏎ restore · C restore all · q ",
                theme.style(Role::Muted),
            ))
            .alignment(Alignment::Center),
        )
        .style(theme.style(Role::Body).bg(theme.paper()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let body_width = inner.width as usize;
    let h = inner.height as usize;
    let lines: Vec<Line> = v
        .groups
        .iter()
        .enumerate()
        .map(|(i, group)| {
            let focused = i == v.cursor;
            let marker = if focused { "▸ " } else { "  " };
            let count = format!("{}× ", group.len());
            let names = group
                .iter()
                .map(|p| basename(p))
                .collect::<Vec<_>>()
                .join("  ·  ");
            let reserve = marker.chars().count() + count.chars().count();
            if focused {
                // Full-width rounded selection bar (reserve two cells for caps).
                let budget = body_width.saturating_sub(reserve + 2).max(12);
                return crate::view::rounded_line(
                    format!("{marker}{count}{}", truncate_end(&names, budget)),
                    body_width as u16,
                    theme,
                );
            }
            let budget = body_width.saturating_sub(reserve).max(12);
            Line::from(vec![
                Span::styled(marker.to_string(), theme.style(Role::Accent)),
                Span::styled(count, theme.style(Role::Muted)),
                Span::styled(truncate_end(&names, budget), theme.style(Role::Muted)),
            ])
        })
        .collect();

    let total = lines.len();
    let offset = v.cursor.saturating_sub(h / 2).min(total.saturating_sub(h));
    let visible: Vec<Line> = lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(visible), inner);
    // Each line is one group, so a line's index is its cursor index.
    let mut hits: Vec<(usize, Rect)> = Vec::new();
    for j in offset..(offset + h).min(total) {
        hits.push((
            j,
            Rect {
                x: inner.x,
                y: inner.y + (j - offset) as u16,
                width: inner.width,
                height: 1,
            },
        ));
    }
    app.mouse.overlay_rows = hits;
}

/// Basename of a path.
fn basename(p: &str) -> String {
    std::path::Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

/// Truncate `s` from the right to at most `max` characters, ending with an ellipsis
/// when shortened.
fn truncate_end(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head: String = chars[..max.saturating_sub(1)].iter().collect();
    format!("{head}…")
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
