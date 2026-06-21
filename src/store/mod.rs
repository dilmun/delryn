//! Persistence: SQLite under `~/.config/delryn` (single dir, configurable root
//! later). For now it holds per-book reading progress; the library index and
//! annotations land in the same database in later phases. See `DESIGN.md` §8.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::config::ViewMode;

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
    edited       INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS annotations (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT NOT NULL,
    section    INTEGER NOT NULL,
    quote      TEXT NOT NULL,
    note       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
";

/// A bookmark or note, anchored to content by a text quote (reflow-stable).
pub struct Annotation {
    pub id: i64,
    pub section: usize,
    pub quote: String,
    pub note: String,
}

/// Which slice of the library to show.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibrarySection {
    Recent,
    All,
    Favorites,
    Reading,
    Series,
    Duplicates,
}

impl LibrarySection {
    pub fn label(self) -> &'static str {
        match self {
            LibrarySection::Recent => "Recent",
            LibrarySection::All => "All Books",
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
        let dir = config_dir();
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
        // Full-text index (graceful: skipped if FTS5 isn't compiled in).
        let _ = conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(path UNINDEXED, body);",
        );
        Ok(Store { conn })
    }

    pub fn load_progress(&self, path: &str) -> Option<Progress> {
        self.conn
            .query_row(
                "SELECT section, frac, view_mode, theme FROM progress WHERE path = ?1",
                params![path],
                |row| {
                    let section: i64 = row.get(0)?;
                    let frac: f64 = row.get(1)?;
                    let view: String = row.get(2)?;
                    let theme: String = row.get(3)?;
                    Ok(Progress {
                        section: section.max(0) as usize,
                        frac: frac as f32,
                        view_mode: ViewMode::from_label(&view),
                        theme,
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn save_progress(
        &self,
        path: &str,
        section: usize,
        frac: f32,
        view_mode: ViewMode,
        theme: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO progress (path, section, frac, view_mode, theme, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                section = excluded.section,
                frac = excluded.frac,
                view_mode = excluded.view_mode,
                theme = excluded.theme,
                updated_at = excluded.updated_at",
            params![
                path,
                section as i64,
                frac as f64,
                view_mode.label(),
                theme,
                now_secs()
            ],
        )?;
        Ok(())
    }

    /// Has this file changed (or never been scanned)?
    pub fn needs_scan(&self, path: &str, mtime: i64, size: u64) -> bool {
        let row: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT mtime, size FROM books WHERE path = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        match row {
            Some((m, s)) => m != mtime || s != size as i64,
            None => true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_book(
        &self,
        path: &str,
        title: &str,
        author: &str,
        year: Option<i32>,
        size: u64,
        sections: usize,
        mtime: i64,
        series: &str,
        series_index: Option<f32>,
        publisher: &str,
    ) -> Result<()> {
        // On rescan, file stats (size/sections/mtime) always refresh, but the
        // descriptive fields are preserved when the user has hand-edited them
        // (`edited = 1`) so a re-index never clobbers manual corrections.
        self.conn.execute(
            "INSERT INTO books
                (path, title, author, year, size, sections, mtime, added_at,
                 series, series_index, publisher)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(path) DO UPDATE SET
                title  = CASE WHEN edited = 1 THEN title  ELSE excluded.title  END,
                author = CASE WHEN edited = 1 THEN author ELSE excluded.author END,
                year   = CASE WHEN edited = 1 THEN year   ELSE excluded.year   END,
                series = CASE WHEN edited = 1 THEN series ELSE excluded.series END,
                series_index =
                    CASE WHEN edited = 1 THEN series_index ELSE excluded.series_index END,
                publisher =
                    CASE WHEN edited = 1 THEN publisher ELSE excluded.publisher END,
                size = excluded.size, sections = excluded.sections, mtime = excluded.mtime",
            params![
                path,
                title,
                author,
                year,
                size as i64,
                sections as i64,
                mtime,
                now_secs(),
                series,
                series_index,
                publisher,
            ],
        )?;
        Ok(())
    }

    /// Overwrite a book's descriptive metadata with hand-edited values and mark
    /// it `edited` so a future rescan won't revert it (see `upsert_book`).
    #[allow(clippy::too_many_arguments)]
    pub fn update_book_meta(
        &self,
        path: &str,
        title: &str,
        author: &str,
        year: Option<i32>,
        series: &str,
        series_index: Option<f32>,
        publisher: &str,
    ) {
        let _ = self.conn.execute(
            "UPDATE books SET title = ?2, author = ?3, year = ?4, series = ?5, \
             series_index = ?6, publisher = ?7, edited = 1 WHERE path = ?1",
            params![path, title, author, year, series, series_index, publisher],
        );
    }

    pub fn set_favorite(&self, path: &str, favorite: bool) {
        let _ = self.conn.execute(
            "UPDATE books SET favorite = ?2 WHERE path = ?1",
            params![path, favorite as i64],
        );
    }

    pub fn mark_opened(&self, path: &str) {
        let _ = self.conn.execute(
            "INSERT INTO books (path, last_opened, added_at) VALUES (?1, ?2, ?2)
             ON CONFLICT(path) DO UPDATE SET last_opened = ?2",
            params![path, now_secs()],
        );
    }

    /// List books for a section (filtering by `query` substring is done by the
    /// caller).
    pub fn list_books(&self, section: LibrarySection) -> Vec<BookRow> {
        let where_clause = match section {
            LibrarySection::Recent => "b.last_opened > 0",
            LibrarySection::All => "1 = 1",
            LibrarySection::Favorites => "b.favorite = 1",
            LibrarySection::Reading => {
                "b.last_opened > 0 AND p.path IS NOT NULL \
                 AND (p.section + p.frac) < (b.sections * 0.98)"
            }
            // Books that declare a series, grouped by it.
            LibrarySection::Series => "b.series <> ''",
            // Books that share a (case-insensitive) title with another book.
            LibrarySection::Duplicates => {
                "b.title <> '' AND LOWER(b.title) IN \
                 (SELECT LOWER(title) FROM books WHERE title <> '' \
                  GROUP BY LOWER(title) HAVING COUNT(*) > 1)"
            }
        };
        let order = match section {
            // Within the Series view, sort by series then position then title.
            LibrarySection::Series => {
                "b.series COLLATE NOCASE, b.series_index, b.title COLLATE NOCASE"
            }
            LibrarySection::All | LibrarySection::Favorites | LibrarySection::Duplicates => {
                "b.title COLLATE NOCASE"
            }
            _ => "b.last_opened DESC",
        };
        self.query_books(where_clause, order)
    }

    /// All books, ordered by title (used for library-wide search).
    pub fn all_books(&self) -> Vec<BookRow> {
        self.query_books("1 = 1", "b.title COLLATE NOCASE")
    }

    pub fn all_book_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT path FROM books") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                out.extend(rows.flatten());
            }
        }
        out
    }

    /// Replace a book's full-text entry.
    pub fn index_text(&self, path: &str, body: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM fts WHERE path = ?1", params![path])?;
        self.conn.execute(
            "INSERT INTO fts (path, body) VALUES (?1, ?2)",
            params![path, body],
        )?;
        Ok(())
    }

    /// Book paths whose full text matches `query` (phrase match). Empty if FTS
    /// is unavailable or nothing matches.
    pub fn fts_paths(&self, query: &str) -> Vec<String> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let expr = format!("\"{}\"", q.replace('"', "\"\""));
        let mut out = Vec::new();
        if let Ok(mut stmt) = self
            .conn
            .prepare("SELECT path FROM fts WHERE body MATCH ?1 LIMIT 500")
        {
            if let Ok(rows) = stmt.query_map(params![expr], |r| r.get::<_, String>(0)) {
                out.extend(rows.flatten());
            }
        }
        out
    }

    pub fn add_annotation(&self, path: &str, section: usize, quote: &str, note: &str) {
        let _ = self.conn.execute(
            "INSERT INTO annotations (path, section, quote, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![path, section as i64, quote, note, now_secs()],
        );
    }

    pub fn list_annotations(&self, path: &str) -> Vec<Annotation> {
        let mut out = Vec::new();
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, section, quote, note FROM annotations WHERE path = ?1 \
             ORDER BY section, id",
        ) else {
            return out;
        };
        if let Ok(rows) = stmt.query_map(params![path], |r| {
            Ok(Annotation {
                id: r.get(0)?,
                section: r.get::<_, i64>(1)?.max(0) as usize,
                quote: r.get(2)?,
                note: r.get(3)?,
            })
        }) {
            out.extend(rows.flatten());
        }
        out
    }

    pub fn delete_annotation(&self, id: i64) {
        let _ = self
            .conn
            .execute("DELETE FROM annotations WHERE id = ?1", params![id]);
    }

    /// Every annotation with its book path (for export).
    pub fn all_annotations(&self) -> Vec<(String, Annotation)> {
        let mut out = Vec::new();
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT path, id, section, quote, note FROM annotations ORDER BY path, section, id",
        ) else {
            return out;
        };
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Annotation {
                    id: r.get(1)?,
                    section: r.get::<_, i64>(2)?.max(0) as usize,
                    quote: r.get(3)?,
                    note: r.get(4)?,
                },
            ))
        }) {
            out.extend(rows.flatten());
        }
        out
    }

    pub fn add_read_time(&self, path: &str, secs: i64) {
        let _ = self.conn.execute(
            "UPDATE books SET read_seconds = read_seconds + ?2 WHERE path = ?1",
            params![path, secs],
        );
    }

    pub fn total_read_seconds(&self) -> i64 {
        self.conn
            .query_row("SELECT COALESCE(SUM(read_seconds), 0) FROM books", [], |r| {
                r.get(0)
            })
            .unwrap_or(0)
    }

    fn query_books(&self, where_clause: &str, order: &str) -> Vec<BookRow> {
        let sql = format!(
            "SELECT b.path, b.title, b.author, b.year, b.size, b.favorite, b.sections, \
             p.section, p.frac, b.series, b.series_index, b.publisher \
             FROM books b LEFT JOIN progress p ON p.path = b.path \
             WHERE {where_clause} ORDER BY {order}"
        );

        let mut out = Vec::new();
        let Ok(mut stmt) = self.conn.prepare(&sql) else {
            return out;
        };
        let rows = stmt.query_map([], |r| {
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

/// The delryn config/data directory: `$XDG_CONFIG_HOME/delryn` or `~/.config/delryn`
/// (per the project's single-dir decision), with a Windows fallback.
pub fn config_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("delryn");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("delryn");
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("delryn");
    }
    PathBuf::from(".delryn")
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
            .save_progress("/books/a.epub", 9, 0.1, ViewMode::Fill, "gruvbox")
            .unwrap();
        assert_eq!(store.load_progress("/books/a.epub").unwrap().section, 9);

        assert!(store.load_progress("/books/missing.epub").is_none());

        // Annotations + reading time.
        store.add_annotation("/books/a.epub", 3, "the quote", "");
        store.add_annotation("/books/a.epub", 5, "another", "my note");
        let anns = store.list_annotations("/books/a.epub");
        assert_eq!(anns.len(), 2);
        assert_eq!(anns[0].section, 3);
        assert_eq!(anns[1].note, "my note");
        store.delete_annotation(anns[0].id);
        assert_eq!(store.list_annotations("/books/a.epub").len(), 1);

        store
            .upsert_book("/books/a.epub", "A", "Au", None, 10, 8, 1, "", None, "")
            .unwrap();
        store.add_read_time("/books/a.epub", 120);
        store.add_read_time("/books/a.epub", 60);
        assert_eq!(store.total_read_seconds(), 180);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn duplicates_section_groups_same_title() {
        let tmp = std::env::temp_dir().join(format!("delryn_dup_{}", std::process::id()));
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        store.upsert_book("/a.epub", "Dune", "Herbert", None, 1, 1, 1, "", None, "").unwrap();
        store.upsert_book("/b.epub", "dune", "Other", None, 1, 1, 1, "", None, "").unwrap();
        store.upsert_book("/c.epub", "Unique", "Someone", None, 1, 1, 1, "", None, "").unwrap();

        let dups = store.list_books(LibrarySection::Duplicates);
        assert_eq!(dups.len(), 2, "both 'Dune' editions are duplicates");
        assert!(dups.iter().all(|b| b.title.eq_ignore_ascii_case("dune")));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn series_section_sorts_by_series_then_index() {
        let tmp = std::env::temp_dir().join(format!("delryn_series_{}", std::process::id()));
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        store.upsert_book("/f2.epub", "Foundation and Empire", "Asimov", None, 1, 1, 1,
            "Foundation", Some(2.0), "Gnome").unwrap();
        store.upsert_book("/f1.epub", "Foundation", "Asimov", None, 1, 1, 1,
            "Foundation", Some(1.0), "Gnome").unwrap();
        store.upsert_book("/d.epub", "Dune", "Herbert", None, 1, 1, 1,
            "Dune Chronicles", Some(1.0), "Chilton").unwrap();
        store.upsert_book("/x.epub", "Standalone", "Nobody", None, 1, 1, 1,
            "", None, "Self").unwrap();

        let series = store.list_books(LibrarySection::Series);
        let titles: Vec<&str> = series.iter().map(|b| b.title.as_str()).collect();
        // Dune Chronicles < Foundation alphabetically; within Foundation, #1 before #2.
        assert_eq!(titles, vec!["Dune", "Foundation", "Foundation and Empire"]);
        assert!(series.iter().all(|b| !b.series.is_empty()), "no standalone books");
        assert_eq!(series[1].series_index, Some(1.0));
        assert_eq!(series[1].publisher, "Gnome");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn manual_edit_survives_rescan() {
        let tmp = std::env::temp_dir().join(format!("delryn_edit_{}", std::process::id()));
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let store = Store::open_default().unwrap();

        // Initial index, then a hand-edit.
        store.upsert_book("/b.epub", "raw title", "raw author", Some(1990), 1, 5, 1,
            "", None, "").unwrap();
        store.update_book_meta("/b.epub", "Clean Title", "Real Author", Some(2001),
            "My Series", Some(3.0), "Pub");

        // A rescan (file changed: new size/sections) must not clobber the edits,
        // but must still refresh the file stats.
        store.upsert_book("/b.epub", "raw title", "raw author", Some(1990), 999, 9, 2,
            "", None, "").unwrap();

        let b = &store.list_books(LibrarySection::All)[0];
        assert_eq!(b.title, "Clean Title");
        assert_eq!(b.author, "Real Author");
        assert_eq!(b.year, Some(2001));
        assert_eq!(b.series, "My Series");
        assert_eq!(b.series_index, Some(3.0));
        assert_eq!(b.publisher, "Pub");
        assert_eq!(b.size, 999, "file stats still refresh on rescan");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
