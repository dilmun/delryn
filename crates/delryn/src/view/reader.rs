//! Reader view: TOC sidebar · centered measure content · status bar. Sidebar
//! and status bar are independently toggleable, and everything is theme-aware.
//! See `DESIGN.md` §4, §7.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use ratatui_image::picker::Picker;

use crate::HighlightColor;
use crate::app::inline_deck::InlineTarget;
use crate::app::{
    App, Focus, HintKind, ImageGeom, PageTarget, PageView, PanRoom, Reader, Viewport, place_page,
    raster_width_for_crispness,
};
use crate::config::{Config, ViewMode};
use crate::layout::{DisplayLine, LineKind, Run};
use crate::media::ImageBuilder;
use crate::media::ImgKey;
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
    // Graphical math needs the config toggle + a graphics protocol; the cell height
    // sizes equations. Inline math is rasterised only with the extra opt-in — off, it
    // stays the natural Unicode approximation. A change here re-decodes the open
    // sections (image ⇆ Unicode).
    let math_on = config.graphical_math && images.is_some();
    let inline_math_on = math_on && config.graphical_inline_math;
    let cell_h = images.map(|(p, _)| p.font_size().height).unwrap_or(20);
    // The text-column width in px = the last wrap width (cells) × the cell width — the budget a
    // too-wide display equation breaks to when `break_wide_equations` is on.
    let cell_w = images.map(|(p, _)| p.font_size().width).unwrap_or(10);
    let wrap_px = (reader.last_measure as u32).saturating_mul(u32::from(cell_w));
    reader.sync_graphical_math(
        math_on,
        inline_math_on,
        cell_h,
        config.math_scale,
        config.break_wide_equations,
        wrap_px,
    );
    let theme = config.theme;
    reader.code_theme = theme.code_syntect().to_string();
    reader.line_spacing = config.line_spacing;
    reader.paragraph_spacing = config.paragraph_spacing;
    reader.code_wrap = config.code_wrap;
    reader.code_line_numbers = config.code_line_numbers;
    reader.code_label = config.code_language_label;
    reader.code_fold = config.code_fold;
    reader.code_fold_threshold = config.code_fold_threshold;
    reader.table_wrap = config.table_wrap;
    reader.justify = config.justify;
    reader.tidy_spacing = config.tidy_spacing;
    reader.paged = config.paged;
    // The raw continuous flag + view mode; each `continuous_*_active` check gates the
    // rest (reflow → Center only; paged → Center single stack or TwoPage spread stack).
    reader.continuous = config.continuous;
    reader.view_mode = config.view_mode;
    reader.page_gap = config.page_gap;
    reader.side_padding = config.side_padding;
    reader.rtl = config.reading_direction.is_rtl();
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

    // Visual mode always shows its command hint, even with the status bar hidden.
    let status_h = u16::from(show_status || reader.search.searching || reader.selection_active());
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
    } else if reader.selection_active() {
        let style = theme.style(Role::StatusBar);
        let hint = if reader.selection_selecting() {
            " SELECT · h/l/w/b/j/k extend · ^d/^u ½page · y copy · 1-5/H highlight · a note · K look up · Esc "
        } else {
            " CURSOR · h/l/w/b/j/k move · ^d/^u ½page · v select · m bookmark · H highlight · a note · K look up · Esc "
        };
        f.render_widget(Paragraph::new(Line::raw(hint)).style(style), status);
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
    // Planning is pure geometry over the reading position, so it can be redone once
    // the scroll settles (see the re-plan after `clamp_scroll` below).
    let section = reader.section;
    let plan_at = |scroll: usize| {
        super::layout::plan(
            config.view_mode,
            &LayoutCtx {
                body,
                config,
                paged,
                scroll,
                section,
                spread: &spread,
            },
        )
    };
    let planned_scroll = reader.scroll;
    let mut plan = plan_at(planned_scroll);

    // Write back the geometry the nav / scroll / page-mode math reads.
    // `viewport_lines` (one column height) and `page_lines` (the scroll unit) are
    // currently equal in both modes.
    reader.last_measure = plan.measure as usize;
    reader.viewport_lines = plan.page_lines;
    reader.page_lines = plan.page_lines;
    // Total visible lines across the text columns (two in a spread), so the visual
    // caret can roam both pages before the follow scrolls.
    let text_cols = plan
        .placements
        .iter()
        .filter(|p| matches!(p, Placement::Text(_)))
        .count()
        .max(1);
    reader.visible_span = plan.page_lines * text_cols;

    if paged {
        // PDF: hand the page placements to the deck and leave the cells empty so
        // the kitty placements show through — no per-cell drawing, no
        // transmit-on-turn, no black gap.
        if reader.continuous_paged_active() {
            // Continuous: a vertical stack of page slices filling the body. It sizes
            // pages against the *full* body width (even in TwoPage, where the plan's
            // `measure` is a half column) and computes its own columns/zoom.
            reader.last_measure = body.width as usize;
            reader.viewport_lines = body.height as usize;
            reader.page_lines = body.height as usize;
            capture_pdf_stack(reader, images, body, image_policy(config));
            return;
        }
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
    // The plan above used the pre-wrap scroll — settling it needs the wrapped-line
    // count, which needs the plan's `measure`. When settling moves it (entering a
    // section from the bottom, or clamping past the last line), re-plan so the
    // columns, the gutter, and the image targets all draw the current slice rather
    // than a stale one.
    if reader.scroll != planned_scroll {
        plan = plan_at(reader.scroll);
    }

    // Draw each text column: the wrapped-line slice, then its bookmark ribbon
    // (only where the column has margin for it). In continuous mode the single
    // column draws the cross-section render buffer (anchor tail + following heads)
    // instead of one section's slice.
    for placement in &plan.placements {
        let Placement::Text(col) = placement else {
            continue;
        };
        let lines = if reader.reflow_flows() {
            continuous_visible_lines(reader, col.scroll, col.area.height as usize, theme)
        } else {
            visible_lines(reader, col.scroll, col.area.height as usize, theme)
        };
        f.render_widget(
            Paragraph::new(Text::from(lines)).style(theme.text_style()),
            col.area,
        );
        if col.gutter {
            draw_gutter(f, col.area, reader, col.scroll, theme);
        }
        // The `F`/`I` pick-mode: a bright number badge on each visible element in
        // this column (drawn over the cells; images are centred so the left edge
        // is clear of the Kitty graphic).
        if reader.hint_active() {
            draw_hint_badges(f, col.area, reader, col.scroll, theme);
        }
    }

    // Collect this frame's inline-image placement targets (figures + equation rasters,
    // plus cross-section figures in continuous/two-page flow). `App::inline_escapes`
    // reconciles them via the `InlineDeck` (transmit once, place, re-place on scroll),
    // so already-resident images just re-place cheaply and new ones upload paced — no
    // per-cell compositing, no scroll ghosts.
    if images.is_some() {
        reader.begin_inline_frame();
        for placement in &plan.placements {
            if let Placement::Text(col) = placement {
                collect_image_targets(col.area, reader, col.scroll);
                collect_inline_math_targets(col.area, reader, col.scroll);
                // Cross-section flow (continuous Center, or any two-page spread) joins
                // following sections into this column; collect their figures too so a
                // boundary figure fills the column instead of a blank gap. Each column
                // shows a slice `col_offset` rows into the shared buffer.
                if reader.reflow_flows() {
                    let col_offset = col.scroll.saturating_sub(reader.scroll);
                    collect_following_image_targets(col.area, reader, col_offset);
                }
            }
        }
    }
}

/// Draw the pick-mode number badges for this column: a bright `[n]` at the first
/// visible row of each badged element (code block or figure). Badge `n` maps to
/// `targets[n-1]`, so the numbers run in reading order across both spread columns.
fn draw_hint_badges(f: &mut Frame, area: Rect, reader: &Reader, top: usize, theme: Theme) {
    let Some((kind, targets)) = reader.hint() else {
        return;
    };
    let view_end = top.saturating_add(area.height as usize);
    // The `Selection` role (ink on the accent, bold) — the same legible pill as a
    // selected row / search match. It resolves via `on_accent()`, so it stays
    // readable in the `auto` theme too, where `Heading` is `Reset` (no colour).
    let style = theme.style(Role::Selection);
    let buf = f.buffer_mut();
    for (n, &idx) in targets.iter().enumerate() {
        let first = reader.lines.iter().position(|l| match (kind, l.kind) {
            (HintKind::Code, LineKind::Code(x)) => x == idx,
            (HintKind::Image, LineKind::Image(x)) => x == idx,
            _ => false,
        });
        let Some(first) = first else { continue };
        if first < top || first >= view_end {
            continue;
        }
        let y = area.y + (first - top) as u16;
        buf.set_string(area.x, y, format!(" {} ", n + 1), style);
    }
}

/// Collect the *following* sections' figure placement targets for this text column —
/// the anchor's come from [`collect_image_targets`] over `reader.lines`; these come from
/// the joined continuous buffer. `col_offset` is how many buffer rows into the buffer
/// this column starts (0 for the left/only column, one column-height for the two-page
/// right column), so a figure at buffer row `row` lands at screen row `row - col_offset`.
fn collect_following_image_targets(area: Rect, reader: &Reader, col_offset: usize) {
    for (row, key) in reader.continuous_following_images() {
        let Some(plan) = reader.image_plan_by_key(&key) else {
            continue; // not built yet — the reserved rows stay blank this frame
        };
        let (cols, rows, px) = (plan.cols, plan.rows, plan.px);
        // Signed `y`: a figure whose top scrolled above this column's top (or that spans
        // the spread boundary from the left column) gets a negative `y`, so `inline_target`
        // source-crops its top — it shows *partially* instead of vanishing.
        let x = (area.width.saturating_sub(cols) / 2) as i16;
        let y = (row as isize - col_offset as isize) as i16;
        if inline_occluded(reader, area, x, y, cols, rows) {
            continue;
        }
        if let Some(t) = inline_target(key, cols, rows, px, x, y, area) {
            reader.push_inline_target(t);
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
        math_scale: config.math_scale,
        fit_mode: config.image_fit,
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
            // Saturate: a row index past the flow just draws blank, and a render
            // pass must never panic on the scroll it was handed.
            let line = top.saturating_add(row);
            // A note (pen) takes precedence over a bookmark (flag); a highlight
            // (a colour bar in its own hue) marks a line neither claims — its wash
            // already stands out, so the chip is just a margin cue and colour key.
            if reader.is_note_line(line) {
                Line::from(Span::styled("✎", ribbon))
            } else if reader.is_bookmark_line(line) {
                Line::from(Span::styled("⚑", ribbon))
            } else if let Some(color) = reader.highlight_line(line) {
                Line::from(Span::styled("▌", Style::default().fg(color.bg())))
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

/// Compute the continuous-paged vertical page stack for this frame: theme the
/// visible + look-ahead pages, mirror the cell size, and let the reader assemble
/// the [`PageTarget`] slices filling `body`. Like [`capture_pdf_targets`], the
/// cells are left empty so the deck's kitty placements show through; the deck reads
/// `pdf_targets` after the frame. No-op without an image protocol.
fn capture_pdf_stack(
    reader: &mut Reader,
    images: Images,
    body: Rect,
    policy: crate::media::RenderPolicy,
) {
    let Some((picker, _)) = images else {
        reader.clear_page_stack();
        return;
    };
    reader.sync_pages(policy);
    let fs = picker.font_size();
    reader.set_cell_px((fs.width, fs.height));
    reader.capture_page_stack(body);
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

/// Collect placement targets for the ready figure images intersecting
/// `[top, top+height)` of the line flow into `area`, centered. A signed vertical
/// offset lets `inline_target` source-crop an image scrolling past either edge so it
/// shows its visible slice rather than appearing/vanishing whole.
fn collect_image_targets(area: Rect, reader: &Reader, top: usize) {
    let view_end = top.saturating_add(area.height as usize);
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
        let (Some(key), Some(plan)) = (reader.image_key(idx), reader.image_plan(idx)) else {
            continue; // not built yet — the reserved rows stay blank this frame
        };
        let (cols, rows, px) = (plan.cols, plan.rows, plan.px);
        let x = (area.width.saturating_sub(cols) / 2) as i16;
        let y = start as i16 - top as i16; // negative when the top scrolled off
        if inline_occluded(reader, area, x, y, cols, rows) {
            continue;
        }
        if let Some(t) = inline_target(key, cols, rows, px, x, y, area) {
            reader.push_inline_target(t);
        }
    }
}

/// Collect placement targets for the ready inline-math equation rasters over the atom
/// runs in the visible slice of `reader.lines`. Each small image is placed over the
/// blank cells the wrapper reserved for it. Placement is mid-line: `x` is the atom's
/// column within the text (the summed display width of the preceding runs), `y` its
/// line's screen row. A one-row atom covers only its line; a multi-row fraction is
/// centred on the text row, hanging into the blank spacer rows the wrapper reserved
/// above and below. Only the anchor section's atoms exist (a following continuous
/// section shows its Unicode floor until it becomes the anchor).
fn collect_inline_math_targets(area: Rect, reader: &Reader, top: usize) {
    use unicode_width::UnicodeWidthStr;
    let start = top.min(reader.lines.len());
    let end = (top + area.height as usize).min(reader.lines.len());
    for i in start..end {
        let line = &reader.lines[i];
        let mut col = 0usize;
        for run in &line.runs {
            if let Some(id) = run.math
                && let Some(key) = reader.inline_math_key(id)
                && let Some(plan) = reader.image_plan_by_key(&key)
            {
                let (cols, rows, px) = (plan.cols, plan.rows, plan.px);
                let x = col as i16;
                // A multi-row atom (a fraction) is centred on the text row: its top sits
                // `(rows-1)/2` rows *above* this line, in the spacer rows the wrapper
                // reserved above and below. A 1-row atom starts on this line.
                let above = ((rows.saturating_sub(1)) / 2) as i16;
                let y = (i - top) as i16 - above;
                if !inline_occluded(reader, area, x, y, cols, rows)
                    && let Some(t) = inline_target(key, cols, rows, px, x, y, area)
                {
                    reader.push_inline_target(t);
                }
            }
            col += UnicodeWidthStr::width(run.text.as_str());
        }
    }
}

/// Build an [`InlineTarget`] for an image of `cols`×`rows` cells (pixel size `px`)
/// whose top-left sits at column-relative cell `(x, y)` of `area`. Returns `None` when
/// the image is entirely clipped off an edge. A vertically-clipped image carries a
/// **source-pixel crop** so its visible cell-rows show the matching image rows (the
/// raster is whole-cell-padded, so a crop lands on exact rows). Horizontal clipping is
/// not cropped — inline atoms fit within the column and figures are centred, so `x ≥ 0`
/// and the width fits in practice.
fn inline_target(
    key: ImgKey,
    cols: u16,
    rows: u16,
    px: (u32, u32),
    x: i16,
    y: i16,
    area: Rect,
) -> Option<InlineTarget> {
    let rows_i = rows as i16;
    let r0 = (-y).max(0); // image cell-rows clipped off the top
    let r1 = rows_i.min(area.height as i16 - y); // one past the last visible row
    if r1 <= r0 || x >= area.width as i16 {
        return None;
    }
    let vis_rows = (r1 - r0) as u16;
    let x0 = x.max(0) as u16;
    let vis_cols = cols.min(area.width.saturating_sub(x0));
    if vis_cols == 0 {
        return None;
    }
    let rect = Rect::new(area.x + x0, area.y + (y + r0) as u16, vis_cols, vis_rows);
    let crop = (r0 != 0 || r1 != rows_i).then(|| {
        let (pw, ph) = px;
        let per_row = ph as f32 / f32::from(rows.max(1));
        let cy = (r0 as f32 * per_row).round() as u32;
        let ch =
            ((vis_rows as f32 * per_row).round() as u32).clamp(1, ph.saturating_sub(cy).max(1));
        (0, cy, pw, ch)
    });
    Some(InlineTarget { key, rect, crop })
}

/// Whether an image at cell offset `(x, y)` of `area`, spanning `cols`×`rows` cells,
/// overlaps an open popup's occlusion rect — terminal graphics draw above the cell
/// layer, so an image over a popup would paint on top of it.
fn inline_occluded(reader: &Reader, area: Rect, x: i16, y: i16, cols: u16, rows: u16) -> bool {
    let Some(o) = reader.overlay_occlude else {
        return false;
    };
    let left = area.x.saturating_add(x.max(0) as u16);
    let right = area
        .x
        .saturating_add((x + cols as i16).clamp(0, area.width as i16) as u16);
    let row = area.y.saturating_add(y.max(0) as u16);
    let h_overlap = left < o.right() && right > o.x;
    let v_overlap = row < o.bottom() && row + rows.max(1) > o.y;
    h_overlap && v_overlap
}

/// The rendered lines for continuous mode: the cross-section buffer (anchor tail +
/// following sections' heads) styled like [`visible_lines`]. The link-cursor
/// highlight and search matches follow the anchor section — matches are re-found by
/// text so they still light up in following sections, but the cursor is anchor-only.
fn continuous_visible_lines(
    reader: &mut Reader,
    col_scroll: usize,
    count: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    // The cross-section buffer starts at the reading position (`reader.scroll`);
    // this column shows a slice `offset` rows into it — 0 for the left/only column,
    // one column-height for the two-page right column, so the spread flows on.
    let offset = col_scroll.saturating_sub(reader.scroll);
    let buf = reader.continuous_lines(offset + count);
    let start = offset.min(buf.len());
    let end = (offset + count).min(buf.len());
    let matcher = reader.search.matcher.as_ref().filter(|m| !m.is_empty());
    let sel = reader.selected_anchor();
    buf[start..end]
        .iter()
        .enumerate()
        .map(|(off, l)| {
            let idx = col_scroll + off;
            to_ratatui(l, theme, &line_decor(reader, idx, matcher, sel))
        })
        .collect()
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
            let idx = start + off;
            to_ratatui(l, theme, &line_decor(reader, idx, matcher, sel))
        })
        .collect()
}

/// Per-line decoration inputs for [`to_ratatui`]: search matches, the link cursor,
/// committed highlight spans, and the live visual selection + caret.
struct LineDecor<'a> {
    matcher: Option<&'a Matcher>,
    /// The link-cursor character range on this line, if any.
    cursor: Option<(usize, usize)>,
    /// Committed highlight spans (`[start, end)` + colour) on this line.
    highlights: &'a [(usize, usize, HighlightColor)],
    /// The visual selection's character range on this line, if any.
    selection: Option<(usize, usize)>,
    /// The visual caret's column on this line, if the caret is here.
    caret: Option<usize>,
}

/// Gather the decorations for display line `idx` from the reader's live state.
fn line_decor<'a>(
    reader: &'a Reader,
    idx: usize,
    matcher: Option<&'a Matcher>,
    sel: Option<&'a crate::app::AnchorHit>,
) -> LineDecor<'a> {
    LineDecor {
        matcher,
        cursor: sel.filter(|h| h.line == idx).map(|h| (h.start, h.end)),
        highlights: reader.highlight_spans(idx),
        selection: reader.selection_span_on(idx),
        caret: reader
            .selection_caret()
            .filter(|(l, _)| *l == idx)
            .map(|(_, c)| c),
    }
}

