//! Mouse handling: the hit-rects captured during render and the click/scroll
//! routing that consults them.

use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::{App, EditMode, EditTab, Focus, LOOKUP_FIELDS, LibPane, Mode, Overlay, SortKey};

/// Max gap between two clicks on the same book to count as a double-click (→ open).
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Layout facts captured during the last render: pane rects for mouse hit-testing
/// plus the width-dependent metrics the library input handlers need. Kept here,
/// out of `LibraryState`, so render stays a pure function of state — the view
/// *writes* these each frame, the input layer *reads* them, and neither the
/// semantic state nor the render output depends on the other.
#[derive(Default)]
pub struct LayoutMetrics {
    pub sidebar: Option<Rect>,
    pub content: Option<Rect>,
    /// Library book-list pane rect, for routing the wheel to the pane under the
    /// cursor (so it doesn't scroll the list while the mouse is over another pane).
    pub lib_list: Option<Rect>,
    /// Library detail pane rect (right side), when shown.
    pub lib_detail: Option<Rect>,
    /// Sort keys `s` cycles — only the columns actually drawn at this width.
    pub sort_cycle: Vec<SortKey>,
    /// On-screen book rows, for vim half/full-page navigation.
    pub visible_rows: usize,
    /// Grid columns, for grid j/k row stepping.
    pub grid_cols: usize,
}

/// Clickable regions captured during the last render, for mouse hit-testing.
/// Rebuilt every frame by the view layer; consulted by `on_mouse`.
#[derive(Default)]
pub struct MouseHits {
    /// Library: (book index, screen rect) for each visible row / grid cell.
    pub books: Vec<(usize, Rect)>,
    /// Editor tab strip: (tab, rect).
    pub edit_tabs: Vec<(EditTab, Rect)>,
    /// Editor Details/File fields: (row index, value-start column, rect).
    pub edit_fields: Vec<(usize, u16, Rect)>,
    /// Editor Online/Cover results: (result index, rect).
    pub edit_results: Vec<(usize, Rect)>,
    /// Editor search bar rect.
    pub edit_search: Option<Rect>,
}

impl MouseHits {
    pub fn clear(&mut self) {
        self.books.clear();
        self.edit_tabs.clear();
        self.edit_fields.clear();
        self.edit_results.clear();
        self.edit_search = None;
    }
}

