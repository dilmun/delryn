//! The settings popup: a scoped (Reading vs Library) list of adjustable options
//! and the keys that move through and change them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Mode, Overlay};
use crate::config::Config;
use crate::document::BookFormat;
use crate::ui::TextInput;

/// Open settings popup. Scoped to the mode it was opened from — Reading settings
/// in the reader, Library settings in the library — so the two never mix. Options
/// are grouped into [`SettingTab`]s; `tab` is the active one and `row` the cursor
/// within it.
pub struct Settings {
    pub scope: Mode,
    pub tab: usize,
    pub row: usize,
    /// When adding a library folder on the Sources tab, the in-progress path the
    /// user is typing. `None` outside that inline edit. Only the Sources tab uses
    /// it; the input owns the keyboard while it's `Some`.
    pub adding: Option<TextInput>,
    /// The `/` filter. While `Some` the text is being edited; Enter commits it to
    /// `query` (keeping the results, returning the keys to the list) and Esc drops
    /// it. Either way `query` is what the view filters by.
    pub filter: Option<TextInput>,
    /// The committed filter text, empty when not filtering.
    pub query: String,
}

impl Settings {
    /// The filter in effect right now — the live edit if one is open, else the
    /// committed query.
    pub fn active_query(&self) -> &str {
        match &self.filter {
            Some(input) => input.text(),
            None => &self.query,
        }
    }

    /// Whether a filter is narrowing the list (so the view shows matches across
    /// every tab instead of the active tab's rows).
    pub fn filtering(&self) -> bool {
        !self.active_query().trim().is_empty()
    }
}

