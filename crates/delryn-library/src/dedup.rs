//! Duplicate detection across formats. Books are grouped by a normalized key —
//! ISBN when present, otherwise normalized title + first-author surname — so the
//! same work in two files (e.g. an EPUB and a converted copy, or two editions
//! with the same ISBN) lands in one group regardless of filename or extension.

use std::collections::{HashMap, HashSet};

use delryn_model::naming::{main_title, normalize_isbn};
use delryn_store::BookRow;

/// Groups of book indices that are duplicates of one another (each group has ≥2
/// members), ordered by first appearance.
pub fn duplicate_groups(books: &[BookRow]) -> Vec<Vec<usize>> {
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, b) in books.iter().enumerate() {
        if let Some(key) = dedup_key(b) {
            by_key.entry(key).or_default().push(i);
        }
    }
    let mut groups: Vec<Vec<usize>> = by_key.into_values().filter(|g| g.len() > 1).collect();
    groups.sort_by_key(|g| g[0]); // stable, document order
    groups
}

/// The set of paths that belong to some duplicate group.
pub fn duplicate_paths(books: &[BookRow]) -> HashSet<String> {
    duplicate_groups(books)
        .into_iter()
        .flatten()
        .map(|i| books[i].path.clone())
        .collect()
}

/// The grouping key for a book, or `None` if it has too little metadata to
/// match on (no ISBN and no title).
fn dedup_key(b: &BookRow) -> Option<String> {
    let isbn = normalize_isbn(&b.isbn).unwrap_or_default();
    if !isbn.is_empty() {
        return Some(format!("isbn:{isbn}"));
    }
    let title = norm_title(&b.title);
    if title.is_empty() {
        return None;
    }
    Some(format!("ta:{title}|{}", norm_author(&b.author)))
}

/// Normalized title: subtitle stripped, lowercased, punctuation removed, a
/// leading article dropped, whitespace collapsed.
fn norm_title(title: &str) -> String {
    let base = main_title(title).to_lowercase();
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    if matches!(words.first(), Some(&("the" | "a" | "an"))) && words.len() > 1 {
        words.remove(0);
    }
    words.join(" ")
}

/// Normalized first-author surname (lowercased alphanumerics), so "Frank
/// Herbert" and "Herbert, Frank" match. Falls back to the whole first author.
fn norm_author(author: &str) -> String {
    // First author from a list separated by `&`, `;`, or " and ".
    let first = author
        .split(['&', ';'])
        .next()
        .unwrap_or(author)
        .split(" and ")
        .next()
        .unwrap_or(author)
        .trim();
    let surname = if let Some((last, _)) = first.split_once(',') {
        last.trim() // "Herbert, Frank" → "Herbert"
    } else {
        first.split_whitespace().last().unwrap_or(first) // "Frank Herbert" → "Herbert"
    };
    surname
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(path: &str, title: &str, author: &str, isbn: &str) -> BookRow {
        BookRow {
            path: path.into(),
            title: title.into(),
            author: author.into(),
            year: None,
            size: 0,
            favorite: false,
            pct: 0,
            series: String::new(),
            series_index: None,
            publisher: String::new(),
            subtitle: String::new(),
            isbn: isbn.into(),
            language: String::new(),
            converted: false,
            rating: 0,
        }
    }

    #[test]
    fn matches_across_format_by_normalized_title_author() {
        let books = vec![
            book("/a/Dune.epub", "Dune", "Frank Herbert", ""),
            book("/b/dune.epub", "DUNE: A Novel", "Herbert, Frank", ""),
            book("/c/Other.epub", "Something Else", "Someone", ""),
        ];
        let groups = duplicate_groups(&books);
        assert_eq!(groups.len(), 1, "one duplicate group");
        assert_eq!(groups[0], vec![0, 1]);
        let paths = duplicate_paths(&books);
        assert!(paths.contains("/a/Dune.epub") && paths.contains("/b/dune.epub"));
        assert!(!paths.contains("/c/Other.epub"));
    }

    #[test]
    fn isbn_takes_precedence_over_title() {
        // Same ISBN, different displayed title → still duplicates.
        let books = vec![
            book("/a.epub", "The Art of War", "Sun Tzu", "978-0-14-045991-9"),
            book(
                "/b.epub",
                "Art of War (Annotated)",
                "Tzu, Sun",
                "9780140459919",
            ),
        ];
        assert_eq!(duplicate_groups(&books).len(), 1);
    }

    #[test]
    fn leading_article_and_subtitle_ignored() {
        let books = vec![
            book("/a.epub", "The Rust Programming Language", "Klabnik", ""),
            book(
                "/b.epub",
                "Rust Programming Language: 2nd Ed",
                "Klabnik",
                "",
            ),
        ];
        assert_eq!(duplicate_groups(&books).len(), 1);
    }

    #[test]
    fn distinct_books_are_not_grouped() {
        let books = vec![
            book("/a.epub", "Book One", "Author A", ""),
            book("/b.epub", "Book Two", "Author B", ""),
        ];
        assert!(duplicate_groups(&books).is_empty());
        assert!(duplicate_paths(&books).is_empty());
    }
}
