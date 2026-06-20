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
";

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
}

/// The delryn config/data directory: `$XDG_CONFIG_HOME/delryn` or `~/.config/delryn`
/// (per the project's single-dir decision), with a Windows fallback.
fn config_dir() -> PathBuf {
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
