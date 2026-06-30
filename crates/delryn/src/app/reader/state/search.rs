//! In-book search state: the query prompt and history, the active matcher, the
//! match list, and the current-match cursor.

use crate::search::{Matcher, SearchMode};

/// All in-book search state, owned by `Reader` as `reader.search`.
pub struct SearchState {
    /// The search prompt is open (typing a query).
    pub searching: bool,
    /// The query being typed.
    pub input: String,
    /// Match mode (plain / regex / fuzzy).
    pub mode: SearchMode,
    /// The active matcher (set when a search runs); drives highlighting.
    pub matcher: Option<Matcher>,
    /// Matching `(section, line)` positions across the whole book.
    pub(crate) matches: Vec<(usize, usize)>,
    /// Cursor into `matches` (the current match).
    pub idx: usize,
    /// Recent queries, most-recent last; recalled with Up/Down in the prompt.
    pub(crate) history: Vec<String>,
    /// Position while browsing history in the prompt (None = editing fresh).
    pub(crate) history_pos: Option<usize>,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            searching: false,
            input: String::new(),
            mode: SearchMode::Plain,
            matcher: None,
            matches: Vec::new(),
            idx: 0,
            history: Vec::new(),
            history_pos: None,
        }
    }
}
