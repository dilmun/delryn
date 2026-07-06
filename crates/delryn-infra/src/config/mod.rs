//! Settings model — `Config` is the single source of truth, serialized straight
//! to/from `~/.config/delryn/config.toml`.
//!
//! `Config` derives `Serialize`/`Deserialize` directly (with `#[serde(default)]`
//! so missing keys fall back to the built-in defaults and unknown keys from
//! older/newer files are ignored). The few fields that have no natural TOML
//! encoding go through small `#[serde(with = …)]` helpers: the active [`Theme`]
//! is stored by its `name`, and each option enum by its `label()` string (see
//! [`enums`]). See `DESIGN.md` §7–8.

mod enums;

pub use enums::{
    GridSize, ImageFit, ImageMode, LibLayout, ReadingDirection, ReadingMode, ViewMode,
};

use serde::{Deserialize, Serialize};

use crate::theme::{self, Theme};
use enums::ReadingProfile;

/// The `[status]` config block: which status-bar segments show (context/title is
/// always shown), plus their per-zone order, the segment separator, and an
/// optional clock. Segment names for the zone lists are the `SegmentId` labels
/// ("position", "percent", "gauge", "page", "zoom", "search", "theme", "view",
/// "continuous", "manga", "clock"); unlisted segments keep their built-in order,
/// so a zone list only *reorders* — hide a segment with its toggle instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusFields {
    pub theme: bool,
    pub view: bool,
    pub position: bool,
    pub percent: bool,
    pub gauge: bool,
    /// Show a wall-clock (HH:MM) segment.
    pub clock: bool,
    /// Divider drawn between segments in a zone (rendered with a space each side).
    pub separator: String,
    /// Explicit segment order for the Left zone (by `SegmentId` label).
    pub left: Vec<String>,
    /// Explicit segment order for the Center zone.
    pub center: Vec<String>,
    /// Explicit segment order for the Right zone.
    pub right: Vec<String>,
}

