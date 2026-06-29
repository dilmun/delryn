//! Book rows: scan/upsert, metadata edits, favorites, and list queries.

use super::*;

impl Store {
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

    // A book row is 13 descriptive/stat columns; the flat signature mirrors the
    // `books` table 1:1 and reads clearer than a param struct that would just
    // shadow the schema (documented scoped suppression).
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
        subtitle: &str,
        isbn: &str,
        language: &str,
    ) -> Result<()> {
        // On rescan, file stats (size/sections/mtime) always refresh, but the
        // descriptive fields are preserved when the user has hand-edited them
        // (`edited = 1`) so a re-index never clobbers manual corrections.
        self.conn.execute(
            "INSERT INTO books
                (path, title, author, year, size, sections, mtime, added_at,
                 series, series_index, publisher, subtitle, isbn, language)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(path) DO UPDATE SET
                title  = CASE WHEN edited = 1 THEN title  ELSE excluded.title  END,
                author = CASE WHEN edited = 1 THEN author ELSE excluded.author END,
                year   = CASE WHEN edited = 1 THEN year   ELSE excluded.year   END,
                series = CASE WHEN edited = 1 THEN series ELSE excluded.series END,
                series_index =
                    CASE WHEN edited = 1 THEN series_index ELSE excluded.series_index END,
                publisher =
                    CASE WHEN edited = 1 THEN publisher ELSE excluded.publisher END,
                subtitle = CASE WHEN edited = 1 THEN subtitle ELSE excluded.subtitle END,
                isbn     = CASE WHEN edited = 1 THEN isbn     ELSE excluded.isbn     END,
                language = CASE WHEN edited = 1 THEN language ELSE excluded.language END,
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
                subtitle,
                isbn,
                language,
            ],
        )?;
        Ok(())
    }

    /// Record whether a book's EPUB looks converted. A derived file fact, so it's
    /// set on every index (independent of the `edited` hand-edit guard).
    pub fn set_converted(&self, path: &str, converted: bool) {
        let _ = self.conn.execute(
            "UPDATE books SET converted = ?2 WHERE path = ?1",
            params![path, converted as i64],
        );
    }

    /// Overwrite a book's descriptive metadata with hand-edited values and mark
    /// it `edited` so a future rescan won't revert it (see `upsert_book`).
    // The editable book columns, 1:1 with the UPDATE below; a param struct would
    // only mirror the table schema (documented scoped suppression).
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
        subtitle: &str,
        isbn: &str,
        language: &str,
    ) {
        let _ = self.conn.execute(
            "UPDATE books SET title = ?2, author = ?3, year = ?4, series = ?5, \
             series_index = ?6, publisher = ?7, subtitle = ?8, isbn = ?9, \
             language = ?10, edited = 1 WHERE path = ?1",
            params![
                path,
                title,
                author,
                year,
                series,
                series_index,
                publisher,
                subtitle,
                isbn,
                language
            ],
        );
    }

    /// Repoint every per-book record from `old` to `new` after a file rename
    /// (books row + progress, annotations, and shelf memberships).
    pub fn rename_book_path(&self, old: &str, new: &str) {
        for sql in [
            "UPDATE books SET path = ?2 WHERE path = ?1",
            "UPDATE progress SET path = ?2 WHERE path = ?1",
            "UPDATE annotations SET path = ?2 WHERE path = ?1",
            "UPDATE shelves SET path = ?2 WHERE path = ?1",
        ] {
            let _ = self.conn.execute(sql, params![old, new]);
        }
    }

    /// Forget a book entirely — its row plus all path-keyed data (progress,
    /// annotations, shelf membership, full-text). Used to prune entries whose
    /// file no longer exists.
    pub fn remove_book(&self, path: &str) {
        for sql in [
            "DELETE FROM books WHERE path = ?1",
            "DELETE FROM progress WHERE path = ?1",
            "DELETE FROM annotations WHERE path = ?1",
            "DELETE FROM shelves WHERE path = ?1",
            "DELETE FROM fts WHERE path = ?1",
        ] {
            let _ = self.conn.execute(sql, params![path]);
        }
    }

    /// Set a book's user rating (clamped to 0..=5; 0 means unrated).
    pub fn set_rating(&self, path: &str, rating: u8) {
        let _ = self.conn.execute(
            "UPDATE books SET rating = ?2 WHERE path = ?1",
            params![path, rating.min(5)],
        );
    }

    /// Set a book's manual reading-status override (empty = derive from progress).
    pub fn set_status(&self, path: &str, status: &str) {
        let _ = self.conn.execute(
            "UPDATE books SET status = ?2 WHERE path = ?1",
            params![path, status],
        );
    }

    /// Set a book's tags (already normalised; empty clears them).
    pub fn set_tags(&self, path: &str, tags: &str) {
        let _ = self.conn.execute(
            "UPDATE books SET tags = ?2 WHERE path = ?1",
            params![path, tags],
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

    /// The SQL `WHERE` predicate selecting a section's books, over the standard
    /// `books b LEFT JOIN progress p` shape. Shared by `list_books` (rows) and
    /// `count_books` (totals) so the two never drift.
    fn section_where(section: LibrarySection) -> &'static str {
        match section {
            LibrarySection::Recent => "b.last_opened > 0",
            LibrarySection::All => "1 = 1",
            // Format filters, by file extension — auto-populated from the path.
            LibrarySection::Pdf => "LOWER(b.path) LIKE '%.pdf'",
            LibrarySection::Epub => "LOWER(b.path) LIKE '%.epub'",
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
        }
    }

    /// List books for a section (filtering by `query` substring is done by the
    /// caller).
    pub fn list_books(&self, section: LibrarySection) -> Vec<BookRow> {
        let where_clause = Self::section_where(section);
        let order = match section {
            // Within the Series view, sort by series then position then title.
            LibrarySection::Series => {
                "b.series COLLATE NOCASE, b.series_index, b.title COLLATE NOCASE"
            }
            LibrarySection::All
            | LibrarySection::Pdf
            | LibrarySection::Epub
            | LibrarySection::Favorites
            | LibrarySection::Duplicates => "b.title COLLATE NOCASE",
            _ => "b.last_opened DESC",
        };
        self.query_books(where_clause, order)
    }

    /// Number of books in a section (mirrors `list_books`' filter). The in-app
    /// cross-format Duplicates view is counted by the caller via `dedup`, so its
    /// SQL count here is title-only and unused by the sidebar.
    pub fn count_books(&self, section: LibrarySection) -> usize {
        let where_clause = Self::section_where(section);
        let sql = format!(
            "SELECT COUNT(*) FROM books b LEFT JOIN progress p ON p.path = b.path \
             WHERE {where_clause}"
        );
        self.conn
            .query_row(&sql, [], |r| r.get::<_, i64>(0))
            .unwrap_or(0)
            .max(0) as usize
    }

    /// All books, ordered by title (used for library-wide search).
    pub fn all_books(&self) -> Vec<BookRow> {
        self.query_books("1 = 1", "b.title COLLATE NOCASE")
    }

    pub fn all_book_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT path FROM books")
            && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0))
        {
            out.extend(rows.flatten());
        }
        out
    }
}
