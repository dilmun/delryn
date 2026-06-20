//! Application state and event dispatch.
//!
//! Two top-level modes behaving like tabs (Library | Reader). For now the
//! Library is a stub; the Reader is the working EPUB vertical slice. See
//! `DESIGN.md` §4, §6.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::config::Config;
use crate::document::epub::EpubDocument;
use crate::document::{Block, Document, OutlineItem, normalize_label};
use crate::input::{self, Action, Pending};
use crate::layout::wrap_blocks;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Library,
    Reader,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Content,
    Sidebar,
}

/// Rects from the last render, used for mouse hit-testing.
#[derive(Default)]
pub struct LayoutRects {
    pub sidebar: Option<Rect>,
    pub content: Option<Rect>,
}

pub struct Reader {
    pub doc: Box<dyn Document>,
    pub outline: Vec<OutlineItem>,
    pub section: usize,
    pub blocks: Vec<Block>,
    /// Wrapped display lines of the current section, valid for `wrap_width`.
    pub lines: Vec<String>,
    pub wrap_width: usize,
    /// Index of the top visible line within `lines`.
    pub scroll: usize,
    pub focus: Focus,
    pub sidebar_sel: usize,
    /// Height of one column in lines, refreshed each draw.
    pub viewport_lines: usize,
    /// Total lines visible at once (2 columns in two-page mode), for scroll math.
    pub page_lines: usize,
    /// Wrap width used by the last render; used to locate jump targets.
    pub last_measure: usize,
}

impl Reader {
    pub fn new(mut doc: Box<dyn Document>) -> Result<Self> {
        let outline = doc.outline().to_vec();
        let first = doc.load_section(0).unwrap_or_default();
        Ok(Self {
            doc,
            outline,
            section: 0,
            blocks: first.blocks,
            lines: Vec::new(),
            wrap_width: 0,
            scroll: 0,
            focus: Focus::Content,
            sidebar_sel: 0,
            viewport_lines: 1,
            page_lines: 1,
            last_measure: 72,
        })
    }

    /// Re-wrap the current section if the measure changed.
    pub fn ensure_wrapped(&mut self, width: usize) {
        if width != self.wrap_width {
            self.lines = wrap_blocks(&self.blocks, width);
            self.wrap_width = width;
        }
    }

    pub fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.page_lines.max(1))
    }

    pub fn clamp_scroll(&mut self) {
        let m = self.max_scroll();
        if self.scroll > m {
            self.scroll = m;
        }
    }

    pub fn load(&mut self, section: usize) {
        if section >= self.doc.section_count() {
            return;
        }
        self.blocks = match self.doc.load_section(section) {
            Ok(s) => s.blocks,
            Err(_) => Vec::new(),
        };
        self.section = section;
        self.scroll = 0;
        self.wrap_width = 0; // force a re-wrap on next draw
    }

    /// Scroll down, flowing into the next chapter at the bottom edge.
    pub fn scroll_down(&mut self, n: usize) {
        let max = self.max_scroll();
        if self.scroll < max {
            self.scroll = (self.scroll + n).min(max);
        } else if self.section + 1 < self.doc.section_count() {
            self.load(self.section + 1);
        }
    }

    /// Scroll up, flowing into the previous chapter at the top edge.
    pub fn scroll_up(&mut self, n: usize) {
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_sub(n);
        } else if self.section > 0 {
            self.load(self.section - 1);
            self.scroll = usize::MAX; // clamped to the bottom on next draw
        }
    }

    /// Navigate to a section and, if given, scroll to the line whose text
    /// matches `locator` (a heading). Misses fall back to the section top.
    pub fn jump_to(&mut self, section: usize, locator: Option<&str>) {
        if section != self.section {
            self.load(section);
        } else {
            self.scroll = 0;
        }
        if let Some(text) = locator {
            self.ensure_wrapped(self.last_measure.max(1));
            if let Some(line) = find_line(&self.lines, text) {
                self.scroll = line;
            }
        }
        self.focus = Focus::Content;
    }

    pub fn sidebar_move(&mut self, delta: isize) {
        if self.outline.is_empty() {
            return;
        }
        let last = self.outline.len() as isize - 1;
        let s = (self.sidebar_sel as isize + delta).clamp(0, last);
        self.sidebar_sel = s as usize;
    }

    /// Overall reading progress in `[0, 1]`.
    pub fn progress(&self) -> f32 {
        let n = self.doc.section_count().max(1) as f32;
        let within = if self.lines.is_empty() {
            0.0
        } else {
            self.scroll as f32 / self.lines.len() as f32
        };
        ((self.section as f32) + within) / n
    }

    pub fn chapter_title(&self) -> String {
        self.outline
            .iter()
            .find(|e| e.section == self.section && e.depth == 0)
            .map(|e| e.label.clone())
            .unwrap_or_else(|| format!("Section {}", self.section + 1))
    }
}

