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

/// How the library lists books: a metadata table, a dense table, or a cover grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibLayout {
    List,
    Compact,
    Grid,
}

impl LibLayout {
    pub fn next(self) -> Self {
        match self {
            LibLayout::List => LibLayout::Compact,
            LibLayout::Compact => LibLayout::Grid,
            LibLayout::Grid => LibLayout::List,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            LibLayout::List => LibLayout::Grid,
            LibLayout::Compact => LibLayout::List,
            LibLayout::Grid => LibLayout::Compact,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LibLayout::List => "list",
            LibLayout::Compact => "compact",
            LibLayout::Grid => "grid",
        }
    }

    pub fn from_label(s: &str) -> LibLayout {
        match s {
            "compact" => LibLayout::Compact,
            "grid" => LibLayout::Grid,
            _ => LibLayout::List,
        }
    }
}

/// Cover-card size for the library grid view. Card dimensions are in terminal
/// cells, sized ~4:3 (cols:rows) so a typical 2:3 portrait cover fills the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridSize {
    Small,
    Medium,
    Large,
}

impl GridSize {
    pub fn next(self) -> Self {
        match self {
            GridSize::Small => GridSize::Medium,
            GridSize::Medium => GridSize::Large,
            GridSize::Large => GridSize::Large,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            GridSize::Small => GridSize::Small,
            GridSize::Medium => GridSize::Small,
            GridSize::Large => GridSize::Medium,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GridSize::Small => "small",
            GridSize::Medium => "medium",
            GridSize::Large => "large",
        }
    }

    pub fn from_label(s: &str) -> GridSize {
        match s {
            "small" => GridSize::Small,
            "large" => GridSize::Large,
            _ => GridSize::Medium,
        }
    }

    /// Cover-card width × height in cells (excludes the gutter and title rows).
    pub fn card(self) -> (u16, u16) {
        match self {
            GridSize::Small => (12, 9),
            GridSize::Medium => (16, 12),
            GridSize::Large => (22, 16),
        }
    }
}

/// Bounds for the per-side text padding (percent of the content pane width).
pub const MAX_SIDE_PADDING: u16 = 40;
/// Smallest text column we'll ever wrap to, so heavy padding on a narrow
/// terminal still leaves a readable line.
pub const MIN_TEXT_COLS: u16 = 20;
/// Maximum extra blank lines between text lines.
pub const MAX_LINE_SPACING: u8 = 3;
/// Upper bound for the inline-image resolution cap (longest side, px). `0` means
/// no cap — images fill the text column. A cap trades size for a faster transmit
/// to the terminal.
pub const MAX_IMAGE_PX: u16 = 4096;

#[derive(Debug, Clone)]
pub struct Config {
    /// Text padding from each edge, as a percent of the content pane width, so
    /// the reading column scales with the window (Center mode).
    pub side_padding: u16,
    /// Extra blank lines between wrapped text lines (0 = single-spaced).
    pub line_spacing: u8,
    /// Blank lines between blocks/paragraphs.
    pub paragraph_spacing: u8,
    pub view_mode: ViewMode,
    pub theme: Theme,
    /// Soft-wrap code blocks to the column (true) vs. keep lines intact and
    /// scroll horizontally (false).
    pub code_wrap: bool,
    /// Keep scrolling within the current chapter (true) instead of flowing into
    /// the next/previous one at the edges.
    pub chapter_lock: bool,
    pub show_sidebar: bool,
    pub show_status: bool,
    /// Distraction-free: hide chrome regardless of the show_* flags.
    pub focus_mode: bool,
    pub mouse_enabled: bool,
    pub status: StatusFields,
    /// Max inline-image resolution (longest side, px). Caps the data sent to the
    /// terminal so big figures don't stall scrolling.
    pub image_max_px: u16,
    /// Directories scanned for the library.
    pub library_paths: Vec<String>,
    /// How the library lists books (table / dense table / cover grid).
    pub library_layout: LibLayout,
    /// Cover-card size for the grid view.
    pub library_grid_size: GridSize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            side_padding: 6,
            line_spacing: 0,
            paragraph_spacing: 1,
            view_mode: ViewMode::Center,
            theme: theme::default_theme(),
            code_wrap: true,
            chapter_lock: false,
            show_sidebar: true,
            show_status: true,
            focus_mode: false,
            mouse_enabled: true,
            status: StatusFields::default(),
            image_max_px: 0, // no cap by default — images fill the text column
            library_paths: Vec::new(),
            library_layout: LibLayout::List,
            library_grid_size: GridSize::Medium,
        }
    }
}

/// On-disk mirror of the global defaults (`~/.config/delryn/config.toml`).
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct ConfigFile {
    side_padding: u16,
    line_spacing: u8,
    paragraph_spacing: u8,
    view_mode: String,
    theme: String,
    code_wrap: bool,
    chapter_lock: bool,
    show_sidebar: bool,
    show_status: bool,
    mouse_enabled: bool,
    status: StatusFields,
    image_max_px: u16,
    library_paths: Vec<String>,
    library_layout: String,
    library_grid_size: String,
}

impl Default for ConfigFile {
    fn default() -> Self {
        let c = Config::default();
        Self {
            side_padding: c.side_padding,
            line_spacing: c.line_spacing,
            paragraph_spacing: c.paragraph_spacing,
            view_mode: c.view_mode.label().to_string(),
            theme: c.theme.name.to_string(),
            code_wrap: c.code_wrap,
            chapter_lock: c.chapter_lock,
            show_sidebar: c.show_sidebar,
            show_status: c.show_status,
            mouse_enabled: c.mouse_enabled,
            status: c.status,
            image_max_px: c.image_max_px,
            library_paths: c.library_paths,
            library_layout: c.library_layout.label().to_string(),
            library_grid_size: c.library_grid_size.label().to_string(),
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
        c.side_padding = cf.side_padding.min(MAX_SIDE_PADDING);
        c.line_spacing = cf.line_spacing.min(MAX_LINE_SPACING);
        c.paragraph_spacing = cf.paragraph_spacing.min(3);
        c.view_mode = ViewMode::from_label(&cf.view_mode);
        if let Some(t) = theme::by_name(&cf.theme) {
            c.theme = t;
        }
        c.code_wrap = cf.code_wrap;
        c.chapter_lock = cf.chapter_lock;
        c.show_sidebar = cf.show_sidebar;
        c.show_status = cf.show_status;
        c.mouse_enabled = cf.mouse_enabled;
        c.status = cf.status;
        c.image_max_px = cf.image_max_px.min(MAX_IMAGE_PX);
        c.library_paths = cf.library_paths;
        c.library_layout = LibLayout::from_label(&cf.library_layout);
        c.library_grid_size = GridSize::from_label(&cf.library_grid_size);
        c
    }

    /// Persist the current settings as the global defaults (best-effort).
    pub fn save(&self) {
        let cf = ConfigFile {
            side_padding: self.side_padding,
            line_spacing: self.line_spacing,
            paragraph_spacing: self.paragraph_spacing,
            view_mode: self.view_mode.label().to_string(),
            theme: self.theme.name.to_string(),
            code_wrap: self.code_wrap,
            chapter_lock: self.chapter_lock,
            show_sidebar: self.show_sidebar,
            show_status: self.show_status,
            mouse_enabled: self.mouse_enabled,
            status: self.status,
            image_max_px: self.image_max_px,
            library_paths: self.library_paths.clone(),
            library_layout: self.library_layout.label().to_string(),
            library_grid_size: self.library_grid_size.label().to_string(),
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