/// One adjustable setting (identity, not position — so section headers can be
/// inserted freely without re-indexing the change handler).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingItem {
    ReadingMode,
    Theme,
    ViewMode,
    SidePadding,
    MaxMeasure,
    PageGap,
    CoverOffset,
    ReadingDirection,
    LineSpacing,
    ParagraphSpacing,
    Justify,
    Hyphenate,
    TidySpacing,
    ShowSidebar,
    ShowStatus,
    BoldBorders,
    StatusTheme,
    StatusView,
    StatusPosition,
    StatusPercent,
    StatusGauge,
    StatusClock,
    ImageMaxPx,
    ImageWidthPct,
    ImageFit,
    ImageMode,
    GraphicalMath,
    GraphicalInlineMath,
    MathScale,
    BreakWideEquations,
    CodeWrap,
    CodeLineNumbers,
    CodeLanguageLabel,
    CodeFold,
    CodeFoldThreshold,
    TableWrap,
    Paged,
    Continuous,
    ChapterLock,
    TrimMargins,
    PdfMargin,
    Mouse,
    /// Word-lookup (`K`) sources — the Lookup tab.
    LookupSdcv,
    LookupDictionary,
    LookupWikipedia,
    LookupTranslate,
    TranslateTo,
    LibLayout,
    GridSize,
    /// A configured library source folder on the Sources tab (carries its index
    /// into `config.library_paths`). `d`/Delete removes it.
    Source(usize),
    /// The "Add folder…" action row on the Sources tab (opens the path input).
    AddSource,
    /// The "Rescan now" action row on the Sources tab (re-indexes every source).
    RescanNow,
    /// The "Find my books" action row on the Sources tab (searches the home
    /// directory for folders holding books and offers them).
    FindSources,
    /// Show/hide an optional library column (carries its key from `LIB_COLUMNS`).
    Column(&'static str),
    /// Duplicate resolver: always mark converted copies for deletion.
    DupConvertedDelete,
    /// Duplicate resolver: a format's keep priority (carries its label, e.g. "PDF").
    DupFormat(&'static str),
}

impl SettingItem {
    pub fn label(self) -> &'static str {
        match self {
            SettingItem::ReadingMode => "Reading mode",
            SettingItem::Theme => "Theme",
            SettingItem::ViewMode => "View mode",
            SettingItem::SidePadding => "Side margin %",
            SettingItem::MaxMeasure => "Max text width",
            SettingItem::PageGap => "Two-page gap",
            SettingItem::CoverOffset => "First page alone",
            SettingItem::ReadingDirection => "Reading direction",
            SettingItem::LineSpacing => "Line spacing",
            SettingItem::ParagraphSpacing => "Paragraph spacing",
            SettingItem::Justify => "Justify text",
            SettingItem::Hyphenate => "Hyphenate",
            SettingItem::TidySpacing => "Tidy spacing",
            SettingItem::ShowSidebar => "Sidebar by default",
            SettingItem::ShowStatus => "Status bar by default",
            SettingItem::BoldBorders => "Bold popup borders",
            SettingItem::StatusTheme => "Theme",
            SettingItem::StatusView => "View",
            SettingItem::StatusPosition => "Position",
            SettingItem::StatusPercent => "Percent",
            SettingItem::StatusGauge => "Gauge",
            SettingItem::StatusClock => "Clock",
            SettingItem::ImageMaxPx => "Max resolution (px)",
            SettingItem::ImageWidthPct => "Figure width %",
            SettingItem::ImageFit => "Image & equation sizing",
            SettingItem::ImageMode => "Image mode",
            SettingItem::GraphicalMath => "Graphical math",
            SettingItem::GraphicalInlineMath => "Graphical inline math",
            SettingItem::MathScale => "Math size %",
            SettingItem::BreakWideEquations => "Break wide equations",
            SettingItem::CodeWrap => "Wrap long lines",
            SettingItem::CodeLineNumbers => "Line numbers",
            SettingItem::CodeLanguageLabel => "Language label",
            SettingItem::CodeFold => "Fold long code",
            SettingItem::CodeFoldThreshold => "Fold threshold",
            SettingItem::TableWrap => "Wrap tables",
            SettingItem::Paged => "Page mode",
            SettingItem::Continuous => "Continuous scroll",
            SettingItem::ChapterLock => "Chapter lock",
            SettingItem::TrimMargins => "Trim PDF margins",
            SettingItem::PdfMargin => "PDF margin crop %",
            SettingItem::Mouse => "Mouse",
            SettingItem::LookupSdcv => "Local dictionary (sdcv)",
            SettingItem::LookupDictionary => "Online dictionary",
            SettingItem::LookupWikipedia => "Wikipedia summary",
            SettingItem::LookupTranslate => "Translation",
            SettingItem::TranslateTo => "Translate to",
            SettingItem::Source(_) => "Folder",
            SettingItem::AddSource => "Add folder…",
            SettingItem::RescanNow => "Rescan now",
            SettingItem::FindSources => "Find my books…",
            SettingItem::LibLayout => "Layout",
            SettingItem::GridSize => "Cover size",
            SettingItem::Column(key) => crate::config::LIB_COLUMNS
                .iter()
                .find(|(k, _, _)| *k == key)
                .map(|(_, label, _)| *label)
                .unwrap_or(key),
            SettingItem::DupConvertedDelete => "Converted copies: always delete",
            SettingItem::DupFormat(label) => label,
        }
    }

    /// One line explaining what the option does, shown under the focused row.
    /// Labels alone are ambiguous ("Tidy spacing", "Image & equation sizing"), so
    /// each says what changes on screen rather than restating the label.
    pub fn help(self) -> &'static str {
        match self {
            SettingItem::ReadingMode => {
                "A preset that sets several options at once; changing any of them switches to Custom."
            }
            SettingItem::Theme => "Colour scheme for text, chrome, and recoloured images.",
            SettingItem::ViewMode => "One column of text, or two side-by-side pages.",
            SettingItem::SidePadding => "Blank margin on each side, as a percent of the window.",
            SettingItem::MaxMeasure => {
                "Widest the text column may get, in characters; \"off\" lets it fill the margins."
            }
            SettingItem::PageGap => "Blank columns between the two pages in two-page view.",
            SettingItem::CoverOffset => {
                "Show the first page by itself, so later spreads pair up like a printed book."
            }
            SettingItem::ReadingDirection => {
                "Right-to-left swaps the page order and side for manga and Arabic/Hebrew books."
            }
            SettingItem::LineSpacing => "Blank rows added between every line of text.",
            SettingItem::ParagraphSpacing => "Blank rows added between paragraphs.",
            SettingItem::Justify => {
                "Pad spaces so both edges line up; a line needing wide gaps stays ragged."
            }
            SettingItem::Hyphenate => {
                "Break long words across lines with a hyphen, which keeps justified gaps small."
            }
            SettingItem::TidySpacing => {
                "Collapse runs of blank lines and stray indents from sloppy publisher markup."
            }
            SettingItem::ShowSidebar => "Whether the contents sidebar is open when a book opens.",
            SettingItem::ShowStatus => "Whether the status bar is shown when a book opens.",
            SettingItem::BoldBorders => "Draw popup and pane borders with a heavier line.",
            SettingItem::StatusTheme => "Show the current theme name in the status bar.",
            SettingItem::StatusView => "Show the current view mode in the status bar.",
            SettingItem::StatusPosition => "Show the chapter/page position in the status bar.",
            SettingItem::StatusPercent => "Show percent read in the status bar.",
            SettingItem::StatusGauge => "Show the progress bar in the status bar.",
            SettingItem::StatusClock => "Show the wall clock in the status bar.",
            SettingItem::ImageMaxPx => {
                "Cap the longest side of an image in pixels; off sends it at full resolution."
            }
            SettingItem::ImageWidthPct => "How much of the text column a figure fills.",
            SettingItem::ImageFit => {
                "Normalized sizes every graphic to the text; publisher size honours the authored width."
            }
            SettingItem::ImageMode => {
                "How images are recoloured for the theme — invert, tint, or leave alone."
            }
            SettingItem::GraphicalMath => {
                "Render display equations as images; off falls back to Unicode approximations."
            }
            SettingItem::GraphicalInlineMath => {
                "Also render equations that sit inside a line of prose, not just standalone ones."
            }
            SettingItem::MathScale => "Size equations relative to the surrounding text.",
            SettingItem::BreakWideEquations => {
                "Split an equation too wide for the column into stacked lines instead of shrinking it."
            }
            SettingItem::CodeWrap => "Wrap long code lines instead of scrolling them sideways.",
            SettingItem::CodeLineNumbers => "Number the lines in code blocks.",
            SettingItem::CodeLanguageLabel => "Show the detected language above a code block.",
            SettingItem::CodeFold => "Collapse long code blocks, expandable with the fold key.",
            SettingItem::CodeFoldThreshold => "How many lines a code block needs before it folds.",
            SettingItem::TableWrap => {
                "Wrap text inside table cells instead of letting wide tables scroll."
            }
            SettingItem::Paged => {
                "Turn whole pages and keep the position on a page boundary, instead of \
                 scrolling by line. (A two-page spread always turns a whole spread.)"
            }
            SettingItem::Continuous => {
                "Flow the next chapter in as you reach the bottom, with no break between them."
            }
            SettingItem::ChapterLock => {
                "Stop at each chapter's edges instead of crossing into the next."
            }
            SettingItem::TrimMargins => {
                "Crop the white border off PDF pages so the text fills more."
            }
            SettingItem::PdfMargin => "How much of a PDF page edge to crop when trimming.",
            SettingItem::Mouse => "Click, scroll, and select with the mouse.",
            SettingItem::LookupSdcv => "Look words up in local sdcv dictionaries (needs sdcv).",
            SettingItem::LookupDictionary => "Look words up in an online dictionary.",
            SettingItem::LookupWikipedia => "Include a short Wikipedia summary in lookups.",
            SettingItem::LookupTranslate => "Include a translation in lookups.",
            SettingItem::TranslateTo => "Language that lookups translate into.",
            SettingItem::Source(_) => "A folder scanned for books. Press d to remove it.",
            SettingItem::AddSource => "Type a folder path to scan it for books.",
            SettingItem::RescanNow => "Re-read every source folder and refresh the library.",
            SettingItem::FindSources => {
                "Search your home folder for folders holding books, then pick which to add."
            }
            SettingItem::LibLayout => "Show the library as a list of rows or a grid of covers.",
            SettingItem::GridSize => "How large the covers are in grid layout.",
            SettingItem::Column(_) => "Show this column in the library list.",
            SettingItem::DupConvertedDelete => {
                "When resolving duplicates, always mark format-converted copies for deletion."
            }
            SettingItem::DupFormat(_) => {
                "Which format to keep when the same book exists in several. l/h reorders."
            }
        }
    }

    /// Whether this option is an action rather than a value (the Sources tab's
    /// rows). Actions have nothing to reset and are skipped by reset-all.
    pub fn is_action(self) -> bool {
        matches!(
            self,
            SettingItem::Source(_)
                | SettingItem::AddSource
                | SettingItem::RescanNow
                | SettingItem::FindSources
        )
    }

    /// Whether the option currently holds its default. Compared through
    /// [`value`](Self::value) so it stays correct automatically as options are
    /// added — a new setting can't be forgotten here the way a hand-written
    /// per-field comparison would be.
    pub fn is_default(self, c: &Config) -> bool {
        self.is_action() || self.value(c) == self.value(&Config::default())
    }

    /// Restore this option's default. Actions are ignored.
    pub fn reset(self, c: &mut Config) {
        let d = Config::default();
        match self {
            // A preset writes several fields at once, so re-apply it wholesale.
            SettingItem::ReadingMode => c.apply_reading_mode(d.reading_mode()),
            SettingItem::Theme => c.theme = d.theme,
            SettingItem::ViewMode => c.view_mode = d.view_mode,
            SettingItem::SidePadding => c.side_padding = d.side_padding,
            SettingItem::MaxMeasure => c.max_measure = d.max_measure,
            SettingItem::PageGap => c.page_gap = d.page_gap,
            SettingItem::CoverOffset => c.cover_offset = d.cover_offset,
            SettingItem::ReadingDirection => c.reading_direction = d.reading_direction,
            SettingItem::LineSpacing => c.line_spacing = d.line_spacing,
            SettingItem::ParagraphSpacing => c.paragraph_spacing = d.paragraph_spacing,
            SettingItem::Justify => c.justify = d.justify,
            SettingItem::Hyphenate => c.hyphenate = d.hyphenate,
            SettingItem::TidySpacing => c.tidy_spacing = d.tidy_spacing,
            SettingItem::ShowSidebar => c.show_sidebar = d.show_sidebar,
            SettingItem::ShowStatus => c.show_status = d.show_status,
            SettingItem::BoldBorders => c.bold_borders = d.bold_borders,
            SettingItem::StatusTheme => c.status.theme = d.status.theme,
            SettingItem::StatusView => c.status.view = d.status.view,
            SettingItem::StatusPosition => c.status.position = d.status.position,
            SettingItem::StatusPercent => c.status.percent = d.status.percent,
            SettingItem::StatusGauge => c.status.gauge = d.status.gauge,
            SettingItem::StatusClock => c.status.clock = d.status.clock,
            SettingItem::ImageMaxPx => c.image_max_px = d.image_max_px,
            SettingItem::ImageWidthPct => c.image_width_pct = d.image_width_pct,
            SettingItem::ImageFit => c.image_fit = d.image_fit,
            SettingItem::ImageMode => c.image_mode = d.image_mode,
            SettingItem::GraphicalMath => c.graphical_math = d.graphical_math,
            SettingItem::GraphicalInlineMath => c.graphical_inline_math = d.graphical_inline_math,
            SettingItem::MathScale => c.math_scale = d.math_scale,
            SettingItem::BreakWideEquations => c.break_wide_equations = d.break_wide_equations,
            SettingItem::CodeWrap => c.code_wrap = d.code_wrap,
            SettingItem::CodeLineNumbers => c.code_line_numbers = d.code_line_numbers,
            SettingItem::CodeLanguageLabel => c.code_language_label = d.code_language_label,
            SettingItem::CodeFold => c.code_fold = d.code_fold,
            SettingItem::CodeFoldThreshold => c.code_fold_threshold = d.code_fold_threshold,
            SettingItem::TableWrap => c.table_wrap = d.table_wrap,
            SettingItem::Paged => c.paged = d.paged,
            SettingItem::Continuous => c.continuous = d.continuous,
            SettingItem::ChapterLock => c.chapter_lock = d.chapter_lock,
            SettingItem::TrimMargins => c.pdf_trim = d.pdf_trim,
            SettingItem::PdfMargin => c.pdf_margin_pct = d.pdf_margin_pct,
            SettingItem::Mouse => c.mouse_enabled = d.mouse_enabled,
            SettingItem::LookupSdcv => c.lookup_sdcv = d.lookup_sdcv,
            SettingItem::LookupDictionary => c.lookup_dictionary = d.lookup_dictionary,
            SettingItem::LookupWikipedia => c.lookup_wikipedia = d.lookup_wikipedia,
            SettingItem::LookupTranslate => c.lookup_translate = d.lookup_translate,
            SettingItem::TranslateTo => c.translate_to = d.translate_to,
            SettingItem::LibLayout => c.library_layout = d.library_layout,
            SettingItem::GridSize => c.library_grid_size = d.library_grid_size,
            SettingItem::Column(key) => {
                if d.column_on(key) != c.column_on(key) {
                    c.toggle_column(key);
                }
            }
            SettingItem::DupConvertedDelete => c.dup_converted_delete = d.dup_converted_delete,
            SettingItem::DupFormat(_) => c.dup_format_order = d.dup_format_order,
            SettingItem::Source(_)
            | SettingItem::AddSource
            | SettingItem::RescanNow
            | SettingItem::FindSources => {}
        }
    }

    /// The current value, formatted for display.
    pub fn value(self, c: &Config) -> String {
        let onoff = |b: bool| if b { "on" } else { "off" }.to_string();
        match self {
            SettingItem::ReadingMode => c.reading_mode().label().to_string(),
            SettingItem::Theme => c.theme.name.to_string(),
            SettingItem::ViewMode => c.view_mode.label().to_string(),
            SettingItem::SidePadding => c.side_padding.to_string(),
            SettingItem::MaxMeasure => match c.max_measure {
                0 => "off".to_string(),
                n => n.to_string(),
            },
            SettingItem::PageGap => c.page_gap.to_string(),
            SettingItem::CoverOffset => onoff(c.cover_offset),
            SettingItem::ReadingDirection => match c.reading_direction {
                crate::config::ReadingDirection::Ltr => "left-to-right".to_string(),
                crate::config::ReadingDirection::Rtl => "right-to-left (manga)".to_string(),
            },
            SettingItem::LineSpacing => c.line_spacing.to_string(),
            SettingItem::ParagraphSpacing => c.paragraph_spacing.to_string(),
            SettingItem::Justify => onoff(c.justify),
            SettingItem::Hyphenate => onoff(c.hyphenate),
            SettingItem::TidySpacing => onoff(c.tidy_spacing),
            SettingItem::ShowSidebar => onoff(c.show_sidebar),
            SettingItem::ShowStatus => onoff(c.show_status),
            SettingItem::BoldBorders => onoff(c.bold_borders),
            SettingItem::StatusTheme => onoff(c.status.theme),
            SettingItem::StatusView => onoff(c.status.view),
            SettingItem::StatusPosition => onoff(c.status.position),
            SettingItem::StatusPercent => onoff(c.status.percent),
            SettingItem::StatusGauge => onoff(c.status.gauge),
            SettingItem::StatusClock => onoff(c.status.clock),
            SettingItem::ImageMaxPx => {
                if c.image_max_px == 0 {
                    "off".into()
                } else {
                    c.image_max_px.to_string()
                }
            }
            SettingItem::ImageWidthPct => format!("{}%", c.image_width_pct),
            SettingItem::ImageFit => match c.image_fit {
                crate::config::ImageFit::Fit => "normalized",
                crate::config::ImageFit::Faithful => "publisher size",
            }
            .to_string(),
            SettingItem::ImageMode => c.image_mode.label().to_string(),
            SettingItem::GraphicalMath => onoff(c.graphical_math),
            SettingItem::GraphicalInlineMath => onoff(c.graphical_inline_math),
            SettingItem::MathScale => format!("{}%", c.math_scale),
            SettingItem::BreakWideEquations => onoff(c.break_wide_equations),
            SettingItem::CodeWrap => onoff(c.code_wrap),
            SettingItem::CodeLineNumbers => onoff(c.code_line_numbers),
            SettingItem::CodeLanguageLabel => onoff(c.code_language_label),
            SettingItem::CodeFold => onoff(c.code_fold),
            SettingItem::CodeFoldThreshold => format!("{} lines", c.code_fold_threshold),
            SettingItem::TableWrap => onoff(c.table_wrap),
            SettingItem::Paged => onoff(c.paged),
            SettingItem::Continuous => onoff(c.continuous),
            SettingItem::ChapterLock => onoff(c.chapter_lock),
            SettingItem::TrimMargins => onoff(c.pdf_trim),
            SettingItem::PdfMargin => format!("{}%", c.pdf_margin_pct),
            SettingItem::Mouse => onoff(c.mouse_enabled),
            SettingItem::LookupSdcv => onoff(c.lookup_sdcv),
            SettingItem::LookupDictionary => onoff(c.lookup_dictionary),
            SettingItem::LookupWikipedia => onoff(c.lookup_wikipedia),
            SettingItem::LookupTranslate => onoff(c.lookup_translate),
            SettingItem::TranslateTo => c.translate_lang_label().to_string(),
            // Sources-tab rows render bespoke (see `view::settings`); the generic
            // label/value path is never taken for them.
            SettingItem::Source(_)
            | SettingItem::AddSource
            | SettingItem::RescanNow
            | SettingItem::FindSources => String::new(),
            SettingItem::LibLayout => c.library_layout.label().to_string(),
            SettingItem::GridSize => c.library_grid_size.label().to_string(),
            SettingItem::Column(key) => onoff(c.column_on(key)),
            SettingItem::DupConvertedDelete => onoff(c.dup_converted_delete),
            SettingItem::DupFormat(label) => match c.dup_format_rank(label) {
                usize::MAX => "—".into(),
                rank => format!("keep #{}", rank + 1),
            },
        }
    }
}

