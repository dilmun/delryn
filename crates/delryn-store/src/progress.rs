//! Reading progress and accumulated reading-time persistence.

use super::*;

impl Store {
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

    pub fn add_read_time(&self, path: &str, secs: i64) {
        let _ = self.conn.execute(
            "UPDATE books SET read_seconds = read_seconds + ?2 WHERE path = ?1",
            params![path, secs],
        );
    }

    pub fn total_read_seconds(&self) -> i64 {
        self.conn
            .query_row(
                "SELECT COALESCE(SUM(read_seconds), 0) FROM books",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    // --- Collections (shelves) -------------------------------------------
}
