//! Collections (shelves): create, membership, rename/delete, and listing.

use super::*;

impl Store {
    /// Create an (initially empty) collection. Collections are first-class
    /// (a names table), so one can exist before any book is filed onto it.
    /// Idempotent; blank names ignored.
    pub fn create_collection(&self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let _ = self.conn.execute(
            "INSERT OR IGNORE INTO collections (name) VALUES (?1)",
            params![name],
        );
    }

    /// File a book onto a named collection (idempotent), creating the collection
    /// if it doesn't exist yet. Blank names are ignored.
    pub fn add_to_shelf(&self, path: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.create_collection(name);
        let _ = self.conn.execute(
            "INSERT OR IGNORE INTO shelves (path, name) VALUES (?1, ?2)",
            params![path, name],
        );
    }

    /// Remove a book from a collection. A collection with no books left simply
    /// stops appearing (membership is its only definition).
    pub fn remove_from_shelf(&self, path: &str, name: &str) {
        let _ = self.conn.execute(
            "DELETE FROM shelves WHERE path = ?1 AND name = ?2",
            params![path, name],
        );
    }

    /// Rename a collection across all its books. If `new` already exists, the
    /// two merge (books on both are deduped by the (path, name) primary key).
    /// A blank new name is ignored.
    pub fn rename_shelf(&self, old: &str, new: &str) {
        let new = new.trim();
        if new.is_empty() || new == old {
            return;
        }
        // OR IGNORE skips rows that would collide with an existing `new` entry;
        // the follow-up delete clears those now-merged leftovers.
        self.create_collection(new);
        let _ = self.conn.execute(
            "UPDATE OR IGNORE shelves SET name = ?2 WHERE name = ?1",
            params![old, new],
        );
        let _ = self
            .conn
            .execute("DELETE FROM shelves WHERE name = ?1", params![old]);
        let _ = self
            .conn
            .execute("DELETE FROM collections WHERE name = ?1", params![old]);
    }

    /// Delete a collection entirely (its name + every book's membership in it).
    pub fn delete_shelf(&self, name: &str) {
        let _ = self
            .conn
            .execute("DELETE FROM shelves WHERE name = ?1", params![name]);
        let _ = self
            .conn
            .execute("DELETE FROM collections WHERE name = ?1", params![name]);
    }

    /// Collection names a book belongs to, sorted.
    pub fn shelves_for(&self, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = self
            .conn
            .prepare("SELECT name FROM shelves WHERE path = ?1 ORDER BY name COLLATE NOCASE")
            && let Ok(rows) = stmt.query_map(params![path], |r| r.get::<_, String>(0))
        {
            out.extend(rows.flatten());
        }
        out
    }

    /// All collections with their book counts, sorted by name. Includes empty
    /// collections (count 0) so freshly-created ones still appear.
    pub fn all_shelves(&self) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT c.name, COUNT(s.path) FROM collections c \
             LEFT JOIN shelves s ON s.name = c.name \
             GROUP BY c.name ORDER BY c.name COLLATE NOCASE",
        ) && let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as usize))
        }) {
            out.extend(rows.flatten());
        }
        out
    }

    /// Books on a collection, ordered by title.
    pub fn books_in_shelf(&self, name: &str) -> Vec<BookRow> {
        self.query_books_sql(
            "JOIN shelves s ON s.path = b.path",
            "s.name = ?1",
            "b.title COLLATE NOCASE",
            params![name],
        )
    }
}