/// A row in the settings popup: a non-selectable section header or a setting.
pub enum SettingRow {
    /// A non-selectable heading. Owned because the filter view synthesises
    /// "Tab \u203a Section" breadcrumbs that no static string can express.
    Section(std::borrow::Cow<'static, str>),
    Item(SettingItem),
}

/// A top-level group of settings shown under one tab.
pub struct SettingTab {
    pub title: &'static str,
    pub rows: Vec<SettingRow>,
}

/// The tabs for a settings scope. Each scope is self-contained: the reader shows
/// only reading settings, the library only library settings (global toggles like
/// Theme/Mouse appear in both). Within a tab, [`SettingRow::Section`] headers
/// sub-group related options.
pub fn settings_tabs(scope: Mode, config: &Config) -> Vec<SettingTab> {
    use SettingItem::*;
    use SettingRow::{Item as I, Section as S};
    let tab = |title, rows| SettingTab { title, rows };
    match scope {
        // Tabs are ordered most-frequently-used first (the popup opens on the
        // first tab): typography tweaks are constant, content/pagination
        // occasional, chrome rarer, mouse rarely.
        Mode::Reader => vec![
            tab(
                "Reading",
                vec![
                    S("Profile".into()),
                    I(ReadingMode),
                    S("Typography".into()),
                    I(Theme),
                    I(ViewMode),
                    I(SidePadding),
                    I(MaxMeasure),
                    I(PageGap),
                    I(CoverOffset),
                    I(ReadingDirection),
                    I(LineSpacing),
                    I(ParagraphSpacing),
                    I(Justify),
                    I(Hyphenate),
                ],
            ),
            tab(
                "Content",
                vec![
                    S("Images".into()),
                    I(ImageMaxPx),
                    I(ImageWidthPct),
                    I(ImageFit),
                    I(ImageMode),
                    I(GraphicalMath),
                    I(GraphicalInlineMath),
                    I(MathScale),
                    I(BreakWideEquations),
                    S("Code".into()),
                    I(CodeWrap),
                    I(CodeLineNumbers),
                    I(CodeLanguageLabel),
                    I(CodeFold),
                    I(CodeFoldThreshold),
                    S("Tables & text".into()),
                    I(TableWrap),
                    I(TidySpacing),
                    S("Pagination".into()),
                    I(Paged),
                    I(Continuous),
                    I(ChapterLock),
                    S("PDF".into()),
                    I(TrimMargins),
                    I(PdfMargin),
                ],
            ),
            tab(
                "Chrome",
                vec![
                    S("Panes".into()),
                    I(ShowSidebar),
                    I(ShowStatus),
                    I(BoldBorders),
                    S("Status bar segments".into()),
                    I(StatusTheme),
                    I(StatusView),
                    I(StatusPosition),
                    I(StatusPercent),
                    I(StatusGauge),
                    I(StatusClock),
                ],
            ),
            tab(
                "Lookup",
                vec![
                    S("Word lookup (K)".into()),
                    I(LookupSdcv),
                    I(LookupDictionary),
                    I(LookupWikipedia),
                    S("Translation".into()),
                    I(LookupTranslate),
                    I(TranslateTo),
                ],
            ),
            tab("Input", vec![S("Mouse".into()), I(Mouse)]),
        ],
        Mode::Library => {
            // The star + Title columns are always on; the rest are user-toggled.
            let mut columns = vec![S("Show / hide".into())];
            columns.extend(
                crate::config::LIB_COLUMNS
                    .iter()
                    .map(|(key, _, _)| I(Column(key))),
            );
            // Duplicate-resolver preferences: a converted-copies toggle and the
            // per-format keep priority (l/h on a format raises/lowers its rank).
            let mut dups = vec![
                S("Auto-select".into()),
                I(DupConvertedDelete),
                S("Keep priority — l/h".into()),
            ];
            dups.extend(BookFormat::ALL.iter().map(|f| I(DupFormat(f.label()))));
            // The Sources tab manages the library's scanned folders: one row per
            // configured folder (built from the live path list), an add-folder
            // action, and a rescan action. First-run (empty library) still lands
            // here — not because it's first, but via `open_sources_if_empty`, which
            // finds the tab by title.
            let mut sources = vec![S("Folders".into())];
            sources.extend((0..config.library_paths.len()).map(|i| I(Source(i))));
            sources.push(I(AddSource));
            sources.push(I(FindSources));
            sources.push(I(RescanNow));
            // Ordered most-frequently-used first: view/columns are constant tweaks,
            // appearance occasional, sources set-and-forget, dup-prefs rare.
            vec![
                tab("View", vec![S("Layout".into()), I(LibLayout), I(GridSize)]),
                tab("Columns", columns),
                tab(
                    "General",
                    vec![
                        S("Appearance".into()),
                        I(Theme),
                        I(BoldBorders),
                        S("Input".into()),
                        I(Mouse),
                    ],
                ),
                tab("Sources", sources),
                tab("Duplicates", dups),
            ]
        }
    }
}

/// The rows of one tab (empty if the index is out of range).
pub fn tab_rows(scope: Mode, tab: usize, config: &Config) -> Vec<SettingRow> {
    settings_tabs(scope, config)
        .into_iter()
        .nth(tab)
        .map(|t| t.rows)
        .unwrap_or_default()
}

/// Every option across every tab whose label or description matches `query`,
/// each under a "Tab › Section" breadcrumb so a match is placeable without
/// remembering which tab owns it. Sources-tab action rows are skipped — they run
/// commands rather than holding a value, so they don't belong in a value search.
pub fn filtered_rows(scope: Mode, query: &str, config: &Config) -> Vec<SettingRow> {
    let q = query.trim().to_lowercase();
    let mut out = Vec::new();
    for tab in settings_tabs(scope, config) {
        let mut section = String::new();
        let mut crumbed = String::new();
        for row in tab.rows {
            match row {
                SettingRow::Section(title) => section = title.into_owned(),
                SettingRow::Item(item) if !item.is_action() => {
                    let hit = item.label().to_lowercase().contains(&q)
                        || item.help().to_lowercase().contains(&q);
                    if !hit {
                        continue;
                    }
                    let crumb = format!("{} › {}", tab.title, section);
                    if crumb != crumbed {
                        out.push(SettingRow::Section(crumb.clone().into()));
                        crumbed = crumb;
                    }
                    out.push(SettingRow::Item(item));
                }
                SettingRow::Item(_) => {}
            }
        }
    }
    out
}

/// The rows the popup is showing: the filter results when one is active, else the
/// active tab's own rows. Every navigation, edit, and render path goes through
/// this so filtered and unfiltered modes can't diverge.
pub fn visible_rows(scope: Mode, tab: usize, query: &str, config: &Config) -> Vec<SettingRow> {
    if query.trim().is_empty() {
        tab_rows(scope, tab, config)
    } else {
        filtered_rows(scope, query, config)
    }
}

/// Index of the first selectable item in a tab (skips a leading section header).
pub fn first_setting_row(scope: Mode, tab: usize, config: &Config) -> usize {
    tab_rows(scope, tab, config)
        .iter()
        .position(|r| matches!(r, SettingRow::Item(_)))
        .unwrap_or(0)
}

impl App {
    pub(crate) fn settings_key(&mut self, key: KeyEvent) {
        if !matches!(self.overlay, Overlay::Settings(_)) {
            return;
        }
        // While typing a new library folder path, the inline input owns every key.
        if matches!(&self.overlay, Overlay::Settings(s) if s.adding.is_some()) {
            self.settings_add_key(key);
            return;
        }
        // The `/` filter takes text keys but leaves the movement keys alone, so the
        // list can be walked without first dismissing the filter. Any edit can
        // change the match set, so re-clamp the cursor onto a real row after it.
        if self.settings_filter_key(key) {
            self.settings_move(0);
            return;
        }
        match key.code {
            // Esc backs out of a filter first, so a search doesn't cost the popup.
            KeyCode::Esc if self.settings_clear_filter() => self.settings_move(0),
            KeyCode::Esc | KeyCode::Char(';') | KeyCode::Char('q') => {
                self.overlay = Overlay::None;
                self.config.save();
            }
            KeyCode::Char('/') => self.settings_begin_filter(),
            KeyCode::Tab => self.settings_tab(1),
            KeyCode::BackTab => self.settings_tab(-1),
            KeyCode::Char('j') | KeyCode::Down => self.settings_move(1),
            KeyCode::Char('k') | KeyCode::Up => self.settings_move(-1),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.settings_change(1),
            KeyCode::Char('h') | KeyCode::Left => self.settings_change(-1),
            // Vim half-page (Ctrl-d/u · PgDn/Up) and jump-to-ends (g/G) over the
            // options — settings_move skips section headers.
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.settings_move(5)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.settings_move(-5)
            }
            KeyCode::PageDown => self.settings_move(5),
            KeyCode::PageUp => self.settings_move(-5),
            KeyCode::Char('g') | KeyCode::Home => self.settings_move(-9999),
            KeyCode::Char('G') | KeyCode::End => self.settings_move(9999),
            KeyCode::Char('r') => self.settings_reset_focused(),
            KeyCode::Char('R') => self.settings_reset_tab(),
            // Remove the focused library source (no-op on any other row/tab).
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
                self.settings_delete_source()
            }
            _ => {}
        }
    }

    /// Keys while the `/` filter is open. Returns whether the key was consumed —
    /// movement keys are deliberately left for the normal handler so typing and
    /// navigating interleave. Enter keeps the results and hands the keys back to
    /// the list; Esc drops the filter entirely.
    fn settings_filter_key(&mut self, key: KeyEvent) -> bool {
        let Overlay::Settings(s) = &mut self.overlay else {
            return false;
        };
        let Some(input) = &mut s.filter else {
            return false;
        };
        match key.code {
            KeyCode::Enter => {
                s.query = input.text().trim().to_string();
                s.filter = None;
                true
            }
            KeyCode::Esc => {
                s.filter = None;
                s.query.clear();
                true
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::PageUp
            | KeyCode::PageDown => false,
            _ => {
                input.handle_key(key);
                true
            }
        }
    }

    /// Open the `/` filter, seeded with any committed query so refining a search
    /// doesn't start from scratch.
    fn settings_begin_filter(&mut self) {
        if let Overlay::Settings(s) = &mut self.overlay {
            let mut input = TextInput::new();
            input.set(s.query.clone());
            s.filter = Some(input);
        }
    }

    /// Drop any active filter (Esc from the list, or closing the popup).
    fn settings_clear_filter(&mut self) -> bool {
        let Overlay::Settings(s) = &mut self.overlay else {
            return false;
        };
        if s.filter.is_none() && s.query.is_empty() {
            return false;
        }
        s.filter = None;
        s.query.clear();
        true
    }

    /// Key handling while the inline "add folder" path input is open: Enter
    /// commits the path, Esc cancels, everything else edits the text.
    fn settings_add_key(&mut self, key: KeyEvent) {
        let committed = {
            let Overlay::Settings(s) = &mut self.overlay else {
                return;
            };
            let Some(input) = &mut s.adding else {
                return;
            };
            match key.code {
                KeyCode::Enter => {
                    let path = input.text().to_string();
                    s.adding = None;
                    Some(path)
                }
                KeyCode::Esc => {
                    s.adding = None;
                    None
                }
                _ => {
                    input.handle_key(key);
                    None
                }
            }
        };
        if let Some(path) = committed {
            self.commit_add_source(&path);
        }
    }

    /// Restore the focused option's default (`r`). A no-op on section headers and
    /// on the Sources tab's action rows, which have no value to restore.
    fn settings_reset_focused(&mut self) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let Some(SettingRow::Item(item)) =
            visible_rows(s.scope, s.tab, s.active_query(), &self.config)
                .into_iter()
                .nth(s.row)
        else {
            return;
        };
        if item.is_action() || item.is_default(&self.config) {
            return;
        }
        let before = super::dispatch::layout_key(&self.config);
        item.reset(&mut self.config);
        self.config.save();
        self.settings_relayout(&before);
        self.flash_settings(format!(
            "Reset {} to {}",
            item.label(),
            item.value(&self.config)
        ));
    }

    /// Restore every option on the current tab (`R`), reporting how many changed.
    fn settings_reset_tab(&mut self) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let (scope, tab) = (s.scope, s.tab);
        let query = s.active_query().to_string();
        let stale: Vec<SettingItem> = visible_rows(scope, tab, &query, &self.config)
            .into_iter()
            .filter_map(|r| match r {
                SettingRow::Item(i) if !i.is_action() && !i.is_default(&self.config) => Some(i),
                _ => None,
            })
            .collect();
        if stale.is_empty() {
            self.flash_settings("Already at defaults".into());
            return;
        }
        let n = stale.len();
        let before = super::dispatch::layout_key(&self.config);
        for item in stale {
            item.reset(&mut self.config);
        }
        self.config.save();
        self.settings_relayout(&before);
        self.flash_settings(format!("Reset {n} option(s) to defaults"));
    }

    /// Re-anchor and repaint after a settings edit changed the layout. The popup
    /// writes `config` directly, bypassing `App::apply`, so the bookkeeping a
    /// keybinding would have done has to happen here too — without it, changing
    /// the margin or spacing re-wraps the section and drifts the reading position.
    fn settings_relayout(&mut self, before: &super::dispatch::LayoutKey) {
        if super::dispatch::layout_key(&self.config) == *before {
            return;
        }
        if let Some(r) = self.reader.as_mut() {
            r.hold_reflow_position();
            r.request_repaint();
        }
    }

    /// Report a settings action in whichever surface the popup was opened over.
    fn flash_settings(&mut self, msg: String) {
        match self.reader.as_mut() {
            Some(r) => r.flash = Some(msg),
            None => self.library.flash = Some(msg),
        }
    }

    /// Open the inline path input on the Sources tab's "Add folder…" row.
    fn begin_add_source(&mut self) {
        if let Overlay::Settings(s) = &mut self.overlay {
            s.adding = Some(TextInput::new());
        }
    }

    /// Commit a typed folder path: validate it's a real directory, register it
    /// (deduped) as a library source, scan it, and refresh the list — flashing
    /// the outcome. Invalid or duplicate input flashes a note and changes nothing.
    fn commit_add_source(&mut self, raw: &str) {
        let raw = raw.trim();
        if raw.is_empty() {
            return;
        }
        let root = crate::library::normalize_root(raw);
        if !std::path::Path::new(&root).is_dir() {
            self.library.flash = Some(format!("Not a folder: {raw}"));
            return;
        }
        let existed = self.config.library_paths.contains(&root);
        if !existed {
            self.config.library_paths.push(root.clone());
            self.config.save();
        }
        // The new Source row shows at once (the tab reads config live); scanning
        // the folder happens in the background so a big folder doesn't block.
        self.settings_move(0); // keep the cursor on a real item after the row grew
        let label = if existed {
            format!("Already added {root}")
        } else {
            format!("Added {root}")
        };
        self.start_scan(false, false, label);
    }

    /// Remove the library source under the cursor (Sources tab). Drops its books
    /// from the index too, then refreshes. No-op when the focused row isn't a
    /// source folder.
    fn settings_delete_source(&mut self) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let Some(SettingRow::Item(SettingItem::Source(idx))) =
            visible_rows(s.scope, s.tab, s.active_query(), &self.config)
                .into_iter()
                .nth(s.row)
        else {
            return;
        };
        if idx >= self.config.library_paths.len() {
            return;
        }
        let root = self.config.library_paths.remove(idx);
        self.config.save();
        // Drop everything no longer inside a configured folder — this root's books
        // plus any pre-existing orphans (e.g. a one-off opened file's bare row).
        let dropped = match &self.session.store {
            Some(store) => crate::library::prune_outside_roots(&self.config.library_paths, store),
            None => 0,
        };
        self.refresh_library();
        self.library.flash = Some(format!("Removed {root} · dropped {dropped} book(s)"));
        // The row set shrank; keep the cursor on a real item.
        self.settings_move(0);
    }

    /// Re-index every configured library source in the background (incremental —
    /// unchanged files skipped), pruning vanished files and orphaned rows. Runs off
    /// the UI thread so a large library stays responsive; the completion flash
    /// reports the count (see [`App::poll_scan`]).
    fn rescan_sources(&mut self) {
        self.start_scan(false, true, "Rescanning".to_string());
    }

    /// Open Library settings on the Sources tab — the folder manager. Used on
    /// first run / whenever no folders are configured (see [`App::open_sources_if_empty`]).
    pub(crate) fn open_sources_settings(&mut self) {
        let scope = Mode::Library;
        let tab = settings_tabs(scope, &self.config)
            .iter()
            .position(|t| t.title == "Sources")
            .unwrap_or(0);
        let row = first_setting_row(scope, tab, &self.config);
        self.overlay = Overlay::Settings(Settings {
            scope,
            tab,
            row,
            adding: None,
            filter: None,
            query: String::new(),
        });
    }

    /// If we're in the library with no source folders configured, open the
    /// Sources manager so the first thing a new user sees is where to add one.
    /// A no-op in the reader or once at least one folder exists.
    pub fn open_sources_if_empty(&mut self) {
        if self.mode == Mode::Library && self.config.library_paths.is_empty() {
            self.open_sources_settings();
        }
    }

    /// Switch to tab index `i` (clamped), parking the cursor on its first option —
    /// the mouse counterpart to Tab / Shift-Tab.
    pub(crate) fn settings_goto_tab(&mut self, i: usize) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let n = settings_tabs(s.scope, &self.config).len();
        if n == 0 {
            return;
        }
        let tab = i.min(n - 1);
        let row = first_setting_row(s.scope, tab, &self.config);
        if let Overlay::Settings(s) = &mut self.overlay {
            s.tab = tab;
            s.row = row;
        }
    }

    /// Switch tab by `delta` (wrapping), parking the cursor on its first option.
    fn settings_tab(&mut self, delta: isize) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let n = settings_tabs(s.scope, &self.config).len();
        if n == 0 {
            return;
        }
        let tab = (s.tab as isize + delta).rem_euclid(n as isize) as usize;
        let row = first_setting_row(s.scope, tab, &self.config);
        if let Overlay::Settings(s) = &mut self.overlay {
            s.tab = tab;
            s.row = row;
        }
    }

    /// Move the settings cursor by `delta` items within the active tab, skipping
    /// section headers.
    pub(crate) fn settings_move(&mut self, delta: isize) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let rows = visible_rows(s.scope, s.tab, s.active_query(), &self.config);
        let items: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, SettingRow::Item(_)))
            .map(|(i, _)| i)
            .collect();
        if items.is_empty() {
            return;
        }
        let cur = items.iter().position(|&i| i == s.row).unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, items.len() as isize - 1) as usize;
        if let Overlay::Settings(s) = &mut self.overlay {
            s.row = items[next];
        }
    }

    pub(crate) fn settings_change(&mut self, delta: i32) {
        use crate::config::{
            MAX_LINE_SPACING, MAX_MEASURE_CAP, MAX_PAGE_GAP, MAX_SIDE_PADDING, MIN_MEASURE_CAP,
        };
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        // Resolve the focused row (in the active tab) to a setting identity.
        let Some(SettingRow::Item(item)) =
            visible_rows(s.scope, s.tab, s.active_query(), &self.config)
                .into_iter()
                .nth(s.row)
        else {
            return;
        };
        // Sources-tab action rows run a command rather than stepping a value —
        // dispatch them before borrowing `config` mutably below.
        match item {
            SettingItem::AddSource => return self.begin_add_source(),
            SettingItem::RescanNow => return self.rescan_sources(),
            SettingItem::FindSources => return self.start_discover(),
            SettingItem::Source(_) => return, // delete via `d`/Delete, not change
            _ => {}
        }
        // Snapshot before the edit so a layout-affecting option re-anchors below.
        let before = super::dispatch::layout_key(&self.config);
        let c = &mut self.config;
        match item {
            SettingItem::ReadingMode => {
                let mode = if delta > 0 {
                    c.reading_mode().next()
                } else {
                    c.reading_mode().prev()
                };
                c.apply_reading_mode(mode);
            }
            SettingItem::Theme => {
                c.theme = if delta > 0 {
                    c.theme.next()
                } else {
                    c.theme.prev()
                }
            }
            SettingItem::ViewMode => {
                c.view_mode = if delta > 0 {
                    c.view_mode.next()
                } else {
                    c.view_mode.prev()
                }
            }
            SettingItem::SidePadding => {
                c.side_padding =
                    (c.side_padding as i32 + delta).clamp(0, MAX_SIDE_PADDING as i32) as u16
            }
            // "off" (0) sits just below the range rather than inside it: stepping
            // down off the floor turns the cap off, stepping up from off re-enters
            // at the floor, so one key reaches every state without a second toggle.
            SettingItem::MaxMeasure => {
                let stepped = match (c.max_measure, delta) {
                    (0, d) if d > 0 => MIN_MEASURE_CAP as i32,
                    (0, _) => 0,
                    (n, d) => n as i32 + d,
                };
                c.max_measure = if stepped < MIN_MEASURE_CAP as i32 {
                    0
                } else {
                    stepped.min(MAX_MEASURE_CAP as i32) as u16
                };
            }
            SettingItem::PageGap => {
                c.page_gap = (c.page_gap as i32 + delta).clamp(0, MAX_PAGE_GAP as i32) as u16
            }
            SettingItem::CoverOffset => c.cover_offset = !c.cover_offset,
            SettingItem::ReadingDirection => {
                c.reading_direction = if delta > 0 {
                    c.reading_direction.next()
                } else {
                    c.reading_direction.prev()
                }
            }
            SettingItem::LineSpacing => {
                c.line_spacing =
                    (c.line_spacing as i32 + delta).clamp(0, MAX_LINE_SPACING as i32) as u8
            }
            SettingItem::ParagraphSpacing => {
                c.paragraph_spacing = (c.paragraph_spacing as i32 + delta).clamp(0, 3) as u8
            }
            SettingItem::Justify => c.justify = !c.justify,
            SettingItem::Hyphenate => c.hyphenate = !c.hyphenate,
            SettingItem::TidySpacing => c.tidy_spacing = !c.tidy_spacing,
            SettingItem::ShowSidebar => c.show_sidebar = !c.show_sidebar,
            SettingItem::ShowStatus => c.show_status = !c.show_status,
            SettingItem::BoldBorders => c.bold_borders = !c.bold_borders,
            SettingItem::StatusTheme => c.status.theme = !c.status.theme,
            SettingItem::StatusView => c.status.view = !c.status.view,
            SettingItem::StatusPosition => c.status.position = !c.status.position,
            SettingItem::StatusPercent => c.status.percent = !c.status.percent,
            SettingItem::StatusGauge => c.status.gauge = !c.status.gauge,
            SettingItem::StatusClock => c.status.clock = !c.status.clock,
            SettingItem::ImageMaxPx => {
                // 0 = off (uncapped); otherwise step in 128px increments.
                c.image_max_px = (c.image_max_px as i32 + delta * 128)
                    .clamp(0, crate::config::MAX_IMAGE_PX as i32)
                    as u16
            }
            SettingItem::ImageWidthPct => {
                use crate::config::{MAX_IMAGE_WIDTH_PCT, MIN_IMAGE_WIDTH_PCT};
                // Step in 5% increments within the allowed band.
                c.image_width_pct = (c.image_width_pct as i32 + delta * 5)
                    .clamp(MIN_IMAGE_WIDTH_PCT as i32, MAX_IMAGE_WIDTH_PCT as i32)
                    as u16
            }
            SettingItem::ImageFit => {
                c.image_fit = if delta > 0 {
                    c.image_fit.next()
                } else {
                    c.image_fit.prev()
                }
            }
            SettingItem::ImageMode => {
                c.image_mode = if delta > 0 {
                    c.image_mode.next()
                } else {
                    c.image_mode.prev()
                }
            }
            SettingItem::GraphicalMath => c.graphical_math = !c.graphical_math,
            SettingItem::GraphicalInlineMath => c.graphical_inline_math = !c.graphical_inline_math,
            SettingItem::BreakWideEquations => c.break_wide_equations = !c.break_wide_equations,
            SettingItem::MathScale => {
                use crate::config::{MAX_MATH_SCALE, MIN_MATH_SCALE};
                // Step in 10% increments within the allowed band.
                c.math_scale = (c.math_scale as i32 + delta * 10)
                    .clamp(MIN_MATH_SCALE as i32, MAX_MATH_SCALE as i32)
                    as u16
            }
            SettingItem::CodeWrap => c.code_wrap = !c.code_wrap,
            SettingItem::CodeLineNumbers => c.code_line_numbers = !c.code_line_numbers,
            SettingItem::CodeLanguageLabel => c.code_language_label = !c.code_language_label,
            SettingItem::CodeFold => c.code_fold = !c.code_fold,
            SettingItem::CodeFoldThreshold => {
                c.code_fold_threshold =
                    (c.code_fold_threshold as i32 + delta * 5).clamp(5, 200) as usize
            }
            SettingItem::TableWrap => c.table_wrap = !c.table_wrap,
            SettingItem::Paged => c.paged = !c.paged,
            SettingItem::Continuous => c.continuous = !c.continuous,
            SettingItem::ChapterLock => c.chapter_lock = !c.chapter_lock,
            SettingItem::TrimMargins => c.pdf_trim = !c.pdf_trim,
            SettingItem::PdfMargin => {
                use crate::config::MAX_PDF_MARGIN_PCT;
                c.pdf_margin_pct =
                    (c.pdf_margin_pct as i32 + delta).clamp(0, MAX_PDF_MARGIN_PCT as i32) as u16
            }
            SettingItem::Mouse => c.mouse_enabled = !c.mouse_enabled,
            SettingItem::LookupSdcv => c.lookup_sdcv = !c.lookup_sdcv,
            SettingItem::LookupDictionary => c.lookup_dictionary = !c.lookup_dictionary,
            SettingItem::LookupWikipedia => c.lookup_wikipedia = !c.lookup_wikipedia,
            SettingItem::LookupTranslate => c.lookup_translate = !c.lookup_translate,
            SettingItem::TranslateTo => c.step_translate_to(delta > 0),
            SettingItem::LibLayout => {
                c.library_layout = if delta > 0 {
                    c.library_layout.next()
                } else {
                    c.library_layout.prev()
                }
            }
            SettingItem::GridSize => {
                c.library_grid_size = if delta > 0 {
                    c.library_grid_size.next()
                } else {
                    c.library_grid_size.prev()
                }
            }
            // Handled (and returned) above; listed so the match stays exhaustive.
            SettingItem::Source(_)
            | SettingItem::AddSource
            | SettingItem::RescanNow
            | SettingItem::FindSources => {}
            SettingItem::Column(key) => c.toggle_column(key),
            SettingItem::DupConvertedDelete => c.dup_converted_delete = !c.dup_converted_delete,
            // l/right/Enter (delta > 0) promotes the format toward "keep #1".
            SettingItem::DupFormat(label) => c.move_dup_format(label, delta > 0),
        }
        self.settings_relayout(&before);
    }

    /// Open Library settings on the Duplicates tab — the resolve overlay's
    /// preferences. Closes the overlay (settings replaces it); reopen with `D`.
    pub(crate) fn open_dup_settings(&mut self) {
        self.overlay = Overlay::None;
        let scope = Mode::Library;
        let tab = settings_tabs(scope, &self.config)
            .iter()
            .position(|t| t.title == "Duplicates")
            .unwrap_or(0);
        let row = first_setting_row(scope, tab, &self.config);
        self.overlay = Overlay::Settings(Settings {
            scope,
            tab,
            row,
            adding: None,
            filter: None,
            query: String::new(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every option must explain itself: a blank or label-echoing description makes
    /// the help pane worse than nothing, and it's easy to forget one when adding a
    /// setting. Walks both scopes so no tab is missed.
    #[test]
    fn every_setting_has_a_distinct_help_line() {
        let config = Config::default();
        for scope in [Mode::Reader, Mode::Library] {
            for tab in settings_tabs(scope, &config) {
                for row in tab.rows {
                    let SettingRow::Item(item) = row else {
                        continue;
                    };
                    let help = item.help();
                    assert!(
                        help.len() > 10,
                        "{:?} needs a real description, got {help:?}",
                        item
                    );
                    assert_ne!(help, item.label(), "{item:?} help just repeats the label");
                }
            }
        }
    }

    /// A stock config must report every option as default, or the "changed" dots
    /// light up on a fresh install. This is the check that keeps `is_default`
    /// honest as options are added, since it compares formatted values.
    #[test]
    fn a_default_config_marks_nothing_as_changed() {
        let config = Config::default();
        for scope in [Mode::Reader, Mode::Library] {
            for tab in settings_tabs(scope, &config) {
                for row in tab.rows {
                    let SettingRow::Item(item) = row else {
                        continue;
                    };
                    assert!(
                        item.is_default(&config),
                        "{item:?} differs on a stock config"
                    );
                }
            }
        }
    }

    /// Reset has to actually restore the default — and the dot has to clear with it.
    #[test]
    fn reset_restores_the_default_value() {
        let mut config = Config::default();
        config.side_padding += 7;
        config.line_spacing += 2;
        assert!(!SettingItem::SidePadding.is_default(&config));

        SettingItem::SidePadding.reset(&mut config);
        assert!(SettingItem::SidePadding.is_default(&config));
        assert_eq!(config.side_padding, Config::default().side_padding);
        // Untouched options stay untouched.
        assert!(!SettingItem::LineSpacing.is_default(&config));
    }

    /// The filter searches labels *and* descriptions, across every tab, and tags
    /// each hit with the tab it came from so a match can be placed.
    #[test]
    fn filter_matches_across_tabs_with_breadcrumbs() {
        let config = Config::default();
        let rows = filtered_rows(Mode::Reader, "equation", &config);
        let items: Vec<SettingItem> = rows
            .iter()
            .filter_map(|r| match r {
                SettingRow::Item(i) => Some(*i),
                _ => None,
            })
            .collect();
        assert!(
            items.contains(&SettingItem::BreakWideEquations),
            "label match missing"
        );
        assert!(
            items.contains(&SettingItem::ImageFit),
            "description-only match missing (its label says nothing about equations)"
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r, SettingRow::Section(s) if s.contains('›'))),
            "matches need a Tab › Section breadcrumb"
        );
    }

    /// Sources rows run commands rather than holding values, so a value search must
    /// not surface them and reset must leave them alone.
    #[test]
    fn filter_and_reset_skip_action_rows() {
        let config = Config::default();
        for row in filtered_rows(Mode::Library, "folder", &config) {
            if let SettingRow::Item(item) = row {
                assert!(
                    !item.is_action(),
                    "{item:?} is an action and can't be a hit"
                );
            }
        }
        assert!(SettingItem::AddSource.is_default(&config));
    }

    /// An empty query falls back to the plain tab, so one row source serves both
    /// modes and they can't drift.
    #[test]
    fn an_empty_query_shows_the_plain_tab() {
        let config = Config::default();
        let plain = tab_rows(Mode::Reader, 0, &config).len();
        assert_eq!(visible_rows(Mode::Reader, 0, "", &config).len(), plain);
        assert_eq!(visible_rows(Mode::Reader, 0, "   ", &config).len(), plain);
    }
}
