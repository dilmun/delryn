//! Persistence: SQLite under `~/.config/delryn` (single dir, configurable root
//! later). For now it holds per-book reading progress; the library index and
//! annotations land in the same database in later phases. See `DESIGN.md` §8.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use delryn_infra::config::ViewMode;

// Store methods are split by entity across these modules; each contributes an
// `impl Store` block (the schema, shared book query, and `now_secs` live here).
mod annotations;
mod books;
mod dups;
mod progress;
mod search;
mod shelves;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS progress (
    path       TEXT PRIMARY KEY,
    section    INTEGER NOT NULL,
    frac       REAL NOT NULL,
    view_mode  TEXT NOT NULL,
    theme      TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS books (
    path         TEXT PRIMARY KEY,
    title        TEXT NOT NULL DEFAULT '',
    author       TEXT NOT NULL DEFAULT '',
    year         INTEGER,
    size         INTEGER NOT NULL DEFAULT 0,
    sections     INTEGER NOT NULL DEFAULT 0,
    favorite     INTEGER NOT NULL DEFAULT 0,
    added_at     INTEGER NOT NULL DEFAULT 0,
    last_opened  INTEGER NOT NULL DEFAULT 0,
    mtime        INTEGER NOT NULL DEFAULT 0,
    series       TEXT NOT NULL DEFAULT '',
    series_index REAL,
    publisher    TEXT NOT NULL DEFAULT '',
    edited       INTEGER NOT NULL DEFAULT 0,
    subtitle     TEXT NOT NULL DEFAULT '',
    isbn         TEXT NOT NULL DEFAULT '',
    language     TEXT NOT NULL DEFAULT '',
    converted    INTEGER NOT NULL DEFAULT 0,
    rating       INTEGER NOT NULL DEFAULT 0,
    status       TEXT NOT NULL DEFAULT '',
    tags         TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS annotations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT NOT NULL,
    section    INTEGER NOT NULL,
    quote      TEXT NOT NULL,
    note       TEXT NOT NULL DEFAULT '',
    name       TEXT NOT NULL DEFAULT '',
    folder     TEXT NOT NULL DEFAULT '',
    kind       INTEGER NOT NULL DEFAULT 0,
    color      INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS shelves (
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (path, name)
);
CREATE TABLE IF NOT EXISTS collections (
    name TEXT PRIMARY KEY
);
CREATE TABLE IF NOT EXISTS dismissed_dups (
    signature TEXT PRIMARY KEY
);
CREATE TABLE IF NOT EXISTS dup_links (
    a      TEXT NOT NULL,
    b      TEXT NOT NULL,
    signal TEXT NOT NULL DEFAULT 'cover',
    PRIMARY KEY (a, b)
);
";

/// Current schema version. Bump when you append a migration step in [`migrate`].
const USER_VERSION: i64 = 3;

/// Bring the database up to [`USER_VERSION`], guarded by SQLite's `user_version`
/// pragma so each step runs **once** — not on every open as the old inline block
/// did. Steps are append-only: add `if version < N { … }` and bump
/// `USER_VERSION`; never edit a shipped step.
fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    // Step 1 — base schema (idempotent `CREATE … IF NOT EXISTS`): creates every
    // table for a fresh database, a no-op for one that already has them.
    if version < 1 {
        conn.execute_batch(SCHEMA)?;
        // Full-text index — graceful: skipped if FTS5 isn't compiled in.
        let _ = conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(path UNINDEXED, body);",
        );
    }
    // Step 2 — columns added after the original schema, for databases created
    // before they existed. Idempotent (see [`legacy_column_backfill`]).
    if version < 2 {
        legacy_column_backfill(conn);
    }
    // Step 3 — highlight colour (palette index) on annotations, for databases
    // created before highlights existed. Best-effort: the `ADD COLUMN` errors (and
    // is ignored) when the column is already present from `SCHEMA` on a fresh DB.
    if version < 3 {
        let _ = conn.execute(
            "ALTER TABLE annotations ADD COLUMN color INTEGER NOT NULL DEFAULT 0",
            [],
        );
    }
    conn.pragma_update(None, "user_version", USER_VERSION)?;
    Ok(())
}

