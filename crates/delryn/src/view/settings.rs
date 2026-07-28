//! Settings popup (`;`), scoped to the current mode — Reading settings in the
//! reader, Library settings in the library — so the two never mix. Options are
//! grouped into tabs (Tab / Shift-Tab to switch); the body scrolls when a tab is
//! taller than the window. Edits the live config. See `DESIGN.md` §7.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use crate::app::{App, Mode, Overlay, SettingItem, SettingRow, settings_tabs, visible_rows};
use crate::config::Config;
use crate::theme::Role;
use crate::ui::TextInput;

/// Column where each option's value is shown (label left, value right).
const VALUE_COL: usize = 30;

pub fn render(f: &mut Frame, app: &mut App) {
    let Overlay::Settings(state) = &app.overlay else {
        return;
    };
    let (scope_mode, active_tab, sel_row) = (state.scope, state.tab, state.row);
    let adding = state.adding.as_ref();
    let query = state.active_query().to_string();
    let filtering = state.filtering();
    let match_count = if state.filtering() {
        visible_rows(state.scope, state.tab, &query, &app.config)
            .iter()
            .filter(|r| matches!(r, SettingRow::Item(_)))
            .count()
    } else {
        0
    };
    let typing_filter = state.filter.is_some();
    let theme = app.config.theme;
    let bold = app.config.bold_borders;
    let area = super::overlay_rect(f.area(), app.overlay_large);
    f.render_widget(Clear, area);

    let bg = theme.paper();
    let scope = match scope_mode {
        Mode::Reader => "Reading",
        Mode::Library => "Library",
    };
    let tabs = settings_tabs(scope_mode, &app.config);
    // The Sources tab adds a delete affordance, so its help line differs.
    let is_sources = tabs.get(active_tab).is_some_and(|t| t.title == "Sources");
    let help = if typing_filter {
        " type to filter · ↑↓ move · ←→ change · ⏎ keep · Esc clear "
    } else if filtering {
        " ↑↓ move · ←→ change · / refine · r/R reset · Esc clear filter "
    } else if is_sources {
        " Tab section · ↑↓ move · ⏎ act · d delete · / search · q close "
    } else {
        " Tab section · ↑↓ move · ←→ change · r/R reset · / search · q close "
    };

    let block = super::overlay_frame(theme, bold)
        .title(Span::styled(
            format!(" {scope} Settings "),
            theme.style(Role::Title),
        ))
        .title_bottom(
            Line::from(Span::styled(help, theme.style(Role::Muted))).alignment(Alignment::Center),
        )
        .style(theme.style(Role::Body).bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The focused option's one-line explanation, pinned to the bottom so the body
    // above never reflows as the cursor moves. Wrapped to at most two rows; a
    // cramped window drops it entirely rather than eating the options.
    let help_text = focused_help(scope_mode, active_tab, sel_row, &query, &app.config);
    let help_lines = help_text
        .map(|t| wrap_help(t, inner.width.saturating_sub(4)).len() as u16)
        .unwrap_or(0);
    // 2 rows of chrome (rule + blank) only pay for themselves with room to spare.
    let help_h = if help_lines > 0 && inner.height > help_lines + 8 {
        help_lines + 2
    } else {
        0
    };

    let chunks = Layout::vertical([
        Constraint::Length(1),      // tab bar
        Constraint::Length(1),      // divider rule
        Constraint::Length(1),      // spacer
        Constraint::Min(0),         // body
        Constraint::Length(help_h), // help pane
    ])
    .split(inner);

    // Filtering searches every tab, so the tab strip would be misleading — the
    // query line replaces it and no tab is clickable while it's up.
    let tab_hits = if filtering {
        render_filter_line(f, chunks[0], &query, typing_filter, match_count, theme);
        Vec::new()
    } else {
        render_tab_bar(f, chunks[0], &tabs, active_tab, theme)
    };

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
        &query,
        adding,
        theme,
    );

    if help_h > 0
        && let Some(text) = help_text
    {
        render_help(f, chunks[4], text, theme);
    }
    app.mouse.overlay_tabs = tab_hits;
    app.mouse.overlay_rows = row_hits;
}

/// The focused row's help text, or `None` when the cursor sits on a section
/// header or the tab is empty.
fn focused_help(
    scope: Mode,
    tab: usize,
    row: usize,
    query: &str,
    config: &Config,
) -> Option<&'static str> {
    match visible_rows(scope, tab, query, config).into_iter().nth(row) {
        Some(SettingRow::Item(item)) => Some(item.help()),
        _ => None,
    }
}

