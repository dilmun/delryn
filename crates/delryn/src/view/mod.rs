//! Render dispatch by mode. The view layer is format-agnostic — it only ever
//! sees the `Document` model and app state. See `DESIGN.md` §2.

pub mod annotations;
pub mod bulk_rename;
pub mod image;
pub mod library;
pub mod meta_edit;
pub mod palette;
pub mod reader;
pub mod settings;
pub mod shelf_picker;
pub mod stats;
pub mod status;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::{App, Mode};

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

/// The exact screen rect an open blocking overlay covers this frame, if any —
/// matched to each popup's own `centered(...)` geometry (keep in sync with the
/// corresponding `*::render`). The full-screen image viewer covers everything.
fn overlay_occlusion(area: Rect, app: &App) -> Option<Rect> {
    if app.image_view.is_some() {
        return Some(area);
    }
    if app.settings.is_some() {
        return Some(centered(area, 64, 28));
    }
    if let Some(p) = app.palette.as_ref() {
        let visible = p.filtered().len().min(10) as u16;
        return Some(centered(area, 56, visible + 3));
    }
    if let Some(s) = app.stats.as_ref() {
        return Some(centered(
            area,
            46,
            (10 + s.top_authors.len() as u16).min(22),
        ));
    }
    if app.annot.is_some() {
        return Some(centered(area, 74, 20));
    }
    None
}

/// The app's standard responsive sidebar split. The sidebar takes `pct`% of the
/// width, clamped to `[min, max]` cells; below `collapse_below` columns it
/// collapses (returns `None`) so the main pane keeps the room. A one-cell gap
/// separates the two. Every multi-pane view uses this so layouts stay consistent
/// and reflow with the window.
pub fn sidebar_split(
    area: Rect,
    pct: u16,
    min: u16,
    max: u16,
    collapse_below: u16,
) -> (Option<Rect>, Rect) {
    if area.width < collapse_below {
        return (None, area);
    }
    let want = (area.width.saturating_mul(pct) / 100).clamp(min, max);
    // Always leave the main pane at least half the width.
    let w = want.min(area.width / 2);
    let cols = Layout::horizontal([
        Constraint::Length(w),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);
    (Some(cols[0]), cols[2])
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
    if app.settings.is_some() {
        settings::render(f, app);
    }
    annotations::render(f, app);
    if app.image_view.is_some() {
        image::render(f, app);
    }
    if app.meta_edit.is_some() {
        meta_edit::render(f, app);
    }
    if app.shelf_picker.is_some() {
        shelf_picker::render(f, app);
    }
    if app.bulk_rename.is_some() {
        bulk_rename::render(f, app);
    }
    if app.stats.is_some() {
        stats::render(f, app);
    }
    if app.palette.is_some() {
        palette::render(f, app);
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
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;

    let chars: Vec<char> = val.chars().collect();
    let len = chars.len();
    let caret = caret.min(len);
    let win = width.max(2);
    // Anchor the window so the caret sits at its right edge — guarantees the
    // caret (and the text being typed) is always on screen.
    let start = (caret + 1).saturating_sub(win);
    let text = Style::default()
        .fg(theme.heading)
        .add_modifier(Modifier::BOLD);
    let cursor = Style::default()
        .fg(theme.on_accent())
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if start > 0 {
        spans.push(Span::styled("…", Style::default().fg(theme.muted)));
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