impl Default for StatusFields {
    fn default() -> Self {
        Self {
            theme: true,
            view: true,
            position: true,
            // The gauge is the default progress indicator; the numeric percent is
            // off by default (opt in with `[status] percent = true`).
            percent: false,
            gauge: true,
            clock: false,
            separator: "·".to_string(),
            left: Vec::new(),
            center: Vec::new(),
            right: Vec::new(),
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
/// Upper bound for the PDF margin-trim crop (percent per edge). Capped well below
/// 50 % so the crop can't collapse the page or bite deep into content.
pub const MAX_PDF_MARGIN_PCT: u16 = 20;
/// Upper bound for the inline-image resolution cap (longest side, px). `0` means
/// no cap — images fill the text column. A cap trades size for a faster transmit
/// to the terminal.
pub const MAX_IMAGE_PX: u16 = 4096;
/// Bounds for the default figure width (% of the reading column). Figures without
/// an authored size are normalized to this so they read consistently across books.
pub const MIN_IMAGE_WIDTH_PCT: u16 = 20;
pub const MAX_IMAGE_WIDTH_PCT: u16 = 100;

/// Graphical-math display size, as a percent of the default (100 = the built-in
/// ~2-text-line size); scales the rendered equation up or down.
pub const MIN_MATH_SCALE: u16 = 50;
pub const MAX_MATH_SCALE: u16 = 250;

/// Equation-picture display size, as a percent of the auto-sized default (100 =
/// the auto-detected readable size). Scales publisher equation *images* (not real
/// graphical math, which uses `math_scale`) up or down.
pub const MIN_EQUATION_SCALE: u16 = 50;
pub const MAX_EQUATION_SCALE: u16 = 300;

/// Book-format labels in the default duplicate keep-priority (high → low). Kept as
/// labels (not `BookFormat`, which lives in a crate that depends on this one).
pub const DUP_FORMAT_ORDER: [&str; 4] = ["EPUB", "PDF", "MOBI", "AZW3"];

/// Reconcile a stored format keep-order with the known formats: drop unknown/
/// duplicate labels, keep the user's order, and append any missing formats at the
/// end — so the list is always the complete set in the user's preferred order.
fn normalize_format_order(stored: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(DUP_FORMAT_ORDER.len());
    for f in stored {
        if DUP_FORMAT_ORDER.contains(&f.as_str()) && !out.contains(&f) {
            out.push(f);
        }
    }
    for f in DUP_FORMAT_ORDER {
        if !out.iter().any(|x| x == f) {
            out.push(f.to_string());
        }
    }
    out
}

/// (De)serialize a [`Theme`] as its `name` string — the only stable, portable
/// handle to a built-in theme. An unknown or missing name resolves to the
/// default theme, exactly as `load` did historically.
mod theme_serde {
    use super::{Theme, theme};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &Theme, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(t.name)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Theme, D::Error> {
        let name = String::deserialize(d)?;
        Ok(theme::by_name(&name).unwrap_or_else(theme::default_theme))
    }
}

/// The global reader/library settings, persisted to `config.toml`.
///
/// Field order is the on-disk key order, so it must not be reshuffled casually.
/// `focus_mode` is `#[serde(skip)]` — it's a transient session toggle, never
/// written, and resets to `false` on load.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Text padding from each edge, as a percent of the content pane width, so
    /// the reading column scales with the window. Applied in every view mode.
    pub side_padding: u16,
    /// Gap (in cells) between the two columns of the two-page spread.
    pub page_gap: u16,
    /// In two-page mode, show the first page alone (like a book cover), then pair
    /// (2,3), (4,5)… so facing pages line up as in a physical book.
    pub cover_offset: bool,
    /// Reading direction for paged spreads: left-to-right, or right-to-left for
    /// manga / manhua (the facing pages swap sides). Reflowable text is unaffected.
    #[serde(with = "enums::reading_direction_serde")]
    pub reading_direction: ReadingDirection,
    /// Trim baked-in whitespace margins from PDF pages so the content fills the
    /// viewport (bigger text). Toggle with `x` in the reader.
    pub pdf_trim: bool,
    /// PDF margin trim amount: the percent of each page edge cropped when
    /// `pdf_trim` is on. A *constant* crop (same for every page) so the displayed
    /// page width stays identical across pages, regardless of their own varying
    /// margins. Clamped to [`MAX_PDF_MARGIN_PCT`]; `0` shows the whole page.
    pub pdf_margin_pct: u16,
    /// Extra blank lines between wrapped text lines (0 = single-spaced).
    pub line_spacing: u8,
    /// Blank lines between blocks/paragraphs.
    pub paragraph_spacing: u8,
    #[serde(with = "enums::view_mode_serde")]
    pub view_mode: ViewMode,
    #[serde(with = "theme_serde")]
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
    /// Continuous scroll across sections (single-column / Center mode). Reflowable
    /// content shares the tail of one chapter and the head of the next in the
    /// viewport so a boundary scrolls seamlessly; paged (PDF) documents stack their
    /// page images vertically and scroll through them a row at a time. Overridden
    /// by `chapter_lock`.
    pub continuous: bool,
    /// Keep scrolling within the current chapter (true) instead of flowing into
    /// the next/previous one at the edges.
    pub chapter_lock: bool,
    pub show_sidebar: bool,
    pub show_status: bool,
    /// Distraction-free: hide chrome regardless of the show_* flags. Transient —
    /// not persisted, resets to `false` each session.
    #[serde(skip)]
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
    #[serde(with = "enums::image_mode_serde")]
    pub image_mode: ImageMode,
    /// Whether figures are normalized to a consistent size (`Fit`) or shown at the
    /// publisher's authored size (`Faithful`). Distinct from `image_mode` (colour).
    #[serde(with = "enums::image_fit_serde")]
    pub image_fit: ImageFit,
    /// Render display equations as images (LaTeX → picture) instead of the Unicode
    /// approximation, when a LaTeX source and a graphics protocol are available.
    /// Falls back to Unicode otherwise.
    pub graphical_math: bool,
    /// Graphical-math display size as a percent of the default (100 = built-in).
    pub math_scale: u16,
    /// Publisher equation-*image* display size as a percent of the auto-sized
    /// default (100 = auto). Enlarges/shrinks uncaptioned equation pictures; low-
    /// resolution ones are already auto-boosted toward a readable size, this tunes
    /// that on top. Independent of `math_scale` (which sizes real rendered math).
    pub equation_scale: u16,
    /// Directories scanned for the library.
    pub library_paths: Vec<String>,
    /// How the library lists books (table / dense table / cover grid).
    #[serde(with = "enums::lib_layout_serde")]
    pub library_layout: LibLayout,
    /// Cover-card size for the grid view.
    #[serde(with = "enums::grid_size_serde")]
    pub library_grid_size: GridSize,
    /// Visible optional library columns (keys from [`LIB_COLUMNS`]); user-
    /// toggleable. The star + Title columns are always shown.
    pub library_columns: Vec<String>,
    /// Duplicate auto-select: always mark a converted/repackaged copy for deletion
    /// when an un-converted alternative is in the same group.
    pub dup_converted_delete: bool,
    /// Duplicate auto-select: book-format keep priority (high → low), as format
    /// labels ("EPUB", "PDF", …). Earlier = preferred to keep.
    pub dup_format_order: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            side_padding: 6,
            page_gap: 5,
            cover_offset: false,
            reading_direction: ReadingDirection::default(),
            pdf_trim: true,
            pdf_margin_pct: 6,
            line_spacing: 0,
            paragraph_spacing: 1,
            view_mode: ViewMode::Center,
            theme: theme::default_theme(),
            code_wrap: true,
            table_wrap: true,
            justify: false,
            tidy_spacing: true,
            paged: false,
            continuous: false,
            chapter_lock: false,
            show_sidebar: true,
            show_status: true,
            focus_mode: false,
            mouse_enabled: true,
            status: StatusFields::default(),
            image_max_px: 0,     // no cap by default — images fill the text column
            image_width_pct: 85, // normalize unsized figures to 85% of the column
            image_mode: ImageMode::default(),
            image_fit: ImageFit::default(),
            graphical_math: true,
            math_scale: 100,
            equation_scale: 100,
            library_paths: Vec::new(),
            library_layout: LibLayout::List,
            library_grid_size: GridSize::Medium,
            library_columns: LIB_COLUMNS.iter().map(|(k, _)| k.to_string()).collect(),
            dup_converted_delete: false,
            dup_format_order: DUP_FORMAT_ORDER.iter().map(|s| s.to_string()).collect(),
        }
    }
}

