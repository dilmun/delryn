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

/// How body text is laid out in the content pane. Both layouts use the same
/// per-side edge padding (`side_padding`); two-page adds a configurable gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// A single column, centered within the side padding.
    Center,
    /// Two side-by-side columns — a two-page spread.
    TwoPage,
}

impl ViewMode {
    pub fn next(self) -> Self {
        match self {
            ViewMode::Center => ViewMode::TwoPage,
            ViewMode::TwoPage => ViewMode::Center,
        }
    }

    pub fn prev(self) -> Self {
        self.next()
    }

    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Center => "center",
            ViewMode::TwoPage => "two-page",
        }
    }

    pub fn from_label(s: &str) -> ViewMode {
        match s {
            "two-page" => ViewMode::TwoPage,
            _ => ViewMode::Center,
        }
    }
}

/// A reading-experience preset: a named bundle of layout / chrome / flow
/// settings. `Custom` is the derived state when the live settings match no
/// preset (e.g. after the reader has tweaked an individual setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingMode {
    Custom,
    Study,
    Research,
    Presentation,
}

/// The reading settings a preset bundles. Every preset fixes all of these, so a
/// live config can be compared field-for-field to recognise the active preset.
/// Deliberately excludes `view_mode` (Center / TwoPage): the page layout is a
/// personal choice a preset shouldn't yank out from under the reader.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ReadingProfile {
    side_padding: u16,
    line_spacing: u8,
    paragraph_spacing: u8,
    show_sidebar: bool,
    show_status: bool,
    chapter_lock: bool,
    paged: bool,
}

impl ReadingMode {
    pub fn label(self) -> &'static str {
        match self {
            ReadingMode::Custom => "custom",
            ReadingMode::Study => "study",
            ReadingMode::Research => "research",
            ReadingMode::Presentation => "presentation",
        }
    }

    /// Next/previous *applyable* preset. Cycles through the three real presets;
    /// from `Custom` it enters the cycle rather than landing back on `Custom`.
    pub fn next(self) -> Self {
        match self {
            ReadingMode::Custom | ReadingMode::Presentation => ReadingMode::Study,
            ReadingMode::Study => ReadingMode::Research,
            ReadingMode::Research => ReadingMode::Presentation,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ReadingMode::Custom | ReadingMode::Study => ReadingMode::Presentation,
            ReadingMode::Research => ReadingMode::Study,
            ReadingMode::Presentation => ReadingMode::Research,
        }
    }

    /// The settings this preset stands for (`None` for `Custom`).
    fn profile(self) -> Option<ReadingProfile> {
        let p = match self {
            ReadingMode::Custom => return None,
            // Deep, careful reading of one chapter: comfortable margins + spacing,
            // navigation + progress visible, stay put in the chapter.
            ReadingMode::Study => ReadingProfile {
                side_padding: 10,
                line_spacing: 1,
                paragraph_spacing: 1,
                show_sidebar: true,
                show_status: true,
                chapter_lock: true,
                paged: false,
            },
            // Scanning / cross-referencing across the whole book: denser and a
            // touch wider than default, flows freely between chapters.
            ReadingMode::Research => ReadingProfile {
                side_padding: 4,
                line_spacing: 0,
                paragraph_spacing: 1,
                show_sidebar: true,
                show_status: true,
                chapter_lock: false,
                paged: false,
            },
            // Distraction-free, slide-like: wide airy margins, no chrome, page flips.
            ReadingMode::Presentation => ReadingProfile {
                side_padding: 18,
                line_spacing: 1,
                paragraph_spacing: 2,
                show_sidebar: false,
                show_status: false,
                chapter_lock: false,
                paged: true,
            },
        };
        Some(p)
    }
}

/// How the library lists books: a metadata table, a dense table, a cover-card
/// grid, or an immersive cover wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibLayout {
    List,
    Compact,
    Grid,
    Wall,
}

impl LibLayout {
    pub fn next(self) -> Self {
        match self {
            LibLayout::List => LibLayout::Compact,
            LibLayout::Compact => LibLayout::Grid,
            LibLayout::Grid => LibLayout::Wall,
            LibLayout::Wall => LibLayout::List,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            LibLayout::List => LibLayout::Wall,
            LibLayout::Compact => LibLayout::List,
            LibLayout::Grid => LibLayout::Compact,
            LibLayout::Wall => LibLayout::Grid,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LibLayout::List => "list",
            LibLayout::Compact => "compact",
            LibLayout::Grid => "grid",
            LibLayout::Wall => "wall",
        }
    }

