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
use ratatui::layout::Rect;

use crate::app::{App, Mode};

/// Resolve the theme's text + page colours into an [`media::Ink`] for recolouring
/// math/line-art images. Unknown colours (the default theme's terminal-default
/// `Reset`/`None`) fall back to a black-ink-on-white-page matte — the publisher's
/// intended look, always legible. The two colours are forced apart if they'd
/// otherwise be too close to read.
pub fn theme_ink(theme: crate::theme::Theme) -> crate::media::Ink {
    let ink = rgb_of(theme.fg).unwrap_or([0, 0, 0]);
    let paper = theme.bg.and_then(rgb_of).unwrap_or([255, 255, 255]);
    // Guarantee contrast: if ink and paper are close in luminance, snap ink to
    // black or white opposite the paper.
    let lum = |c: [u8; 3]| 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32;
    let (ink, paper) = if (lum(ink) - lum(paper)).abs() < 64.0 {
        let opposite = if lum(paper) < 128.0 {
            [235, 235, 235]
        } else {
            [20, 20, 20]
        };
        (opposite, paper)
    } else {
        (ink, paper)
    };
    crate::media::Ink { ink, paper }
}

/// Concrete sRGB for a ratatui [`Color`], or `None` for terminal-relative colours
/// (`Reset`, palette indices) whose true RGB we can't know.
fn rgb_of(c: ratatui::style::Color) -> Option<[u8; 3]> {
    use ratatui::style::Color::*;
    Some(match c {
        Rgb(r, g, b) => [r, g, b],
        Black => [0, 0, 0],
        White => [238, 238, 238],
        Red => [205, 49, 49],
        Green => [13, 188, 121],
        Yellow => [229, 229, 16],
        Blue => [36, 114, 200],
        Magenta => [188, 63, 188],
        Cyan => [17, 168, 205],
        Gray => [180, 180, 180],
        DarkGray => [102, 102, 102],
        LightRed => [241, 76, 76],
        LightGreen => [35, 209, 139],
        LightYellow => [245, 245, 67],
        LightBlue => [59, 142, 234],
        LightMagenta => [214, 112, 214],
        LightCyan => [41, 184, 219],
        Reset | Indexed(_) => return None,
    })
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
    use ratatui::style::{Color, Modifier, Style};
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
        .fg(theme.bg.unwrap_or(Color::Black))
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
