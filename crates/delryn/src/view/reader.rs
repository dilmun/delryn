//! Reader view: TOC sidebar · centered measure content · status bar. Sidebar
//! and status bar are independently toggleable, and everything is theme-aware.
//! See `DESIGN.md` §4, §7.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage};

use crate::app::{
    App, Focus, ImageGeom, PageTarget, PageView, PanRoom, Reader, Viewport, place_page,
    raster_width_for_crispness,
};
use crate::config::{Config, ViewMode};
use crate::layout::{DisplayLine, LineKind, Run};
use crate::media::ImageBuilder;
use crate::search::Matcher;
use crate::theme::{Role, Theme};
use crate::view::layout::{GUTTER_COLS, LayoutCtx, Placement};

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
    reader.table_wrap = config.table_wrap;
    reader.justify = config.justify;
    reader.tidy_spacing = config.tidy_spacing;
    reader.paged = config.paged;
    reader.spread = matches!(config.view_mode, ViewMode::TwoPage) && reader.is_paged_image();
    reader.cover_offset = config.cover_offset;
    reader.chapter_lock = config.chapter_lock;
    reader.set_trim(config.pdf_trim, config.pdf_margin_pct);
    let area = f.area();

    // Distraction-free hides chrome regardless of the show_* flags.
    let show_sidebar = config.show_sidebar && !config.focus_mode;
    let show_status = config.show_status && !config.focus_mode;

    // Paint the themed background across the whole screen first.
    if theme.bg.is_some() {
        f.render_widget(Block::default().style(theme.text_style()), area);
    }

    let status_h = u16::from(show_status || reader.search.searching);
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(status_h)]).split(area);
    let body = rows[0];
    let status = rows[1];

    // The TOC sidebar uses the shared responsive split (~33% of the width,
    // collapsing on a narrow window so the text keeps the room).
    let (sidebar_area, content_area) = if show_sidebar {
        super::sidebar_split(body, 33, 16, 32, 58)
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
    if reader.search.searching {
        let style = theme.style(Role::StatusBar);
        let prompt = format!("[{}] /{}", reader.search.mode.label(), reader.search.input);
        f.render_widget(Paragraph::new(Line::raw(prompt)).style(style), status);
    } else if show_status {
        crate::view::status::render_reader(f, status, reader, config, theme);
    }
}

fn render_sidebar(f: &mut Frame, area: Rect, reader: &Reader, theme: Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.style(Role::Border))
        .title(Span::styled(" Contents ", theme.style(Role::Title)))
        .style(theme.text_style());
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
            if Some(row) == marked {
                // The current chapter / cursor row → a rounded selection bar.
                crate::view::rounded_line(text, inner.width, theme)
            } else {
                let mut s = theme.style(Role::Body);
                if let Some(bg) = theme.bg {
                    s = s.bg(bg);
                }
                Line::from(Span::styled(text, s))
            }
        })
        .collect();

    f.render_widget(
        Paragraph::new(Text::from(lines)).style(theme.text_style()),
        inner,
    );
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
        theme.style(Role::Heading),
    )))
    .alignment(Alignment::Center)
    .style(theme.text_style());
    f.render_widget(header, header_area);

    // Plan the frame: the active strategy maps the body + reading position +
    // content kind to a list of placements (paged pages, or reflowed text
    // columns). The renderer below just draws whatever it's handed.
    let paged = reader.is_paged_image();
    let spread = if paged {
        reader.spread_pages()
    } else {
        Vec::new()
    };
    let plan = super::layout::plan(
        config.view_mode,
        &LayoutCtx {
            body,
            config,
            paged,
            scroll: reader.scroll,
            section: reader.section,
            spread: &spread,
        },
    );

    // Write back the geometry the nav / scroll / page-mode math reads.
    // `viewport_lines` (one column height) and `page_lines` (the scroll unit) are
    // currently equal in both modes.
    reader.last_measure = plan.measure as usize;
    reader.viewport_lines = plan.page_lines;
    reader.page_lines = plan.page_lines;

    if paged {
        // PDF: hand the page placements to the deck and leave the cells empty so
        // the kitty placements show through — no per-cell drawing, no
        // transmit-on-turn, no black gap.
        let areas: Vec<(usize, Rect)> = plan
            .placements
            .iter()
            .filter_map(|p| match p {
                Placement::Page { section, area } => Some((*section, *area)),
                Placement::Text(_) => None,
            })
            .collect();
        capture_pdf_targets(reader, images, &areas, image_policy(config));
        return;
    }

    // Reflow: images align to the column and scale with it; sync (row estimate +
    // background builds) must run before wrapping.
    if let Some((picker, builder)) = images {
        reader.sync_images(
            builder,
            picker,
            image_geom(config, plan.measure, plan.page_lines.max(1) as u16),
        );
    }
    reader.ensure_wrapped(plan.measure as usize);
    reader.resolve_pending();
    reader.clamp_scroll();

    // Draw each text column: the wrapped-line slice, then its bookmark ribbon
    // (only where the column has margin for it).
    for placement in &plan.placements {
        let Placement::Text(col) = placement else {
            continue;
        };
        let lines = visible_lines(reader, col.scroll, col.area.height as usize, theme);
        f.render_widget(
            Paragraph::new(Text::from(lines)).style(theme.text_style()),
            col.area,
        );
        if col.gutter {
            draw_gutter(f, col.area, reader, col.scroll, theme);
        }
    }

    // Inline figures: deferred until scrolling settles so the heavy transmit
    // doesn't stutter motion; each column shows the figures in its own slice.
    if images.is_some() && !reader.is_scrolling() {
        for placement in &plan.placements {
            if let Placement::Text(col) = placement {
                draw_images_in(f, col.area, reader, col.scroll);
            }
        }
    }
}

