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
use crate::config::{Config, LibLayout};
use crate::store::{BookRow, LibrarySection};
use crate::theme::Theme;

/// Smallest book-list width to keep when sizing the side panes (they collapse to
/// preserve it on a narrow window) — generous so panes drop before the list
/// gets cramped, matching the reader's comfortable collapse.
const MIN_LIST: u16 = 48;

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

pub(crate) use books::sort_cycle;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = app.config.theme;
    let area = f.area();
    if theme.bg.is_some() {
        f.render_widget(Block::default().style(theme.text_style()), area);
    }

    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let body = rows[0];

    let grid = app.config.library_layout == LibLayout::Grid;
    // Sidebar (left) + detail (right) are responsive percentage panes that
    // collapse on a narrow window (shared app-standard split). The grid view is
    // itself a cover wall, so it skips the detail pane.
    let (sidebar, rest) = if app.lib_show_sidebar {
        super::sidebar_split(body, app.lib_sidebar_pct, 16, 40, MIN_LIST)
    } else {
        (None, body)
    };
    let (list_area, detail) = if !grid && app.lib_detail {
        super::detail_split(rest, app.lib_detail_pct, 24, 56, MIN_LIST)
    } else {
        (rest, None)
    };

    if let Some(sb) = sidebar {
        sections::render_sections(f, sb, app, theme, app.lib_pane == LibPane::Sidebar);
    }
    if grid {
        grid::render_grid(f, list_area, app, theme, app.lib_pane == LibPane::List);
    } else {
        books::render_books(f, list_area, app, theme, app.lib_pane == LibPane::List);
    }
    if let Some(d) = detail {
        detail::render_detail(f, d, app, theme, app.lib_pane == LibPane::Detail);
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