    pub fn from_label(s: &str) -> LibLayout {
        match s {
            "compact" => LibLayout::Compact,
            "grid" => LibLayout::Grid,
            "wall" => LibLayout::Wall,
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
    XLarge,
}

impl GridSize {
    pub fn next(self) -> Self {
        match self {
            GridSize::Small => GridSize::Medium,
            GridSize::Medium => GridSize::Large,
            GridSize::Large => GridSize::XLarge,
            GridSize::XLarge => GridSize::XLarge,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            GridSize::Small => GridSize::Small,
            GridSize::Medium => GridSize::Small,
            GridSize::Large => GridSize::Medium,
            GridSize::XLarge => GridSize::Large,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GridSize::Small => "small",
            GridSize::Medium => "medium",
            GridSize::Large => "large",
            GridSize::XLarge => "xlarge",
        }
    }

    pub fn from_label(s: &str) -> GridSize {
        match s {
            "small" => GridSize::Small,
            "large" => GridSize::Large,
            "xlarge" => GridSize::XLarge,
            _ => GridSize::Medium,
        }
    }

    /// Cover-card width × height in cells (excludes the gutter and title rows).
    pub fn card(self) -> (u16, u16) {
        match self {
            GridSize::Small => (12, 9),
            GridSize::Medium => (16, 12),
            GridSize::Large => (22, 16),
            GridSize::XLarge => (30, 22),
        }
    }
}

/// How book images are adapted to the active theme. See `DESIGN.md` §7 and the
/// "Theming & content coherence" plan. The mode is part of the image cache key,
/// so changing it re-renders on the fly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageMode {
    /// Smart, per-content: recolour line-art/equations to the theme, keep
    /// pictures faithful (transparency flattened onto the page). The best general
    /// default.
    #[default]
    Auto,
    /// Auto, plus lightness-invert opaque light-background figures (charts,
    /// diagrams, screenshots) so they're dark-friendly with detail intact. True
    /// photos that happen to be light-backed invert too — the trade for comfort.
    InvertBackgrounds,
    /// Never recolour or invert; only flatten transparency onto the page so
    /// nothing is invisible. Original colours preserved (equations keep their ink
    /// colour, which may be faint on a dark theme).
    Faithful,
}

impl ImageMode {
    pub fn next(self) -> Self {
        match self {
            ImageMode::Auto => ImageMode::InvertBackgrounds,
            ImageMode::InvertBackgrounds => ImageMode::Faithful,
            ImageMode::Faithful => ImageMode::Auto,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ImageMode::Auto => ImageMode::Faithful,
            ImageMode::InvertBackgrounds => ImageMode::Auto,
            ImageMode::Faithful => ImageMode::InvertBackgrounds,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ImageMode::Auto => "auto",
            ImageMode::InvertBackgrounds => "invert",
            ImageMode::Faithful => "faithful",
        }
    }

