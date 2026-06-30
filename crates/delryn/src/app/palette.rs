//! Command palette (`:`): a fuzzy-filtered launcher for library actions —
//! jump to a section/collection, change sort/view, toggle panes, open stats,
//! export. Self-contained: each command runs via the app's public state + the
//! pub(crate) refresh, so no extra visibility surface is needed.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, LibView, Overlay, SortKey};
use crate::store::LibrarySection;
use crate::ui::TextInput;

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
    pub input: TextInput,
    pub sel: usize,
    items: Vec<PaletteItem>,
}

impl Palette {
    /// Items matching the current query, best-ranked first.
    pub fn filtered(&self) -> Vec<&PaletteItem> {
        delryn_library::fuzzy::rank(self.input.text(), &self.items, |it| it.label.as_str())
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
            item("Sort by Type", Command::Sort(SortKey::Type)),
            item("Sort by Source", Command::Sort(SortKey::Source)),
            item("Sort by Rating", Command::Sort(SortKey::Rating)),
            item("Sort by Progress", Command::Sort(SortKey::Progress)),
            item("Sort by Size", Command::Sort(SortKey::Size)),
            item("Sort by Status", Command::Sort(SortKey::Status)),
            item("Sort by Tags", Command::Sort(SortKey::Tags)),
            item("Sort: Section order", Command::Sort(SortKey::Default)),
            item("Sort: Reverse direction", Command::ToggleSortDir),
            item("View: Cycle layout", Command::CycleLayout),
            item("Toggle sidebar", Command::ToggleSidebar),
            item("Toggle detail pane", Command::ToggleDetail),
            item("Library statistics", Command::Stats),
            item("Export to CSV", Command::Export),
        ];
        // Jump-to-collection commands for each existing shelf.
        for (name, _) in &self.library.shelves {
            items.push(item(
                format!("Go: collection · {name}"),
                Command::JumpShelf(name.clone()),
            ));
        }
        self.overlay = Overlay::Palette(Palette {
            input: TextInput::new(),
            sel: 0,
            items,
        });
    }

    /// Keys while the palette is open.
    pub(crate) fn palette_key(&mut self, key: KeyEvent) {
        let Overlay::Palette(p) = &mut self.overlay else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Up => {
                p.sel = p.sel.saturating_sub(1);
            }
            KeyCode::Down => {
                let n = p.filtered().len();
                p.sel = (p.sel + 1).min(n.saturating_sub(1));
            }
            KeyCode::Enter => {
                let cmd = p.filtered().get(p.sel).map(|it| it.cmd.clone());
                self.overlay = Overlay::None;
                if let Some(cmd) = cmd {
                    self.run_command(cmd);
                }
            }
            _ => {
                // Editing the query resets the selection to the top match.
                let before = p.input.text().len();
                p.input.handle_key(key);
                if p.input.text().len() != before {
                    p.sel = 0;
                }
            }
        }
    }

    fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::JumpSection(s) => {
                self.library.view = LibView::Section(s);
                self.library.sel = 0;
                self.refresh_library();
            }
            Command::JumpShelf(name) => {
                self.library.view = LibView::Shelf(name);
                self.library.sel = 0;
                self.refresh_library();
            }
            Command::Sort(key) => {
                // Explicit pick starts ascending; cycling with `s` toggles direction.
                self.library.sort = key;
                self.library.sort_desc = false;
                self.refresh_library();
            }
            Command::ToggleSortDir => {
                self.library.sort_desc = !self.library.sort_desc;
                self.refresh_library();
            }
            Command::CycleLayout => {
                self.config.library_layout = self.config.library_layout.next();
                self.config.save();
            }
            Command::ToggleSidebar => self.library.show_sidebar = !self.library.show_sidebar,
            Command::ToggleDetail => self.library.detail = !self.library.detail,
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
