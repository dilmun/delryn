//! Render dispatch by mode. The view layer is format-agnostic — it only ever
//! sees the `Document` model and app state. See `DESIGN.md` §2.

pub mod annotations;
pub mod bidi;
pub mod bulk_rename;
pub mod dup_resolve;
pub mod image;
pub mod layout;
pub mod library;
pub mod meta_edit;
pub mod palette;
pub mod reader;
pub mod settings;
pub mod shelf_picker;
pub mod stats;
pub mod status;
pub mod word_lookup;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::{App, Mode, Overlay};

/// Adapt the theme's resolved (ink, paper) sRGB into a [`media::Ink`] for
/// recolouring math/line-art images. The colour resolution itself lives on the
/// theme (the single source of truth); this is just the format-layer adapter.
pub fn theme_ink(theme: crate::theme::Theme) -> crate::media::Ink {
    let (ink, paper) = theme.image_ink();
    crate::media::Ink { ink, paper }
}

/// Terminal cell size in pixels (w, h), for sizing image render rects. Falls
/// back to a typical 10×20 cell when no graphics picker is available.
pub fn image_font(app: &App) -> (u16, u16) {
    app.picker
        .as_ref()
        .map(|p| {
            let fs = p.font_size();
            (fs.width, fs.height)
        })
        .unwrap_or((10, 20))
}

/// Largest centered sub-rect of `area` whose pixel aspect matches an image of
/// `dims` pixels, given the terminal cell size `font` (px w,h). Rendering a cover
/// into this rect fills it edge-to-edge with no letterbox — the only margins are
/// the centered slack on the non-limiting axis.
pub fn cover_image_rect(area: Rect, font: (u16, u16), dims: (u32, u32)) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }
    let (cw, ch) = (font.0.max(1) as u32, font.1.max(1) as u32);
    let (iw, ih) = (dims.0.max(1), dims.1.max(1));
    let area_px_w = area.width as u32 * cw;
    let area_px_h = area.height as u32 * ch;
    // Fit the cover's aspect inside the area in pixel space, then back to cells.
    let render_px_w = area_px_w.min(area_px_h * iw / ih);
    let render_px_h = render_px_w * ih / iw;
    let cols = (((render_px_w + cw / 2) / cw).max(1) as u16).min(area.width);
    let rows = (((render_px_h + ch / 2) / ch).max(1) as u16).min(area.height);
    Rect {
        x: area.x + (area.width - cols) / 2,
        y: area.y + (area.height - rows) / 2,
        width: cols,
        height: rows,
    }
}

/// The exact screen rect an open blocking overlay covers this frame, if any, so
/// the reader can skip inline images the popup would clobber. Every bordered
/// window shares one geometry ([`overlay_rect`]); the full-screen image viewer
/// covers everything.
fn overlay_occlusion(area: Rect, app: &App) -> Option<Rect> {
    if matches!(app.overlay, Overlay::ImageView(_)) {
        return Some(area);
    }
    if app.overlay.is_resizable_window() {
        return Some(overlay_rect(area, app.overlay_large));
    }
    None
}

/// The width a side pane should take in `area_w` columns — `pct`% clamped to
/// `[min, max]` cells — or `None` if it should collapse because it wouldn't leave
/// the main pane at least `min_main` columns (plus a 1-cell gap). The single
/// responsive rule shared by every multi-pane view.
fn side_width(area_w: u16, pct: u16, min: u16, max: u16, min_main: u16) -> Option<u16> {
    let want = (area_w.saturating_mul(pct) / 100).clamp(min, max);
    (area_w >= want + 1 + min_main).then_some(want)
}