/// The pre-versioning column/seed backfill ([`migrate`] step 2). Every statement
/// is best-effort: an `ADD COLUMN` errors (and is ignored) when the column is
/// already present, so this is safe to run on a fresh database too — the columns
/// it can't add already came from `SCHEMA`. `user_version` gates it out after the
/// first run, so it never executes on a steady-state database.
fn legacy_column_backfill(conn: &Connection) {
    // Reading progress predating the theme column.
    let _ = conn.execute(
        "ALTER TABLE progress ADD COLUMN theme TEXT NOT NULL DEFAULT ''",
        [],
    );
    // Reading-time accounting.
    let _ = conn.execute(
        "ALTER TABLE books ADD COLUMN read_seconds INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Series / publisher metadata.
    let _ = conn.execute(
        "ALTER TABLE books ADD COLUMN series TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute("ALTER TABLE books ADD COLUMN series_index REAL", []);
    let _ = conn.execute(
        "ALTER TABLE books ADD COLUMN publisher TEXT NOT NULL DEFAULT ''",
        [],
    );
    // Manual-edit guard: 1 once a book's metadata is hand-edited, so a rescan
    // won't overwrite it (see `upsert_book`).
    let _ = conn.execute(
        "ALTER TABLE books ADD COLUMN edited INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Extra descriptive fields.
    for col in ["subtitle", "isbn", "language"] {
        let _ = conn.execute(
            &format!("ALTER TABLE books ADD COLUMN {col} TEXT NOT NULL DEFAULT ''"),
            [],
        );
    }
    // Whether the EPUB looks converted/repackaged vs an original publisher file.
    let _ = conn.execute(
        "ALTER TABLE books ADD COLUMN converted INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // User rating (0 unrated … 5 stars).
    let _ = conn.execute(
        "ALTER TABLE books ADD COLUMN rating INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Manual reading-status override (paused/dropped/reference; empty = derive).
    let _ = conn.execute(
        "ALTER TABLE books ADD COLUMN status TEXT NOT NULL DEFAULT ''",
        [],
    );
    // User tags (free-form, normalised; library-only).
    let _ = conn.execute(
        "ALTER TABLE books ADD COLUMN tags TEXT NOT NULL DEFAULT ''",
        [],
    );
    // Bookmark organisation: a custom name and a folder.
    for col in ["name", "folder"] {
        let _ = conn.execute(
            &format!("ALTER TABLE annotations ADD COLUMN {col} TEXT NOT NULL DEFAULT ''"),
            [],
        );
    }
    // Kind discriminator (0 = bookmark, 1 = note); tag pre-existing note rows.
    let _ = conn.execute(
        "ALTER TABLE annotations ADD COLUMN kind INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute("UPDATE annotations SET kind = 1 WHERE note <> ''", []);
    // First-class collections: seed the names table from existing memberships.
    let _ = conn.execute(
        "INSERT OR IGNORE INTO collections (name) SELECT DISTINCT name FROM shelves",
        [],
    );
}

/// `annotations.kind`: a bookmark (a place), a note (a place + commentary), or a
/// highlight (a place marked in a colour — see `color`).
pub const KIND_BOOKMARK: i64 = 0;
pub const KIND_NOTE: i64 = 1;
pub const KIND_HIGHLIGHT: i64 = 2;

/// A bookmark, note, or highlight, anchored to content by a text quote
/// (reflow-stable). `name` is an optional user label (shown instead of the quote);
/// `folder` is an optional group (empty = ungrouped); `note` is the commentary body
/// (notes only); `kind` discriminates the three; `color` is the highlight's palette
/// index (0 and unused for bookmarks/notes).
#[derive(Clone)]
pub struct Annotation {
    pub id: i64,
    pub section: usize,
    pub quote: String,
    pub note: String,
    pub name: String,
    pub folder: String,
    pub kind: i64,
    pub color: i64,
}

impl Annotation {
    /// Whether this annotation is a note (carries commentary) vs a bare bookmark.
    pub fn is_note(&self) -> bool {
        self.kind == KIND_NOTE
    }

    /// Whether this annotation is a colour highlight.
    pub fn is_highlight(&self) -> bool {
        self.kind == KIND_HIGHLIGHT
    }
}

/// Which slice of the library to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibrarySection {
    Recent,
    All,
    Pdf,
    Epub,
    Favorites,
    Reading,
    Series,
    Duplicates,
}

impl LibrarySection {
    /// The fixed sections, in sidebar / Tab-cycle order.
    pub const ALL: [LibrarySection; 8] = [
        LibrarySection::Recent,
        LibrarySection::All,
        LibrarySection::Pdf,
        LibrarySection::Epub,
        LibrarySection::Favorites,
        LibrarySection::Reading,
        LibrarySection::Series,
        LibrarySection::Duplicates,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LibrarySection::Recent => "Recent",
            LibrarySection::All => "All Books",
            LibrarySection::Pdf => "PDFs",
            LibrarySection::Epub => "EPUBs",
            LibrarySection::Favorites => "Favorites",
            LibrarySection::Reading => "Currently Reading",
            LibrarySection::Series => "Series",
            LibrarySection::Duplicates => "Duplicates",
        }
    }
}

/// A library list row.
pub struct BookRow {
    pub path: String,
    pub title: String,
    pub author: String,
    pub year: Option<i32>,
    pub size: u64,
    pub favorite: bool,
    /// Reading progress percent (0 if unstarted).
    pub pct: u8,
    /// Series name, empty if none.
    pub series: String,
    /// Position within the series, if any.
    pub series_index: Option<f32>,
    /// Publisher, empty if none.
    pub publisher: String,
    /// Subtitle, empty if none.
    pub subtitle: String,
    /// ISBN, empty if none.
    pub isbn: String,
    /// Language, empty if none.
    pub language: String,
    /// True when the EPUB looks converted/repackaged (e.g. by calibre) rather
    /// than an original publisher file.
    pub converted: bool,
    /// User rating, 0 (unrated) to 5 stars.
    pub rating: u8,
    /// Manual reading-status override (paused/dropped/reference); empty means
    /// the status is derived from reading progress.
    pub status: String,
    /// User tags: free-form, normalised (lowercased, trimmed, deduped),
    /// comma-separated. Empty when untagged. Library-only.
    pub tags: String,
}

/// Saved reading position for a book.
pub struct Progress {
    pub section: usize,
    /// Scroll position within the section, as a fraction `[0, 1]` (width-robust).
    pub frac: f32,
    pub view_mode: ViewMode,
    /// Theme name, empty if none saved.
    pub theme: String,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the database under the config directory.
    pub fn open_default() -> Result<Store> {
        let dir = delryn_infra::paths::config_dir();
        std::fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("delryn.db"))?;
        // A background library scan opens its own connection and writes while the
        // UI reads. WAL lets those overlap without `SQLITE_BUSY` (readers never
        // block the writer); the busy_timeout covers the rarer two-writer overlap
        // (a UI edit landing mid-scan). Both are best-effort — a filesystem that
        // rejects WAL simply keeps the default journal.
        let _ = conn.busy_timeout(std::time::Duration::from_millis(5000));
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
        migrate(&conn)?;
        Ok(Store { conn })
    }

    fn query_books(&self, where_clause: &str, order: &str) -> Vec<BookRow> {
        self.query_books_sql("", where_clause, order, [])
    }

    /// Shared book query. `join` adds extra FROM joins (the column list and the
    /// `progress` join are fixed); `params` binds any `?` placeholders.
    fn query_books_sql<P: rusqlite::Params>(
        &self,
        join: &str,
        where_clause: &str,
        order: &str,
        params: P,
    ) -> Vec<BookRow> {
        let sql = format!(
            "SELECT b.path, b.title, b.author, b.year, b.size, b.favorite, b.sections, \
             p.section, p.frac, b.series, b.series_index, b.publisher, \
             b.subtitle, b.isbn, b.language, b.converted, b.rating, b.status, b.tags \
             FROM books b LEFT JOIN progress p ON p.path = b.path {join} \
             WHERE {where_clause} ORDER BY {order}"
        );

        let mut out = Vec::new();
        let Ok(mut stmt) = self.conn.prepare(&sql) else {
            return out;
        };
        let rows = stmt.query_map(params, |r| {
            let sections: i64 = r.get(6)?;
            let section: Option<i64> = r.get(7)?;
            let frac: Option<f64> = r.get(8)?;
            let pct = match (section, sections) {
                (Some(s), n) if n > 0 => {
                    let pos = s as f64 + frac.unwrap_or(0.0);
                    ((pos / n as f64) * 100.0).clamp(0.0, 100.0) as u8
                }
                _ => 0,
            };
            Ok(BookRow {
                path: r.get(0)?,
                title: r.get(1)?,
                author: r.get(2)?,
                year: r.get::<_, Option<i64>>(3)?.map(|y| y as i32),
                size: r.get::<_, i64>(4)?.max(0) as u64,
                favorite: r.get::<_, i64>(5)? != 0,
                pct,
                series: r.get(9)?,
                series_index: r.get::<_, Option<f64>>(10)?.map(|i| i as f32),
                publisher: r.get(11)?,
                subtitle: r.get(12)?,
                isbn: r.get(13)?,
                language: r.get(14)?,
                converted: r.get::<_, i64>(15)? != 0,
                rating: r.get::<_, i64>(16)?.clamp(0, 5) as u8,
                status: r.get(17)?,
                tags: r.get(18)?,
            })
        });
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                out.push(row);
            }
        }
        out
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_stamps_version_adds_columns_and_is_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, USER_VERSION);
        // Columns from both SCHEMA (tags) and the backfill (read_seconds) exist.
        assert!(
            conn.prepare("SELECT read_seconds, tags, status FROM books")
                .is_ok()
        );
        // Re-running is a no-op and must not error (the steady-state open path).
        migrate(&conn).unwrap();
        let v2: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v2, USER_VERSION);
    }

    #[test]
    fn migrate_backfills_a_pre_versioning_database() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // A pre-versioning DB: original tables missing later columns, version 0.
        conn.execute_batch(
            "CREATE TABLE books (path TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE annotations (id INTEGER PRIMARY KEY, path TEXT, note TEXT NOT NULL DEFAULT '');
             CREATE TABLE shelves (path TEXT, name TEXT);",
        )
        .unwrap();
        migrate(&conn).unwrap();
        // The backfill added the columns the old schema lacked.
        assert!(
            conn.prepare("SELECT read_seconds, rating, status, tags FROM books")
                .is_ok()
        );
        assert!(conn.prepare("SELECT kind, folder FROM annotations").is_ok());
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, USER_VERSION);
    }

    #[test]
    fn progress_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("delryn_test_{}", std::process::id()));
        // SAFETY: single-threaded test; sets the config dir for this process.
        let _env = delryn_infra::test_env_guard();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };

        let store = Store::open_default().unwrap();
        store
            .save_progress("/books/a.epub", 5, 0.42, ViewMode::TwoPage, "dracula")
            .unwrap();

        let p = store.load_progress("/books/a.epub").unwrap();
        assert_eq!(p.section, 5);
        assert!((p.frac - 0.42).abs() < 1e-4);
        assert_eq!(p.view_mode, ViewMode::TwoPage);
        assert_eq!(p.theme, "dracula");

        // Upsert overwrites.
        store
            .save_progress("/books/a.epub", 9, 0.1, ViewMode::Center, "gruvbox")
            .unwrap();
        assert_eq!(store.load_progress("/books/a.epub").unwrap().section, 9);

        assert!(store.load_progress("/books/missing.epub").is_none());

        // Bookmarks: quick drops, delete, and folder grouping.
        store.add_bookmark("/books/a.epub", 3, "the quote");
        store.add_bookmark("/books/a.epub", 5, "another");
        let marks = store.list_bookmarks("/books/a.epub");
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].section, 3);
        store.delete_annotation(marks[0].id);
        let marks = store.list_bookmarks("/books/a.epub");
        assert_eq!(marks.len(), 1);

        // Names and folders: a name labels the bookmark; folders group entries.
        let kept = marks[0].id;
        store.set_annotation_name(kept, "Key idea");
        store.set_annotation_folder(kept, "Research");
        store.add_bookmark("/books/a.epub", 1, "intro line"); // ungrouped
        store.add_bookmark("/books/a.epub", 2, "method line");
        let method = store
            .list_bookmarks("/books/a.epub")
            .into_iter()
            .find(|a| a.section == 2)
            .unwrap()
            .id;
        store.set_annotation_folder(method, "Research");

        // Named folders sort before ungrouped; within a folder, by reading order.
        let grouped = store.list_bookmarks("/books/a.epub");
        assert_eq!(
            grouped
                .iter()
                .map(|a| (a.folder.as_str(), a.section))
                .collect::<Vec<_>>(),
            vec![("Research", 2), ("Research", 5), ("", 1)],
        );
        assert_eq!(grouped[1].name, "Key idea");

        store
            .upsert_book(
                "/books/a.epub",
                "A",
                "Au",
                None,
                10,
                8,
                1,
                "",
                None,
                "",
                "",
                "",
                "",
            )
            .unwrap();
        store.add_read_time("/books/a.epub", 120);
        store.add_read_time("/books/a.epub", 60);
        assert_eq!(store.total_read_seconds(), 180);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn notes_carry_commentary_and_list_with_bookmarks() {
        let tmp = std::env::temp_dir().join(format!("delryn_notes_{}", std::process::id()));
        let _env = delryn_infra::test_env_guard();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        let p = "/books/notes.epub";
        store.add_bookmark(p, 2, "a place");
        store.add_note(p, 4, "anchored line", "my thought about this");

        // The unified list has both, kind-tagged; the bookmark list has just the mark.
        let all = store.list_annotations(p);
        assert_eq!(all.len(), 2);
        let note = all.iter().find(|a| a.is_note()).unwrap();
        assert_eq!(note.section, 4);
        assert_eq!(note.note, "my thought about this");
        assert!(!all.iter().find(|a| a.section == 2).unwrap().is_note());
        assert_eq!(store.list_bookmarks(p).len(), 1);

        // Editing a note's commentary persists.
        store.set_annotation_note(note.id, "revised thought");
        let revised = store
            .list_annotations(p)
            .into_iter()
            .find(|a| a.is_note())
            .unwrap();
        assert_eq!(revised.note, "revised thought");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn highlights_carry_a_colour_and_are_kind_filtered() {
        let tmp = std::env::temp_dir().join(format!("delryn_hl_{}", std::process::id()));
        let _env = delryn_infra::test_env_guard();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        let p = "/books/hl.epub";
        store.add_bookmark(p, 1, "a place");
        store.add_highlight(p, 3, "marked line", 2);

        // The unified list carries the highlight with its colour; the bookmark list
        // excludes it (kind-filtered).
        let all = store.list_annotations(p);
        assert_eq!(all.len(), 2);
        let hl = all.iter().find(|a| a.is_highlight()).unwrap();
        assert_eq!((hl.section, hl.color), (3, 2));
        assert_eq!(
            store.list_bookmarks(p).len(),
            1,
            "highlights aren't bookmarks"
        );

        // Recolouring persists.
        store.set_annotation_color(hl.id, 4);
        let recoloured = store
            .list_annotations(p)
            .into_iter()
            .find(|a| a.is_highlight())
            .unwrap();
        assert_eq!(recoloured.color, 4);

        // The insert reports its row id, which is how the caller recolours the
        // highlight it just made instead of stacking a second one over it.
        let id = store
            .add_highlight(p, 5, "another line", 1)
            .expect("insert reports its id");
        store.set_annotation_color(id, 3);
        let by_id = store
            .list_annotations(p)
            .into_iter()
            .find(|a| a.id == id)
            .expect("the reported id addresses the new row");
        assert_eq!((by_id.section, by_id.color), (5, 3));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn duplicates_section_groups_same_title() {
        let tmp = std::env::temp_dir().join(format!("delryn_dup_{}", std::process::id()));
        let _env = delryn_infra::test_env_guard();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        store
            .upsert_book(
                "/a.epub", "Dune", "Herbert", None, 1, 1, 1, "", None, "", "", "", "",
            )
            .unwrap();
        store
            .upsert_book(
                "/b.epub", "dune", "Other", None, 1, 1, 1, "", None, "", "", "", "",
            )
            .unwrap();
        store
            .upsert_book(
                "/c.epub", "Unique", "Someone", None, 1, 1, 1, "", None, "", "", "", "",
            )
            .unwrap();

        let dups = store.list_books(LibrarySection::Duplicates);
        assert_eq!(dups.len(), 2, "both 'Dune' editions are duplicates");
        assert!(dups.iter().all(|b| b.title.eq_ignore_ascii_case("dune")));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn count_books_matches_list_books_per_section() {
        let tmp = std::env::temp_dir().join(format!("delryn_count_{}", std::process::id()));
        let _env = delryn_infra::test_env_guard();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        store
            .upsert_book(
                "/a.pdf",
                "Alpha",
                "Au",
                None,
                1,
                1,
                1,
                "Saga",
                Some(1.0),
                "",
                "",
                "",
                "",
            )
            .unwrap();
        store
            .upsert_book(
                "/b.epub", "Beta", "Au", None, 1, 1, 1, "", None, "", "", "", "",
            )
            .unwrap();
        store
            .upsert_book(
                "/c.EPUB", "Gamma", "Au", None, 1, 1, 1, "", None, "", "", "", "",
            )
            .unwrap();
        store.set_favorite("/a.pdf", true);

        // Format filters are case-insensitive on the extension.
        assert_eq!(store.count_books(LibrarySection::All), 3);
        assert_eq!(store.count_books(LibrarySection::Pdf), 1);
        assert_eq!(store.count_books(LibrarySection::Epub), 2);
        assert_eq!(store.count_books(LibrarySection::Favorites), 1);
        assert_eq!(store.count_books(LibrarySection::Series), 1);

        // The count must never drift from the row list it mirrors.
        for s in LibrarySection::ALL {
            assert_eq!(
                store.count_books(s),
                store.list_books(s).len(),
                "count_books != list_books for {s:?}",
            );
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn series_section_sorts_by_series_then_index() {
        let tmp = std::env::temp_dir().join(format!("delryn_series_{}", std::process::id()));
        let _env = delryn_infra::test_env_guard();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        store
            .upsert_book(
                "/f2.epub",
                "Foundation and Empire",
                "Asimov",
                None,
                1,
                1,
                1,
                "Foundation",
                Some(2.0),
                "Gnome",
                "",
                "",
                "",
            )
            .unwrap();
        store
            .upsert_book(
                "/f1.epub",
                "Foundation",
                "Asimov",
                None,
                1,
                1,
                1,
                "Foundation",
                Some(1.0),
                "Gnome",
                "",
                "",
                "",
            )
            .unwrap();
        store
            .upsert_book(
                "/d.epub",
                "Dune",
                "Herbert",
                None,
                1,
                1,
                1,
                "Dune Chronicles",
                Some(1.0),
                "Chilton",
                "",
                "",
                "",
            )
            .unwrap();
        store
            .upsert_book(
                "/x.epub",
                "Standalone",
                "Nobody",
                None,
                1,
                1,
                1,
                "",
                None,
                "Self",
                "",
                "",
                "",
            )
            .unwrap();

        let series = store.list_books(LibrarySection::Series);
        let titles: Vec<&str> = series.iter().map(|b| b.title.as_str()).collect();
        // Dune Chronicles < Foundation alphabetically; within Foundation, #1 before #2.
        assert_eq!(titles, vec!["Dune", "Foundation", "Foundation and Empire"]);
        assert!(
            series.iter().all(|b| !b.series.is_empty()),
            "no standalone books"
        );
        assert_eq!(series[1].series_index, Some(1.0));
        assert_eq!(series[1].publisher, "Gnome");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn shelves_membership_and_listing() {
        let tmp = std::env::temp_dir().join(format!("delryn_shelf_{}", std::process::id()));
        let _env = delryn_infra::test_env_guard();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        store
            .upsert_book(
                "/a.epub", "Alpha", "A", None, 1, 1, 1, "", None, "", "", "", "",
            )
            .unwrap();
        store
            .upsert_book(
                "/b.epub", "Beta", "B", None, 1, 1, 1, "", None, "", "", "", "",
            )
            .unwrap();

        store.add_to_shelf("/a.epub", "Sci-Fi");
        store.add_to_shelf("/a.epub", "Sci-Fi"); // idempotent
        store.add_to_shelf("/b.epub", "Sci-Fi");
        store.add_to_shelf("/a.epub", "To Read");
        store.add_to_shelf("/a.epub", "  "); // blank ignored

        assert_eq!(store.shelves_for("/a.epub"), vec!["Sci-Fi", "To Read"]);

        let shelves = store.all_shelves();
        assert_eq!(shelves, vec![("Sci-Fi".into(), 2), ("To Read".into(), 1)]);

        let scifi = store.books_in_shelf("Sci-Fi");
        let titles: Vec<&str> = scifi.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(titles, vec!["Alpha", "Beta"]);

        // Collections are first-class: removing the last member leaves the
        // (now empty) collection in the listing with count 0.
        store.remove_from_shelf("/a.epub", "To Read");
        assert_eq!(
            store.all_shelves(),
            vec![("Sci-Fi".into(), 2), ("To Read".into(), 0)]
        );
        // Deleting it removes the collection entirely.
        store.delete_shelf("To Read");
        assert_eq!(store.all_shelves(), vec![("Sci-Fi".into(), 2)]);

        // An empty collection can be created on its own and persists.
        store.create_collection("Wishlist");
        assert!(
            store
                .all_shelves()
                .iter()
                .any(|(n, c)| n == "Wishlist" && *c == 0)
        );
        // Rename merges on collision.
        store.rename_shelf("Wishlist", "Sci-Fi");
        assert!(!store.all_shelves().iter().any(|(n, _)| n == "Wishlist"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn manual_edit_survives_rescan() {
        let tmp = std::env::temp_dir().join(format!("delryn_edit_{}", std::process::id()));
        let _env = delryn_infra::test_env_guard();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        // Initial index, then a hand-edit.
        store
            .upsert_book(
                "/b.epub",
                "raw title",
                "raw author",
                Some(1990),
                1,
                5,
                1,
                "",
                None,
                "",
                "",
                "",
                "",
            )
            .unwrap();
        store.update_book_meta(
            "/b.epub",
            "Clean Title",
            "Real Author",
            Some(2001),
            "My Series",
            Some(3.0),
            "Pub",
            "A Subtitle",
            "9780000000001",
            "eng",
        );

        // A rescan (file changed: new size/sections) must not clobber the edits,
        // but must still refresh the file stats.
        store
            .upsert_book(
                "/b.epub",
                "raw title",
                "raw author",
                Some(1990),
                999,
                9,
                2,
                "",
                None,
                "",
                "",
                "",
                "",
            )
            .unwrap();

        let b = &store.list_books(LibrarySection::All)[0];
        assert_eq!(b.title, "Clean Title");
        assert_eq!(b.author, "Real Author");
        assert_eq!(b.year, Some(2001));
        assert_eq!(b.series, "My Series");
        assert_eq!(b.series_index, Some(3.0));
        assert_eq!(b.publisher, "Pub");
        assert_eq!(b.subtitle, "A Subtitle");
        assert_eq!(b.isbn, "9780000000001");
        assert_eq!(b.language, "eng");
        assert_eq!(b.size, 999, "file stats still refresh on rescan");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
