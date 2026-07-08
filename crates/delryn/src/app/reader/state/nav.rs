//! In-book navigation state: heading/anchor/footnote/bookmark indexes recomputed
//! on re-wrap, the link cursor, cached cross-reference targets, and the jump-list
//! history.

use std::collections::{HashMap, HashSet};

use crate::HighlightColor;

use super::super::AnchorHit;

/// A reading position, for the navigation (back/forward) history.
#[derive(Clone, Copy)]
pub struct Pos {
    pub section: usize,
    pub scroll: usize,
}

/// All in-book navigation state, owned by `Reader` as `reader.nav`.
#[derive(Default)]
pub struct NavState {
    /// The last navigation was backward (to a lower section). Prefetch loads the
    /// direction of travel first, so reverse paging isn't starved.
    pub nav_back: bool,
    /// Cached (outline index, line) for the current section's entries, recomputed
    /// on re-wrap; drives the TOC scroll-spy cheaply.
    pub heading_lines: Vec<(usize, usize)>,
    /// Followable inline anchors in reading order (rebuilt on re-wrap).
    pub anchors: Vec<AnchorHit>,
    /// Footnote id → its definition's first display line (rebuilt on re-wrap).
    pub footnote_def_line: HashMap<String, usize>,
    /// All bookmarks for the open book, as `(section, quote)`. Pushed by the app
    /// whenever annotations change; the source for the gutter markers.
    pub bookmarks: Vec<(usize, String)>,
    /// All notes for the open book, as `(section, quote)` (commentary lives in the
    /// store); the source for the note gutter markers.
    pub notes: Vec<(usize, String)>,
    /// All highlights for the open book, as `(section, quote, colour)`; the source
    /// for the highlight line wash and gutter chip.
    pub highlights: Vec<(usize, String, HighlightColor)>,
    /// Current-section bookmark lines (quotes resolved to display lines on
    /// re-wrap), so the view can mark them in the left gutter cheaply.
    pub bookmark_lines: HashSet<usize>,
    /// Current-section note lines (resolved like `bookmark_lines`).
    pub note_lines: HashSet<usize>,
    /// Current-section highlight lines → their colour (resolved like
    /// `bookmark_lines`), so the view can wash the line and mark the gutter.
    pub highlight_lines: HashMap<usize, HighlightColor>,
    /// Cross-reference/citation targets for one section: `(section, id→locator)`,
    /// cached so repeated lookups in the current section don't re-parse it.
    pub targets_cache: Option<(usize, Vec<(String, String)>)>,
    /// Link-cursor position: index into `anchors`, set in link-follow mode.
    pub anchor_sel: Option<usize>,
    /// Last active (scroll-spy) row the TOC auto-followed to.
    pub last_active: Option<usize>,
    /// Navigation history (jump list).
    pub back_stack: Vec<Pos>,
    pub fwd_stack: Vec<Pos>,
}