/// The image render policy for the current frame: theme tint + adaptation mode.
fn image_policy(config: &Config) -> crate::media::RenderPolicy {
    crate::media::RenderPolicy {
        tint: super::theme_ink(config.theme),
        mode: config.image_mode,
    }
}

/// The image geometry for this frame: the column width + row cap plus the
/// config'd pixel cap, figure width %, and theme policy.
fn image_geom(config: &Config, avail: u16, max_rows: u16) -> ImageGeom {
    ImageGeom {
        avail,
        max_rows,
        max_px: config.image_max_px,
        width_pct: config.image_width_pct,
        policy: image_policy(config),
    }
}

/// The picker + background builder, present only when the terminal supports
/// images. Bundled so the render functions take one argument instead of two.
type Images<'a> = Option<(&'a Picker, &'a ImageBuilder)>;

/// Draw the bookmark ribbon in the left gutter for any bookmarked line visible in
/// `[top, top + height)`. The marker sits `GUTTER_COLS` cells left of the text so
/// a one-cell gap separates it from the prose. No-op if that falls off-screen
/// (callers gate on having real margin). The glyph is a monochrome flag that
/// takes the theme accent, so it recolours when the theme changes.
fn draw_gutter(f: &mut Frame, text_area: Rect, reader: &Reader, top: usize, theme: Theme) {
    let Some(x) = text_area.x.checked_sub(GUTTER_COLS) else {
        return;
    };
    let gutter = Rect {
        x,
        y: text_area.y,
        width: 1,
        height: text_area.height,
    };
    let ribbon = theme.style(Role::AccentStrong);
    let lines: Vec<Line> = (0..text_area.height as usize)
        .map(|row| {
            if reader.is_bookmark_line(top + row) {
                Line::from(Span::styled("⚑", ribbon))
            } else {
                Line::raw("")
            }
        })
        .collect();
    f.render_widget(
        Paragraph::new(Text::from(lines)).style(theme.text_style()),
        gutter,
    );
}

/// Where a page sits within its area — centred (single page), or hugging the
/// spine (a spread's two pages meet in the middle so there's no wasted gutter).
#[derive(Clone, Copy)]
enum PageAlign {
    Center,
    /// Align to the right edge of the area (a spread's left page).
    Right,
    /// Align to the left edge of the area (a spread's right page).
    Left,
}

