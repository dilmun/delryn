//! The settings popup: a scoped (Reading vs Library) list of adjustable options
//! and the keys that move through and change them.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Mode, Overlay};
use crate::config::Config;
use crate::document::BookFormat;

/// Open settings popup. Scoped to the mode it was opened from — Reading settings
/// in the reader, Library settings in the library — so the two never mix. Options
/// are grouped into [`SettingTab`]s; `tab` is the active one and `row` the cursor
/// within it.
pub struct Settings {
    pub scope: Mode,
    pub tab: usize,
    pub row: usize,
}

/// One adjustable setting (identity, not position — so section headers can be
/// inserted freely without re-indexing the change handler).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingItem {
    ReadingMode,
    Theme,
    ViewMode,
    SidePadding,
    PageGap,
    CoverOffset,
    ReadingDirection,
    LineSpacing,
    ParagraphSpacing,
    Justify,
    TidySpacing,
    ShowSidebar,
    ShowStatus,
    StatusTheme,
    StatusView,
    StatusPosition,
    StatusPercent,
    StatusGauge,
    ImageMaxPx,
    ImageWidthPct,
    ImageFit,
    ImageMode,
    GraphicalMath,
    MathScale,
    EquationScale,
    CodeWrap,
    TableWrap,
    Paged,
    Continuous,
    ChapterLock,
    TrimMargins,
    PdfMargin,
    Mouse,
    LibLayout,
    GridSize,
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
            SettingItem::PageGap => "Two-page gap",
            SettingItem::CoverOffset => "First page alone",
            SettingItem::ReadingDirection => "Reading direction",
            SettingItem::LineSpacing => "Line spacing",
            SettingItem::ParagraphSpacing => "Paragraph spacing",
            SettingItem::Justify => "Justify text",
            SettingItem::TidySpacing => "Tidy spacing",
            SettingItem::ShowSidebar => "Sidebar by default",
            SettingItem::ShowStatus => "Status bar by default",
            SettingItem::StatusTheme => "Theme",
            SettingItem::StatusView => "View",
            SettingItem::StatusPosition => "Position",
            SettingItem::StatusPercent => "Percent",
            SettingItem::StatusGauge => "Gauge",
            SettingItem::ImageMaxPx => "Max resolution (px)",
            SettingItem::ImageWidthPct => "Figure width %",
            SettingItem::ImageFit => "Figure sizing",
            SettingItem::ImageMode => "Image mode",
            SettingItem::GraphicalMath => "Graphical math",
            SettingItem::MathScale => "Math size %",
            SettingItem::EquationScale => "Equation size %",
            SettingItem::CodeWrap => "Wrap code blocks",
            SettingItem::TableWrap => "Wrap tables",
            SettingItem::Paged => "Page mode",
            SettingItem::Continuous => "Continuous scroll",
            SettingItem::ChapterLock => "Chapter lock",
            SettingItem::TrimMargins => "Trim PDF margins",
            SettingItem::PdfMargin => "PDF margin crop %",
            SettingItem::Mouse => "Mouse",
            SettingItem::LibLayout => "Layout",
            SettingItem::GridSize => "Cover size",
            SettingItem::Column(key) => crate::config::LIB_COLUMNS
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, label)| *label)
                .unwrap_or(key),
            SettingItem::DupConvertedDelete => "Converted copies: always delete",
            SettingItem::DupFormat(label) => label,
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
            SettingItem::PageGap => c.page_gap.to_string(),
            SettingItem::CoverOffset => onoff(c.cover_offset),
            SettingItem::ReadingDirection => match c.reading_direction {
                crate::config::ReadingDirection::Ltr => "left-to-right".to_string(),
                crate::config::ReadingDirection::Rtl => "right-to-left (manga)".to_string(),
            },
            SettingItem::LineSpacing => c.line_spacing.to_string(),
            SettingItem::ParagraphSpacing => c.paragraph_spacing.to_string(),
            SettingItem::Justify => onoff(c.justify),
            SettingItem::TidySpacing => onoff(c.tidy_spacing),
            SettingItem::ShowSidebar => onoff(c.show_sidebar),
            SettingItem::ShowStatus => onoff(c.show_status),
            SettingItem::StatusTheme => onoff(c.status.theme),
            SettingItem::StatusView => onoff(c.status.view),
            SettingItem::StatusPosition => onoff(c.status.position),
            SettingItem::StatusPercent => onoff(c.status.percent),
            SettingItem::StatusGauge => onoff(c.status.gauge),
            SettingItem::ImageMaxPx => {
                if c.image_max_px == 0 {
                    "off".into()
                } else {
                    c.image_max_px.to_string()
                }
            }
            SettingItem::ImageWidthPct => format!("{}%", c.image_width_pct),
            SettingItem::ImageFit => c.image_fit.label().to_string(),
            SettingItem::ImageMode => c.image_mode.label().to_string(),
            SettingItem::GraphicalMath => onoff(c.graphical_math),
            SettingItem::MathScale => format!("{}%", c.math_scale),
            SettingItem::EquationScale => format!("{}%", c.equation_scale),
            SettingItem::CodeWrap => onoff(c.code_wrap),
            SettingItem::TableWrap => onoff(c.table_wrap),
            SettingItem::Paged => onoff(c.paged),
            SettingItem::Continuous => onoff(c.continuous),
            SettingItem::ChapterLock => onoff(c.chapter_lock),
            SettingItem::TrimMargins => onoff(c.pdf_trim),
            SettingItem::PdfMargin => format!("{}%", c.pdf_margin_pct),
            SettingItem::Mouse => onoff(c.mouse_enabled),
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
    Section(&'static str),
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
pub fn settings_tabs(scope: Mode) -> Vec<SettingTab> {
    use SettingItem::*;
    use SettingRow::{Item as I, Section as S};
    let tab = |title, rows| SettingTab { title, rows };
    match scope {
        Mode::Reader => vec![
            tab(
                "Reading",
                vec![
                    S("Profile"),
                    I(ReadingMode),
                    S("Typography"),
                    I(Theme),
                    I(ViewMode),
                    I(SidePadding),
                    I(PageGap),
                    I(CoverOffset),
                    I(ReadingDirection),
                    I(LineSpacing),
                    I(ParagraphSpacing),
                    I(Justify),
                ],
            ),
            tab(
                "Chrome",
                vec![
                    S("Panes"),
                    I(ShowSidebar),
                    I(ShowStatus),
                    S("Status bar segments"),
                    I(StatusTheme),
                    I(StatusView),
                    I(StatusPosition),
                    I(StatusPercent),
                    I(StatusGauge),
                ],
            ),
            tab(
                "Content",
                vec![
                    S("Images"),
                    I(ImageMaxPx),
                    I(ImageWidthPct),
                    I(ImageFit),
                    I(ImageMode),
                    I(GraphicalMath),
                    I(MathScale),
                    I(EquationScale),
                    S("Blocks"),
                    I(CodeWrap),
                    I(TableWrap),
                    I(TidySpacing),
                    S("Pagination"),
                    I(Paged),
                    I(Continuous),
                    I(ChapterLock),
                    S("PDF"),
                    I(TrimMargins),
                    I(PdfMargin),
                ],
            ),
            tab("Input", vec![S("Mouse"), I(Mouse)]),
        ],
        Mode::Library => {
            // The star + Title columns are always on; the rest are user-toggled.
            let mut columns = vec![S("Show / hide")];
            columns.extend(
                crate::config::LIB_COLUMNS
                    .iter()
                    .map(|(key, _)| I(Column(key))),
            );
            // Duplicate-resolver preferences: a converted-copies toggle and the
            // per-format keep priority (l/h on a format raises/lowers its rank).
            let mut dups = vec![
                S("Auto-select"),
                I(DupConvertedDelete),
                S("Keep priority — l/h"),
            ];
            dups.extend(BookFormat::ALL.iter().map(|f| I(DupFormat(f.label()))));
            vec![
                tab("View", vec![S("Layout"), I(LibLayout), I(GridSize)]),
                tab("Columns", columns),
                tab("Duplicates", dups),
                tab(
                    "General",
                    vec![S("Appearance"), I(Theme), S("Input"), I(Mouse)],
                ),
            ]
        }
    }
}

/// The rows of one tab (empty if the index is out of range).
pub fn tab_rows(scope: Mode, tab: usize) -> Vec<SettingRow> {
    settings_tabs(scope)
        .into_iter()
        .nth(tab)
        .map(|t| t.rows)
        .unwrap_or_default()
}

/// Index of the first selectable item in a tab (skips a leading section header).
pub fn first_setting_row(scope: Mode, tab: usize) -> usize {
    tab_rows(scope, tab)
        .iter()
        .position(|r| matches!(r, SettingRow::Item(_)))
        .unwrap_or(0)
}

impl App {
    pub(crate) fn settings_key(&mut self, key: KeyEvent) {
        if !matches!(self.overlay, Overlay::Settings(_)) {
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char(';') | KeyCode::Char('q') => {
                self.overlay = Overlay::None;
                self.config.save();
            }
            KeyCode::Tab => self.settings_tab(1),
            KeyCode::BackTab => self.settings_tab(-1),
            KeyCode::Char('j') | KeyCode::Down => self.settings_move(1),
            KeyCode::Char('k') | KeyCode::Up => self.settings_move(-1),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.settings_change(1),
            KeyCode::Char('h') | KeyCode::Left => self.settings_change(-1),
            _ => {}
        }
    }

    /// Switch tab by `delta` (wrapping), parking the cursor on its first option.
    fn settings_tab(&mut self, delta: isize) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let n = settings_tabs(s.scope).len();
        if n == 0 {
            return;
        }
        let tab = (s.tab as isize + delta).rem_euclid(n as isize) as usize;
        let row = first_setting_row(s.scope, tab);
        if let Overlay::Settings(s) = &mut self.overlay {
            s.tab = tab;
            s.row = row;
        }
    }

    /// Move the settings cursor by `delta` items within the active tab, skipping
    /// section headers.
    fn settings_move(&mut self, delta: isize) {
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        let rows = tab_rows(s.scope, s.tab);
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

    fn settings_change(&mut self, delta: i32) {
        use crate::config::{MAX_LINE_SPACING, MAX_PAGE_GAP, MAX_SIDE_PADDING};
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        // Resolve the focused row (in the active tab) to a setting identity.
        let Some(SettingRow::Item(item)) = tab_rows(s.scope, s.tab).into_iter().nth(s.row) else {
            return;
        };
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
            SettingItem::TidySpacing => c.tidy_spacing = !c.tidy_spacing,
            SettingItem::ShowSidebar => c.show_sidebar = !c.show_sidebar,
            SettingItem::ShowStatus => c.show_status = !c.show_status,
            SettingItem::StatusTheme => c.status.theme = !c.status.theme,
            SettingItem::StatusView => c.status.view = !c.status.view,
            SettingItem::StatusPosition => c.status.position = !c.status.position,
            SettingItem::StatusPercent => c.status.percent = !c.status.percent,
            SettingItem::StatusGauge => c.status.gauge = !c.status.gauge,
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
            SettingItem::MathScale => {
                use crate::config::{MAX_MATH_SCALE, MIN_MATH_SCALE};
                // Step in 10% increments within the allowed band.
                c.math_scale = (c.math_scale as i32 + delta * 10)
                    .clamp(MIN_MATH_SCALE as i32, MAX_MATH_SCALE as i32)
                    as u16
            }
            SettingItem::EquationScale => {
                use crate::config::{MAX_EQUATION_SCALE, MIN_EQUATION_SCALE};
                // Step in 10% increments within the allowed band.
                c.equation_scale = (c.equation_scale as i32 + delta * 10)
                    .clamp(MIN_EQUATION_SCALE as i32, MAX_EQUATION_SCALE as i32)
                    as u16
            }
            SettingItem::CodeWrap => c.code_wrap = !c.code_wrap,
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
            SettingItem::Column(key) => c.toggle_column(key),
            SettingItem::DupConvertedDelete => c.dup_converted_delete = !c.dup_converted_delete,
            // l/right/Enter (delta > 0) promotes the format toward "keep #1".
            SettingItem::DupFormat(label) => c.move_dup_format(label, delta > 0),
        }
    }

    /// Open Library settings on the Duplicates tab — the resolve overlay's
    /// preferences. Closes the overlay (settings replaces it); reopen with `D`.
    pub(crate) fn open_dup_settings(&mut self) {
        self.overlay = Overlay::None;
        let scope = Mode::Library;
        let tab = settings_tabs(scope)
            .iter()
            .position(|t| t.title == "Duplicates")
            .unwrap_or(0);
        let row = first_setting_row(scope, tab);
        self.overlay = Overlay::Settings(Settings { scope, tab, row });
    }
}
