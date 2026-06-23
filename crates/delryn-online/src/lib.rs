//! Online book-metadata + cover lookup via Open Library — no API key, and the
//! only free source with structured series data. Blocking HTTP (`ureq`); call
//! from a worker thread, never the render path. See the `delryn-metadata-api`
//! reference for endpoints.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use serde::Deserialize;

/// Identify the app to Open Library for the higher rate tier (per their docs).
const USER_AGENT: &str = "delryn/0.1 (+https://github.com/dilmun/delryn)";
const SEARCH_URL: &str = "https://openlibrary.org/search.json";
/// Fields requested from the search endpoint (keeps the payload small).
const FIELDS: &str = "title,subtitle,author_name,first_publish_year,publisher,isbn,series_name,series_position,cover_i";

/// A search hit, normalized to delryn's editable metadata fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Candidate {
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub publisher: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f32>,
    pub isbn: Option<String>,
    pub cover_id: Option<i64>,
}

impl Candidate {
    /// Authors joined for display.
    pub fn author_line(&self) -> String {
        self.authors.join(", ")
    }

    /// URL of the large cover image, if one is known. `?default=false` makes the
    /// server 404 (rather than serve a blank placeholder) when there's no cover.
    pub fn cover_url(&self) -> Option<String> {
        if let Some(id) = self.cover_id {
            return Some(format!(
                "https://covers.openlibrary.org/b/id/{id}-L.jpg?default=false"
            ));
        }
        self.isbn
            .as_ref()
            .map(|isbn| format!("https://covers.openlibrary.org/b/isbn/{isbn}-L.jpg?default=false"))
    }
}

/// Free-text Open Library search (title + author together). Returns up to
/// `limit` candidates; empty on any network/parse error (callers degrade).
pub fn search(query: &str, limit: usize) -> Vec<Candidate> {
    let url = search_url(query, limit);
    let Ok(mut resp) = ureq::get(&url).header("User-Agent", USER_AGENT).call() else {
        return Vec::new();
    };
    match resp.body_mut().read_json::<SearchResp>() {
        Ok(r) => r.into_candidates(limit),
        Err(_) => Vec::new(),
    }
}

/// One candidate cover image from some source, for the cover picker. Carries a
/// human label (shown in the list) and the image URL (fetched on preview).
#[derive(Debug, Clone, PartialEq)]
pub struct CoverHit {
    pub source: String,
    pub url: String,
}

// ISBN normalization is a pure heuristic shared with the format/extract layers.
pub use delryn_model::naming::normalize_isbn;

/// Google Books cover-by-ISBN image (no API key, unlike the JSON API which is
/// rate-limited anonymously). `zoom` 1 is a reliable small thumbnail; higher
/// zooms are HD when the book has them, else a generic placeholder.
fn gb_cover_url(isbn: &str, zoom: u8) -> String {
    format!(
        "https://books.google.com/books/content?vid=ISBN{isbn}&printsec=frontcover&img=1&zoom={zoom}"
    )
}

