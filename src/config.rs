//! Settings model.
//!
//! For now this is in-memory defaults only. TOML persistence under
//! `~/.config/delryn/config.toml` and the mode-scoped `;` settings popup land
//! with the store/settings modules — see `DESIGN.md` §7–8.

use serde::{Deserialize, Serialize};

use crate::theme::{self, Theme};

/// Which segments the status bar shows (title is always shown).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StatusFields {
    pub theme: bool,
    pub view: bool,
    pub position: bool,
    pub percent: bool,
    pub gauge: bool,
}

impl Default for StatusFields {
    fn default() -> Self {
        Self {
            theme: true,
            view: true,
            position: true,
            percent: true,
            gauge: true,
        }
    }
}

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

    pub fn prev(self) -> Self {
        match self {
            ViewMode::Center => ViewMode::TwoPage,
            ViewMode::Fill => ViewMode::Center,
            ViewMode::TwoPage => ViewMode::Fill,
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

/// Bounds for the adjustable content measure.
pub const MIN_MEASURE: u16 = 40;
pub const MAX_MEASURE: u16 = 120;
/// Maximum extra blank lines between text lines.
pub const MAX_LINE_SPACING: u8 = 3;

#[derive(Debug, Clone)]
pub struct Config {
    /// Maximum text column ("measure"); body text is centered within it.
    pub measure_width: u16,
    /// Extra blank lines between wrapped text lines (0 = single-spaced).
    pub line_spacing: u8,
    /// Blank lines between blocks/paragraphs.
    pub paragraph_spacing: u8,
    pub view_mode: ViewMode,
    pub theme: Theme,
    pub show_sidebar: bool,
    pub show_status: bool,
    /// Distraction-free: hide chrome regardless of the show_* flags.
    pub focus_mode: bool,
    pub mouse_enabled: bool,
    pub status: StatusFields,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            measure_width: 72,
            line_spacing: 0,
            paragraph_spacing: 1,
            view_mode: ViewMode::Center,
            theme: theme::default_theme(),
            show_sidebar: true,
            show_status: true,
            focus_mode: false,
            mouse_enabled: true,
            status: StatusFields::default(),
        }
    }
}

/// On-disk mirror of the global defaults (`~/.config/delryn/config.toml`).
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct ConfigFile {
    measure_width: u16,
    line_spacing: u8,
    paragraph_spacing: u8,
    view_mode: String,
    theme: String,
    show_sidebar: bool,
    show_status: bool,
    mouse_enabled: bool,
    status: StatusFields,
}

impl Default for ConfigFile {
    fn default() -> Self {
        let c = Config::default();
        Self {
            measure_width: c.measure_width,
            line_spacing: c.line_spacing,
            paragraph_spacing: c.paragraph_spacing,
            view_mode: c.view_mode.label().to_string(),
            theme: c.theme.name.to_string(),
            show_sidebar: c.show_sidebar,
            show_status: c.show_status,
            mouse_enabled: c.mouse_enabled,
            status: c.status,
        }
    }
}

fn config_path() -> std::path::PathBuf {
    crate::store::config_dir().join("config.toml")
}

impl Config {
    /// Load global defaults from `config.toml`, falling back to built-ins.
    pub fn load() -> Config {
        let mut c = Config::default();
        let Ok(text) = std::fs::read_to_string(config_path()) else {
            return c;
        };
        let Ok(cf) = toml::from_str::<ConfigFile>(&text) else {
            return c;
        };
        c.measure_width = cf.measure_width.clamp(MIN_MEASURE, MAX_MEASURE);
        c.line_spacing = cf.line_spacing.min(MAX_LINE_SPACING);
        c.paragraph_spacing = cf.paragraph_spacing.min(3);
        c.view_mode = ViewMode::from_label(&cf.view_mode);
        if let Some(t) = theme::by_name(&cf.theme) {
            c.theme = t;
        }
        c.show_sidebar = cf.show_sidebar;
        c.show_status = cf.show_status;
        c.mouse_enabled = cf.mouse_enabled;
        c.status = cf.status;
        c
    }

    /// Persist the current settings as the global defaults (best-effort).
    pub fn save(&self) {
        let cf = ConfigFile {
            measure_width: self.measure_width,
            line_spacing: self.line_spacing,
            paragraph_spacing: self.paragraph_spacing,
            view_mode: self.view_mode.label().to_string(),
            theme: self.theme.name.to_string(),
            show_sidebar: self.show_sidebar,
            show_status: self.show_status,
            mouse_enabled: self.mouse_enabled,
            status: self.status,
        };
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = toml::to_string_pretty(&cf) {
            let _ = std::fs::write(path, text);
        }
    }
}
