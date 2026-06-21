//! Render dispatch by mode. The view layer is format-agnostic — it only ever
//! sees the `Document` model and app state. See `DESIGN.md` §2.

pub mod annotations;
pub mod image;
pub mod library;
pub mod meta_edit;
pub mod reader;
pub mod settings;

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::{App, Mode};

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
}
