//! Reader view: TOC sidebar · centered measure content · status bar. Sidebar
//! and status bar are independently toggleable. See `DESIGN.md` §4.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::{App, Focus, Reader};
use crate::config::{Config, ViewMode};
use crate::layout::{DisplayLine, LineKind, Run};

const GAUGE_WIDTH: usize = 16;

pub fn render(f: &mut Frame, app: &mut App) {
    let App {
        config,
        reader,
        last_layout,
        ..
    } = app;
    let Some(reader) = reader.as_mut() else {
        return;
    };
    let area = f.area();

    let status_h = u16::from(config.show_status);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(status_h)]).split(area);
    let body = rows[0];
    let status = rows[1];

    let (sidebar_area, content_area) = if config.show_sidebar {
        let sw = (body.width / 3).clamp(16, 32);
        let cols = Layout::horizontal([Constraint::Length(sw), Constraint::Min(0)]).split(body);
        (Some(cols[0]), cols[1])
    } else {
        (None, body)
    };

    last_layout.sidebar = sidebar_area;
    last_layout.content = Some(content_area);

    if let Some(sb) = sidebar_area {
        render_sidebar(f, sb, reader);
    }
    render_content(f, content_area, reader, config);
    if config.show_status {
        render_status(f, status, reader, config);
    }
}

fn render_sidebar(f: &mut Frame, area: Rect, reader: &Reader) {
    let items: Vec<ListItem> = reader
        .outline
        .iter()
        .map(|e| {
            let indent = "  ".repeat(e.depth);
            let here = e.section == reader.section && e.depth == 0;
            let marker = if here { "▸ " } else { "  " };
            let style = if here {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!("{indent}{marker}{}", e.label),
                style,
            )))
        })
        .collect();

    let block = Block::default().borders(Borders::ALL).title("Contents");
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    if reader.focus == Focus::Sidebar {
        state.select(Some(reader.sidebar_sel));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn render_content(f: &mut Frame, area: Rect, reader: &mut Reader, config: &Config) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);
    let header_area = rows[0];
    let body = rows[1];

    let header = Paragraph::new(Line::from(Span::styled(
        reader.chapter_title(),
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    f.render_widget(header, header_area);

    match config.view_mode {
        ViewMode::Center => render_column(f, body, reader, true, config.measure_width),
        ViewMode::Fill => render_column(f, body, reader, false, config.measure_width),
        ViewMode::TwoPage => render_two_page(f, body, reader, config.measure_width),
    }
}

/// One text column. `centered` caps to the measure and centers it; otherwise
/// the text fills the pane (minus a thin gutter).
fn render_column(f: &mut Frame, body: Rect, reader: &mut Reader, centered: bool, measure_cfg: u16) {
    let measure = if centered {
        measure_cfg.min(body.width.saturating_sub(2)).max(1)
    } else {
        body.width.saturating_sub(2).max(1)
    };
    let left_pad = body.width.saturating_sub(measure) / 2;
    let cols = Layout::horizontal([
        Constraint::Length(left_pad),
        Constraint::Length(measure),
        Constraint::Min(0),
    ])
    .split(body);
    let text_area = cols[1];

    reader.viewport_lines = text_area.height as usize;
    reader.page_lines = reader.viewport_lines;
    reader.last_measure = measure as usize;
    reader.ensure_wrapped(measure as usize);
    reader.resolve_pending();
    reader.clamp_scroll();

    let lines = visible_lines(reader, reader.scroll, reader.viewport_lines);
    f.render_widget(Paragraph::new(Text::from(lines)), text_area);
}

/// Two side-by-side columns forming a spread; the right column continues from
/// the left, so scrolling flows left-to-right.
fn render_two_page(f: &mut Frame, body: Rect, reader: &mut Reader, measure_cfg: u16) {
    const GAP: u16 = 3;
    let col_w = (body.width.saturating_sub(GAP) / 2).min(measure_cfg).max(1);
    let side_pad = body.width.saturating_sub(col_w * 2 + GAP) / 2;
    let cols = Layout::horizontal([
        Constraint::Length(side_pad),
        Constraint::Length(col_w),
        Constraint::Length(GAP),
        Constraint::Length(col_w),
        Constraint::Min(0),
    ])
    .split(body);
    let left_area = cols[1];
    let right_area = cols[3];

    let h = left_area.height as usize;
    reader.viewport_lines = h;
    reader.page_lines = h * 2;
    reader.last_measure = col_w as usize;
    reader.ensure_wrapped(col_w as usize);
    reader.resolve_pending();
    reader.clamp_scroll();

    let left = visible_lines(reader, reader.scroll, h);
    let right = visible_lines(reader, reader.scroll + h, h);
    f.render_widget(Paragraph::new(Text::from(left)), left_area);
    f.render_widget(Paragraph::new(Text::from(right)), right_area);
}

fn visible_lines(reader: &Reader, start: usize, count: usize) -> Vec<Line<'static>> {
    let start = start.min(reader.lines.len());
    let end = (start + count).min(reader.lines.len());
    reader.lines[start..end].iter().map(to_ratatui).collect()
}

fn to_ratatui(line: &DisplayLine) -> Line<'static> {
    let spans: Vec<Span> = line
        .runs
        .iter()
        .map(|r| Span::styled(r.text.clone(), run_style(r, line.kind)))
        .collect();
    Line::from(spans)
}

/// Map a run + line-kind to a ratatui style. Syntax-highlighted runs carry an
/// explicit colour; everything else is modifier-only until colour themes land.
fn run_style(run: &Run, kind: LineKind) -> Style {
    let mut style = Style::default();
    if run.style.bold || matches!(kind, LineKind::Heading(_)) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if run.style.italic || matches!(kind, LineKind::Quote) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if let Some((r, g, b)) = run.fg {
        // Syntax colour: show at full strength.
        style = style.fg(Color::Rgb(r, g, b));
    } else if matches!(kind, LineKind::Quote | LineKind::Rule | LineKind::Code) {
        // Quotes, rules, and the code gutter/plain code: dim.
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

fn render_status(f: &mut Frame, area: Rect, reader: &Reader, config: &Config) {
    let meta = reader.doc.metadata();
    let left = if meta.authors.is_empty() {
        meta.title.clone()
    } else {
        format!("{} — {}", meta.title, meta.author_line())
    };

    let pct = (reader.progress() * 100.0).round() as u32;
    let right = format!(
        "{} · {}/{} · {}%  {}",
        config.view_mode.label(),
        reader.section + 1,
        reader.doc.section_count(),
        pct,
        gauge(reader.progress(), GAUGE_WIDTH),
    );

    let width = area.width as usize;
    let used = left.chars().count() + right.chars().count() + 2;
    let pad = width.saturating_sub(used);
    let line = format!(" {left}{}{right} ", " ".repeat(pad));

    let style = Style::default().add_modifier(Modifier::REVERSED);
    f.render_widget(Paragraph::new(Line::raw(line)).style(style), area);
}

fn gauge(frac: f32, width: usize) -> String {
    let filled = (frac.clamp(0.0, 1.0) * width as f32).round() as usize;
    let mut s = String::with_capacity(width * 3);
    s.extend(std::iter::repeat_n('█', filled));
    s.extend(std::iter::repeat_n('░', width - filled));
    s
}
