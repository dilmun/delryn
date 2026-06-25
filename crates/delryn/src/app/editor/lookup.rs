//! Metadata-editor: online metadata + cover lookup. The Lookup/Online and
//! Cover tab interaction (seed/search keys, result browsing) and the background
//! execution (Open Library search, cover fetch/preview, result application).
//! Child of `editor`, so it reaches the editor shell's private MetaEdit helpers.

use super::*;

impl App {
    /// Re-seed the Lookup and Cover searches from the *current* Details title and
    /// author when they've changed since the last seed (e.g. after `x` extract or
    /// a manual edit). A no-op when unchanged, so manual search edits are kept.
    pub(crate) fn reseed_search_from_details(&mut self) {
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

    /// Lookup tab, navigate mode: `j/k` flow through the three seed fields and
    /// then the results; Enter edits a field or applies a result; `/` re-runs the
    /// search; typing on a field starts editing it.
    pub(crate) fn lookup_nav_key(&mut self, key: KeyEvent) {
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
                    self.open_diff(focus - LOOKUP_FIELDS);
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
    pub(crate) fn lookup_edit_key(&mut self, key: KeyEvent) {
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

    /// Online/Cover tabs, browsing the results: `/` or typing opens the search
    /// bar; j/k move the selection; Enter applies (metadata on Online, the
    /// previewed cover on Cover).
    pub(crate) fn online_nav_key(&mut self, key: KeyEvent) {
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
                    self.open_diff(idx);
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
    pub(crate) fn online_query_key(&mut self, key: KeyEvent) {
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
    pub(crate) fn online_search(&mut self) {
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
    /// Open the metadata-diff overlay for result `idx`: current Details vs the
    /// candidate, one row per field, ticking the fields whose remote value
    /// differs (and is present), so the reader can review before applying.
    fn open_diff(&mut self, idx: usize) {
        let Some(ed) = self.meta_edit.as_mut() else {
            return;
        };
        let Some(c) = ed.search().results.get(idx).cloned() else {
            return;
        };
        // Candidate-fillable fields are the first 8 of META_FIELDS (Title…ISBN);
        // Language (8) isn't carried by the metadata APIs.
        let rows: Vec<DiffRow> = (0..8)
            .map(|field| {
                let remote = remote_value(&c, field);
                let apply = !remote.is_empty() && remote != ed.values[field];
                DiffRow {
                    field,
                    remote,
                    apply,
                }
            })
            .collect();
        ed.diff = Some(MetaDiff {
            rows,
            row: 0,
            cover_url: c.cover_url(),
        });
    }

    /// Apply the ticked diff rows into the Details fields, fetch the candidate's
    /// cover, and close the diff onto the Details tab for a final review.
    pub(crate) fn apply_diff(&mut self) {
        let cover_url = {
            let Some(ed) = self.meta_edit.as_mut() else {
                return;
            };
            let Some(diff) = ed.diff.take() else {
                return;
            };
            for r in &diff.rows {
                if r.apply {
                    ed.values[r.field] = r.remote.clone();
                }
            }
            ed.tab = EditTab::Details;
            ed.mode = EditMode::Nav;
            ed.row = 0;
            ed.status_on(EditTab::Details, "applied — review, then ^S to save");
            ed.cover_pending = diff.cover_url.is_some();
            diff.cover_url
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
}

/// A candidate's value for `META_FIELDS` index `field` (empty when absent).
/// Language (index 8) isn't carried by the metadata APIs, so it's never filled.
fn remote_value(c: &Candidate, field: usize) -> String {
    match field {
        0 => c.title.clone(),
        1 => c.author_line(),
        2 => c.year.map(|y| y.to_string()).unwrap_or_default(),
        3 => c.series.clone().unwrap_or_default(),
        4 => c.series_index.map(fmt_series_index).unwrap_or_default(),
        5 => c.publisher.clone().unwrap_or_default(),
        6 => c.subtitle.clone().unwrap_or_default(),
        7 => c.isbn.clone().unwrap_or_default(),
        _ => String::new(),
    }
}
