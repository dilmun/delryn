//! Render dispatch by mode. The view layer is format-agnostic — it only ever
//! sees the `Document` model and app state. See `DESIGN.md` §2.

pub mod library;
pub mod reader;
pub mod settings;

use ratatui::Frame;

use crate::app::{App, Mode};

pub fn render(f: &mut Frame, app: &mut App) {
    match app.mode {
        Mode::Reader => reader::render(f, app),
        Mode::Library => library::render(f, app),
    }
    if app.settings.is_some() {
        settings::render(f, app);
    }
}
