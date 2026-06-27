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
";

/// A bookmark or note, anchored to content by a text quote (reflow-stable).
/// `name` is an optional user label (shown instead of the quote); `folder` is an
/// optional group (empty = ungrouped).
pub struct Annotation {
    pub id: i64,
    pub section: usize,
    pub quote: String,
    pub note: String,
    pub name: String,
    pub folder: String,
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
        conn.execute_batch(SCHEMA)?;
        // Migrate older databases that predate the theme column.
        let _ = conn.execute(
            "ALTER TABLE progress ADD COLUMN theme TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Reading-time accounting (migrate older databases).
        let _ = conn.execute(
            "ALTER TABLE books ADD COLUMN read_seconds INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Series / publisher metadata (migrate older databases).
        let _ = conn.execute(
            "ALTER TABLE books ADD COLUMN series TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = conn.execute("ALTER TABLE books ADD COLUMN series_index REAL", []);
        let _ = conn.execute(
            "ALTER TABLE books ADD COLUMN publisher TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Manual-edit guard: 1 once a book's metadata is hand-edited, so a
        // rescan won't overwrite it (see `upsert_book`).
        let _ = conn.execute(
            "ALTER TABLE books ADD COLUMN edited INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Extra descriptive fields (migrate older databases).
        for col in ["subtitle", "isbn", "language"] {
            let _ = conn.execute(
                &format!("ALTER TABLE books ADD COLUMN {col} TEXT NOT NULL DEFAULT ''"),
                [],
            );
        }
        // Whether the EPUB looks converted/repackaged vs an original publisher
        // file (a file fact, refreshed every index — not subject to `edited`).
        let _ = conn.execute(
            "ALTER TABLE books ADD COLUMN converted INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // User rating (0 unrated … 5 stars); user-set, preserved across rescans.
        let _ = conn.execute(
            "ALTER TABLE books ADD COLUMN rating INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Manual reading-status override (paused/dropped/reference; empty = derive
        // from progress); user-set, preserved across rescans.
        let _ = conn.execute(
            "ALTER TABLE books ADD COLUMN status TEXT NOT NULL DEFAULT ''",
            [],
        );
        // User tags (free-form, comma-separated, normalised; preserved across
        // rescans). Library-only — never written back to the file.
        let _ = conn.execute(
            "ALTER TABLE books ADD COLUMN tags TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Bookmark organisation: a custom name and a folder (migrate older DBs).
        for col in ["name", "folder"] {
            let _ = conn.execute(
                &format!("ALTER TABLE annotations ADD COLUMN {col} TEXT NOT NULL DEFAULT ''"),
                [],
            );
        }
        // Kind discriminator (0 = bookmark, 1 = note). Notes are a Phase 4 concern;
        // bookmarks stay a pure list. Tag any pre-existing note-bearing rows as
        // notes so they don't surface in the bookmarks overlay.
        let _ = conn.execute(
            "ALTER TABLE annotations ADD COLUMN kind INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("UPDATE annotations SET kind = 1 WHERE note <> ''", []);
        // First-class collections: seed the names table from existing memberships
        // so collections created before this migration keep showing.
        let _ = conn.execute(
            "INSERT OR IGNORE INTO collections (name) SELECT DISTINCT name FROM shelves",
            [],
        );
        // Full-text index (graceful: skipped if FTS5 isn't compiled in).
        let _ = conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(path UNINDEXED, body);",
        );
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
