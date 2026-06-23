//! Metadata editor: the tabbed editor over one book (Details · Cover · Lookup),
//! its online metadata + cover search (background Open Library / cover lookup),
//! and persistence back to the store and the book file.

use std::thread;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use delryn_model::naming::{first_author, looks_like_id, main_title};

use crate::document::{Metadata, epub};
use crate::media;
use crate::online::{self, Candidate};

use super::confirm::ConfirmAction;
use super::{
    App, COVER_DEBOUNCE, embed_cover_into_file, fmt_series_index, str_delete_at, str_delete_before,
    str_insert,
};

/// Editable book-metadata fields, in display order. `Year` and `Series #`
/// hold numeric text, validated on save.
pub const META_FIELDS: [&str; 9] = [
    "Title",
    "Author",
    "Year",
    "Series",
    "Series #",
    "Publisher",
    "Subtitle",
    "ISBN",
    "Language",
];
/// Field index of the Year field (validated as an integer).
const F_YEAR: usize = 2;
/// Field index of the Series-position field (validated as a float).
const F_INDEX: usize = 4;
/// Field index of the Subtitle field.
const F_SUBTITLE: usize = 6;

/// Most online matches to fetch (a short list to pick from).
pub const ONLINE_LIMIT: usize = 5;

/// Tabs of the metadata editor. (Renaming lives in the bulk-rename popup.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTab {
    Details,
    Cover,
    Online,
}

impl EditTab {
    pub const ALL: [EditTab; 3] = [EditTab::Details, EditTab::Cover, EditTab::Online];
    pub fn label(self) -> &'static str {
        match self {
            EditTab::Details => "Details",
            EditTab::Cover => "Cover",
            EditTab::Online => "Lookup",
        }
    }
}

/// Whether the focused text field is being navigated between or typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    Nav,
    Edit,
}

/// A message from a background Open Library worker.
pub enum OnlineMsg {
    Results(Vec<Candidate>),
    /// Cover-tab candidates from the multi-source cover search.
    Covers(Vec<online::CoverHit>),
    Cover(Option<Vec<u8>>),
    /// Cover-tab preview: (cover URL, bytes) for the highlighted result.
    Preview(String, Option<Vec<u8>>),
}

/// One search bar's state — query, edit flag, results, selection. The Online and
/// Cover tabs each own an independent instance so typing in one never disturbs
/// the other.
#[derive(Default)]
pub struct Search {
    /// Free-text query (the search bar).
    pub q: String,
    /// Editing the query (vs. browsing results).
    pub editing: bool,
    /// Selected result index.
    pub row: usize,
    pub results: Vec<Candidate>,
    /// A search is in flight.
    pub fetching: bool,
}

/// Number of editable seed fields on the Lookup tab (Title, Author). Year is
/// deliberately excluded — it's free-text noise to the metadata APIs, not a
/// real publication-year filter, and a stale year can hide the right edition.
pub const LOOKUP_FIELDS: usize = 2;

/// The Lookup (Online) tab's structured search form: editable Title/Author
/// fields from which a read-only query is composed, plus the combined keyboard
/// focus that flows from the fields into the results list. Seeded from the book's
/// metadata with a filename fallback (see [`App::open_meta_edit`]).
#[derive(Default)]
pub struct LookupForm {
    pub name: String,
    pub author: String,
    /// Combined focus: 0=Title, 1=Author, then `LOOKUP_FIELDS + i` for result row `i`.
    pub focus: usize,
    /// Editing the focused field (vs. browsing fields/results).
    pub editing: bool,
    /// Caret within the field being edited.
    pub cursor: usize,
}

