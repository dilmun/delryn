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

use crate::app::{App, Mode, Overlay, SettingItem, SettingRow, settings_tabs, tab_rows};
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
    let theme = app.config.theme;
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
    let help = if is_sources {
        " Tab section · ↑↓ move · ⏎ act · d delete · q close "
    } else {
        " Tab section · ↑↓ move · ←→ change · q close "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.style(Role::BorderFocus))
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
        adding,
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
#[allow(clippy::too_many_arguments)]
fn render_body(
    f: &mut Frame,
    area: Rect,
    config: &Config,
    scope: Mode,
    tab: usize,
    sel_row: usize,
    adding: Option<&TextInput>,
    theme: crate::theme::Theme,
) -> Vec<(usize, Rect)> {
    let rows = tab_rows(scope, tab, config);
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
    if selected {
        crate::view::rounded_line(
            format!("  ▸ {label}{}{value}", " ".repeat(pad)),
            width,
            theme,
        )
    } else {
        Line::from(vec![
            Span::styled(format!("    {label}"), theme.style(Role::Body)),
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
        crate::view::rounded_line(format!("  ▸ {text}"), width, theme)
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
    // Compose "▸ <path>  …  del" so the hint sits at the bar's right edge,
    // trimming the path (with an ellipsis) when it would otherwise collide.
    let inner = (width as usize).saturating_sub(2);
    let del = "del";
    let prefix = "  ▸ ";
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
