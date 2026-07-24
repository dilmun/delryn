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
            SettingItem::Source(_) | SettingItem::AddSource | SettingItem::RescanNow => {
                String::new()
            }
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
                "Content",
                vec![
                    S("Images"),
                    I(ImageMaxPx),
                    I(ImageWidthPct),
                    I(ImageFit),
                    I(ImageMode),
                    I(GraphicalMath),
                    I(GraphicalInlineMath),
                    I(MathScale),
                    I(BreakWideEquations),
                    S("Code"),
                    I(CodeWrap),
                    I(CodeLineNumbers),
                    I(CodeLanguageLabel),
                    I(CodeFold),
                    I(CodeFoldThreshold),
                    S("Tables & text"),
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
            tab(
                "Chrome",
                vec![
                    S("Panes"),
                    I(ShowSidebar),
                    I(ShowStatus),
                    I(BoldBorders),
                    S("Status bar segments"),
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
                    S("Word lookup (K)"),
                    I(LookupSdcv),
                    I(LookupDictionary),
                    I(LookupWikipedia),
                    S("Translation"),
                    I(LookupTranslate),
                    I(TranslateTo),
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
            // The Sources tab manages the library's scanned folders: one row per
            // configured folder (built from the live path list), an add-folder
            // action, and a rescan action. First-run (empty library) still lands
            // here — not because it's first, but via `open_sources_if_empty`, which
            // finds the tab by title.
            let mut sources = vec![S("Folders")];
            sources.extend((0..config.library_paths.len()).map(|i| I(Source(i))));
            sources.push(I(AddSource));
            sources.push(I(RescanNow));
            // Ordered most-frequently-used first: view/columns are constant tweaks,
            // appearance occasional, sources set-and-forget, dup-prefs rare.
            vec![
                tab("View", vec![S("Layout"), I(LibLayout), I(GridSize)]),
                tab("Columns", columns),
                tab(
                    "General",
                    vec![
                        S("Appearance"),
                        I(Theme),
                        I(BoldBorders),
                        S("Input"),
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
            // Remove the focused library source (no-op on any other row/tab).
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
                self.settings_delete_source()
            }
            _ => {}
        }
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
            tab_rows(s.scope, s.tab, &self.config)
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
        let rows = tab_rows(s.scope, s.tab, &self.config);
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
        use crate::config::{MAX_LINE_SPACING, MAX_PAGE_GAP, MAX_SIDE_PADDING};
        let Overlay::Settings(s) = &self.overlay else {
            return;
        };
        // Resolve the focused row (in the active tab) to a setting identity.
        let Some(SettingRow::Item(item)) = tab_rows(s.scope, s.tab, &self.config)
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
            SettingItem::Source(_) => return, // delete via `d`/Delete, not change
            _ => {}
        }
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
            SettingItem::Source(_) | SettingItem::AddSource | SettingItem::RescanNow => {}
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
        });
    }
}