/// Standard responsive split with a **left** sidebar: `(sidebar, main)`. The
/// sidebar collapses (→ `None`, main takes all) when the window is too narrow to
/// keep `min_main` columns for the main pane.
pub fn sidebar_split(
    area: Rect,
    pct: u16,
    min: u16,
    max: u16,
    min_main: u16,
) -> (Option<Rect>, Rect) {
    match side_width(area.width, pct, min, max, min_main) {
        Some(w) => {
            let cols = Layout::horizontal([
                Constraint::Length(w),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
            (Some(cols[0]), cols[2])
        }
        None => (None, area),
    }
}

/// Standard responsive split with a **right** pane (e.g. a detail/preview):
/// `(main, side)`. Mirrors [`sidebar_split`]; same collapse rule.
pub fn detail_split(
    area: Rect,
    pct: u16,
    min: u16,
    max: u16,
    min_main: u16,
) -> (Rect, Option<Rect>) {
    match side_width(area.width, pct, min, max, min_main) {
        Some(w) => {
            let cols = Layout::horizontal([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(w),
            ])
            .split(area);
            (cols[0], Some(cols[2]))
        }
        None => (area, None),
    }
}

// ── Rounded selection highlights ─────────────────────────────────────────────
// Every selection in the app — book list, sidebar, TOC, tabs, overlay lists — is
// drawn as a rounded capsule instead of a hard rectangle, for one consistent
// look. Powerline rounded end-caps (a solid half-circle that bulges away from the
// bar: left cap curves left, right cap curves right) painted in the bar's accent
// colour on the page behind it do the rounding. Needs a Nerd/Powerline font
// (delryn's terminal ships FiraCode Nerd Font). The whole look is defined here, so
// tuning it is a one-place change.

const CAP_LEFT: &str = "\u{e0b6}";
const CAP_RIGHT: &str = "\u{e0b4}";

/// The cap glyph style over a given background `bg` — the bar's accent colour on
/// whatever sits behind the bar, so the rounded ends blend in. Most selections
/// sit on the page ([`cap_style`]); the status pill and editor tabs sit on the
/// status/paper background and pass it explicitly.
fn cap_style_on(theme: crate::theme::Theme, bg: ratatui::style::Color) -> ratatui::style::Style {
    use crate::theme::Role;
    ratatui::style::Style::default()
        .fg(theme.color(Role::Accent))
        .bg(bg)
}

/// The cap glyph style over the page background (the common case).
fn cap_style(theme: crate::theme::Theme) -> ratatui::style::Style {
    cap_style_on(theme, theme.bg.unwrap_or(ratatui::style::Color::Reset))
}

/// Round the ends of an already-drawn selection highlight `bar` (its on-screen
/// rect): paint the caps in the one cell just left and just right of it. The
/// caller must leave those two margin cells free (inset the widget one column
/// each side). Used for the widgets that draw their own full-width highlight
/// (the book `Table`; see [`round_list`]).
pub fn round_bar(f: &mut Frame, bar: Rect, theme: crate::theme::Theme) {
    let cap = cap_style(theme);
    let right_x = bar.x + bar.width;
    let buf = f.buffer_mut();
    let max_x = buf.area().right();
    for row in 0..bar.height {
        let y = bar.y + row;
        if bar.x > 0 {
            buf.set_string(bar.x - 1, y, CAP_LEFT, cap);
        }
        if right_x < max_x {
            buf.set_string(right_x, y, CAP_RIGHT, cap);
        }
    }
}

/// Render a `List` (which draws its own full-width `highlight_style` bar) inset
/// one column each side so the rounded caps have room, then round the selected
/// row. The one call sites that use `List` + `ListState` should use instead of
/// `render_stateful_widget` + a bare `highlight_style`.
pub fn round_list(
    f: &mut Frame,
    area: Rect,
    list: ratatui::widgets::List<'_>,
    state: &mut ratatui::widgets::ListState,
    theme: crate::theme::Theme,
) {
    let inset = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    f.render_stateful_widget(list, inset, state);
    if let Some(sel) = state.selected() {
        let off = state.offset();
        if sel >= off {
            let sy = inset.y + (sel - off) as u16;
            if sy < inset.y + inset.height {
                round_bar(
                    f,
                    Rect {
                        x: inset.x,
                        y: sy,
                        width: inset.width,
                        height: 1,
                    },
                    theme,
                );
            }
        }
    }
}

/// A full-width rounded selection bar as a standalone `Line`: `text` on the accent
/// fill, padded/truncated to `width`, with rounded caps at the ends. For the
/// manual (`Paragraph`) selected rows — the TOC, palette, settings, shelf picker.
pub fn rounded_line(
    text: impl Into<String>,
    width: u16,
    theme: crate::theme::Theme,
) -> ratatui::text::Line<'static> {
    use crate::theme::Role;
    use ratatui::text::{Line, Span};

    let inner_w = (width as usize).saturating_sub(2);
    let mut s = text.into();
    let n = s.chars().count();
    if n > inner_w {
        s = s.chars().take(inner_w).collect();
    } else {
        s.extend(std::iter::repeat_n(' ', inner_w - n));
    }
    Line::from(vec![
        Span::styled(CAP_LEFT, cap_style(theme)),
        Span::styled(s, theme.style(Role::Selection)),
        Span::styled(CAP_RIGHT, cap_style(theme)),
    ])
}

/// A content-hugging rounded pill (caps snug around `text`) for inline selections
/// that aren't full-width rows — a tab strip, a status pill. `cap_bg` is the
/// colour behind the pill (the page, the status bar, the editor paper) so the
/// rounded ends blend into it.
pub fn pill_spans_on(
    text: impl Into<String>,
    theme: crate::theme::Theme,
    cap_bg: ratatui::style::Color,
) -> Vec<ratatui::text::Span<'static>> {
    use crate::theme::Role;
    use ratatui::text::Span;
    let cap = cap_style_on(theme, cap_bg);
    vec![
        Span::styled(CAP_LEFT, cap),
        Span::styled(format!(" {} ", text.into()), theme.style(Role::Selection)),
        Span::styled(CAP_RIGHT, cap),
    ]
}

