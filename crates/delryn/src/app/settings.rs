//! The settings popup: a scoped (Reading vs Library) list of adjustable options
//! and the keys that move through and change them.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Mode};
use crate::config::Config;

/// Open settings popup. Scoped to the mode it was opened from — Reading settings
/// in the reader, Library settings in the library — so the two never mix.
pub struct Settings {
    pub scope: Mode,
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
    LineSpacing,
    ParagraphSpacing,
    ShowSidebar,
    ShowStatus,
    StatusTheme,
    StatusView,
    StatusPosition,
    StatusPercent,
    StatusGauge,
    ImageMaxPx,
    ImageMode,
    CodeWrap,
    TableWrap,
    Paged,
    ChapterLock,
    Mouse,
    LibLayout,
    GridSize,
}

impl SettingItem {
    pub fn label(self) -> &'static str {
        match self {
            SettingItem::ReadingMode => "Reading mode",
            SettingItem::Theme => "Theme",
            SettingItem::ViewMode => "View mode",
            SettingItem::SidePadding => "Side margin %",
            SettingItem::PageGap => "Two-page gap",
            SettingItem::LineSpacing => "Line spacing",
            SettingItem::ParagraphSpacing => "Paragraph spacing",
            SettingItem::ShowSidebar => "Sidebar by default",
            SettingItem::ShowStatus => "Status bar by default",
            SettingItem::StatusTheme => "Theme",
            SettingItem::StatusView => "View",
            SettingItem::StatusPosition => "Position",
            SettingItem::StatusPercent => "Percent",
            SettingItem::StatusGauge => "Gauge",
            SettingItem::ImageMaxPx => "Max resolution (px)",
            SettingItem::ImageMode => "Image mode",
            SettingItem::CodeWrap => "Wrap code blocks",
            SettingItem::TableWrap => "Wrap tables",
            SettingItem::Paged => "Page mode",
            SettingItem::ChapterLock => "Chapter lock",
            SettingItem::Mouse => "Mouse",
            SettingItem::LibLayout => "Layout",
            SettingItem::GridSize => "Cover size",
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
            SettingItem::LineSpacing => c.line_spacing.to_string(),
            SettingItem::ParagraphSpacing => c.paragraph_spacing.to_string(),
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
            SettingItem::ImageMode => c.image_mode.label().to_string(),
            SettingItem::CodeWrap => onoff(c.code_wrap),
            SettingItem::TableWrap => onoff(c.table_wrap),
            SettingItem::Paged => onoff(c.paged),
            SettingItem::ChapterLock => onoff(c.chapter_lock),
            SettingItem::Mouse => onoff(c.mouse_enabled),
            SettingItem::LibLayout => c.library_layout.label().to_string(),
            SettingItem::GridSize => c.library_grid_size.label().to_string(),
        }
    }
}

/// A row in the settings popup: a non-selectable section header or a setting.
pub enum SettingRow {
    Section(&'static str),
    Item(SettingItem),
}

/// The grouped rows for a settings scope (section headers + items). Each scope
/// is self-contained: the reader shows only reading settings, the library only
/// library settings (global toggles like Theme/Mouse appear in both).
pub fn settings_rows(scope: Mode) -> Vec<SettingRow> {
    use SettingItem::*;
    use SettingRow::{Item as I, Section as S};
    match scope {
        Mode::Reader => vec![
            S("Profile"),
            I(ReadingMode),
            S("Typography"),
            I(Theme),
            I(ViewMode),
            I(SidePadding),
            I(PageGap),
            I(LineSpacing),
            I(ParagraphSpacing),
            S("Chrome"),
            I(ShowSidebar),
            I(ShowStatus),
            S("Status bar segments"),
            I(StatusTheme),
            I(StatusView),
            I(StatusPosition),
            I(StatusPercent),
            I(StatusGauge),
            S("Content"),
            I(ImageMaxPx),
            I(ImageMode),
            I(CodeWrap),
            I(TableWrap),
            I(Paged),
            I(ChapterLock),
            S("Input"),
            I(Mouse),
        ],
        Mode::Library => vec![
            S("View"),
            I(LibLayout),
            I(GridSize),
            S("Appearance"),
            I(Theme),
            S("Input"),
            I(Mouse),
        ],
    }
}

/// Index of the first selectable item in a scope (skips a leading section header).
pub fn first_setting_row(scope: Mode) -> usize {
    settings_rows(scope)
        .iter()
        .position(|r| matches!(r, SettingRow::Item(_)))
        .unwrap_or(0)
}

impl App {
    pub(crate) fn settings_key(&mut self, key: KeyEvent) {
        if self.settings.is_none() {
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char(';') | KeyCode::Char('q') => {
                self.settings = None;
                self.config.save();
            }
            KeyCode::Char('j') | KeyCode::Down => self.settings_move(1),
            KeyCode::Char('k') | KeyCode::Up => self.settings_move(-1),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.settings_change(1),
            KeyCode::Char('h') | KeyCode::Left => self.settings_change(-1),
            _ => {}
        }
    }

    /// Move the settings cursor by `delta` items, skipping section headers.
    fn settings_move(&mut self, delta: isize) {
        let Some(s) = self.settings.as_ref() else {
            return;
        };
        let rows = settings_rows(s.scope);
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
        if let Some(s) = self.settings.as_mut() {
            s.row = items[next];
        }
    }

    fn settings_change(&mut self, delta: i32) {
        use crate::config::{MAX_LINE_SPACING, MAX_PAGE_GAP, MAX_SIDE_PADDING};
        let Some(s) = self.settings.as_ref() else {
            return;
        };
        // Resolve the focused row to a setting identity.
        let Some(SettingRow::Item(item)) = settings_rows(s.scope).into_iter().nth(s.row) else {
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
            SettingItem::LineSpacing => {
                c.line_spacing =
                    (c.line_spacing as i32 + delta).clamp(0, MAX_LINE_SPACING as i32) as u8
            }
            SettingItem::ParagraphSpacing => {
                c.paragraph_spacing = (c.paragraph_spacing as i32 + delta).clamp(0, 3) as u8
            }
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
            SettingItem::ImageMode => {
                c.image_mode = if delta > 0 {
                    c.image_mode.next()
                } else {
                    c.image_mode.prev()
                }
            }
            SettingItem::CodeWrap => c.code_wrap = !c.code_wrap,
            SettingItem::TableWrap => c.table_wrap = !c.table_wrap,
            SettingItem::Paged => c.paged = !c.paged,
            SettingItem::ChapterLock => c.chapter_lock = !c.chapter_lock,
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
        }
    }
}
