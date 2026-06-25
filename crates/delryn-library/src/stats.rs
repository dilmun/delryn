//! Library statistics — a pure summary over the indexed books plus the stored
//! total reading time. Used by the stats overlay; no I/O here so it's testable.

use delryn_store::BookRow;

/// Progress percent at/above which a book counts as finished (mirrors the query
/// module's reading-status threshold).
const FINISHED_PCT: u8 = 98;
/// How many top authors to surface.
const TOP_AUTHORS: usize = 5;

/// A snapshot of the library.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryStats {
    pub total: usize,
    pub finished: usize,
    pub reading: usize,
    pub unread: usize,
    pub favorites: usize,
    pub rated: usize,
    /// Mean rating over *rated* books (0.0 when none are rated).
    pub avg_rating: f32,
    /// Total reading time across the library, in seconds.
    pub read_seconds: i64,
    /// `(author, book count)`, most-prolific first.
    pub top_authors: Vec<(String, usize)>,
}

/// Compute library statistics from the book rows and stored reading time.
pub fn compute(books: &[BookRow], read_seconds: i64) -> LibraryStats {
    let mut s = LibraryStats {
        total: books.len(),
        read_seconds,
        ..Default::default()
    };
    let mut rating_sum = 0u32;
    let mut authors: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for b in books {
        match b.pct {
            0 => s.unread += 1,
            p if p >= FINISHED_PCT => s.finished += 1,
            _ => s.reading += 1,
        }
        if b.favorite {
            s.favorites += 1;
        }
        if b.rating > 0 {
            s.rated += 1;
            rating_sum += u32::from(b.rating);
        }
        let author = b.author.trim();
        if !author.is_empty() {
            *authors.entry(author.to_string()).or_default() += 1;
        }
    }
    if s.rated > 0 {
        s.avg_rating = rating_sum as f32 / s.rated as f32;
    }
    let mut authors: Vec<(String, usize)> = authors.into_iter().collect();
    // Most books first; ties broken alphabetically for stable output.
    authors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    authors.truncate(TOP_AUTHORS);
    s.top_authors = authors;
    s
}

/// Format a duration in seconds as a compact human string (`12h 30m`, `45m`).
pub fn fmt_duration(secs: i64) -> String {
    let mins = secs / 60;
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(author: &str, pct: u8, fav: bool, rating: u8) -> BookRow {
        BookRow {
            path: format!("/{author}-{pct}-{rating}"),
            title: "T".into(),
            author: author.into(),
            year: None,
            size: 0,
            favorite: fav,
            pct,
            series: String::new(),
            series_index: None,
            publisher: String::new(),
            subtitle: String::new(),
            isbn: String::new(),
            language: String::new(),
            converted: false,
            rating,
            status: String::new(),
        }
    }

    #[test]
    fn counts_statuses_favorites_and_ratings() {
        let books = vec![
            book("Knuth", 0, false, 0), // unread
            book("Knuth", 50, true, 4), // reading, fav, rated 4
            book("Tzu", 100, false, 5), // finished, rated 5
            book("Tzu", 99, false, 0),  // finished
        ];
        let s = compute(&books, 3 * 3600 + 30 * 60);
        assert_eq!(s.total, 4);
        assert_eq!(s.unread, 1);
        assert_eq!(s.reading, 1);
        assert_eq!(s.finished, 2);
        assert_eq!(s.favorites, 1);
        assert_eq!(s.rated, 2);
        assert!((s.avg_rating - 4.5).abs() < f32::EPSILON);
        assert_eq!(s.read_seconds, 3 * 3600 + 30 * 60);
    }

    #[test]
    fn top_authors_ranked_by_count_then_name() {
        let books = vec![
            book("Knuth", 0, false, 0),
            book("Knuth", 0, false, 0),
            book("Abbott", 0, false, 0),
        ];
        let s = compute(&books, 0);
        assert_eq!(s.top_authors[0], ("Knuth".into(), 2));
        assert_eq!(s.top_authors[1], ("Abbott".into(), 1));
    }

    #[test]
    fn duration_formats() {
        assert_eq!(fmt_duration(3 * 3600 + 30 * 60), "3h 30m");
        assert_eq!(fmt_duration(45 * 60), "45m");
        assert_eq!(fmt_duration(0), "0m");
    }

    #[test]
    fn empty_library() {
        let s = compute(&[], 0);
        assert_eq!(s.total, 0);
        assert_eq!(s.avg_rating, 0.0);
        assert!(s.top_authors.is_empty());
    }
}