impl App {
    /// Handle a mouse event; returns whether it changed anything (so the loop only
    /// repaints then — a mouse-move flood from any-motion reporting is ignored).
    pub fn on_mouse(&mut self, m: MouseEvent) -> bool {
        if !self.config.mouse_enabled {
            return false;
        }
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let up = matches!(m.kind, MouseEventKind::ScrollUp);
                let d: isize = if up { -3 } else { 3 };
                match self.mode {
                    Mode::Reader => self.reader_wheel(m.column, m.row, d),
                    Mode::Library => self.library_wheel(m.column, m.row, d),
                }
            }
            MouseEventKind::Down(button) => self.mouse_down(m.column, m.row, button, m.modifiers),
            _ => false,
        }
    }

    /// Wheel in the reader: scroll whichever pane the cursor is over — the TOC
    /// (without changing the selection), the continuous PDF stack, a paged flip, or
    /// eased line-scroll.
    fn reader_wheel(&mut self, col: u16, row: u16, d: isize) -> bool {
        let over_sidebar = self
            .last_layout
            .sidebar
            .is_some_and(|sb| sb.contains((col, row).into()));
        let paged = self.config.paged;
        let Some(r) = self.reader.as_mut() else {
            return false;
        };
        if over_sidebar {
            r.sidebar_wheel(d);
        } else if r.continuous_paged_active() {
            // Continuous PDF: the wheel scrolls the vertical page stack in row units.
            if d > 0 {
                r.scroll_down(d as usize);
            } else {
                r.scroll_up((-d) as usize);
            }
        } else if paged || r.is_paged_image() {
            // Whole-page rasters (PDF) flip pages instead of eased line-scroll.
            if d > 0 {
                r.page_forward();
            } else {
                r.page_backward();
            }
        } else {
            r.queue_scroll(d);
        }
        true
    }

    /// Wheel in the library: scroll only the pane under the cursor — the sections
    /// sidebar, or the book list (extending a live visual range). The detail pane and
    /// empty areas don't scroll the list. Modal overlays swallow it.
    fn library_wheel(&mut self, col: u16, row: u16, d: isize) -> bool {
        if self.pending_confirm.is_some()
            || !matches!(self.overlay, Overlay::None)
            || self.library.filtering
        {
            return false;
        }
        let pt = (col, row).into();
        let (sidebar, detail, list) = (
            self.last_layout.sidebar,
            self.last_layout.lib_detail,
            self.last_layout.lib_list,
        );
        if sidebar.is_some_and(|r| r.contains(pt)) {
            // The sections list is short and each move reloads the book list, so
            // step one section per notch (not the list's multi-row scroll amount).
            self.lib_side_move(d.signum());
            true
        } else if detail.is_some_and(|r| r.contains(pt)) {
            false // detail isn't a scrollable list — don't touch the book list
        } else if list.is_some_and(|r| r.contains(pt)) {
            self.lib_move(d);
            self.lib_visual_sync();
            true
        } else {
            false
        }
    }

    /// Route a mouse-down to the active overlay / mode using the hit rects captured
    /// during the last render. Returns whether it changed anything.
    fn mouse_down(&mut self, col: u16, row: u16, button: MouseButton, mods: KeyModifiers) -> bool {
        // A pending confirmation is modal — swallow clicks until it's answered.
        if self.pending_confirm.is_some() {
            return false;
        }
        if matches!(self.overlay, Overlay::MetaEdit(_)) {
            if button == MouseButton::Left {
                self.editor_click(col, row);
            }
            return true;
        }
        // Any other overlay is keyboard-driven (no hit rects); swallow the click.
        if !matches!(self.overlay, Overlay::None) {
            return false;
        }
        match self.mode {
            Mode::Reader => {
                if button == MouseButton::Left {
                    self.mouse_click(col, row);
                }
                true
            }
            Mode::Library => self.library_click(col, row, button, mods),
        }
    }

    /// Library click. Left-click moves the cursor to the book (double-click opens
    /// it); Shift+left-click range-selects to it; right-click toggles it in the
    /// multi-selection (the mouse counterpart to `Space`). Returns whether a book
    /// was hit.
    fn library_click(
        &mut self,
        col: u16,
        row: u16,
        button: MouseButton,
        mods: KeyModifiers,
    ) -> bool {
        let pt = (col, row).into();
        let Some(&(idx, _)) = self.mouse.books.iter().find(|(_, r)| r.contains(pt)) else {
            return false;
        };
        let idx = idx.min(self.library.books.len().saturating_sub(1));
        self.library.pane = LibPane::List;

        if button == MouseButton::Right {
            self.lib_mouse_toggle_mark(idx);
            self.last_click = None;
            return true;
        }
        if button == MouseButton::Left && mods.contains(KeyModifiers::SHIFT) {
            self.lib_mouse_range_to(idx);
            self.last_click = None;
            return true;
        }
        if button != MouseButton::Left {
            self.library.sel = idx;
            return true;
        }
        // Plain left-click: a second click on the same book opens it; else select.
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(prev, t)| prev == idx && now.duration_since(t) < DOUBLE_CLICK);
        self.library.sel = idx;
        if double {
            self.last_click = None;
            self.open_selected();
        } else {
            self.last_click = Some((idx, now));
        }
        true
    }

    /// Editor click: switch tab, focus + edit a field (caret at the click),
    /// open the search bar, or pick a result.
    fn editor_click(&mut self, col: u16, row: u16) {
        let pt = (col, row).into();
        if let Some(&(tab, _)) = self.mouse.edit_tabs.iter().find(|(_, r)| r.contains(pt)) {
            self.meta_edit_goto_tab(tab);
            return;
        }
        if self.mouse.edit_search.is_some_and(|r| r.contains(pt)) {
            self.online_begin_query(None);
            return;
        }
        if let Some(&(idx, vstart, _)) = self
            .mouse
            .edit_fields
            .iter()
            .find(|(_, _, r)| r.contains(pt))
        {
            if let Overlay::MetaEdit(e) = &mut self.overlay {
                match e.tab {
                    EditTab::Online => {
                        // A Lookup seed field: focus + edit it, caret at the click.
                        e.lookup.focus = idx.min(LOOKUP_FIELDS - 1);
                        e.lookup.editing = true;
                        e.lookup.focus_caret(col.saturating_sub(vstart) as usize);
                    }
                    _ => {
                        e.row = idx;
                        e.mode = EditMode::Edit;
                        let pos = col.saturating_sub(vstart) as usize;
                        if let Some(field) = e.values.get_mut(e.row) {
                            field.set_cursor(pos);
                        }
                    }
                }
            }
            return;
        }
        let hit = self
            .mouse
            .edit_results
            .iter()
            .find(|(_, r)| r.contains(pt))
            .map(|&(idx, _)| idx);
        if let Some(idx) = hit
            && let Overlay::MetaEdit(e) = &mut self.overlay
        {
            if e.tab == EditTab::Online {
                // Move the combined focus onto the clicked result row.
                e.lookup.editing = false;
                e.lookup.focus = LOOKUP_FIELDS + idx;
                e.online.row = idx;
            } else {
                e.search_mut().row = idx;
            }
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
        if let Some(r) = self.reader.as_mut() {
            // Screen row → list index, accounting for the scrolled viewport.
            let idx = r.sidebar_offset + (row - first) as usize;
            let vis = r.outline_visible();
            if let Some(&oi) = vis.get(idx) {
                r.sidebar_sel = idx;
                r.focus = Focus::Sidebar;
                if let Some(item) = r.outline.get(oi).cloned() {
                    r.jump_to(item.section, item.locator.as_deref());
                }
            }
        }
    }
}