/// First wrapped line whose normalized text matches `needle`. Prefers a line
/// that *is* the heading before falling back to a substring match, so a short
/// heading like "Linux" lands on the header rather than an earlier mention.
fn find_line(lines: &[String], needle: &str) -> Option<usize> {
    let n = normalize_label(needle);
    if n.is_empty() {
        return None;
    }
    if let Some(i) = lines.iter().position(|l| normalize_label(l) == n) {
        return Some(i);
    }
    lines.iter().position(|l| {
        let line = normalize_label(l);
        !line.is_empty() && (line.contains(&n) || (n.len() >= 8 && n.contains(&line)))
    })
}

pub struct App {
    pub mode: Mode,
    pub config: Config,
    pub reader: Option<Reader>,
    pub last_layout: LayoutRects,
    pub pending: Pending,
    pub should_quit: bool,
}

impl App {
    pub fn open_book(path: &str) -> Result<Self> {
        let doc = EpubDocument::open(path)?;
        let reader = Reader::new(Box::new(doc))?;
        Ok(Self {
            mode: Mode::Reader,
            config: Config::default(),
            reader: Some(reader),
            last_layout: LayoutRects::default(),
            pending: Pending::default(),
            should_quit: false,
        })
    }

    pub fn library() -> Self {
        Self {
            mode: Mode::Library,
            config: Config::default(),
            reader: None,
            last_layout: LayoutRects::default(),
            pending: Pending::default(),
            should_quit: false,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match self.mode {
            Mode::Reader => {
                let action = input::map_key(key, &mut self.pending);
                self.apply(action);
            }
            Mode::Library => self.library_key(key),
        }
    }

    fn library_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => self.should_quit = true,
            // With a book loaded, jump back into it.
            KeyCode::Enter | KeyCode::Char('l') if self.reader.is_some() => {
                self.mode = Mode::Reader;
            }
            _ => {}
        }
    }

    fn apply(&mut self, action: Action) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back => self.mode = Mode::Library, // book stays loaded
            Action::Down(n) => match reader.focus {
                Focus::Content => reader.scroll_down(n),
                Focus::Sidebar => reader.sidebar_move(n as isize),
            },
            Action::Up(n) => match reader.focus {
                Focus::Content => reader.scroll_up(n),
                Focus::Sidebar => reader.sidebar_move(-(n as isize)),
            },
            Action::HalfDown => reader.scroll_down(reader.page_lines.max(2) / 2),
            Action::HalfUp => reader.scroll_up(reader.page_lines.max(2) / 2),
            Action::PageDown => reader.scroll_down(reader.page_lines.max(1)),
            Action::PageUp => reader.scroll_up(reader.page_lines.max(1)),
            Action::Top => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_sel = 0;
                } else {
                    reader.scroll = 0;
                }
            }
            Action::Bottom => {
                if reader.focus == Focus::Sidebar {
                    reader.sidebar_sel = reader.outline.len().saturating_sub(1);
                } else {
                    reader.scroll = reader.max_scroll();
                }
            }
            Action::ToggleStatus => self.config.show_status = !self.config.show_status,
            Action::CycleView => self.config.view_mode = self.config.view_mode.next(),
            Action::ToggleSidebar => {
                self.config.show_sidebar = !self.config.show_sidebar;
                if !self.config.show_sidebar {
                    reader.focus = Focus::Content;
                }
            }
            Action::FocusToggle => {
                // Tab moves focus into the sidebar (showing it first if hidden),
                // then back to the content.
                if !self.config.show_sidebar {
                    self.config.show_sidebar = true;
                    reader.focus = Focus::Sidebar;
                } else if reader.focus == Focus::Content {
                    reader.focus = Focus::Sidebar;
                } else {
                    reader.focus = Focus::Content;
                }
            }
            Action::Activate => {
                if reader.focus == Focus::Sidebar {
                    if let Some(item) = reader.outline.get(reader.sidebar_sel).cloned() {
                        reader.jump_to(item.section, item.locator.as_deref());
                    }
                }
            }
            Action::None => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if !self.config.mouse_enabled || self.mode != Mode::Reader {
            return;
        }
        match m.kind {
            MouseEventKind::ScrollDown => {
                if let Some(r) = self.reader.as_mut() {
                    r.scroll_down(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(r) = self.reader.as_mut() {
                    r.scroll_up(3);
                }
            }
            MouseEventKind::Down(_) => self.mouse_click(m.column, m.row),
            _ => {}
        }
    }

    /// Click on a sidebar row selects and jumps to that TOC entry.
    fn mouse_click(&mut self, col: u16, row: u16) {
        let Some(sb) = self.last_layout.sidebar else {
            return;
        };
        let in_x = col >= sb.x && col < sb.x + sb.width;
        // Account for the sidebar's top/bottom border.
        let first = sb.y + 1;
        let last = sb.y + sb.height.saturating_sub(1);
        if !in_x || row < first || row >= last {
            return;
        }
        let idx = (row - first) as usize;
        if let Some(r) = self.reader.as_mut() {
            if let Some(item) = r.outline.get(idx).cloned() {
                r.sidebar_sel = idx;
                r.jump_to(item.section, item.locator.as_deref());
            }
        }
    }
}
