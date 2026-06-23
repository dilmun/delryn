//! Command palette (`:`): a fuzzy-filtered launcher for library actions —
//! jump to a section/collection, change sort/view, toggle panes, open stats,
//! export. Self-contained: each command runs via the app's public state + the
//! pub(crate) refresh, so no extra visibility surface is needed.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, LibView, SortKey};
use crate::store::LibrarySection;

/// An executable palette action.
#[derive(Clone)]
pub enum Command {
    JumpSection(LibrarySection),
    JumpShelf(String),
    Sort(SortKey),
    ToggleSortDir,
    CycleLayout,
    ToggleSidebar,
    ToggleDetail,
    Stats,
    Export,
}

/// One palette row: its display label and the command it runs.
pub struct PaletteItem {
    pub label: String,
    pub cmd: Command,
}

/// Open command-palette state.
pub struct Palette {
    pub query: String,
    pub cursor: usize,
    pub sel: usize,
    items: Vec<PaletteItem>,
}

impl Palette {
    /// Items matching the current query, best-ranked first.
    pub fn filtered(&self) -> Vec<&PaletteItem> {
        delryn_library::fuzzy::rank(&self.query, &self.items, |it| it.label.as_str())
    }
}

impl App {
    /// Open the command palette (library actions + jump-to targets).
    pub(crate) fn open_palette(&mut self) {
        let mut items = vec![
            item("Go: Recent", Command::JumpSection(LibrarySection::Recent)),
            item("Go: All Books", Command::JumpSection(LibrarySection::All)),
            item(
                "Go: Favorites",
                Command::JumpSection(LibrarySection::Favorites),
            ),
            item(
                "Go: Currently Reading",
                Command::JumpSection(LibrarySection::Reading),
            ),
            item("Go: Series", Command::JumpSection(LibrarySection::Series)),
            item(
                "Go: Duplicates",
                Command::JumpSection(LibrarySection::Duplicates),
            ),
            item("Sort by Title", Command::Sort(SortKey::Title)),
            item("Sort by Author", Command::Sort(SortKey::Author)),
            item("Sort by Year", Command::Sort(SortKey::Year)),
            item("Sort by Rating", Command::Sort(SortKey::Rating)),
            item("Sort by Progress", Command::Sort(SortKey::Progress)),
            item("Sort: Reverse direction", Command::ToggleSortDir),
            item("View: Cycle layout", Command::CycleLayout),
            item("Toggle sidebar", Command::ToggleSidebar),
            item("Toggle detail pane", Command::ToggleDetail),
            item("Library statistics", Command::Stats),
            item("Export to CSV", Command::Export),
        ];
        // Jump-to-collection commands for each existing shelf.
        for (name, _) in &self.lib_shelves {
            items.push(item(
                format!("Go: collection · {name}"),
                Command::JumpShelf(name.clone()),
            ));
        }
        self.palette = Some(Palette {
            query: String::new(),
            cursor: 0,
            sel: 0,
            items,
        });
    }

    /// Keys while the palette is open.
    pub(crate) fn palette_key(&mut self, key: KeyEvent) {
        let Some(p) = self.palette.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Up => {
                p.sel = p.sel.saturating_sub(1);
            }
            KeyCode::Down => {
                let n = p.filtered().len();
                p.sel = (p.sel + 1).min(n.saturating_sub(1));
            }
            KeyCode::Left => p.cursor = p.cursor.saturating_sub(1),
            KeyCode::Right => p.cursor = (p.cursor + 1).min(p.query.chars().count()),
            KeyCode::Backspace => {
                let cur = p.cursor;
                if super::str_delete_before(&mut p.query, cur) {
                    p.cursor -= 1;
                    p.sel = 0;
                }
            }
            KeyCode::Char(c) => {
                let cur = p.cursor;
                super::str_insert(&mut p.query, cur, c);
                p.cursor += 1;
                p.sel = 0;
            }
            KeyCode::Enter => {
                let cmd = p.filtered().get(p.sel).map(|it| it.cmd.clone());
                self.palette = None;
                if let Some(cmd) = cmd {
                    self.run_command(cmd);
                }
            }
            _ => {}
        }
    }

    fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::JumpSection(s) => {
                self.lib_view = LibView::Section(s);
                self.lib_sel = 0;
                self.refresh_library();
            }
            Command::JumpShelf(name) => {
                self.lib_view = LibView::Shelf(name);
                self.lib_sel = 0;
                self.refresh_library();
            }
            Command::Sort(key) => {
                self.lib_sort = key;
                self.refresh_library();
            }
            Command::ToggleSortDir => {
                self.lib_sort_desc = !self.lib_sort_desc;
                self.refresh_library();
            }
            Command::CycleLayout => {
                self.config.library_layout = self.config.library_layout.next();
                self.config.save();
            }
            Command::ToggleSidebar => self.lib_show_sidebar = !self.lib_show_sidebar,
            Command::ToggleDetail => self.lib_detail = !self.lib_detail,
            Command::Stats => self.open_stats(),
            Command::Export => self.export_library(),
        }
    }
}

fn item(label: impl Into<String>, cmd: Command) -> PaletteItem {
    PaletteItem {
        label: label.into(),
        cmd,
    }
}