/// Greedy word-wrap of the help sentence to `width`, capped at two rows so the
/// pane's height never surprises the layout above it.
fn wrap_help(text: &str, width: u16) -> Vec<String> {
    let width = width.max(20) as usize;
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match out.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => {
                if out.len() == 2 {
                    // Out of rows — mark the truncation rather than dropping words silently.
                    if let Some(line) = out.last_mut() {
                        let keep: String = line.chars().take(width.saturating_sub(1)).collect();
                        *line = format!("{}…", keep.trim_end());
                    }
                    break;
                }
                out.push(word.to_string());
            }
        }
    }
    out
}

/// The help pane: a dotted rule, then the wrapped sentence in the muted role.
fn render_help(f: &mut Frame, area: Rect, text: &str, theme: crate::theme::Theme) {
    let mut lines = vec![Line::styled(
        "┈".repeat(area.width as usize),
        theme.style(Role::Hint),
    )];
    lines.extend(
        wrap_help(text, area.width.saturating_sub(4))
            .into_iter()
            .map(|l| Line::styled(format!("  {l}"), theme.style(Role::Muted))),
    );
    f.render_widget(Paragraph::new(lines), area);
}

/// The `/` filter line, shown in place of the tab strip while a query is active:
/// the query with a block cursor while typing, and the match count on the right.
fn render_filter_line(
    f: &mut Frame,
    area: Rect,
    query: &str,
    typing: bool,
    matches: usize,
    theme: crate::theme::Theme,
) {
    let cursor = if typing { "█" } else { "" };
    let left = format!("  /{query}{cursor}");
    let right = match matches {
        0 => "  no matches  ".to_string(),
        1 => "  1 match  ".to_string(),
        n => format!("  {n} matches  "),
    };
    let gap = (area.width as usize)
        .saturating_sub(left.chars().count() + right.chars().count())
        .max(1);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left, theme.style(Role::Accent)),
            Span::raw(" ".repeat(gap)),
            Span::styled(right, theme.style(Role::Muted)),
        ])),
        area,
    );
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
    // Every tab occupies the same width whether active or not — the active pill is
    // two rounded caps + two inner spaces (title + 4), so an inactive tab pads two
    // spaces each side to match. Equal widths keep the centred strip from shifting
    // left/right as the active tab changes.
    let mut widths: Vec<u16> = Vec::with_capacity(tabs.len());
    for (i, t) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let w = t.title.chars().count() as u16;
        if i == active {
            spans.extend(super::pill_spans(t.title, theme));
        } else {
            spans.push(Span::styled(
                format!("  {}  ", t.title),
                theme.style(Role::Muted),
            ));
        }
        widths.push(w + 4);
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
#[allow(clippy::too_many_arguments)]
fn render_body(
    f: &mut Frame,
    area: Rect,
    config: &Config,
    scope: Mode,
    tab: usize,
    sel_row: usize,
    query: &str,
    adding: Option<&TextInput>,
    theme: crate::theme::Theme,
) -> Vec<(usize, Rect)> {
    let rows = visible_rows(scope, tab, query, config);
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
                item_lines.push((i, lines.len()));
                if selected {
                    sel_line = lines.len();
                }
                lines.push(setting_line(
                    *item, config, selected, adding, area.width, theme,
                ));
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

    // A slim scrollbar only when the tab overflows the body. `content_length` is
    // the number of scroll *positions*, not of lines: ratatui clamps `position` to
    // `content_length - 1`, so passing the line count leaves the thumb short of the
    // bottom by exactly one viewport (a full tab parked the thumb mid-track).
    if total > h {
        let mut sb = ScrollbarState::new(max_off + 1).position(offset);
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

/// Render one option row. Most settings show a label + value; the Sources tab's
/// rows (a folder, the add-folder action, rescan) render bespoke. Selected rows
/// get the shared full-width rounded accent bar.
fn setting_line(
    item: SettingItem,
    config: &Config,
    selected: bool,
    adding: Option<&TextInput>,
    width: u16,
    theme: crate::theme::Theme,
) -> Line<'static> {
    match item {
        SettingItem::Source(idx) => source_line(
            config
                .library_paths
                .get(idx)
                .map(String::as_str)
                .unwrap_or(""),
            selected,
            width,
            theme,
        ),
        // While adding, the "Add folder…" row becomes the inline path input.
        SettingItem::AddSource => match adding {
            Some(input) => add_input_line(input, width, theme),
            None => action_line("＋ Add folder…", selected, width, theme),
        },
        SettingItem::RescanNow => action_line("⟳ Rescan now", selected, width, theme),
        other => value_line(other, config, selected, width, theme),
    }
}

/// A standard `label … value` option row.
fn value_line(
    item: SettingItem,
    config: &Config,
    selected: bool,
    width: u16,
    theme: crate::theme::Theme,
) -> Line<'static> {
    let label = item.label();
    let value = item.value(config);
    let pad = VALUE_COL.saturating_sub(label.chars().count() + 4);
    // A 4-cell gutter carrying one mark: a dot when the value differs from its
    // default. The selection needs no arrow — the highlight bar already says which
    // row it is — so both states use the same gutter and the label column is fixed.
    let dot = if item.is_default(config) { ' ' } else { '●' };
    if selected {
        crate::view::rounded_line(
            format!(" {dot}  {label}{}{value}", " ".repeat(pad)),
            width,
            theme,
        )
    } else {
        Line::from(vec![
            Span::styled(format!(" {dot}"), theme.style(Role::Accent)),
            Span::styled(format!("  {label}"), theme.style(Role::Body)),
            Span::raw(" ".repeat(pad)),
            Span::styled(value, Style::default().fg(theme.color(Role::Heading))),
        ])
    }
}