    pub fn from_label(s: &str) -> ImageMode {
        match s {
            "invert" => ImageMode::InvertBackgrounds,
            "faithful" => ImageMode::Faithful,
            _ => ImageMode::Auto,
        }
    }
}

/// The optional library list columns (key, display label), in display order.
/// The star + Title columns are always shown; these can be toggled and also
/// drop automatically on a narrow window.
pub const LIB_COLUMNS: [(&str, &str); 8] = [
    ("author", "Author"),
    ("year", "Year"),
    ("type", "Type"),
    ("source", "Source"),
    ("progress", "Progress"),
    ("size", "Size"),
    ("status", "Status"),
    ("tags", "Tags"),
];

/// Bounds for the per-side text padding (percent of the content pane width).
pub const MAX_SIDE_PADDING: u16 = 40;
/// Upper bound for the two-page column gap (cells).
pub const MAX_PAGE_GAP: u16 = 16;
/// Smallest text column we'll ever wrap to, so heavy padding on a narrow
/// terminal still leaves a readable line.
pub const MIN_TEXT_COLS: u16 = 20;
/// Maximum extra blank lines between text lines.
pub const MAX_LINE_SPACING: u8 = 3;
/// Upper bound for the inline-image resolution cap (longest side, px). `0` means
/// no cap — images fill the text column. A cap trades size for a faster transmit
/// to the terminal.
pub const MAX_IMAGE_PX: u16 = 4096;
/// Bounds for the default figure width (% of the reading column). Figures without
/// an authored size are normalized to this so they read consistently across books.
pub const MIN_IMAGE_WIDTH_PCT: u16 = 20;
pub const MAX_IMAGE_WIDTH_PCT: u16 = 100;

#[derive(Debug, Clone)]
pub struct Config {
    /// Text padding from each edge, as a percent of the content pane width, so
    /// the reading column scales with the window. Applied in every view mode.
    pub side_padding: u16,
    /// Gap (in cells) between the two columns of the two-page spread.
    pub page_gap: u16,
    /// In two-page mode, show the first page alone (like a book cover), then pair
    /// (2,3), (4,5)… so facing pages line up as in a physical book.
    pub cover_offset: bool,
    /// Extra blank lines between wrapped text lines (0 = single-spaced).
    pub line_spacing: u8,
    /// Blank lines between blocks/paragraphs.
    pub paragraph_spacing: u8,
    pub view_mode: ViewMode,
    pub theme: Theme,
    /// Soft-wrap code blocks to the column (true) vs. keep lines intact and
    /// scroll horizontally (false).
    pub code_wrap: bool,
    /// Word-wrap table cells to their column (true) vs. truncate with `…` (false).
    pub table_wrap: bool,
    /// Fully justify body text to the column width (true) vs. ragged-right /
    /// left-aligned (false). The last line of a paragraph is never justified.
    pub justify: bool,
    /// Tidy converter artifacts in body text — collapse the stray space some
    /// EPUBs leave between a short styled variable and a hyphenated suffix
    /// (`t -distribution` → `t-distribution`). Leaves numbers and prose alone.
    pub tidy_spacing: bool,
    /// Paginated reading: vertical navigation flips whole pages snapped to page
    /// boundaries (true) vs. continuous line scrolling (false).
    pub paged: bool,
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
    /// Default figure display width as a percent of the reading column, for images
    /// without an authored size — normalizes figure sizes across books.
    pub image_width_pct: u16,
    /// How book images adapt to the theme (recolour / invert / faithful).
    pub image_mode: ImageMode,
    /// Directories scanned for the library.
    pub library_paths: Vec<String>,
    /// How the library lists books (table / dense table / cover grid).
    pub library_layout: LibLayout,
    /// Cover-card size for the grid view.
    pub library_grid_size: GridSize,
    /// Visible optional library columns (keys from [`LIB_COLUMNS`]); user-
    /// toggleable. The star + Title columns are always shown.
    pub library_columns: Vec<String>,
}

impl Config {
    /// Whether an optional library column is shown.
    pub fn column_on(&self, key: &str) -> bool {
        self.library_columns.iter().any(|c| c == key)
    }

