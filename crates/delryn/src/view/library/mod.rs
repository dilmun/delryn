//! Library view — sections sidebar + a sortable book table, a cover grid, and a
//! detail pane. See `DESIGN.md` §5.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui_image::{Resize, StatefulImage};

use crate::app::{App, LibPane, LibView, SortKey};
use crate::config::LibLayout;
use crate::store::{BookRow, LibrarySection};
use crate::theme::Theme;

/// Minimum body width before the detail pane is shown.
const DETAIL_MIN_WIDTH: u16 = 90;

/// Title rows under each grid cover.
const LABEL_H: u16 = 2;
/// Cover protocols built per frame, so a screenful pops in over a few frames.
const GRID_BUILD_PER_FRAME: usize = 2;

// Sub-views; `render` orchestrates them. Shared helpers (`base`, `pane_block`,
// `fmt_size`, `series_suffix`, `fmt_idx`) stay here and are called from children.
mod books;
mod detail;
mod grid;
mod sections;
mod status;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let area = f.area();
    if theme.bg.is_some() {
        f.render_widget(Block::default().style(theme.text_style()), area);
    }

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let body = rows[0];

    let grid = app.config.library_layout == LibLayout::Grid;
    let show_sidebar = app.lib_show_sidebar;
    // Detail pane: only for the list views, when wanted and there's room (the
    // grid is itself a cover view, so it takes the full width).
    let show_detail = !grid && app.lib_detail && body.width >= DETAIL_MIN_WIDTH;
    // Clamp pane widths so the list always keeps a usable middle.
    let cap = (body.width / 3).max(1);
    let sidebar_w = app.lib_sidebar_w.min(cap);
    let detail_w = app.lib_detail_w.min(cap);

    let mut constraints = Vec::new();
    if show_sidebar {
        constraints.push(Constraint::Length(sidebar_w));
    }
    constraints.push(Constraint::Min(10));
    if show_detail {
        constraints.push(Constraint::Length(detail_w));
    }
    let cols = Layout::horizontal(constraints).split(body);

    let mut i = 0;
    if show_sidebar {
        sections::render_sections(f, cols[i], app, theme, app.lib_pane == LibPane::Sidebar);
        i += 1;
    }
    let list_area = cols[i];
    i += 1;
    if grid {
        grid::render_grid(f, list_area, app, theme, app.lib_pane == LibPane::List);
    } else {
        books::render_books(f, list_area, app, theme, app.lib_pane == LibPane::List);
    }
    if show_detail {
        detail::render_detail(f, cols[i], app, theme, app.lib_pane == LibPane::Detail);
    }
    status::render_status(f, rows[1], app, theme);
}

/// A bordered pane block whose border + title turn accent when the pane is
/// focused, else muted.
fn pane_block(title: &str, focused: bool, theme: Theme) -> Block<'static> {
    let border = if focused { theme.accent } else { theme.muted };
    let mut title_style = Style::default().fg(if focused { theme.accent } else { theme.muted });
    if focused {
        title_style = title_style.add_modifier(Modifier::BOLD);
    }
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(title.to_string(), title_style))
        .style(theme.text_style())
}

/// `  Foundation #2` for a series book, else empty. The leading spaces separate
/// it from the title.
fn series_suffix(b: &BookRow) -> String {
    if b.series.is_empty() {
        return String::new();
    }
    match b.series_index {
        Some(i) => format!("  {} #{}", b.series, fmt_idx(i)),
        None => format!("  {}", b.series),
    }
}

/// Series index without a trailing `.0` (`2.0` → "2", `2.5` → "2.5").
fn fmt_idx(i: f32) -> String {
    if (i.fract()).abs() < f32::EPSILON {
        format!("{}", i as i64)
    } else {
        format!("{i}")
    }
}

fn fmt_size(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        format!("{kb:.0}K")
    } else {
        format!("{:.1}M", kb / 1024.0)
    }
}