fn config_path() -> std::path::PathBuf {
    crate::paths::config_dir().join("config.toml")
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

    /// Keep-priority rank of a format label (0 = most preferred to keep); an
    /// unknown label sorts last.
    pub fn dup_format_rank(&self, label: &str) -> usize {
        self.dup_format_order
            .iter()
            .position(|f| f == label)
            .unwrap_or(usize::MAX)
    }

    /// Move the format at `label` one step up (toward "keep", `up = true`) or down
    /// in the keep-priority order. No-op if it's already at the end in that
    /// direction or isn't present.
    pub fn move_dup_format(&mut self, label: &str, up: bool) {
        let Some(i) = self.dup_format_order.iter().position(|f| f == label) else {
            return;
        };
        let j = if up {
            i.checked_sub(1)
        } else {
            i.checked_add(1)
                .filter(|&j| j < self.dup_format_order.len())
        };
        if let Some(j) = j {
            self.dup_format_order.swap(i, j);
        }
    }

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
    ///
    /// Missing keys use the [`Default`] values, unknown keys are ignored, and the
    /// option enums / theme resolve through their serde helpers. The post-load
    /// fixups below re-impose the live invariants (clamps, known-column and
    /// format-order reconciliation) that the on-disk values aren't trusted to hold.
    pub fn load() -> Config {
        let Ok(text) = std::fs::read_to_string(config_path()) else {
            return Config::default();
        };
        let Ok(mut c) = toml::from_str::<Config>(&text) else {
            return Config::default();
        };
        c.side_padding = c.side_padding.min(MAX_SIDE_PADDING);
        c.page_gap = c.page_gap.min(MAX_PAGE_GAP);
        c.pdf_margin_pct = c.pdf_margin_pct.min(MAX_PDF_MARGIN_PCT);
        c.line_spacing = c.line_spacing.min(MAX_LINE_SPACING);
        c.paragraph_spacing = c.paragraph_spacing.min(3);
        c.image_max_px = c.image_max_px.min(MAX_IMAGE_PX);
        c.image_width_pct = c
            .image_width_pct
            .clamp(MIN_IMAGE_WIDTH_PCT, MAX_IMAGE_WIDTH_PCT);
        c.math_scale = c.math_scale.clamp(MIN_MATH_SCALE, MAX_MATH_SCALE);
        c.equation_scale = c
            .equation_scale
            .clamp(MIN_EQUATION_SCALE, MAX_EQUATION_SCALE);
        // Keep only known column keys; an empty list (all hidden) is valid.
        c.library_columns
            .retain(|k| LIB_COLUMNS.iter().any(|(key, _)| key == k));
        c.dup_format_order = normalize_format_order(std::mem::take(&mut c.dup_format_order));
        c
    }

    /// Persist the current settings as the global defaults (best-effort).
    pub fn save(&self) {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Format guard: a config written in the historical on-disk shape (option
    /// enums + theme as strings, `[status]` sub-table) must still deserialize to
    /// the expected `Config`. Also proves unknown keys are ignored and missing
    /// keys fall back to defaults — so older/newer files keep loading.
    #[test]
    fn deserializes_historical_on_disk_format() {
        let sample = r#"
side_padding = 9
page_gap = 7
cover_offset = true
line_spacing = 2
paragraph_spacing = 2
view_mode = "two-page"
theme = "dracula"
code_wrap = false
justify = true
image_mode = "faithful"
library_layout = "grid"
library_grid_size = "large"
library_columns = ["author", "year"]
dup_format_order = ["PDF", "EPUB", "MOBI", "AZW3"]
future_unknown_key = "ignored"

[status]
theme = false
view = false
position = true
percent = true
gauge = false
"#;
        let c: Config = toml::from_str(sample).expect("historical format parses");
        assert_eq!(c.side_padding, 9);
        assert_eq!(c.view_mode, ViewMode::TwoPage);
        assert_eq!(c.theme.name, "dracula");
        assert!(!c.code_wrap);
        assert_eq!(c.image_mode, ImageMode::Faithful);
        assert_eq!(c.library_layout, LibLayout::Grid);
        assert_eq!(c.library_grid_size, GridSize::Large);
        assert_eq!(c.library_columns, vec!["author", "year"]);
        assert_eq!(c.dup_format_order, vec!["PDF", "EPUB", "MOBI", "AZW3"]);
        assert!(!c.status.theme && !c.status.gauge && c.status.position);
        // A key absent from the sample falls back to its default.
        assert!(c.table_wrap);
        assert_eq!(c.image_width_pct, 85);
    }

    /// An unknown theme / enum label deserializes to the historical default
    /// rather than failing the whole load.
    #[test]
    fn unknown_labels_fall_back_to_defaults() {
        let sample = r#"
view_mode = "??"
theme = "no-such-theme"
image_mode = "??"
library_layout = "??"
library_grid_size = "??"
"#;
        let c: Config = toml::from_str(sample).expect("unknown labels still parse");
        assert_eq!(c.view_mode, ViewMode::Center);
        assert_eq!(c.theme.name, theme::default_theme().name);
        assert_eq!(c.image_mode, ImageMode::Auto);
        assert_eq!(c.library_layout, LibLayout::List);
        assert_eq!(c.library_grid_size, GridSize::Medium);
    }

    /// save → load round-trips every persisted field (including the enum/theme
    /// helpers and the `[status]` sub-table), and `focus_mode` is never written.
    #[test]
    fn save_load_round_trips() {
        let _g = crate::test_env_guard();
        let dir = std::env::temp_dir().join(format!("delryn-cfg-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: serialized by `test_env_guard`; this is the documented way the
        // suite points `config_dir` at a scratch location.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        let c = Config {
            side_padding: 11,
            view_mode: ViewMode::TwoPage,
            theme: theme::by_name("dracula").unwrap(),
            image_mode: ImageMode::Faithful,
            library_layout: LibLayout::Grid,
            library_grid_size: GridSize::Large,
            library_paths: vec!["/books".into()],
            dup_format_order: vec!["PDF".into(), "EPUB".into(), "MOBI".into(), "AZW3".into()],
            status: StatusFields {
                gauge: false,
                ..StatusFields::default()
            },
            focus_mode: true, // transient — must not survive the round-trip
            ..Config::default()
        };
        c.save();

        let back = Config::load();
        assert_eq!(back.side_padding, 11);
        assert_eq!(back.view_mode, ViewMode::TwoPage);
        assert_eq!(back.theme.name, "dracula");
        assert_eq!(back.image_mode, ImageMode::Faithful);
        assert_eq!(back.library_layout, LibLayout::Grid);
        assert_eq!(back.library_grid_size, GridSize::Large);
        assert_eq!(back.library_paths, vec!["/books".to_string()]);
        assert_eq!(back.dup_format_order, c.dup_format_order);
        assert!(!back.status.gauge);
        assert!(!back.focus_mode, "focus_mode is transient, never persisted");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
