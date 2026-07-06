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
use super::{App, COVER_DEBOUNCE, Overlay, embed_cover_into_file, fmt_series_index};
use crate::ui::TextInput;

// The online metadata/cover lookup (search + background execution) lives in a
// child module; it reaches this shell's private MetaEdit helpers via `super::`.
mod lookup;

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

/// One row of the metadata diff: a [`META_FIELDS`] field, its remote value, and
/// whether the reader has ticked it to apply.
pub struct DiffRow {
    pub field: usize,
    pub remote: String,
    pub apply: bool,
}

/// The metadata diff overlay: current-vs-remote for a picked online candidate,
/// with per-field selection. Applying writes the ticked remote values into the
/// Details fields (and fetches the candidate's cover).
pub struct MetaDiff {
    pub rows: Vec<DiffRow>,
    /// Cursor row into `rows`.
    pub row: usize,
    /// The candidate's cover URL, fetched when the diff is applied.
    pub cover_url: Option<String>,
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
    pub q: TextInput,
    /// Editing the query (vs. browsing results).
    pub editing: bool,
    /// Selected result index.
    pub row: usize,
    pub results: Vec<Candidate>,
    /// A search is in flight.
    pub fetching: bool,
}

/// Editable seed fields on the Lookup tab, in order: Title, Author, Year, ISBN.
pub const LOOKUP_FIELDS: usize = 4;
/// Index of the ISBN seed field — special: it drives an exact-edition lookup
/// (search by ISBN alone) and auto-fills from the book's own ISBN when focused.
pub const LOOKUP_ISBN: usize = 3;

/// The Lookup (Online) tab's structured search form: editable Title/Author
/// fields from which a read-only query is composed, plus the combined keyboard
/// focus that flows from the fields into the results list. Seeded from the book's
/// metadata with a filename fallback (see [`App::open_meta_edit`]).
#[derive(Default)]
pub struct LookupForm {
    pub name: TextInput,
    pub author: TextInput,
    pub year: TextInput,
    /// ISBN — when set, the search runs on this alone (exact edition). Empty by
    /// default; auto-filled from the book's own ISBN when the field is focused.
    pub isbn: TextInput,
    /// Combined focus: 0..LOOKUP_FIELDS seed fields, then `LOOKUP_FIELDS + i` for
    /// result row `i`.
    pub focus: usize,
    /// Editing the focused field (vs. browsing fields/results).
    pub editing: bool,
}

impl LookupForm {
    /// The composed, read-only query. With an ISBN set it's an exact-edition
    /// lookup — `isbn:<digits>` alone (the surest disambiguator when an author
    /// has near-identical titles). Otherwise `name author year`, with punctuation
    /// noise flattened so messy metadata can't break the search.
    pub fn query(&self) -> String {
        let isbn = self.isbn.text().trim();
        if !isbn.is_empty() {
            let digits = online::normalize_isbn(isbn).unwrap_or_else(|| isbn.to_string());
            return format!("isbn:{digits}");
        }
        let raw = [
            self.name.text().trim(),
            self.author.text().trim(),
            self.year.text().trim(),
        ]
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
            0 => self.name.text(),
            1 => self.author.text(),
            2 => self.year.text(),
            _ => self.isbn.text(),
        }
    }

    fn field_mut(&mut self, i: usize) -> &mut TextInput {
        match i {
            0 => &mut self.name,
            1 => &mut self.author,
            2 => &mut self.year,
            _ => &mut self.isbn,
        }
    }

    /// Move the caret of the focused seed field to char index `pos` (clamped) —
    /// used to place it where a mouse click landed.
    pub(crate) fn focus_caret(&mut self, pos: usize) {
        let i = self.focus.min(LOOKUP_FIELDS - 1);
        self.field_mut(i).set_cursor(pos);
    }

    /// Caret position within seed field `i` (for rendering its editing caret).
    pub(crate) fn field_cursor(&self, i: usize) -> usize {
        match i {
            0 => self.name.cursor(),
            1 => self.author.cursor(),
            2 => self.year.cursor(),
            _ => self.isbn.cursor(),
        }
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
    pub values: Vec<TextInput>,
    /// Values as declared by the EPUB file, for reset-to-source.
    pub original: Vec<String>,
    /// Focused field.
    pub row: usize,

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

    /// Open metadata-diff overlay (current vs a picked online candidate), if any.
    pub diff: Option<MetaDiff>,
}

impl MetaEdit {
    /// Whether a text field is currently being typed into (so a bare `f` inserts
    /// the character rather than resizing the overlay).
    pub fn is_typing(&self) -> bool {
        self.mode == EditMode::Edit || self.lookup.editing || self.cover_search.editing
    }

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