/// Render display lines to styled ratatui lines with no decorations (no
/// selection / search / highlight overlay) — for read-only panels such as the
/// fullscreen code viewer. Reuses the per-run [`run_style`].
pub(crate) fn plain_lines(lines: &[DisplayLine], theme: Theme) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            Line::from(
                line.runs
                    .iter()
                    .map(|r| Span::styled(r.text.clone(), run_style(r, line.kind, theme)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn to_ratatui(line: &DisplayLine, theme: Theme, decor: &LineDecor) -> Line<'static> {
    // The common case — no decorations — keeps each run as one span.
    if decor.matcher.is_none()
        && decor.cursor.is_none()
        && decor.highlights.is_empty()
        && decor.selection.is_none()
        && decor.caret.is_none()
    {
        let spans: Vec<Span> = line
            .runs
            .iter()
            .map(|r| Span::styled(r.text.clone(), run_style(r, line.kind, theme)))
            .collect();
        return Line::from(spans);
    }

    // Expand to per-char base styles so ranges can override individual cells.
    let mut chars: Vec<(char, Style)> = Vec::new();
    for run in &line.runs {
        let style = run_style(run, line.kind, theme);
        for c in run.text.chars() {
            chars.push((c, style));
        }
    }
    let mut matched = vec![false; chars.len()];
    if let Some(m) = decor.matcher {
        let text: String = chars.iter().map(|(c, _)| *c).collect();
        for (s, e) in m.highlight_ranges(&text) {
            for flag in matched.iter_mut().take(e.min(chars.len())).skip(s) {
                *flag = true;
            }
        }
    }

    let hilite = theme.style(Role::Match);
    // The link cursor and the visual caret both stand out via reverse video; the
    // visual *selection* uses the accent selection style.
    let cursor_style = theme.style(Role::Cursor);
    let selection_style = theme.style(Role::Selection);
    let (cs, ce) = decor.cursor.unwrap_or((usize::MAX, usize::MAX));
    let (ss, se) = decor.selection.unwrap_or((usize::MAX, usize::MAX));

    // Per-cell style, most-specific first: caret → selection → link cursor →
    // search match → committed highlight wash → the run's own style.
    let cell_style = |idx: usize, base: Style| -> Style {
        if decor.caret == Some(idx) {
            cursor_style
        } else if idx >= ss && idx < se {
            selection_style
        } else if idx >= cs && idx < ce {
            cursor_style
        } else if matched[idx] {
            hilite
        } else if let Some((_, _, color)) = decor
            .highlights
            .iter()
            .find(|(s, e, _)| idx >= *s && idx < *e)
        {
            let (bg, fg) = color.wash();
            base.bg(bg).fg(fg)
        } else {
            base
        }
    };

    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut buf_style: Option<Style> = None;
    for (idx, (c, base)) in chars.iter().enumerate() {
        let style = cell_style(idx, *base);
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
    // A caret past the last character (empty or end-of-line) still needs a cell.
    if decor.caret.is_some_and(|c| c >= chars.len()) {
        spans.push(Span::styled(" ".to_string(), cursor_style));
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
    // Inline math (the Unicode approximation shown in prose) reads as the natural
    // terminal-font look: italic + a subtle accent nudge. Skipped on a display-math
    // line (already the accented `Math` role) and on a rasterised atom (`run.math` is
    // `Some` — blank placeholder cells the reader paints an equation over).
    let inline_math = run.style.math && run.math.is_none() && !matches!(kind, LineKind::Math);
    if run.style.italic || inline_math {
        style = style.add_modifier(Modifier::ITALIC);
    }

    // Foreground overrides, lowest to highest precedence.
    if let Some((r, g, b)) = run.fg {
        style = style.fg(Color::Rgb(r, g, b)); // syntax highlight (the one literal)
    } else if run.style.code && matches!(kind, LineKind::Body) {
        style = style.fg(theme.color(Role::Code)); // inline code
    } else if inline_math {
        style = style.fg(theme.color(Role::MathInline)); // subtle math contrast
    }
    if run.style.link {
        // Links read as their theme colour — no underline (it's noisy in a TUI).
        style = style.fg(theme.color(Role::Link));
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::DARK;
    use delryn_model::Inline;

    /// A run tagged as inline math (the Unicode approximation, not a rasterised atom).
    fn math_run() -> Run {
        Run {
            text: "x²".into(),
            style: Inline {
                math: true,
                ..Inline::default()
            },
            ..Run::default()
        }
    }

    #[test]
    fn inline_math_run_is_italic_and_tinted() {
        // In prose, inline math reads as the natural terminal-font look: italic + the
        // subtle `MathInline` tint.
        let s = run_style(&math_run(), LineKind::Body, DARK);
        assert!(
            s.add_modifier.contains(Modifier::ITALIC),
            "inline math is italicised"
        );
        assert_eq!(
            s.fg,
            Some(DARK.color(Role::MathInline)),
            "inline math carries the subtle math tint"
        );
    }

    #[test]
    fn display_math_line_keeps_the_math_role() {
        // On a display-math line the run is already the accented `Math` role; the
        // inline italic/tint must NOT be layered on top.
        let s = run_style(&math_run(), LineKind::Math, DARK);
        assert_eq!(s.fg, Some(DARK.color(Role::Math)), "the display Math ink");
        assert!(
            !s.add_modifier.contains(Modifier::ITALIC),
            "no inline italic on a display-math line"
        );
    }

    #[test]
    fn rasterised_atom_is_not_tinted() {
        // A rasterised inline-math atom (`math = Some`) is blank placeholder cells the
        // reader paints an equation over — it gets no text styling.
        let mut r = math_run();
        r.math = Some(0);
        let s = run_style(&r, LineKind::Body, DARK);
        assert_ne!(
            s.fg,
            Some(DARK.color(Role::MathInline)),
            "a raster atom is not tinted like Unicode math"
        );
    }
}
