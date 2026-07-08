//! Bookmarks, notes, and highlights anchored to content quotes. Rows carry a
//! `kind` ([`KIND_BOOKMARK`] = a place, [`KIND_NOTE`] = a place + commentary,
//! [`KIND_HIGHLIGHT`] = a place marked in a `color`). All quotes are reflow-stable
//! text anchors, not byte offsets.

use super::*;

impl Store {
    /// Drop a bookmark at a reading position (anchored by its quote).
    pub fn add_bookmark(&self, path: &str, section: usize, quote: &str) {
        let _ = self.conn.execute(
            "INSERT INTO annotations (path, section, quote, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![path, section as i64, quote, KIND_BOOKMARK, now_secs()],
        );
    }

    /// Add a note at a reading position: a quote anchor plus the user's commentary.
    pub fn add_note(&self, path: &str, section: usize, quote: &str, note: &str) {
        let _ = self.conn.execute(
            "INSERT INTO annotations (path, section, quote, note, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![path, section as i64, quote, note, KIND_NOTE, now_secs()],
        );
    }

    /// Highlight a reading position in a palette `color` (anchored by its quote).
    pub fn add_highlight(&self, path: &str, section: usize, quote: &str, color: i64) {
        let _ = self.conn.execute(
            "INSERT INTO annotations (path, section, quote, kind, color, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![path, section as i64, quote, KIND_HIGHLIGHT, color, now_secs()],
        );
    }

    /// Bookmarks for one book (see [`Self::list_annotations`] for the ordering).
    pub fn list_bookmarks(&self, path: &str) -> Vec<Annotation> {
        self.list_by_kind(path, Some(KIND_BOOKMARK))
    }

    /// Every annotation (bookmarks and notes) for one book, folder-grouped: named
    /// folders first (alphabetical), then ungrouped, each in reading order.
    pub fn list_annotations(&self, path: &str) -> Vec<Annotation> {
        self.list_by_kind(path, None)
    }

    /// List one book's annotations, optionally restricted to a single `kind`.
    fn list_by_kind(&self, path: &str, kind: Option<i64>) -> Vec<Annotation> {
        let mut out = Vec::new();
        let sql = "SELECT id, section, quote, note, name, folder, kind, color FROM annotations \
             WHERE path = ?1 AND (?2 IS NULL OR kind = ?2) \
             ORDER BY folder = '', folder, section, id";
        let Ok(mut stmt) = self.conn.prepare(sql) else {
            return out;
        };
        if let Ok(rows) = stmt.query_map(params![path, kind], |r| annotation_at(r, 0)) {
            out.extend(rows.flatten());
        }
        out
    }

    /// Set (or clear, with an empty string) a note's commentary body.
    pub fn set_annotation_note(&self, id: i64, note: &str) {
        let _ = self.conn.execute(
            "UPDATE annotations SET note = ?2 WHERE id = ?1",
            params![id, note],
        );
    }

    /// Change a highlight's palette colour.
    pub fn set_annotation_color(&self, id: i64, color: i64) {
        let _ = self.conn.execute(
            "UPDATE annotations SET color = ?2 WHERE id = ?1",
            params![id, color],
        );
    }

    pub fn delete_annotation(&self, id: i64) {
        let _ = self
            .conn
            .execute("DELETE FROM annotations WHERE id = ?1", params![id]);
    }

    /// Set (or clear, with an empty string) a bookmark's custom name.
    pub fn set_annotation_name(&self, id: i64, name: &str) {
        let _ = self.conn.execute(
            "UPDATE annotations SET name = ?2 WHERE id = ?1",
            params![id, name],
        );
    }

    /// Move a bookmark into a folder (empty string = ungrouped).
    pub fn set_annotation_folder(&self, id: i64, folder: &str) {
        let _ = self.conn.execute(
            "UPDATE annotations SET folder = ?2 WHERE id = ?1",
            params![id, folder],
        );
    }

    /// Every bookmark with its book path (for `--export-annotations`).
    pub fn all_bookmarks(&self) -> Vec<(String, Annotation)> {
        let mut out = Vec::new();
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT path, id, section, quote, note, name, folder, kind, color FROM annotations \
             WHERE kind = ?1 ORDER BY path, folder = '', folder, section, id",
        ) else {
            return out;
        };
        if let Ok(rows) = stmt.query_map(params![KIND_BOOKMARK], |r| {
            Ok((r.get::<_, String>(0)?, annotation_at(r, 1)?))
        }) {
            out.extend(rows.flatten());
        }
        out
    }
}

/// Build an `Annotation` from eight consecutive columns starting at `base`:
/// `id, section, quote, note, name, folder, kind, color`.
fn annotation_at(r: &rusqlite::Row, base: usize) -> rusqlite::Result<Annotation> {
    Ok(Annotation {
        id: r.get(base)?,
        section: r.get::<_, i64>(base + 1)?.max(0) as usize,
        quote: r.get(base + 2)?,
        note: r.get(base + 3)?,
        name: r.get(base + 4)?,
        folder: r.get(base + 5)?,
        kind: r.get(base + 6)?,
        color: r.get(base + 7)?,
    })
}
