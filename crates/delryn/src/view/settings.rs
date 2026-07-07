//! Settings popup (`;`), scoped to the current mode — Reading settings in the
//! reader, Library settings in the library — so the two never mix. Options are
//! grouped into tabs (Tab / Shift-Tab to switch); the body scrolls when a tab is
//! taller than the window. Edits the live config. See `DESIGN.md` §7.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::app::{App, Mode, Overlay, SettingRow, settings_tabs, tab_rows};
use crate::config::Config;
use crate::theme::Role;

/// Column where each option's value is shown (label left, value right).
const VALUE_COL: usize = 30;

pub fn render(f: &mut Frame, app: &mut App) {
    let Overlay::Settings(state) = &app.overlay else {
        return;
    };
    let (scope_mode, active_tab, sel_row) = (state.scope, state.tab, state.row);
    let theme = app.config.theme;
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let bg = theme.paper();
    let scope = match scope_mode {
        Mode::Reader => "Reading",
        Mode::Library => "Library",
    };
    let tabs = settings_tabs(scope_mode);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.style(Role::BorderFocus))
        .title(Span::styled(
            format!(" {scope} Settings "),
            theme.style(Role::Title),
        ))
        .title_bottom(
            Line::from(Span::styled(
                " Tab section · ↑↓ move · ←→ change · q close ",
                theme.style(Role::Muted),
            ))
            .alignment(Alignment::Center),
        )
        .style(theme.style(Role::Body).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical([
        Constraint::Length(1), // tab bar
        Constraint::Length(1), // divider rule
        Constraint::Length(1), // spacer
        Constraint::Min(0),    // body
    ])
    .split(inner);

    let tab_hits = render_tab_bar(f, chunks[0], &tabs, active_tab, theme);

    // Divider under the tabs.
    f.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(chunks[1].width as usize),
            theme.style(Role::Hint),
        )),
        chunks[1],
    );

    let row_hits = render_body(
        f,
        chunks[3],
        &app.config,
        scope_mode,
        active_tab,
        sel_row,
        theme,
    );
    app.mouse.overlay_tabs = tab_hits;
    app.mouse.overlay_rows = row_hits;
}

/// The pill-style tab strip, the active tab filled with the accent. Returns each
/// tab's on-screen rect (for mouse hit-testing) — the strip is centre-aligned, so
/// the rects are laid out from the same centred origin ratatui uses.
fn render_tab_bar(
    f: &mut Frame,
    area: Rect,
    tabs: &[crate::app::SettingTab],
    active: usize,
    theme: crate::theme::Theme,
) -> Vec<(usize, Rect)> {
    let mut spans: Vec<Span> = Vec::new();
    // Cell width of each tab as drawn: an active pill adds two rounded caps + two
    // inner spaces (title + 4); an inactive tab just pads a space each side (+2).
    let mut widths: Vec<u16> = Vec::with_capacity(tabs.len());
    for (i, t) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let w = t.title.chars().count() as u16;
        if i == active {
            spans.extend(super::pill_spans(t.title, theme));
            widths.push(w + 4);
        } else {
            spans.push(Span::styled(
                format!(" {} ", t.title),
                theme.style(Role::Muted),
            ));
            widths.push(w + 2);
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        area,
    );
    // Mirror ratatui's centred layout: total width (tabs + 1-cell separators),
    // centred in the area, then walk the tabs left→right.
    let total: u16 = widths.iter().sum::<u16>() + tabs.len().saturating_sub(1) as u16;
    let mut x = area.x + area.width.saturating_sub(total) / 2;
    let mut hits = Vec::with_capacity(tabs.len());
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            x += 1; // separator
        }
        hits.push((
            i,
            Rect {
                x,
                y: area.y,
                width: *w,
                height: 1,
            },
        ));
        x += *w;
    }
    hits
}

/// The active tab's options, scrolled to keep the cursor visible, with a
/// scrollbar when the tab is taller than the body.
fn render_body(
    f: &mut Frame,
    area: Rect,
    config: &Config,
    scope: Mode,
    tab: usize,
    sel_row: usize,
    theme: crate::theme::Theme,
) -> Vec<(usize, Rect)> {
    let rows = tab_rows(scope, tab);
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    // (row index in `rows`, line index) for each clickable option (headers excluded).
    let mut item_lines: Vec<(usize, usize)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        match row {
            SettingRow::Section(title) => {
                if i > 0 {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::styled(
                    format!("  {title}"),
                    theme.style(Role::Hint).add_modifier(Modifier::BOLD),
                ));
            }
            SettingRow::Item(item) => {
                let selected = i == sel_row;
                let label = item.label();
                let value = item.value(config);
                let pad = VALUE_COL.saturating_sub(label.chars().count() + 4);
                item_lines.push((i, lines.len()));
                if selected {
                    sel_line = lines.len();
                    // The selected option → a full-width rounded selection bar.
                    let text = format!("  ▸ {label}{}{value}", " ".repeat(pad));
                    lines.push(crate::view::rounded_line(text, area.width, theme));
                } else {
                    lines.push(Line::from(vec![
                        Span::styled(format!("    {label}"), theme.style(Role::Body)),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(value, Style::default().fg(theme.color(Role::Heading))),
                    ]));
                }
            }
        }
    }

    let h = area.height as usize;
    let total = lines.len();
    let max_off = total.saturating_sub(h);
    // Center the cursor in the body (clamped at the ends), like the book list.
    let offset = sel_line.saturating_sub(h / 2).min(max_off);
    let visible: Vec<Line> = lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(visible), area);

    // A slim scrollbar only when the tab overflows the body.
    if total > h {
        let mut sb = ScrollbarState::new(total).position(offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(theme.style(Role::Accent))
                .track_style(theme.style(Role::Hint)),
            area,
            &mut sb,
        );
    }

    // Screen rect for each visible option, for click hit-testing.
    let mut hits = Vec::with_capacity(item_lines.len());
    for (ri, li) in item_lines {
        if li < offset {
            continue;
        }
        let sy = area.y + (li - offset) as u16;
        if sy >= area.y + area.height {
            continue;
        }
        hits.push((
            ri,
            Rect {
                x: area.x,
                y: sy,
                width: area.width,
                height: 1,
            },
        ));
    }
    hits
}