    /// The field currently being typed into (a Details field or the cover query).
    fn edit_target(&mut self) -> Option<&mut TextInput> {
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
        let t = s.text().trim();
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
        self.original.get(i).map(String::as_str) != self.values.get(i).map(|t| t.text())
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
        let Some(path) = self
            .library
            .books
            .get(self.library.sel)
            .map(|b| b.path.clone())
        else {
            return;
        };
        self.open_meta_edit_path(&path);
    }

    /// Open the metadata editor on the book at `path` — shared by the current
    /// selection and stepping through a multi-book edit queue.
    fn open_meta_edit_path(&mut self, path: &str) {
        let Some(b) = self.library.books.iter().find(|b| b.path == path) else {
            return;
        };
        // Snapshot the fields we need, then drop the borrow on `self.library.books`.
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
            name: TextInput::from(name),
            author: TextInput::from(author),
            // Year seeds from the book; ISBN stays inactive until focused.
            year: TextInput::from(values.get(F_YEAR).cloned().unwrap_or_default()),
            ..LookupForm::default()
        };
        // Snapshot the Details the searches were seeded from (the raw values, so a
        // later change — extract or manual edit — is detected on tab entry).
        let seed_from = (
            values[0].clone(),
            values.get(1).cloned().unwrap_or_default(),
        );
        self.overlay = Overlay::MetaEdit(MetaEdit {
            path,
            book_title,
            tab: EditTab::Details,
            mode: EditMode::Nav,
            values: values.into_iter().map(TextInput::from).collect(),
            original,
            row: 0,
            lookup,
            online: Search::default(),
            cover_search: Search {
                q: TextInput::from(cover_q),
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
            diff: None,
        });
    }