/// Compute and store the PDF page placements for this frame: fit + place each
/// (section, column-area) — margin-trimmed, zoom/pan-aware for a single page,
/// spine-aligned for a spread. The [`PageDeck`] reads `pdf_targets` after the
/// frame and drives the kitty transmit/placement escapes; the columns are left
/// empty so the placed images show through.
fn capture_pdf_targets(
    reader: &mut Reader,
    images: Images,
    areas: &[(usize, Rect)],
    policy: crate::media::RenderPolicy,
) {
    let mut targets = Vec::new();
    // Zoom / pan apply only to a single-page view; a spread shows each page at
    // fit-page (a default, un-zoomed view), its two pages hugging the spine.
    let single = areas.len() == 1;
    let mut room = PanRoom::default();
    let mut step = (0.0, 0.0);
    if let Some((picker, _)) = images {
        // Adapt the visible + look-ahead pages to the theme off-thread; `page_png`
        // (below, and the deck's transmit) then serves the themed PNGs.
        reader.sync_pages(policy);
        let fs = picker.font_size();
        for (i, &(section, area)) in areas.iter().enumerate() {
            // The base raster's dimensions — the always-present source. The raster
            // must be ready to place; else emit no targets so the deck holds the old
            // page(s) up rather than flashing a half spread.
            let Some(base_dims) = reader.base_raster_dims(section) else {
                targets.clear();
                break;
            };
            let view = if single {
                reader.page_view
            } else {
                PageView::default()
            };
            let vp = Viewport {
                cols: area.width,
                rows: area.height,
                cell_w: fs.width,
                cell_h: fs.height,
            };
            // Place against the base raster first (margins trimmed). For a single
            // page this also sizes the viewport-matched crisp re-raster: if the
            // base would upscale, request a wider raster and re-place against it
            // once it's ready (else keep the base this frame).
            let base_content = reader.page_content_box(section, base_dims);
            let base_p = place_page(base_dims, base_content, vp, &view);
            // The effective placement: base, or the crisp raster once it's ready
            // (its width is recorded on the reader so `page_png` serves matching
            // bytes to the deck). Spreads always keep the base — they sit at
            // fit-page and are already crisp.
            let p = if single {
                let want = raster_width_for_crispness(&base_p, base_dims, vp);
                let (_w, dims) = reader.resolve_page_width(section, base_dims, want);
                if dims == base_dims {
                    base_p
                } else {
                    let content = reader.page_content_box(section, dims);
                    place_page(dims, content, vp, &view)
                }
            } else {
                base_p
            };
            let align = if single {
                PageAlign::Center
            } else if i == 0 {
                PageAlign::Right // left page hugs the spine on its right
            } else {
                PageAlign::Left // right page hugs the spine on its left
            };
            let (x, y) = align_page(area, p.cols, p.rows, align);
            targets.push(PageTarget {
                section,
                rect: Rect::new(x, y, p.cols, p.rows),
                crop: p.crop,
            });
            if single {
                room = p.room;
                step = (p.step_x, p.step_y);
            }
        }
    }
    // Record the pan room only once the single page actually placed — an unready
    // frame keeps the previous room so nav doesn't misfire mid-turn.
    if single && !targets.is_empty() {
        reader.set_page_room(room, step);
    }
    reader.pdf_targets = targets;
}

/// Position a `cols`×`rows` page within `area`, vertically centred and
/// horizontally aligned per [`PageAlign`].
fn align_page(area: Rect, cols: u16, rows: u16, align: PageAlign) -> (u16, u16) {
    let y = area.y + area.height.saturating_sub(rows) / 2;
    let x = match align {
        PageAlign::Center => area.x + area.width.saturating_sub(cols) / 2,
        PageAlign::Right => area.x + area.width.saturating_sub(cols),
        PageAlign::Left => area.x,
    };
    (x, y)
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

        // Terminal graphics draw *above* the text layer, so an inline image that
        // overlaps an open popup would paint over it. Skip any image whose cell
        // rect intersects the overlay region; images entirely clear of it still
        // render (the popup just sits beside them).
        if let Some(o) = reader.overlay_occlude {
            let img_left = area.x.saturating_add(x.max(0) as u16);
            let img_right = area
                .x
                .saturating_add((x + plan.cols as i16).clamp(0, area.width as i16) as u16);
            let img_top = area.y.saturating_add(y.max(0) as u16);
            let img_bottom = area
                .y
                .saturating_add((y + plan.rows as i16).clamp(0, area.height as i16) as u16);
            let h_overlap = img_left < o.right() && img_right > o.x;
            let v_overlap = img_top < o.bottom() && img_bottom > o.y;
            if h_overlap && v_overlap {
                continue;
            }
        }

        f.render_widget(SlicedImage::new(&plan.proto, SignedPosition { x, y }), area);
    }
}

