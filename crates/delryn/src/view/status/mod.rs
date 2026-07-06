//! The unified bottom status bar.
//!
//! One renderer over a segment model: each context — the reader, the library, or
//! the active overlay — produces a [`StatusBar`] of zoned, prioritised segments
//! (state/context Left, fields and key hints Right); [`render`](render::render)
//! packs them and drops the lowest-priority segments first when the row is too
//! narrow. Replaces the former three split renderers (the reader's
//! `render_status`, the library status, and the overlay `legend` cascade).

mod clock;
mod producers;
mod render;
mod segment;

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::{App, Reader};
use crate::config::Config;
use crate::theme::Theme;

/// Draw the reader's status bar (title/flash · search · page/position · gauge).
pub fn render_reader(f: &mut Frame, area: Rect, reader: &Reader, config: &Config, theme: Theme) {
    render::render(
        f,
        area,
        &producers::reader_bar(reader, config, theme),
        theme,
        &config.status,
    );
}

/// Draw the library's status bar (context/selection · key hints).
pub fn render_library(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    render::render(
        f,
        area,
        &producers::library_bar(app, theme),
        theme,
        &app.config.status,
    );
}

/// Draw the active overlay's status (context + key hints) over the bottom row,
/// when an overlay is open.
pub fn overlay(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    if let Some(bar) = producers::overlay_bar(app, theme) {
        render::render(f, area, &bar, theme, &app.config.status);
    }
}