    /// Show/hide an optional library column.
    pub fn toggle_column(&mut self, key: &str) {
        if let Some(i) = self.library_columns.iter().position(|c| c == key) {
            self.library_columns.remove(i);
        } else {
            self.library_columns.push(key.to_string());
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            side_padding: 6,
            page_gap: 5,
            cover_offset: false,
            line_spacing: 0,
            paragraph_spacing: 1,
            view_mode: ViewMode::Center,
            theme: theme::default_theme(),
            code_wrap: true,
            table_wrap: true,
            justify: false,
            tidy_spacing: true,
            paged: false,
            chapter_lock: false,
            show_sidebar: true,
            show_status: true,
            focus_mode: false,
            mouse_enabled: true,
            status: StatusFields::default(),
            image_max_px: 0,     // no cap by default — images fill the text column
            image_width_pct: 85, // normalize unsized figures to 85% of the column
            image_mode: ImageMode::default(),
            library_paths: Vec::new(),
            library_layout: LibLayout::List,
            library_grid_size: GridSize::Medium,
            library_columns: LIB_COLUMNS.iter().map(|(k, _)| k.to_string()).collect(),
        }
    }
}

/// On-disk mirror of the global defaults (`~/.config/delryn/config.toml`).
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct ConfigFile {
    side_padding: u16,
    page_gap: u16,
    cover_offset: bool,
    line_spacing: u8,
    paragraph_spacing: u8,
    view_mode: String,
    theme: String,
    code_wrap: bool,
    table_wrap: bool,
    justify: bool,
    tidy_spacing: bool,
    paged: bool,
    chapter_lock: bool,
    show_sidebar: bool,
    show_status: bool,
    mouse_enabled: bool,
    status: StatusFields,
    image_max_px: u16,
    image_width_pct: u16,
    image_mode: String,
    library_paths: Vec<String>,
    library_layout: String,
    library_grid_size: String,
    library_columns: Vec<String>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        let c = Config::default();
        Self {
            side_padding: c.side_padding,
            page_gap: c.page_gap,
            cover_offset: c.cover_offset,
            line_spacing: c.line_spacing,
            paragraph_spacing: c.paragraph_spacing,
            view_mode: c.view_mode.label().to_string(),
            theme: c.theme.name.to_string(),
            code_wrap: c.code_wrap,
            table_wrap: c.table_wrap,
            justify: c.justify,
            tidy_spacing: c.tidy_spacing,
            paged: c.paged,
            chapter_lock: c.chapter_lock,
            show_sidebar: c.show_sidebar,
            show_status: c.show_status,
            mouse_enabled: c.mouse_enabled,
            status: c.status,
            image_max_px: c.image_max_px,
            image_width_pct: c.image_width_pct,
            image_mode: c.image_mode.label().to_string(),
            library_paths: c.library_paths,
            library_layout: c.library_layout.label().to_string(),
            library_grid_size: c.library_grid_size.label().to_string(),
            library_columns: c.library_columns.clone(),
        }
    }
}

fn config_path() -> std::path::PathBuf {
    crate::paths::config_dir().join("config.toml")
}

impl Config {
    /// The preset the live reading settings correspond to, or `Custom` when they
    /// match none (derived, so it stays honest after any individual tweak).
    pub fn reading_mode(&self) -> ReadingMode {
        let current = ReadingProfile {
            side_padding: self.side_padding,
            line_spacing: self.line_spacing,
            paragraph_spacing: self.paragraph_spacing,
            show_sidebar: self.show_sidebar,
            show_status: self.show_status,
            chapter_lock: self.chapter_lock,
            paged: self.paged,
        };
        [
            ReadingMode::Study,
            ReadingMode::Research,
            ReadingMode::Presentation,
        ]
        .into_iter()
        .find(|m| m.profile() == Some(current))
        .unwrap_or(ReadingMode::Custom)
    }