/// A content-hugging rounded pill over the page background (the common case).
pub fn pill_spans(
    text: impl Into<String>,
    theme: crate::theme::Theme,
) -> Vec<ratatui::text::Span<'static>> {
    pill_spans_on(
        text,
        theme,
        theme.bg.unwrap_or(ratatui::style::Color::Reset),
    )
}

/// A centered rect of at most `w`×`h`, clamped to `area` (shared by the popups).
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(1);
    let h = h.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// The standard **compact** size every bordered overlay window opens at.
pub const OVERLAY_COMPACT: (u16, u16) = (80, 24);

/// The centered rect for a bordered overlay window: one standard compact size for
/// all of them, or a single larger size (~94 %×92 % of the screen) when `large`
/// is set (toggled with `f`). Clamped to the screen by [`centered`], so it stays
/// valid on a small terminal. All overlay renderers — and [`overlay_occlusion`] —
/// go through this, so size, centering, and mouse/occlusion never drift apart.
pub fn overlay_rect(area: Rect, large: bool) -> Rect {
    if large {
        let w = (u32::from(area.width) * 94 / 100) as u16;
        let h = (u32::from(area.height) * 92 / 100) as u16;
        centered(area, w, h)
    } else {
        centered(area, OVERLAY_COMPACT.0, OVERLAY_COMPACT.1)
    }
}

pub fn render(f: &mut Frame, app: &mut App) {
    // Hit rects are rebuilt every frame by the renderers below.
    app.mouse.clear();
    // Tell the reader which region an open overlay covers, so it can skip inline
    // images whose left edge the popup would clobber (they'd otherwise leave a
    // black box). Computed before the reader draws.
    let occlude = overlay_occlusion(f.area(), app);
    if let Some(r) = app.reader.as_mut() {
        r.overlay_occlude = occlude;
    }
    match app.mode {
        Mode::Reader => reader::render(f, app),
        Mode::Library => library::render(f, app),
    }
    if matches!(app.overlay, Overlay::Settings(_)) {
        settings::render(f, app);
    }
    annotations::render(f, app);
    if matches!(app.overlay, Overlay::ImageView(_)) {
        image::render(f, app);
    }
    if matches!(app.overlay, Overlay::MetaEdit(_)) {
        meta_edit::render(f, app);
    }
    if matches!(app.overlay, Overlay::ShelfPicker(_)) {
        shelf_picker::render(f, app);
    }
    if matches!(app.overlay, Overlay::BulkRename(_)) {
        bulk_rename::render(f, app);
    }
    if matches!(app.overlay, Overlay::Stats(_)) {
        stats::render(f, app);
    }
    if matches!(app.overlay, Overlay::Palette(_)) {
        palette::render(f, app);
    }
    if matches!(app.overlay, Overlay::WordLookup(_)) {
        word_lookup::render(f, app);
    }
    if matches!(app.overlay, Overlay::DupResolve(_)) {
        dup_resolve::render(f, app);
    }
    if matches!(app.overlay, Overlay::IgnoredView(_)) {
        dup_resolve::render_ignored(f, app);
    }
    // An open overlay shows its shortcuts on the shared bottom status row,
    // drawn last so it sits above the popup (which never reaches that row).
    let a = f.area();
    let bottom = Rect {
        x: a.x,
        y: a.y + a.height.saturating_sub(1),
        width: a.width,
        height: 1,
    };
    status::overlay(f, bottom, app, app.config.theme);
}

/// Truncate `s` to at most `max` display chars, with an ellipsis (shared by the
/// list/popup views).
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Render an editable text value windowed to `width` cells with a block cursor
/// at `caret`, so the caret stays visible no matter how long the value is
/// (a leading `…` marks text scrolled off the left). The single horizontal-
/// scroll primitive shared by every inline text field — editor fields, the
/// search bar, the rename template, and the collection name editor.
pub fn field_spans(
    val: &str,
    caret: usize,
    width: usize,
    theme: crate::theme::Theme,
) -> Vec<ratatui::text::Span<'static>> {
    use crate::theme::Role;
    use ratatui::text::Span;

    let chars: Vec<char> = val.chars().collect();
    let len = chars.len();
    let caret = caret.min(len);
    let win = width.max(2);
    // Anchor the window so the caret sits at its right edge — guarantees the
    // caret (and the text being typed) is always on screen.
    let start = (caret + 1).saturating_sub(win);
    let text = theme.style(Role::Heading);
    let cursor = theme.style(Role::Selection);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if start > 0 {
        spans.push(Span::styled("…", theme.style(Role::Muted)));
    }
    let end = (start + win).min(len);
    for (idx, ch) in chars.iter().enumerate().take(end).skip(start) {
        let st = if idx == caret { cursor } else { text };
        spans.push(Span::styled(ch.to_string(), st));
    }
    if caret >= len {
        spans.push(Span::styled(" ".to_string(), cursor)); // caret past the last char
    }
    spans
}
