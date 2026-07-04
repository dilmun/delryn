//! Mouse handling: the hit-rects captured during render and the click/scroll
//! routing that consults them.

use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::{App, EditMode, EditTab, Focus, LOOKUP_FIELDS, LibPane, Mode, Overlay, SortKey};

/// Layout facts captured during the last render: pane rects for mouse hit-testing
/// plus the width-dependent metrics the library input handlers need. Kept here,
/// out of `LibraryState`, so render stays a pure function of state — the view
/// *writes* these each frame, the input layer *reads* them, and neither the
/// semantic state nor the render output depends on the other.
#[derive(Default)]
pub struct LayoutMetrics {
    pub sidebar: Option<Rect>,
    pub content: Option<Rect>,
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
    pub fn on_mouse(&mut self, m: MouseEvent) {
        if !self.config.mouse_enabled {
            return;
        }
        match m.kind {
            // The wheel scrolls whichever reader pane the cursor is over: the TOC
            // (without changing the selection) or the content.
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                if self.mode != Mode::Reader {
                    return;
                }
                let d: isize = if matches!(m.kind, MouseEventKind::ScrollUp) {
                    -3
                } else {
                    3
                };
                let over_sidebar = self
                    .last_layout
                    .sidebar
                    .is_some_and(|sb| sb.contains((m.column, m.row).into()));
                let paged = self.config.paged;
                if let Some(r) = self.reader.as_mut() {
                    if over_sidebar {
                        r.sidebar_wheel(d);
                    } else if r.continuous_paged_active() {
                        // Continuous PDF: the wheel scrolls the vertical page stack
                        // in row units (re-transmitting the visible slices), not a
                        // whole-page flip.
                        if d > 0 {
                            r.scroll_down(d as usize);
                        } else {
                            r.scroll_up((-d) as usize);
                        }
                    } else if paged || r.is_paged_image() {
                        // Whole-page rasters (PDF) flip pages instead of eased
                        // line-scroll, which would blank/flicker the full-page
                        // image every frame.
                        if d > 0 {
                            r.page_forward();
                        } else {
                            r.page_backward();
                        }
                    } else {
                        r.queue_scroll(d);
                    }
                }
            }
            MouseEventKind::Down(_) => self.mouse_down(m.column, m.row),
            _ => {}
        }
    }

    /// Route a left-click to the active overlay / mode using the hit rects
    /// captured during the last render.
    fn mouse_down(&mut self, col: u16, row: u16) {
        // A pending confirmation is modal — swallow clicks until it's answered.
        if self.pending_confirm.is_some() {
            return;
        }
        if matches!(self.overlay, Overlay::MetaEdit(_)) {
            self.editor_click(col, row);
            return;
        }
        // Other overlays are keyboard-driven (no hit rects); swallow the click.
        if matches!(self.overlay, Overlay::Settings(_))
            || matches!(self.overlay, Overlay::ShelfPicker(_))
            || matches!(self.overlay, Overlay::BulkRename(_))
            || matches!(self.overlay, Overlay::Annot(_))
            || matches!(self.overlay, Overlay::ImageView(_))
            || matches!(self.overlay, Overlay::Prompt(_))
        {
            return;
        }
        match self.mode {
            Mode::Reader => self.mouse_click(col, row),
            Mode::Library => self.library_click(col, row),
        }
    }

    /// Library click: select the clicked book and focus the list pane.
    fn library_click(&mut self, col: u16, row: u16) {
        let pt = (col, row).into();
        if let Some(&(idx, _)) = self.mouse.books.iter().find(|(_, r)| r.contains(pt)) {
            self.library.sel = idx.min(self.library.books.len().saturating_sub(1));
            self.library.pane = LibPane::List;
        }
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
