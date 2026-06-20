//! Settings model.
//!
//! For now this is in-memory defaults only. TOML persistence under
//! `~/.config/delryn/config.toml` and the mode-scoped `;` settings popup land
//! with the store/settings modules — see `DESIGN.md` §7–8.

use crate::theme::{self, Theme};

/// How body text is laid out in the content pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Measure-capped column, centered with gutters.
    Center,
    /// Text fills the pane width (minus a thin gutter).
    Fill,
    /// Two side-by-side columns — a two-page spread.
    TwoPage,
}

impl ViewMode {
    pub fn next(self) -> Self {
        match self {
            ViewMode::Center => ViewMode::Fill,
            ViewMode::Fill => ViewMode::TwoPage,
            ViewMode::TwoPage => ViewMode::Center,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Center => "center",
            ViewMode::Fill => "fill",
            ViewMode::TwoPage => "two-page",
        }
    }

    pub fn from_label(s: &str) -> ViewMode {
        match s {
            "fill" => ViewMode::Fill,
            "two-page" => ViewMode::TwoPage,
            _ => ViewMode::Center,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum text column ("measure"); body text is centered within it.
    pub measure_width: u16,
    pub view_mode: ViewMode,
    pub theme: Theme,
    pub show_sidebar: bool,
    pub show_status: bool,
    pub mouse_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            measure_width: 72,
            view_mode: ViewMode::Center,
            theme: theme::default_theme(),
            show_sidebar: true,
            show_status: true,
            mouse_enabled: true,
        }
    }
}