    /// Begin editing a multi-selection one book at a time: open the editor on the
    /// first selected book and queue the rest. `^S` saves and advances; `Esc`
    /// skips to the next; the editor closes after the last.
    pub(crate) fn start_bulk_edit(&mut self) {
        let mut paths: Vec<String> = self
            .library
            .books
            .iter()
            .filter(|b| self.library.marked.contains(&b.path))
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

    /// The metadata-diff overlay: ↑↓ move, space toggles a field (when it has a
    /// remote value), `a` toggles all, ⏎ applies the ticked rows, Esc cancels.
    fn diff_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if let Overlay::MetaEdit(e) = &mut self.overlay {
                    e.diff = None;
                }
            }
            KeyCode::Enter => self.apply_diff(),
            KeyCode::Up | KeyCode::Char('k') => {
                if let Overlay::MetaEdit(e) = &mut self.overlay
                    && let Some(d) = e.diff.as_mut()
                {
                    d.row = d.row.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Overlay::MetaEdit(e) = &mut self.overlay
                    && let Some(d) = e.diff.as_mut()
                {
                    d.row = (d.row + 1).min(d.rows.len().saturating_sub(1));
                }
            }
            KeyCode::Char(' ') => {
                if let Overlay::MetaEdit(e) = &mut self.overlay
                    && let Some(d) = e.diff.as_mut()
                    && let Some(r) = d.rows.get_mut(d.row)
                    && !r.remote.is_empty()
                {
                    r.apply = !r.apply;
                }
            }
            // Toggle all appliable rows (off if every one is already on).
            KeyCode::Char('a') => {
                if let Overlay::MetaEdit(e) = &mut self.overlay
                    && let Some(d) = e.diff.as_mut()
                {
                    let all_on = d
                        .rows
                        .iter()
                        .filter(|r| !r.remote.is_empty())
                        .all(|r| r.apply);
                    for r in d.rows.iter_mut().filter(|r| !r.remote.is_empty()) {
                        r.apply = !all_on;
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn meta_edit_key(&mut self, key: KeyEvent) {
        let (mode, tab) = match &self.overlay {
            Overlay::MetaEdit(e) => (e.mode, e.tab),
            _ => return,
        };
        // The metadata-diff overlay captures input until applied or cancelled.
        if matches!(&self.overlay, Overlay::MetaEdit(e) if e.diff.is_some()) {
            self.diff_key(key);
            return;
        }
        // Lookup tab: editing one of its seed fields takes all keystrokes.
        if tab == EditTab::Online
            && matches!(&self.overlay, Overlay::MetaEdit(e) if e.lookup.editing)
        {
            self.lookup_edit_key(key);
            return;
        }
        // Cover tab: editing the free-text search bar.
        if tab == EditTab::Cover
            && matches!(&self.overlay, Overlay::MetaEdit(e) if e.cover_search.editing)
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
                    self.overlay = Overlay::None;
                }
            }
            // ^S asks for confirmation first (unless a field is invalid).
            KeyCode::Char('s') if ctrl => {
                if !matches!(&self.overlay, Overlay::MetaEdit(e) if e.has_invalid()) {
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
        let Overlay::MetaEdit(ed) = &self.overlay else {
            return;
        };
        let i = EditTab::ALL.iter().position(|t| *t == ed.tab).unwrap_or(0) as isize;
        let n = EditTab::ALL.len() as isize;
        self.meta_edit_goto_tab(EditTab::ALL[(i + delta).rem_euclid(n) as usize]);
    }

    /// Switch to `tab` (shared by Tab/Shift-Tab and the 1–4 number keys),
    /// running the per-tab on-enter work.
    pub(crate) fn meta_edit_goto_tab(&mut self, tab: EditTab) {
        if let Overlay::MetaEdit(ed) = &mut self.overlay {
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
            && matches!(
                &self.overlay,
                Overlay::MetaEdit(e) if e.cover_hits.is_empty() && !e.cover_search.fetching
            )
        {
            self.online_search();
        }
        // Likewise the Lookup tab auto-searches once from its seeded fields.
        if tab == EditTab::Online
            && matches!(
                &self.overlay,
                Overlay::MetaEdit(e) if e.online.results.is_empty() && !e.online.fetching
            )
        {
            self.online_search();
        }
    }

    /// Details tab, navigate mode: move between fields; Enter edits.
    fn details_nav_key(&mut self, key: KeyEvent) {
        // `x` extracts metadata from the book's own content (own borrow).
        if matches!(key.code, KeyCode::Char('x')) {
            self.details_extract_from_content();
            return;
        }
        let Overlay::MetaEdit(ed) = &mut self.overlay else {
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
                if let Some(field) = ed.values.get_mut(ed.row) {
                    field.end(); // caret to the end of the field's value
                }
            }
            // Reset the focused field (r) or all fields (R) to the EPUB value.
            KeyCode::Char('r') => {
                if let Some(orig) = ed.original.get(ed.row).cloned() {
                    ed.values[ed.row].set(orig);
                }
            }
            KeyCode::Char('R') => {
                ed.values = ed.original.iter().cloned().map(TextInput::from).collect();
            }
            _ => {}
        }
    }

    /// Fill the Details fields from the book's own content (title, subtitle,
    /// author, year, publisher, ISBN) — for converted files with junk metadata
    /// that aren't findable online. Best-effort; the user reviews, then ^S.
    fn details_extract_from_content(&mut self) {
        let Overlay::MetaEdit(ed) = &self.overlay else {
            return;
        };
        let path = ed.path.clone();
        let m = epub::extract_book_metadata(&path);
        let Overlay::MetaEdit(ed) = &mut self.overlay else {
            return;
        };
        let mut filled = Vec::new();
        if let Some(t) = m.title {
            ed.values[0].set(t);
            filled.push("title");
        }
        if let Some(a) = m.author {
            ed.values[1].set(a);
            filled.push("author");
        }
        if let Some(y) = m.year {
            ed.values[F_YEAR].set(y.to_string());
            filled.push("year");
        }
        if let Some(p) = m.publisher {
            ed.values[5].set(p);
            filled.push("publisher");
        }
        if let Some(s) = m.subtitle {
            ed.values[F_SUBTITLE].set(s);
            filled.push("subtitle");
        }
        if let Some(i) = m.isbn {
            ed.values[7].set(i);
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
        let Overlay::MetaEdit(ed) = &mut self.overlay else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Enter => ed.mode = EditMode::Nav,
            _ => {
                if let Some(field) = ed.edit_target() {
                    field.handle_key(key);
                }
            }
        }
    }

    /// Persist the edited fields + any fetched cover (year/index parsed
    /// leniently; blank → unset). Collections are applied live, not here.
    pub(crate) fn save_meta_edit(&mut self) {
        if matches!(&self.overlay, Overlay::MetaEdit(e) if e.has_invalid()) {
            return;
        }
        let Overlay::MetaEdit(ed) = std::mem::replace(&mut self.overlay, Overlay::None) else {
            return;
        };
        let v = |i: usize| ed.values.get(i).map(|s| s.text().trim()).unwrap_or("");
        let year = v(2).parse::<i32>().ok();
        let series_index = v(4).parse::<f32>().ok();
        if let Some(store) = &self.session.store {
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
            self.library.flash = Some(embed_cover_into_file(&ed.path, bytes));
        }
        self.refresh_library();
        // In a multi-book edit, move on to the next; else `take()` left it closed.
        self.advance_edit_queue();
    }
}
