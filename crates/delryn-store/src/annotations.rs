//! Annotations (bookmarks/notes) anchored to content quotes.

use super::*;

impl Store {
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
}