    /// Apply a reading-mode preset to the live settings (a no-op for `Custom`).
    pub fn apply_reading_mode(&mut self, mode: ReadingMode) {
        let Some(p) = mode.profile() else {
            return;
        };
        self.side_padding = p.side_padding;
        self.line_spacing = p.line_spacing;
        self.paragraph_spacing = p.paragraph_spacing;
        self.show_sidebar = p.show_sidebar;
        self.show_status = p.show_status;
        self.chapter_lock = p.chapter_lock;
        self.paged = p.paged;
    }

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
        c.page_gap = cf.page_gap.min(MAX_PAGE_GAP);
        c.cover_offset = cf.cover_offset;
        c.line_spacing = cf.line_spacing.min(MAX_LINE_SPACING);
        c.paragraph_spacing = cf.paragraph_spacing.min(3);
        c.view_mode = ViewMode::from_label(&cf.view_mode);
        if let Some(t) = theme::by_name(&cf.theme) {
            c.theme = t;
        }
        c.code_wrap = cf.code_wrap;
        c.table_wrap = cf.table_wrap;
        c.justify = cf.justify;
        c.tidy_spacing = cf.tidy_spacing;
        c.paged = cf.paged;
        c.chapter_lock = cf.chapter_lock;
        c.show_sidebar = cf.show_sidebar;
        c.show_status = cf.show_status;
        c.mouse_enabled = cf.mouse_enabled;
        c.status = cf.status;
        c.image_max_px = cf.image_max_px.min(MAX_IMAGE_PX);
        c.image_width_pct = cf
            .image_width_pct
            .clamp(MIN_IMAGE_WIDTH_PCT, MAX_IMAGE_WIDTH_PCT);
        c.image_mode = ImageMode::from_label(&cf.image_mode);
        c.library_paths = cf.library_paths;
        c.library_layout = LibLayout::from_label(&cf.library_layout);
        c.library_grid_size = GridSize::from_label(&cf.library_grid_size);
        // Keep only known column keys; an empty list (all hidden) is valid.
        c.library_columns = cf
            .library_columns
            .into_iter()
            .filter(|k| LIB_COLUMNS.iter().any(|(key, _)| key == k))
            .collect();
        c
    }

    /// Persist the current settings as the global defaults (best-effort).
    pub fn save(&self) {
        let cf = ConfigFile {
            side_padding: self.side_padding,
            page_gap: self.page_gap,
            cover_offset: self.cover_offset,
            line_spacing: self.line_spacing,
            paragraph_spacing: self.paragraph_spacing,
            view_mode: self.view_mode.label().to_string(),
            theme: self.theme.name.to_string(),
            code_wrap: self.code_wrap,
            table_wrap: self.table_wrap,
            justify: self.justify,
            tidy_spacing: self.tidy_spacing,
            paged: self.paged,
            chapter_lock: self.chapter_lock,
            show_sidebar: self.show_sidebar,
            show_status: self.show_status,
            mouse_enabled: self.mouse_enabled,
            status: self.status,
            image_max_px: self.image_max_px,
            image_width_pct: self.image_width_pct,
            image_mode: self.image_mode.label().to_string(),
            library_paths: self.library_paths.clone(),
            library_layout: self.library_layout.label().to_string(),
            library_grid_size: self.library_grid_size.label().to_string(),
            library_columns: self.library_columns.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The library layout cycles through all four modes and round-trips through
    /// its persisted label.
    #[test]
    fn lib_layout_cycles_and_round_trips() {
        let order = [
            LibLayout::List,
            LibLayout::Compact,
            LibLayout::Grid,
            LibLayout::Wall,
        ];
        // next() walks the order and wraps; prev() is its inverse.
        for (i, &l) in order.iter().enumerate() {
            assert_eq!(l.next(), order[(i + 1) % order.len()]);
            assert_eq!(l.next().prev(), l);
            // label ↔ from_label round-trips.
            assert_eq!(LibLayout::from_label(l.label()), l);
        }
    }

    #[test]
    fn reading_mode_apply_and_derive() {
        let mut c = Config::default();
        // The built-in defaults are not a preset.
        assert_eq!(c.reading_mode(), ReadingMode::Custom);

        // Applying a preset sets its fields and is then recognised.
        c.apply_reading_mode(ReadingMode::Study);
        assert_eq!(c.reading_mode(), ReadingMode::Study);
        assert!(c.chapter_lock);

        // Presets leave the page layout (view_mode) untouched — it's the reader's
        // choice, not something a preset should override.
        c.view_mode = ViewMode::TwoPage;
        c.apply_reading_mode(ReadingMode::Research);
        assert_eq!(c.view_mode, ViewMode::TwoPage);
        assert_eq!(c.reading_mode(), ReadingMode::Research);

        c.apply_reading_mode(ReadingMode::Presentation);
        assert_eq!(c.reading_mode(), ReadingMode::Presentation);
        assert!(c.paged && !c.show_sidebar && !c.show_status);

        // A manual tweak drops the derived mode back to Custom (stays honest).
        c.side_padding += 1;
        assert_eq!(c.reading_mode(), ReadingMode::Custom);

        // Custom is a no-op to apply.
        let before = c.side_padding;
        c.apply_reading_mode(ReadingMode::Custom);
        assert_eq!(c.side_padding, before);
    }

    #[test]
    fn reading_mode_cycles_through_presets_only() {
        // next() never lands on Custom; from Custom it enters the cycle.
        assert_eq!(ReadingMode::Custom.next(), ReadingMode::Study);
        assert_eq!(ReadingMode::Study.next(), ReadingMode::Research);
        assert_eq!(ReadingMode::Research.next(), ReadingMode::Presentation);
        assert_eq!(ReadingMode::Presentation.next(), ReadingMode::Study);
        assert_eq!(ReadingMode::Custom.prev(), ReadingMode::Presentation);
    }
}