impl LookupForm {
    /// The composed, read-only query — `name author` with punctuation noise
    /// (commas, colons, slashes…) flattened to spaces and collapsed, so messy
    /// metadata like a stray ", Kissinger" can't break the metadata search.
    pub fn query(&self) -> String {
        let raw = [self.name.trim(), self.author.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let flattened: String = raw
            .chars()
            .map(|c| match c {
                ',' | ';' | ':' | '/' | '\\' | '|' | '"' => ' ',
                c => c,
            })
            .collect();
        flattened.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Read a seed field by index (clamped to the last field).
    pub fn field(&self, i: usize) -> &str {
        match i {
            0 => &self.name,
            _ => &self.author,
        }
    }

    fn field_mut(&mut self, i: usize) -> &mut String {
        match i {
            0 => &mut self.name,
            _ => &mut self.author,
        }
    }

    /// Char length of the currently focused seed field.
    pub(crate) fn focused_len(&self) -> usize {
        self.field(self.focus.min(LOOKUP_FIELDS - 1))
            .chars()
            .count()
    }
}

/// Open metadata-edit form: a tabbed, scalable editor over one book.
pub struct MetaEdit {
    pub path: String,
    /// Book title for the popup header.
    pub book_title: String,
    pub tab: EditTab,
    /// Navigate vs. type-into-field (Details/Online query fields).
    pub mode: EditMode,

    // Details tab ---------------------------------------------------------
    /// Current value of each field, indexed to match [`META_FIELDS`].
    pub values: Vec<String>,
    /// Values as declared by the EPUB file, for reset-to-source.
    pub original: Vec<String>,
    /// Focused field.
    pub row: usize,
    /// Cursor position (char index) within the field being edited.
    pub cursor: usize,

    // Online / Cover tabs — independent search state per tab --------------
    /// Lookup (Online) tab: structured Title/Author/Year seed form.
    pub lookup: LookupForm,
    /// Online (metadata) tab search results (the query comes from `lookup`).
    pub online: Search,
    /// Cover tab search.
    pub cover_search: Search,
    /// Cover-tab candidates (multi-source cover URLs), shown in the Cover list.
    pub cover_hits: Vec<online::CoverHit>,
    /// A cover download is in flight.
    pub cover_pending: bool,
    /// Cover bytes to persist on save (the chosen / previewed cover).
    pub cover: Option<Vec<u8>>,
    /// Bytes of the cover currently previewed on the Cover tab, and its URL
    /// (so Enter can stage it without re-fetching).
    pub preview_cover: Option<Vec<u8>>,
    pub preview_url: String,

    /// Transient one-line status (search progress, results, errors).
    pub status: Option<String>,
    /// Which tab `status` belongs to — it's only shown there, so a Cover/Lookup
    /// "searching…" never leaks onto Details.
    pub status_tab: Option<EditTab>,
    /// The Details (title, author) the Lookup/Cover searches were last seeded
    /// from. When the Details change (e.g. via `x` extract or a manual edit),
    /// entering those tabs re-seeds; while unchanged, manual search edits stick.
    pub seed_from: (String, String),
}

impl MetaEdit {
    /// Set the footer status and the tab it belongs to (so it shows only there).
    fn status_on(&mut self, tab: EditTab, msg: impl Into<String>) {
        self.status = Some(msg.into());
        self.status_tab = Some(tab);
    }

    /// The active tab's search state (Cover has its own; everything else uses
    /// the Online search).
    pub fn search(&self) -> &Search {
        match self.tab {
            EditTab::Cover => &self.cover_search,
            _ => &self.online,
        }
    }

    pub(crate) fn search_mut(&mut self) -> &mut Search {
        match self.tab {
            EditTab::Cover => &mut self.cover_search,
            _ => &mut self.online,
        }
    }

    /// Char length of the focused Details field's value.
    fn field_len(&self) -> usize {
        self.values.get(self.row).map_or(0, |s| s.chars().count())
    }

    /// Char length of whichever field is currently being typed into.
    pub(crate) fn cur_field_len(&self) -> usize {
        match self.tab {
            EditTab::Cover => self.cover_search.q.chars().count(),
            EditTab::Online => self.lookup.focused_len(),
            EditTab::Details => self.field_len(),
        }
    }

    /// The string currently being typed into (a Details field or the cover query).
    fn edit_target(&mut self) -> Option<&mut String> {
        match self.tab {
            EditTab::Details => self.values.get_mut(self.row),
            EditTab::Cover => Some(&mut self.cover_search.q),
            EditTab::Online => Some(
                self.lookup
                    .field_mut(self.lookup.focus.min(LOOKUP_FIELDS - 1)),
            ),
        }
    }

    /// Is field `i`'s current value invalid (a numeric field with unparsable,
    /// non-empty text)?
    pub fn field_invalid(&self, i: usize) -> bool {
        let Some(s) = self.values.get(i) else {
            return false;
        };
        let t = s.trim();
        if t.is_empty() {
            return false;
        }
        match i {
            F_YEAR => t.parse::<i32>().is_err(),
            F_INDEX => t.parse::<f32>().is_err(),
            _ => false,
        }
    }

    /// Has field `i` been changed from its EPUB original?
    pub fn changed(&self, i: usize) -> bool {
        self.original.get(i).map(String::as_str) != self.values.get(i).map(String::as_str)
    }

    /// Any field currently invalid (blocks save).
    pub fn has_invalid(&self) -> bool {
        (0..self.values.len()).any(|i| self.field_invalid(i))
    }
}

/// The six editable metadata fields, in [`META_FIELDS`] order, from a document's
/// [`Metadata`]. Shared by the editor's prefill and reset-to-source.
fn meta_fields_from(m: &Metadata) -> Vec<String> {
    vec![
        m.title.clone(),
        m.author_line(),
        m.year.map(|y| y.to_string()).unwrap_or_default(),
        m.series.clone().unwrap_or_default(),
        m.series_index.map(fmt_series_index).unwrap_or_default(),
        m.publisher.clone().unwrap_or_default(),
        m.subtitle.clone().unwrap_or_default(),
        m.identifier.clone().unwrap_or_default(),
        m.language.clone().unwrap_or_default(),
    ]
}

impl App {
    /// Open the tabbed metadata editor on the selected book.
    pub(crate) fn open_meta_edit(&mut self) {
        let Some(path) = self.lib_books.get(self.lib_sel).map(|b| b.path.clone()) else {
            return;
        };
        self.open_meta_edit_path(&path);
    }

    /// Open the metadata editor on the book at `path` — shared by the current
    /// selection and stepping through a multi-book edit queue.
    fn open_meta_edit_path(&mut self, path: &str) {
        let Some(b) = self.lib_books.iter().find(|b| b.path == path) else {
            return;
        };
        // Snapshot the fields we need, then drop the borrow on `self.lib_books`.
        let path = b.path.clone();
        let book_title = b.title.clone();
        let author_raw = b.author.clone();
        let values = vec![
            b.title.clone(),
            b.author.clone(),
            b.year.map(|y| y.to_string()).unwrap_or_default(),
            b.series.clone(),
            b.series_index.map(fmt_series_index).unwrap_or_default(),
            b.publisher.clone(),
            b.subtitle.clone(),
            b.isbn.clone(),
            b.language.clone(),
        ];
        // The EPUB's declared metadata, for per-field reset (best-effort).
        let original = epub::read_metadata(&path)
            .map(|(m, _)| meta_fields_from(&m))
            .unwrap_or_else(|_| vec![String::new(); META_FIELDS.len()]);

        // Seed Title, worst-input-last so a filename never drives the search when
        // real metadata exists: the DB title if it's a real title (not just the
        // filename stem), else the EPUB's declared title, else the filename, else
        // the book's content. `main_title` strips any subtitle after a separator.
        let stem = std::path::Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .trim();
        let db_title = book_title.trim();
        let epub_title = original.first().map(String::as_str).unwrap_or("").trim();
        let real_meta = if !db_title.is_empty() && db_title != stem && !looks_like_id(db_title) {
            Some(db_title)
        } else if !epub_title.is_empty() && !looks_like_id(epub_title) {
            Some(epub_title)
        } else {
            None
        };
        let name = match real_meta {
            Some(t) => main_title(t),
            None => epub::extract_content_title(&path)
                .map(|(t, _)| main_title(&t))
                .filter(|n| !n.is_empty() && !looks_like_id(n))
                .unwrap_or_else(|| main_title(stem)),
        };
        let author = first_author(&author_raw);
        // Cover search is seeded from the SAME clean title + author, not the raw
        // (possibly ID-like) metadata, so its query/results aren't junk.
        let cover_q = format!("{name} {author}")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let lookup = LookupForm {
            name,
            author,
            ..LookupForm::default()
        };
        // Snapshot the Details the searches were seeded from (the raw values, so a
        // later change — extract or manual edit — is detected on tab entry).
        let seed_from = (
            values[0].clone(),
            values.get(1).cloned().unwrap_or_default(),
        );
        let cursor = values[0].chars().count();
        self.meta_edit = Some(MetaEdit {
            path,
            book_title,
            tab: EditTab::Details,
            mode: EditMode::Nav,
            values,
            original,
            row: 0,
            cursor,
            lookup,
            online: Search::default(),
            cover_search: Search {
                q: cover_q,
                ..Search::default()
            },
            cover_hits: Vec::new(),
            cover_pending: false,
            cover: None,
            preview_cover: None,
            preview_url: String::new(),
            status: None,
            status_tab: None,
            seed_from,
        });
    }

    /// Re-seed the Lookup and Cover searches from the *current* Details title and
    /// author when they've changed since the last seed (e.g. after `x` extract or
    /// a manual edit). A no-op when unchanged, so manual search edits are kept.
    fn reseed_search_from_details(&mut self) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        let title = ed.values.first().cloned().unwrap_or_default();
        let author = ed.values.get(1).cloned().unwrap_or_default();
        if ed.seed_from == (title.clone(), author.clone()) {
            return;
        }
        let name = main_title(&title);
        let author1 = first_author(&author);
        ed.cover_search.q = format!("{name} {author1}")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        ed.lookup.name = name;
        ed.lookup.author = author1;
        ed.lookup.focus = 0;
        ed.lookup.editing = false;
        // Drop stale results so the tab re-searches the new seed on entry.
        ed.online.results.clear();
        ed.online.row = 0;
        ed.online.fetching = false;
        ed.cover_hits.clear();
        ed.cover_search.fetching = false;
        ed.status = None;
        ed.status_tab = None;
        ed.seed_from = (title, author);
    }

    /// Begin editing a multi-selection one book at a time: open the editor on the
    /// first selected book and queue the rest. `^S` saves and advances; `Esc`
    /// skips to the next; the editor closes after the last.
    pub(crate) fn start_bulk_edit(&mut self) {
        let mut paths: Vec<String> = self
            .lib_books
            .iter()
            .filter(|b| self.lib_marked.contains(&b.path))
            .map(|b| b.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.edit_total = paths.len();
        let first = paths.remove(0);
        self.edit_queue = paths;
        self.lib_exit_visual(); // the selection is captured in the queue now
        self.open_meta_edit_path(&first);
    }

    /// Advance to the next book in the edit queue. Returns false (and resets the
    /// queue) when there are none left, so the caller can close the editor.
    fn advance_edit_queue(&mut self) -> bool {
        if self.edit_queue.is_empty() {
            self.edit_total = 0;
            return false;
        }
        let next = self.edit_queue.remove(0);
        self.open_meta_edit_path(&next);
        true
    }

    pub(crate) fn meta_edit_key(&mut self, key: KeyEvent) {
        let (mode, tab) = match &self.meta_edit {
            Some(e) => (e.mode, e.tab),
            None => return,
        };
        // Lookup tab: editing one of its seed fields takes all keystrokes.
        if tab == EditTab::Online && self.meta_edit.as_ref().is_some_and(|e| e.lookup.editing) {
            self.lookup_edit_key(key);
            return;
        }
        // Cover tab: editing the free-text search bar.
        if tab == EditTab::Cover
            && self
                .meta_edit
                .as_ref()
                .is_some_and(|e| e.cover_search.editing)
        {
            self.online_query_key(key);
            return;
        }
        // Details edit mode: keystrokes go into the focused field.
        if mode == EditMode::Edit {
            self.meta_edit_typing(key);
            return;
        }
        // Navigate mode.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // In a multi-book queue, Esc skips to the next; otherwise it closes.
            KeyCode::Esc => {
                if !self.advance_edit_queue() {
                    self.meta_edit = None;
                }
            }
            // ^S asks for confirmation first (unless a field is invalid).
            KeyCode::Char('s') if ctrl => {
                if !self.meta_edit.as_ref().is_some_and(MetaEdit::has_invalid) {
                    self.ask_confirm("Save changes?", ConfirmAction::SaveMeta);
                }
            }
            KeyCode::Tab => self.meta_edit_switch_tab(1),
            KeyCode::BackTab => self.meta_edit_switch_tab(-1),
            // Jump straight to a tab by number.
            KeyCode::Char(c @ '1'..='9') => {
                let i = c as usize - '1' as usize;
                if i < EditTab::ALL.len() {
                    self.meta_edit_goto_tab(EditTab::ALL[i]);
                }
            }
            _ => match tab {
                EditTab::Details => self.details_nav_key(key),
                EditTab::Online => self.lookup_nav_key(key),
                EditTab::Cover => self.online_nav_key(key),
            },
        }
    }

    fn meta_edit_switch_tab(&mut self, delta: isize) {
        let Some(ed) = self.meta_edit.as_ref() else {
            return;
        };
        let i = EditTab::ALL.iter().position(|t| *t == ed.tab).unwrap_or(0) as isize;
        let n = EditTab::ALL.len() as isize;
        self.meta_edit_goto_tab(EditTab::ALL[(i + delta).rem_euclid(n) as usize]);
    }

    /// Switch to `tab` (shared by Tab/Shift-Tab and the 1–4 number keys),
    /// running the per-tab on-enter work.
    pub(crate) fn meta_edit_goto_tab(&mut self, tab: EditTab) {
        if let Some(ed) = self.meta_edit.as_mut() {
            ed.tab = tab;
            ed.mode = EditMode::Nav;
        }
        // Entering a search tab picks up any Details changes (e.g. after `x`).
        if matches!(tab, EditTab::Online | EditTab::Cover) {
            self.reseed_search_from_details();
        }
        // Entering the Cover tab runs the cover search once, so candidates appear
        // without a manual search (uses the book's ISBN + the seeded query).
        if tab == EditTab::Cover
            && self
                .meta_edit
                .as_ref()
                .is_some_and(|e| e.cover_hits.is_empty() && !e.cover_search.fetching)
        {
            self.online_search();
        }
        // Likewise the Lookup tab auto-searches once from its seeded fields.
        if tab == EditTab::Online
            && self
                .meta_edit
                .as_ref()
                .is_some_and(|e| e.online.results.is_empty() && !e.online.fetching)
        {
            self.online_search();
        }
    }

    /// Lookup tab, navigate mode: `j/k` flow through the three seed fields and
    /// then the results; Enter edits a field or applies a result; `/` re-runs the
    /// search; typing on a field starts editing it.
    fn lookup_nav_key(&mut self, key: KeyEvent) {
        let (focus, results) = match &self.meta_edit {
            Some(e) => (e.lookup.focus, e.online.results.len()),
            None => return,
        };
        let max_focus = LOOKUP_FIELDS - 1 + results; // last field, or last result
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.lookup_set_focus(focus.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.lookup_set_focus((focus + 1).min(max_focus)),
            // Re-run the search from the current fields.
            KeyCode::Char('/') => self.online_search(),
            KeyCode::Enter => {
                if focus < LOOKUP_FIELDS {
                    self.lookup_begin_edit(None);
                } else {
                    self.apply_candidate(focus - LOOKUP_FIELDS);
                }
            }
            // Typing on a field starts editing it (seeded with the first char).
            KeyCode::Char(c) if !ctrl && focus < LOOKUP_FIELDS => self.lookup_begin_edit(Some(c)),
            _ => {}
        }
    }

    /// Move the Lookup focus and keep the results' selected row in sync.
    fn lookup_set_focus(&mut self, focus: usize) {
        if let Some(e) = self.meta_edit.as_mut() {
            e.lookup.focus = focus;
            e.online.row = focus.saturating_sub(LOOKUP_FIELDS);
        }
    }

    /// Enter edit mode on the focused seed field, optionally appending a first char.
    fn lookup_begin_edit(&mut self, first: Option<char>) {
        let Some(e) = self.meta_edit.as_mut() else {
            return;
        };
        if e.lookup.focus >= LOOKUP_FIELDS {
            return;
        }
        e.lookup.editing = true;
        if let Some(c) = first {
            let i = e.lookup.focus;
            let cur = e.lookup.field(i).chars().count();
            str_insert(e.lookup.field_mut(i), cur, c);
        }
        e.lookup.cursor = e.lookup.focused_len();
    }

    /// Lookup field editing: type into the focused field; Enter runs the search,
    /// Esc just leaves edit mode.
    fn lookup_edit_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.lookup.editing = false;
                }
            }
            KeyCode::Enter => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.lookup.editing = false;
                }
                self.online_search();
            }
            KeyCode::Left => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.lookup.cursor = e.lookup.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.lookup.cursor = (e.lookup.cursor + 1).min(e.lookup.focused_len());
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let i = e.lookup.focus.min(LOOKUP_FIELDS - 1);
                    e.lookup.field_mut(i).clear();
                    e.lookup.cursor = 0;
                }
            }
            KeyCode::Backspace => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let (i, cur) = (e.lookup.focus.min(LOOKUP_FIELDS - 1), e.lookup.cursor);
                    if str_delete_before(e.lookup.field_mut(i), cur) {
                        e.lookup.cursor -= 1;
                    }
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let (i, cur) = (e.lookup.focus.min(LOOKUP_FIELDS - 1), e.lookup.cursor);
                    str_insert(e.lookup.field_mut(i), cur, c);
                    e.lookup.cursor += 1;
                }
            }
            _ => {}
        }
    }

    /// Details tab, navigate mode: move between fields; Enter edits.
    fn details_nav_key(&mut self, key: KeyEvent) {
        // `x` extracts metadata from the book's own content (own borrow).
        if matches!(key.code, KeyCode::Char('x')) {
            self.details_extract_from_content();
            return;
        }
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        let last = META_FIELDS.len() - 1;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => ed.row = ed.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => ed.row = (ed.row + 1).min(last),
            KeyCode::Char('g') => ed.row = 0,
            KeyCode::Char('G') => ed.row = last,
            KeyCode::Enter => {
                ed.mode = EditMode::Edit;
                ed.cursor = ed.field_len();
            }
            // Reset the focused field (r) or all fields (R) to the EPUB value.
            KeyCode::Char('r') => {
                if let Some(orig) = ed.original.get(ed.row).cloned() {
                    ed.values[ed.row] = orig;
                }
            }
            KeyCode::Char('R') => ed.values = ed.original.clone(),
            _ => {}
        }
    }

    /// Fill the Details fields from the book's own content (title, subtitle,
    /// author, year, publisher, ISBN) — for converted files with junk metadata
    /// that aren't findable online. Best-effort; the user reviews, then ^S.
    fn details_extract_from_content(&mut self) {
        let Some(path) = self.meta_edit.as_ref().map(|e| e.path.clone()) else {
            return;
        };
        let m = epub::extract_book_metadata(&path);
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        let mut filled = Vec::new();
        if let Some(t) = m.title {
            ed.values[0] = t;
            filled.push("title");
        }
        if let Some(a) = m.author {
            ed.values[1] = a;
            filled.push("author");
        }
        if let Some(y) = m.year {
            ed.values[F_YEAR] = y.to_string();
            filled.push("year");
        }
        if let Some(p) = m.publisher {
            ed.values[5] = p;
            filled.push("publisher");
        }
        if let Some(s) = m.subtitle {
            ed.values[F_SUBTITLE] = s;
            filled.push("subtitle");
        }
        if let Some(i) = m.isbn {
            ed.values[7] = i;
            filled.push("ISBN");
        }
        ed.row = 0;
        let msg = if filled.is_empty() {
            "nothing found in the book's content".to_string()
        } else {
            format!("extracted {} — review, then ^S", filled.join(", "))
        };
        ed.status_on(EditTab::Details, msg);
    }

    /// Edit mode: type into the focused field (Details or Online query).
    fn meta_edit_typing(&mut self, key: KeyEvent) {
        {
            let Some(ed) = self.meta_edit.as_mut() else {
                return;
            };
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Esc | KeyCode::Enter => ed.mode = EditMode::Nav,
                KeyCode::Left => ed.cursor = ed.cursor.saturating_sub(1),
                KeyCode::Right => ed.cursor = (ed.cursor + 1).min(ed.cur_field_len()),
                KeyCode::Home => ed.cursor = 0,
                KeyCode::End => ed.cursor = ed.cur_field_len(),
                KeyCode::Char('u') if ctrl => {
                    if let Some(s) = ed.edit_target() {
                        s.clear();
                    }
                    ed.cursor = 0;
                }
                KeyCode::Backspace => {
                    let cur = ed.cursor;
                    let removed = ed.edit_target().is_some_and(|s| str_delete_before(s, cur));
                    if removed {
                        ed.cursor -= 1;
                    }
                }
                KeyCode::Delete => {
                    let cur = ed.cursor;
                    if let Some(s) = ed.edit_target() {
                        str_delete_at(s, cur);
                    }
                }
                KeyCode::Char(c) => {
                    let cur = ed.cursor;
                    let mut inserted = false;
                    if let Some(s) = ed.edit_target() {
                        str_insert(s, cur, c);
                        inserted = true;
                    }
                    if inserted {
                        ed.cursor += 1;
                    }
                }
                _ => {}
            }
        }
    }

    /// Online/Cover tabs, browsing the results: `/` or typing opens the search
    /// bar; j/k move the selection; Enter applies (metadata on Online, the
    /// previewed cover on Cover).
    fn online_nav_key(&mut self, key: KeyEvent) {
        let (results, tab) = match &self.meta_edit {
            Some(e) => {
                let n = if e.tab == EditTab::Cover {
                    e.cover_hits.len()
                } else {
                    e.search().results.len()
                };
                (n, e.tab)
            }
            None => return,
        };
        let last = results.saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let s = e.search_mut();
                    s.row = s.row.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let s = e.search_mut();
                    s.row = (s.row + 1).min(last);
                }
            }
            // Open the search bar: `/`, or start typing the query directly.
            KeyCode::Char('/') => self.online_begin_query(None),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.online_begin_query(Some(c))
            }
            KeyCode::Enter => {
                if results == 0 {
                    self.online_begin_query(None);
                } else if tab == EditTab::Cover {
                    self.stage_preview_cover();
                } else {
                    let idx = self.meta_edit.as_ref().map_or(0, |e| e.search().row);
                    self.apply_candidate(idx);
                }
            }
            _ => {}
        }
    }

    /// Enter search-bar editing, optionally seeding the query with a first char.
    pub(crate) fn online_begin_query(&mut self, first: Option<char>) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        ed.search_mut().editing = true;
        if let Some(c) = first {
            let s = ed.search_mut();
            s.q.clear();
            s.q.push(c);
        }
        ed.cursor = ed.search().q.chars().count();
    }

    /// Search-bar editing: type the query; Enter runs the search, Esc exits.
    fn online_query_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.search_mut().editing = false;
                }
            }
            KeyCode::Enter => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.search_mut().editing = false;
                }
                self.online_search();
            }
            KeyCode::Left => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.cursor = e.cursor.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.cursor = (e.cursor + 1).min(e.search().q.chars().count());
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(e) = self.meta_edit.as_mut() {
                    e.search_mut().q.clear();
                    e.cursor = 0;
                }
            }
            KeyCode::Backspace => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let cur = e.cursor;
                    if str_delete_before(&mut e.search_mut().q, cur) {
                        e.cursor -= 1;
                    }
                }
            }
            KeyCode::Char(c) => {
                if let Some(e) = self.meta_edit.as_mut() {
                    let cur = e.cursor;
                    str_insert(&mut e.search_mut().q, cur, c);
                    e.cursor += 1;
                }
            }
            _ => {}
        }
    }

    /// Stage the currently-previewed cover (Cover tab Enter) for save.
    fn stage_preview_cover(&mut self) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        match ed.preview_cover.clone() {
            Some(bytes) => {
                ed.cover = Some(bytes);
                ed.status_on(EditTab::Cover, "cover staged ✓ — ^S to save");
            }
            None => ed.status_on(EditTab::Cover, "no cover to use here"),
        }
    }

    /// Kick off a background search from the query bar: Open Library metadata on
    /// the Online tab, or a multi-source cover search on the Cover tab (which can
    /// run with an empty query, using just the book's ISBN).
    fn online_search(&mut self) {
        let (query, tab, isbn) = {
            let Some(ed) = self.meta_edit.as_mut() else {
                return;
            };
            let tab = ed.tab;
            // The Lookup query is composed from its seed fields; the Cover tab
            // uses its free-text bar (and may run with an empty query via ISBN).
            let query = if tab == EditTab::Cover {
                ed.cover_search.q.clone()
            } else {
                ed.lookup.query()
            };
            if query.trim().is_empty() && tab != EditTab::Cover {
                return;
            }
            // Cancel any other tab's in-flight search (its result is abandoned
            // when we replace online_rx below), then mark this one fetching.
            ed.online.fetching = false;
            ed.cover_search.fetching = false;
            let isbn = ed.values.get(7).cloned().unwrap_or_default();
            if tab == EditTab::Cover {
                ed.cover_hits.clear();
            }
            let s = ed.search_mut();
            s.fetching = true;
            s.results.clear();
            s.row = 0;
            s.q = query.clone();
            ed.status_on(tab, "searching…");
            (query, tab, isbn)
        };
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let msg = if tab == EditTab::Cover {
                OnlineMsg::Covers(online::cover_candidates(&query, &isbn, ONLINE_LIMIT))
            } else {
                OnlineMsg::Results(online::search(&query, ONLINE_LIMIT))
            };
            let _ = tx.send(msg);
        });
        self.online_rx = Some(rx);
    }

    /// Apply candidate `idx` to the Details fields and fetch its cover.
    fn apply_candidate(&mut self, idx: usize) {
        let cover_url = {
            let Some(ed) = self.meta_edit.as_mut() else {
                return;
            };
            let Some(c) = ed.search().results.get(idx).cloned() else {
                return;
            };
            ed.values[0] = c.title.clone();
            ed.values[1] = c.author_line();
            if let Some(sub) = &c.subtitle {
                ed.values[F_SUBTITLE] = sub.clone();
            }
            if let Some(y) = c.year {
                ed.values[2] = y.to_string();
            }
            if let Some(s) = &c.series {
                ed.values[3] = s.clone();
            }
            if let Some(si) = c.series_index {
                ed.values[4] = fmt_series_index(si);
            }
            if let Some(p) = &c.publisher {
                ed.values[5] = p.clone();
            }
            if let Some(isbn) = &c.isbn {
                ed.values[7] = isbn.clone();
            }
            ed.tab = EditTab::Details;
            ed.mode = EditMode::Nav;
            ed.row = 0;
            ed.status_on(EditTab::Details, "applied — review, then ^S to save");
            let url = c.cover_url();
            ed.cover_pending = url.is_some();
            url
        };
        if let Some(url) = cover_url {
            let (tx, rx) = std::sync::mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(OnlineMsg::Cover(online::fetch_cover(&url)));
            });
            self.online_rx = Some(rx);
        }
    }

    /// Drain a finished background Open Library request; returns whether the
    /// view changed. Called from the event loop.
    pub fn poll_online(&mut self) -> bool {
        let Some(rx) = &self.online_rx else {
            return false;
        };
        let Ok(msg) = rx.try_recv() else {
            return false;
        };
        self.online_rx = None;
        let Some(ed) = self.meta_edit.as_mut() else {
            return true;
        };
        match msg {
            OnlineMsg::Results(cands) => {
                let msg = if cands.is_empty() {
                    "no matches".to_string()
                } else {
                    format!("{} match(es) — ↑↓ to browse", cands.len())
                };
                ed.status_on(EditTab::Online, msg);
                ed.online.fetching = false;
                ed.online.row = 0;
                ed.online.results = cands;
            }
            OnlineMsg::Covers(hits) => {
                ed.cover_search.fetching = false;
                ed.cover_search.row = 0;
                let msg = if hits.is_empty() {
                    "no covers found".to_string()
                } else {
                    format!("{} cover(s) — ↑↓ to browse", hits.len())
                };
                ed.status_on(EditTab::Cover, msg);
                ed.cover_hits = hits;
            }
            OnlineMsg::Cover(bytes) => {
                // The applied candidate's cover — the user is on Details now.
                ed.cover_pending = false;
                match bytes {
                    Some(b) => {
                        ed.status_on(EditTab::Details, "cover fetched ✓ — ^S to save");
                        ed.cover = Some(b);
                    }
                    None => ed.status_on(EditTab::Details, "no cover found"),
                }
            }
            OnlineMsg::Preview(url, bytes) => {
                // A previewed cover arrived for the Cover tab.
                self.edit_cover_url = url.clone();
                ed.preview_url = url;
                ed.preview_cover = bytes.clone();
                self.edit_cover = match (&self.picker, &bytes) {
                    (Some(p), Some(b)) => media::build_cover(p, b),
                    _ => None,
                };
            }
        }
        true
    }

    /// Is an Open Library request in flight (keeps the loop polling)?
    pub fn online_active(&self) -> bool {
        self.meta_edit
            .as_ref()
            .is_some_and(|e| e.online.fetching || e.cover_search.fetching || e.cover_pending)
    }

    /// Cover-tab preview: the cover URL of the highlighted result (or empty).
    fn preview_target_url(&self) -> String {
        let Some(ed) = &self.meta_edit else {
            return self.edit_cover_url.clone();
        };
        if ed.tab != EditTab::Cover {
            return self.edit_cover_url.clone();
        }
        ed.cover_hits
            .get(ed.cover_search.row)
            .map(|h| h.url.clone())
            .unwrap_or_default()
    }

    /// Is the Cover-tab preview stale (wants fetching)? Keeps the loop ticking.
    pub fn preview_pending(&self) -> bool {
        self.preview_target_url() != self.edit_cover_url
    }

    /// Debounced background fetch of the highlighted result's cover for the
    /// Cover-tab preview, so arrow-scrolling the list doesn't spam the network.
    pub fn tick_preview(&mut self) {
        let target = self.preview_target_url();
        if target == self.edit_cover_url {
            return;
        }
        if target != self.edit_cover_target {
            self.edit_cover_target = target;
            self.edit_cover_at = Instant::now();
            return;
        }
        if self.edit_cover_at.elapsed() < COVER_DEBOUNCE {
            return;
        }
        // Mark as handled so we don't re-fire; the result arrives via poll.
        self.edit_cover_url = target.clone();
        if target.is_empty() {
            self.edit_cover = None;
            if let Some(ed) = self.meta_edit.as_mut() {
                ed.preview_cover = None;
                ed.preview_url = String::new();
            }
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let url = target.clone();
        thread::spawn(move || {
            let _ = tx.send(OnlineMsg::Preview(url.clone(), online::fetch_cover(&url)));
        });
        self.online_rx = Some(rx);
    }

    /// Persist the edited fields + any fetched cover (year/index parsed
    /// leniently; blank → unset). Collections are applied live, not here.
    pub(crate) fn save_meta_edit(&mut self) {
        if self.meta_edit.as_ref().is_some_and(MetaEdit::has_invalid) {
            return;
        }
        let Some(ed) = self.meta_edit.take() else {
            return;
        };
        let v = |i: usize| ed.values.get(i).map(|s| s.trim()).unwrap_or("");
        let year = v(2).parse::<i32>().ok();
        let series_index = v(4).parse::<f32>().ok();
        if let Some(store) = &self.store {
            store.update_book_meta(
                &ed.path,
                v(0),
                v(1),
                year,
                v(3),
                series_index,
                v(5),
                v(6),
                v(7),
                v(8),
            );
        }
        if let Some(bytes) = &ed.cover {
            let _ = online::save_cover(&ed.path, bytes);
            self.lib_flash = Some(embed_cover_into_file(&ed.path, bytes));
        }
        self.refresh_library();
        // In a multi-book edit, move on to the next; else `take()` left it closed.
        self.advance_edit_queue();
    }
}