/// Candidate covers for a book, gathered from several key-less sources. The
/// book's own ISBN (best for technical/academic titles, which Open Library has
/// no cover for) comes first, then covers from an Open Library title/author
/// search. Does one network call (the OL search); the cover URLs are fetched
/// later, on preview. Deduplicated by URL.
pub fn cover_candidates(query: &str, isbn_raw: &str, limit: usize) -> Vec<CoverHit> {
    let mut hits = Vec::new();
    if let Some(isbn) = normalize_isbn(isbn_raw) {
        hits.push(CoverHit {
            source: "Google Books".into(),
            url: gb_cover_url(&isbn, 1),
        });
        hits.push(CoverHit {
            source: "Google Books HD".into(),
            url: gb_cover_url(&isbn, 3),
        });
        hits.push(CoverHit {
            source: "Open Library".into(),
            url: format!("https://covers.openlibrary.org/b/isbn/{isbn}-L.jpg?default=false"),
        });
    }
    if !query.trim().is_empty() {
        for c in search(query, limit) {
            if let Some(id) = c.cover_id {
                hits.push(CoverHit {
                    source: format!("OL · {}", c.title),
                    url: format!("https://covers.openlibrary.org/b/id/{id}-L.jpg?default=false"),
                });
            } else if let Some(isbn) = c.isbn.as_deref().and_then(normalize_isbn) {
                hits.push(CoverHit {
                    source: format!("GB · {}", c.title),
                    url: gb_cover_url(&isbn, 1),
                });
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    hits.retain(|h| seen.insert(h.url.clone()));
    hits
}

/// Download cover image bytes. `None` on error or an implausibly small body
/// (a stray placeholder).
pub fn fetch_cover(url: &str) -> Option<Vec<u8>> {
    let mut resp = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .ok()?;
    let bytes = resp.body_mut().read_to_vec().ok()?;
    (bytes.len() > 256).then_some(bytes)
}

/// Where a fetched cover for `book_path` is cached (`<config>/covers/<hash>.jpg`).
/// Keyed by a hash of the path so it survives independent of the messy filename.
pub fn cover_cache_path(book_path: &str) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    book_path.hash(&mut h);
    delryn_infra::paths::config_dir()
        .join("covers")
        .join(format!("{:016x}.jpg", h.finish()))
}

/// Persist cover bytes to the cache, creating the directory as needed.
pub fn save_cover(book_path: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let path = cover_cache_path(book_path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn search_url(query: &str, limit: usize) -> String {
    format!(
        "{SEARCH_URL}?q={}&limit={limit}&fields={FIELDS}",
        enc(query)
    )
}

/// Minimal percent-encoding for query values (no extra dependency).
fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize)]
struct SearchResp {
    #[serde(default)]
    docs: Vec<Doc>,
}

impl SearchResp {
    fn into_candidates(self, limit: usize) -> Vec<Candidate> {
        self.docs
            .into_iter()
            .take(limit)
            .map(Doc::into_candidate)
            .collect()
    }
}

#[derive(Deserialize)]
struct Doc {
    #[serde(default)]
    title: String,
    #[serde(default)]
    subtitle: String,
    #[serde(default)]
    author_name: Vec<String>,
    first_publish_year: Option<i32>,
    #[serde(default)]
    publisher: Vec<String>,
    #[serde(default)]
    isbn: Vec<String>,
    #[serde(default)]
    series_name: Vec<String>,
    #[serde(default)]
    series_position: Vec<String>,
    cover_i: Option<i64>,
}

impl Doc {
    fn into_candidate(self) -> Candidate {
        let subtitle = self.subtitle.trim();
        Candidate {
            title: self.title,
            subtitle: (!subtitle.is_empty()).then(|| subtitle.to_string()),
            authors: self.author_name,
            year: self.first_publish_year,
            publisher: self.publisher.into_iter().next(),
            series: self.series_name.into_iter().next(),
            series_index: self
                .series_position
                .first()
                .and_then(|s| s.trim().parse().ok()),
            isbn: self.isbn.into_iter().next(),
            cover_id: self.cover_i,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a representative Open Library `search.json` payload.
    #[test]
    fn parses_search_response() {
        let json = r#"{
            "docs": [
                {
                    "title": "Dune",
                    "author_name": ["Frank Herbert"],
                    "first_publish_year": 1965,
                    "publisher": ["Chilton Books", "Ace"],
                    "isbn": ["9780441013593", "0441013597"],
                    "series_name": ["Dune"],
                    "series_position": ["1"],
                    "cover_i": 11481354
                },
                { "title": "Dune Messiah", "author_name": ["Frank Herbert"] }
            ]
        }"#;
        let resp: SearchResp = serde_json::from_str(json).unwrap();
        let cands = resp.into_candidates(5);
        assert_eq!(cands.len(), 2);

        let d = &cands[0];
        assert_eq!(d.title, "Dune");
        assert_eq!(d.author_line(), "Frank Herbert");
        assert_eq!(d.year, Some(1965));
        assert_eq!(d.publisher.as_deref(), Some("Chilton Books"));
        assert_eq!(d.series.as_deref(), Some("Dune"));
        assert_eq!(d.series_index, Some(1.0));
        assert_eq!(d.isbn.as_deref(), Some("9780441013593"));
        assert_eq!(
            d.cover_url().unwrap(),
            "https://covers.openlibrary.org/b/id/11481354-L.jpg?default=false"
        );

        // Sparse doc: missing fields degrade to None/empty, no cover.
        assert_eq!(cands[1].year, None);
        assert!(cands[1].cover_url().is_none());
    }

    #[test]
    fn isbn_yields_keyless_cover_sources() {
        // Empty query ⇒ no network; only the ISBN-direct sources.
        let hits = cover_candidates("", "urn:isbn:978-3-031-61037-0", 5);
        let urls: Vec<&str> = hits.iter().map(|h| h.url.as_str()).collect();
        assert!(
            urls.iter()
                .any(|u| u.contains("books.google.com") && u.contains("zoom=1"))
        );
        assert!(
            urls.iter()
                .any(|u| u.contains("books.google.com") && u.contains("zoom=3"))
        );
        assert!(
            urls.iter()
                .any(|u| u.contains("covers.openlibrary.org/b/isbn/9783031610370"))
        );
        // A non-ISBN identifier and empty query ⇒ nothing (no covers to offer).
        assert!(cover_candidates("", "calibre:255", 5).is_empty());
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(enc("the lord & co"), "the+lord+%26+co");
        let url = search_url("dune herbert", 5);
        assert!(url.contains("q=dune+herbert"));
        assert!(url.contains("limit=5"));
    }

    /// Live smoke test (network) — run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_search_dune() {
        let cands = search("dune frank herbert", 3);
        assert!(!cands.is_empty(), "expected live results");
        assert!(
            cands
                .iter()
                .any(|c| c.title.to_lowercase().contains("dune"))
        );
    }
}
