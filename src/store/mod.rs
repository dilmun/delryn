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
    path        TEXT PRIMARY KEY,
    title       TEXT NOT NULL DEFAULT '',
    author      TEXT NOT NULL DEFAULT '',
    year        INTEGER,
    size        INTEGER NOT NULL DEFAULT 0,
    sections    INTEGER NOT NULL DEFAULT 0,
    favorite    INTEGER NOT NULL DEFAULT 0,
    added_at    INTEGER NOT NULL DEFAULT 0,
    last_opened INTEGER NOT NULL DEFAULT 0,
    mtime       INTEGER NOT NULL DEFAULT 0
);
";

/// Which slice of the library to show.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibrarySection {
    Recent,
    All,
    Favorites,
    Reading,
}

impl LibrarySection {
    pub fn label(self) -> &'static str {
        match self {
            LibrarySection::Recent => "Recent",
            LibrarySection::All => "All Books",
            LibrarySection::Favorites => "Favorites",
            LibrarySection::Reading => "Currently Reading",
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
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO books (path, title, author, year, size, sections, mtime, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(path) DO UPDATE SET
                title = excluded.title, author = excluded.author, year = excluded.year,
                size = excluded.size, sections = excluded.sections, mtime = excluded.mtime",
            params![
                path,
                title,
                author,
                year,
                size as i64,
                sections as i64,
                mtime,
                now_secs()
            ],
        )?;
        Ok(())
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
        };
        let order = match section {
            LibrarySection::All | LibrarySection::Favorites => "b.title COLLATE NOCASE",
            _ => "b.last_opened DESC",
        };
        let sql = format!(
            "SELECT b.path, b.title, b.author, b.year, b.size, b.favorite, b.sections, \
             p.section, p.frac FROM books b LEFT JOIN progress p ON p.path = b.path \
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
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