fn visible_lines(reader: &Reader, start: usize, count: usize, theme: Theme) -> Vec<Line<'static>> {
    let start = start.min(reader.lines.len());
    let end = (start + count).min(reader.lines.len());
    let matcher = reader.search.matcher.as_ref().filter(|m| !m.is_empty());
    let sel = reader.selected_anchor();
    reader.lines[start..end]
        .iter()
        .enumerate()
        .map(|(off, l)| {
            // The link cursor's highlight range, if it falls on this line.
            let cursor = sel
                .filter(|h| h.line == start + off)
                .map(|h| (h.start, h.end));
            to_ratatui(l, theme, matcher, cursor)
        })
        .collect()
}

fn to_ratatui(
    line: &DisplayLine,
    theme: Theme,
    matcher: Option<&Matcher>,
    cursor: Option<(usize, usize)>,
) -> Line<'static> {
    if matcher.is_none() && cursor.is_none() {
        let spans: Vec<Span> = line
            .runs
            .iter()
            .map(|r| Span::styled(r.text.clone(), run_style(r, line.kind, theme)))
            .collect();
        return Line::from(spans);
    }

    // Expand to per-char styles, mark search-match + link-cursor ranges, regroup.
    let mut chars: Vec<(char, Style)> = Vec::new();
    for run in &line.runs {
        let style = run_style(run, line.kind, theme);
        for c in run.text.chars() {
            chars.push((c, style));
        }
    }
    let mut matched = vec![false; chars.len()];
    if let Some(m) = matcher {
        let text: String = chars.iter().map(|(c, _)| *c).collect();
        for (s, e) in m.highlight_ranges(&text) {
            for flag in matched.iter_mut().take(e.min(chars.len())).skip(s) {
                *flag = true;
            }
        }
    }

    let hilite = theme.style(Role::Match);
    // The link cursor stands out from search via reverse video (theme-agnostic).
    let cursor_style = theme.style(Role::Cursor);
    let (cs, ce) = cursor.unwrap_or((usize::MAX, usize::MAX));

    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut buf_style: Option<Style> = None;
    for (idx, (c, st)) in chars.iter().enumerate() {
        let style = if idx >= cs && idx < ce {
            cursor_style
        } else if matched[idx] {
            hilite
        } else {
            *st
        };
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

/// Map a run + line-kind to a themed ratatui style. The line kind picks a
/// semantic [`Role`] (which carries its emphasis); run-level flags add bold/
/// italic and override the foreground (syntax highlight, inline code, links).
fn run_style(run: &Run, kind: LineKind, theme: Theme) -> Style {
    // The line kind's base role — Heading/Quote/Math carry their own emphasis.
    let role = match kind {
        LineKind::Heading(_) => Role::Heading,
        LineKind::Quote => Role::Quote,
        LineKind::Math => Role::Math, // display equations, accented
        // Rules, the code gutter / unhighlighted code, and footnotes read muted.
        LineKind::Rule | LineKind::Code(_) | LineKind::Footnote(_) => Role::Muted,
        LineKind::Table { .. } | LineKind::Body | LineKind::Image(_) => Role::Body,
    };
    let mut style = theme.style(role);

    // Code blocks and alternating (shaded) table rows sit on a faint "surface"
    // panel; everything else on the page.
    let bg = match kind {
        LineKind::Code(_) | LineKind::Table { shaded: true } => theme.code_surface().or(theme.bg),
        _ => theme.bg,
    };
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    if run.style.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if run.style.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }

    // Foreground overrides, lowest to highest precedence.
    if let Some((r, g, b)) = run.fg {
        style = style.fg(Color::Rgb(r, g, b)); // syntax highlight (the one literal)
    } else if run.style.code && matches!(kind, LineKind::Body) {
        style = style.fg(theme.color(Role::Code)); // inline code
    }
    if run.style.link {
        // Links read as their theme colour — no underline (it's noisy in a TUI).
        style = style.fg(theme.color(Role::Link));
    }
    style
}
