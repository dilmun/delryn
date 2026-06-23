//! Reader view: TOC sidebar · centered measure content · status bar. Sidebar
//! and status bar are independently toggleable, and everything is theme-aware.
//! See `DESIGN.md` §4, §7.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};

use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage};

use crate::app::{App, Focus, Reader};
use crate::config::{Config, ViewMode};
use crate::layout::{DisplayLine, LineKind, Run};
use crate::media::ImageBuilder;
use crate::search::Matcher;
use crate::theme::Theme;

const GAUGE_WIDTH: usize = 16;

pub fn render(f: &mut Frame, app: &mut App) {
    let App {
        config,
        reader,
        last_layout,
        picker,
        image_builder,
        ..
    } = app;
    let Some(reader) = reader.as_mut() else {
        return;
    };
    // Images need both the protocol picker and the background builder.
    let images = picker.as_ref().zip(image_builder.as_ref());
    let theme = config.theme;
    reader.code_theme = theme.syntect.to_string();
    reader.line_spacing = config.line_spacing;
    reader.paragraph_spacing = config.paragraph_spacing;
    reader.code_wrap = config.code_wrap;
    reader.chapter_lock = config.chapter_lock;
    let area = f.area();

    // Distraction-free hides chrome regardless of the show_* flags.
    let show_sidebar = config.show_sidebar && !config.focus_mode;
    let show_status = config.show_status && !config.focus_mode;

    // Paint the themed background across the whole screen first.
    if theme.bg.is_some() {
        f.render_widget(Block::default().style(base(theme)), area);
    }

    let status_h = u16::from(show_status || reader.searching);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(status_h)]).split(area);
    let body = rows[0];
    let status = rows[1];

    let (sidebar_area, content_area) = if show_sidebar {
        let sw = (body.width / 3).clamp(16, 32);
        let cols = Layout::horizontal([Constraint::Length(sw), Constraint::Min(0)]).split(body);
        (Some(cols[0]), cols[1])
    } else {
        (None, body)
    };

    last_layout.sidebar = sidebar_area;
    last_layout.content = Some(content_area);

    if let Some(sb) = sidebar_area {
        reader.update_sidebar_view(sb.height.saturating_sub(2) as usize);
        render_sidebar(f, sb, reader, theme);
    }
    render_content(f, content_area, reader, config, theme, images);
    if reader.searching {
        let style = Style::default().fg(theme.status_fg).bg(theme.status_bg);
        let prompt = format!("[{}] /{}", reader.search_mode.label(), reader.search_input);
        f.render_widget(Paragraph::new(Line::raw(prompt)).style(style), status);
    } else if show_status {
        render_status(f, status, reader, config, theme);
    }
}

/// Base style: theme foreground, plus background if the theme paints one.
fn base(theme: Theme) -> Style {
    let style = Style::default().fg(theme.fg);
    match theme.bg {
        Some(bg) => style.bg(bg),
        None => style,
    }
}

fn render_sidebar(f: &mut Frame, area: Rect, reader: &Reader, theme: Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.muted))
        .title(Span::styled(
            "Contents",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .style(base(theme));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The highlighted row: the keyboard cursor when focused, else the entry at
    // the current reading position (scroll-spy).
    let marked = if reader.focus == Focus::Sidebar {
        Some(reader.sidebar_sel)
    } else {
        reader.active_outline_row()
    };

    // Manual viewport slice from the scroll offset, so the wheel can scroll the
    // TOC freely and the keyboard cursor can stay centered.
    let vis = reader.outline_visible();
    let off = reader.sidebar_offset.min(vis.len().saturating_sub(1));
    let height = inner.height as usize;
    let hilite = Style::default()
        .fg(theme.bg.unwrap_or(Color::Black))
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);

    let lines: Vec<Line> = vis
        .iter()
        .enumerate()
        .skip(off)
        .take(height)
        .map(|(row, &oi)| {
            let e = &reader.outline[oi];
            let indent = "  ".repeat(e.depth);
            let marker = if reader.outline_is_parent(oi) {
                if reader.outline_collapsed(oi) {
                    "▸ "
                } else {
                    "▾ "
                }
            } else {
                "  "
            };
            let text = format!("{indent}{marker}{}", e.label);
            let style = if Some(row) == marked {
                hilite
            } else {
                let mut s = Style::default().fg(theme.fg);
                if let Some(bg) = theme.bg {
                    s = s.bg(bg);
                }
                s
            };
            Line::from(Span::styled(text, style))
        })
        .collect();

    f.render_widget(Paragraph::new(Text::from(lines)).style(base(theme)), inner);
}

