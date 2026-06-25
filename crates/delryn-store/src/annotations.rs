//! Bookmarks (and, from Phase 4, notes) anchored to content quotes. Rows carry a
//! `kind`: this module deals only with bookmarks (`kind = 0`); notes are layered
//! on later. All quotes are reflow-stable text anchors, not byte offsets.

use super::*;

/// `annotations.kind` value for a bookmark (vs a Phase 4 note).
const KIND_BOOKMARK: i64 = 0;

impl Store {
    /// Drop a bookmark at a reading position (anchored by its quote).
    pub fn add_bookmark(&self, path: &str, section: usize, quote: &str) {
        let _ = self.conn.execute(
            "INSERT INTO annotations (path, section, quote, kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![path, section as i64, quote, KIND_BOOKMARK, now_secs()],
        );
    }

    /// Bookmarks for one book, grouped so each folder's entries are contiguous:
    /// named folders first (alphabetical), then ungrouped, each by reading order.
    pub fn list_bookmarks(&self, path: &str) -> Vec<Annotation> {
        let mut out = Vec::new();
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT id, section, quote, note, name, folder FROM annotations \
             WHERE path = ?1 AND kind = ?2 \
             ORDER BY folder = '', folder, section, id",
        ) else {
            return out;
        };
        if let Ok(rows) = stmt.query_map(params![path, KIND_BOOKMARK], |r| annotation_at(r, 0)) {
            out.extend(rows.flatten());
        }
        out
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
            "SELECT path, id, section, quote, note, name, folder FROM annotations \
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

/// Build an `Annotation` from six consecutive columns starting at `base`:
/// `id, section, quote, note, name, folder`.
fn annotation_at(r: &rusqlite::Row, base: usize) -> rusqlite::Result<Annotation> {
    Ok(Annotation {
        id: r.get(base)?,
        section: r.get::<_, i64>(base + 1)?.max(0) as usize,
        quote: r.get(base + 2)?,
        note: r.get(base + 3)?,
        name: r.get(base + 4)?,
        folder: r.get(base + 5)?,
    })
}