/// An action row ("Add folder…", "Rescan now") — just a labelled command.
fn action_line(
    text: &str,
    selected: bool,
    width: u16,
    theme: crate::theme::Theme,
) -> Line<'static> {
    if selected {
        crate::view::rounded_line(format!("    {text}"), width, theme)
    } else {
        Line::from(Span::styled(
            format!("    {text}"),
            theme.style(Role::AccentStrong),
        ))
    }
}

/// A configured library folder. The selected row shows a right-aligned "del"
/// affordance inside the accent bar; unselected rows just show the path.
fn source_line(
    path: &str,
    selected: bool,
    width: u16,
    theme: crate::theme::Theme,
) -> Line<'static> {
    if !selected {
        return Line::from(Span::styled(format!("    {path}"), theme.style(Role::Body)));
    }
    // Compose "<path>  …  del" so the hint sits at the bar's right edge,
    // trimming the path (with an ellipsis) when it would otherwise collide.
    let inner = (width as usize).saturating_sub(2);
    let del = "del";
    let prefix = "    ";
    let budget = inner.saturating_sub(prefix.chars().count() + del.chars().count() + 1);
    let shown = truncate_to(path, budget);
    let left = format!("{prefix}{shown}");
    let gap = inner.saturating_sub(left.chars().count() + del.chars().count());
    crate::view::rounded_line(format!("{left}{}{del}", " ".repeat(gap)), width, theme)
}

/// The inline "add folder" path input, with a caret.
fn add_input_line(input: &TextInput, width: u16, theme: crate::theme::Theme) -> Line<'static> {
    let mut spans = vec![Span::styled("  ＋ ", theme.style(Role::AccentStrong))];
    spans.extend(crate::view::field_spans(
        input.text(),
        input.cursor(),
        (width as usize).saturating_sub(6),
        theme,
    ));
    Line::from(spans)
}

/// Truncate to at most `max` display cells, appending `…` when shortened.
fn truncate_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget};

    /// The scrollbar thumb must actually reach the bottom of the track when the
    /// list is scrolled to its end. `content_length` is the count of scroll
    /// positions; passing the line count instead (the original bug) parked the
    /// thumb a full viewport short — a tab scrolled to the end showed it mid-track.
    #[test]
    fn a_fully_scrolled_list_puts_the_thumb_at_the_bottom() {
        let (total, h) = (30usize, 18u16);
        let max_off = total - h as usize;
        let area = Rect::new(0, 0, 20, h);

        let render = |content_length: usize, position: usize| {
            let mut buf = ratatui::buffer::Buffer::empty(area);
            let mut state = ScrollbarState::new(content_length).position(position);
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .render(area, &mut buf, &mut state);
            // The rows of the scrollbar column carrying the thumb glyph.
            (0..h)
                .filter(|&y| buf[(area.width - 1, y)].symbol() == "█")
                .collect::<Vec<_>>()
        };

        let fixed = render(max_off + 1, max_off);
        assert!(
            fixed.contains(&(h - 1)),
            "thumb should touch the last row, got rows {fixed:?}"
        );
        // And the top still parks it at the top.
        let top = render(max_off + 1, 0);
        assert!(top.contains(&0), "thumb should start at row 0, got {top:?}");
    }
}
