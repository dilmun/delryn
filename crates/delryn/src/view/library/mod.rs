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

use crate::app::{App, LibPane, LibView, Overlay, SortKey};
use crate::config::{Config, LibLayout};
use crate::store::{BookRow, LibrarySection};
use crate::theme::{Role, Theme};

/// Smallest book-list width to keep when sizing the side panes (they collapse to
/// preserve it on a narrow window) — generous so panes drop before the list
/// gets cramped, matching the reader's comfortable collapse.
const MIN_LIST: u16 = 48;

/// Cover protocols built per frame, so a screenful pops in over a few frames.
const GRID_BUILD_PER_FRAME: usize = 2;

// Sub-views; `render` orchestrates them. Shared helpers (`base`, `pane_block`,
// `fmt_size`, `series_suffix`, `fmt_idx`) stay here and are called from children.
mod books;
mod detail;
mod grid;
mod sections;

pub(crate) use books::sort_cycle;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let area = f.area();
    if theme.bg.is_some() {
        f.render_widget(Block::default().style(theme.text_style()), area);
    }

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let body = rows[0];

    // Cover views (the card grid and the cover wall) are themselves full of
    // covers, so they skip the detail pane.
    let cover_view = app.is_grid();
    // Sidebar (left) + detail (right) are responsive percentage panes that
    // collapse on a narrow window (shared app-standard split).
    let (sidebar, rest) = if app.library.show_sidebar {
        super::sidebar_split(body, app.library.sidebar_pct, 16, 40, MIN_LIST)
    } else {
        (None, body)
    };
    let (list_area, detail) = if !cover_view && app.library.detail {
        super::detail_split(rest, app.library.detail_pct, 24, 56, MIN_LIST)
    } else {
        (rest, None)
    };

    // Capture the pane rects so the wheel can target the pane under the cursor.
    app.last_layout.sidebar = sidebar;
    app.last_layout.lib_list = Some(list_area);
    app.last_layout.lib_detail = detail;

    if let Some(sb) = sidebar {
        let side_focused = app.library.pane == LibPane::Sidebar;
        sections::render_sections(f, sb, app, theme, side_focused);
    }
    let focused = app.library.pane == LibPane::List;
    if cover_view {
        grid::render_grid(f, list_area, app, theme, focused);
    } else {
        books::render_books(f, list_area, app, theme, focused);
    }
    if let Some(d) = detail {
        detail::render_detail(f, d, app, theme, app.library.pane == LibPane::Detail);
    }
    crate::view::status::render_library(f, rows[1], app, theme);
}

/// A bordered pane block whose border + title turn accent when the pane is
/// focused, else muted.
fn pane_block(title: &str, focused: bool, theme: Theme) -> Block<'static> {
    let border = if focused {
        theme.color(Role::BorderFocus)
    } else {
        theme.color(Role::Border)
    };
    let mut title_style = Style::default().fg(if focused {
        theme.color(Role::Accent)
    } else {
        theme.color(Role::Muted)
    });
    if focused {
        title_style = title_style.add_modifier(Modifier::BOLD);
    }
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        // Pad the title so the border never touches the text (app-standard, like
        // the image viewer's " Figures " title).
        .title(Span::styled(format!(" {title} "), title_style))
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

pub(crate) fn fmt_size(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        format!("{kb:.0}K")
    } else {
        format!("{:.1}M", kb / 1024.0)
    }
}