fn render_content(
    f: &mut Frame,
    area: Rect,
    reader: &mut Reader,
    config: &Config,
    theme: Theme,
    images: Images,
) {
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);
    let header_area = rows[0];
    let body = rows[1];

    let header = Paragraph::new(Line::from(Span::styled(
        reader.chapter_title(),
        Style::default()
            .fg(theme.heading)
            .add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center)
    .style(base(theme));
    f.render_widget(header, header_area);

    match config.view_mode {
        ViewMode::Center => render_column(
            f,
            body,
            reader,
            config.side_padding,
            config.image_max_px,
            theme,
            images,
        ),
        // Fill: edge-to-edge text (zero side margin).
        ViewMode::Fill => render_column(f, body, reader, 0, config.image_max_px, theme, images),
        ViewMode::TwoPage => render_two_page(f, body, reader, config.image_max_px, theme, images),
    }
}

/// The picker + background builder, present only when the terminal supports
/// images. Bundled so the render functions take one argument instead of two.
type Images<'a> = Option<(&'a Picker, &'a ImageBuilder)>;

/// The reading column width for a given pane width and per-side padding percent.
fn measure_for(pane_width: u16, side_padding: u16) -> u16 {
    let pad = (pane_width as u32 * side_padding as u32 / 100) as u16;
    pane_width
        .saturating_sub(pad.saturating_mul(2))
        .max(crate::config::MIN_TEXT_COLS.min(pane_width).max(1))
}

/// A single reading column padded by `side_padding` percent on each side.
fn render_column(
    f: &mut Frame,
    body: Rect,
    reader: &mut Reader,
    side_padding: u16,
    image_max_px: u16,
    theme: Theme,
    images: Images,
) {
    let measure = measure_for(body.width, side_padding);
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

    // Images align to the text column and scale with it. Sync (which estimates
    // rows + dispatches background builds) must run before wrapping.
    if let Some((picker, builder)) = images {
        reader.sync_images(
            builder,
            picker,
            text_area.width,
            text_area.height.max(1),
            image_max_px,
        );
    }

    reader.ensure_wrapped(measure as usize);
    reader.resolve_pending();
    reader.clamp_scroll();

    let lines = visible_lines(reader, reader.scroll, reader.viewport_lines, theme);
    f.render_widget(
        Paragraph::new(Text::from(lines)).style(base(theme)),
        text_area,
    );

    // Defer the (blocking) image transmit until scrolling settles, so motion
    // stays smooth; the figure pops in when you stop.
    if images.is_some() && !reader.is_scrolling() {
        draw_images_in(f, text_area, reader, reader.scroll);
    }
}

/// Draw the ready figure images that intersect `[top, top+height)` of the line
/// flow into `area`, centered. Uses a sliced protocol with a signed vertical
/// offset so an image scrolling past either edge shows its visible slice
/// (rather than appearing/vanishing whole).
fn draw_images_in(f: &mut Frame, area: Rect, reader: &Reader, top: usize) {
    let view_end = top + area.height as usize;
    let lines = &reader.lines;
    let mut i = 0;
    while i < lines.len() {
        let LineKind::Image(idx) = lines[i].kind else {
            i += 1;
            continue;
        };
        let start = i;
        while i < lines.len() && lines[i].kind == LineKind::Image(idx) {
            i += 1;
        }
        let end = i; // exclusive

        // Skip images entirely outside the viewport.
        if end <= top || start >= view_end {
            continue;
        }
        let Some(plan) = reader.image_plan(idx) else {
            continue;
        };

        let x = (area.width.saturating_sub(plan.cols) / 2) as i16;
        let y = start as i16 - top as i16; // negative when the top scrolled off
        f.render_widget(SlicedImage::new(&plan.proto, SignedPosition { x, y }), area);
    }
}

/// Two side-by-side columns forming a spread; the right column continues from
/// the left, so scrolling flows left-to-right.
fn render_two_page(
    f: &mut Frame,
    body: Rect,
    reader: &mut Reader,
    image_max_px: u16,
    theme: Theme,
    images: Images,
) {
    const GAP: u16 = 3;
    // Each column takes half the pane (minus the gap); a thin outer margin.
    let usable = body.width.saturating_sub(GAP + 4).max(2);
    let col_w = (usable / 2).max(1);
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

    if let Some((picker, builder)) = images {
        reader.sync_images(builder, picker, col_w, h.max(1) as u16, image_max_px);
    }

    reader.ensure_wrapped(col_w as usize);
    reader.resolve_pending();
    reader.clamp_scroll();

    let left = visible_lines(reader, reader.scroll, h, theme);
    let right = visible_lines(reader, reader.scroll + h, h, theme);
    f.render_widget(
        Paragraph::new(Text::from(left)).style(base(theme)),
        left_area,
    );
    f.render_widget(
        Paragraph::new(Text::from(right)).style(base(theme)),
        right_area,
    );

    // Images: left column shows the first `h` rows, right column the next `h`.
    // Deferred while scrolling so the heavy transmit doesn't stutter motion.
    if images.is_some() && !reader.is_scrolling() {
        draw_images_in(f, left_area, reader, reader.scroll);
        draw_images_in(f, right_area, reader, reader.scroll + h);
    }
}

fn visible_lines(reader: &Reader, start: usize, count: usize, theme: Theme) -> Vec<Line<'static>> {
    let start = start.min(reader.lines.len());
    let end = (start + count).min(reader.lines.len());
    let matcher = reader.search_matcher.as_ref().filter(|m| !m.is_empty());
    reader.lines[start..end]
        .iter()
        .map(|l| to_ratatui(l, theme, matcher))
        .collect()
}

