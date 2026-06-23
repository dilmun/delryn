//! Full-text search index (FTS) over book body text.

use super::*;

impl Store {
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
            && let Ok(rows) = stmt.query_map(params![expr], |r| r.get::<_, String>(0))
        {
            out.extend(rows.flatten());
        }
        out
    }
}