fn to_ratatui(line: &DisplayLine, theme: Theme, matcher: Option<&Matcher>) -> Line<'static> {
    let Some(m) = matcher else {
        let spans: Vec<Span> = line
            .runs
            .iter()
            .map(|r| Span::styled(r.text.clone(), run_style(r, line.kind, theme)))
            .collect();
        return Line::from(spans);
    };

    // Expand to per-char styles, mark search-match ranges, then regroup.
    let mut chars: Vec<(char, Style)> = Vec::new();
    for run in &line.runs {
        let style = run_style(run, line.kind, theme);
        for c in run.text.chars() {
            chars.push((c, style));
        }
    }
    let mut matched = vec![false; chars.len()];
    let text: String = chars.iter().map(|(c, _)| *c).collect();
    for (s, e) in m.highlight_ranges(&text) {
        for flag in matched.iter_mut().take(e.min(chars.len())).skip(s) {
            *flag = true;
        }
    }

    let hilite = Style::default()
        .bg(theme.accent)
        .fg(theme.bg.unwrap_or(Color::Black))
        .add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut buf_style: Option<Style> = None;
    for (idx, (c, st)) in chars.iter().enumerate() {
        let style = if matched[idx] { hilite } else { *st };
        if buf_style == Some(style) {
            buf.push(*c);
        } else {
            if let Some(s) = buf_style {
                spans.push(Span::styled(std::mem::take(&mut buf), s));
            }
            buf.push(*c);
            buf_style = Some(style);
        }
    }
    if let Some(s) = buf_style {
        spans.push(Span::styled(buf, s));
    }
    Line::from(spans)
}

/// Map a run + line-kind to a themed ratatui style. Syntax-highlighted runs
/// keep their explicit colour; semantic roles use the theme palette.
fn run_style(run: &Run, kind: LineKind, theme: Theme) -> Style {
    let mut style = Style::default();
    if let Some(bg) = theme.bg {
        style = style.bg(bg);
    }
    if run.style.bold || matches!(kind, LineKind::Heading(_)) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if run.style.italic || matches!(kind, LineKind::Quote) {
        style = style.add_modifier(Modifier::ITALIC);
    }

    let mut fg = match kind {
        LineKind::Heading(_) => theme.heading,
        LineKind::Quote => theme.quote,
        LineKind::Rule => theme.muted,
        LineKind::Code(_) => theme.muted,  // gutter / unhighlighted
        LineKind::Footnote => theme.muted, // notes set apart from the body
        LineKind::Math => theme.heading,   // display equations, accented
        LineKind::Body | LineKind::Image(_) => theme.fg,
    };
    if let Some((r, g, b)) = run.fg {
        fg = Color::Rgb(r, g, b); // syntax highlight
    } else if run.style.code && matches!(kind, LineKind::Body) {
        fg = theme.code_fg; // inline code
    }
    if run.style.link {
        fg = theme.link;
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style.fg(fg)
}

fn render_status(f: &mut Frame, area: Rect, reader: &Reader, config: &Config, theme: Theme) {
    let meta = reader.doc.metadata();
    // A transient flash (e.g. "copied") takes over the left side when present.
    let left = if let Some(flash) = &reader.flash {
        flash.clone()
    } else if meta.authors.is_empty() {
        meta.title.clone()
    } else {
        format!("{} — {}", meta.title, meta.author_line())
    };

    let pct = (reader.progress() * 100.0).round() as u32;
    let sf = config.status;
    let mut parts: Vec<String> = Vec::new();
    if reader.search_matcher.is_some() {
        let n = reader.search_count();
        let cur = if n == 0 { 0 } else { reader.search_idx + 1 };
        parts.push(format!("⌕ {cur}/{n}"));
    }
    if sf.theme {
        parts.push(theme.name.to_string());
    }
    if sf.view {
        parts.push(config.view_mode.label().to_string());
    }
    if sf.position {
        parts.push(format!(
            "{}/{}",
            reader.section + 1,
            reader.doc.section_count()
        ));
    }
    if sf.percent {
        parts.push(format!("{pct}%"));
    }
    let mut right = parts.join(" · ");
    if sf.gauge {
        if !right.is_empty() {
            right.push_str("  ");
        }
        right.push_str(&gauge(reader.progress(), GAUGE_WIDTH));
    }

    let width = area.width as usize;
    let used = left.chars().count() + right.chars().count() + 2;
    let pad = width.saturating_sub(used);
    let line = format!(" {left}{}{right} ", " ".repeat(pad));

    let style = Style::default().fg(theme.status_fg).bg(theme.status_bg);
    f.render_widget(Paragraph::new(Line::raw(line)).style(style), area);
}

fn gauge(frac: f32, width: usize) -> String {
    let filled = (frac.clamp(0.0, 1.0) * width as f32).round() as usize;
    let mut s = String::with_capacity(width * 3);
    s.extend(std::iter::repeat_n('█', filled));
    s.extend(std::iter::repeat_n('░', width - filled));
    s
}
