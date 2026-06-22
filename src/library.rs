//! Library scanning: walk configured directories for EPUBs and index their
//! metadata into the store. Cheap-incremental — unchanged files are skipped.
//! See `DESIGN.md` §5.

use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::document::epub;
use crate::store::Store;

/// Scan the given roots recursively, indexing new/changed EPUBs. Returns the
/// number of books (re)indexed.
pub fn scan(paths: &[String], store: &Store) -> usize {
    let mut indexed = 0;
    for root in paths {
        indexed += scan_dir(Path::new(root), store, false);
    }
    indexed
}

/// Like [`scan`], but re-reads every EPUB regardless of the change-detection
/// cache. Use to backfill metadata for already-indexed books after a schema
/// change (e.g. series/publisher). Hand-edited rows keep their values.
pub fn rescan(paths: &[String], store: &Store) -> usize {
    let mut indexed = 0;
    for root in paths {
        indexed += scan_dir(Path::new(root), store, true);
    }
    indexed
}

/// Build the full-text index for every known book. Returns how many were
/// indexed. Heavier than metadata scanning (parses every section), so it's a
/// separate, explicit step (`delryn --index`).
pub fn index_fulltext(store: &Store) -> usize {
    let mut n = 0;
    for path in store.all_book_paths() {
        if let Ok(text) = epub::read_fulltext(&path) {
            if store.index_text(&path, &text).is_ok() {
                n += 1;
            }
        }
    }
    n
}

fn scan_dir(dir: &Path, store: &Store, force: bool) -> usize {
    let mut indexed = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            indexed += scan_dir(&path, store, force);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub")) {
            if index_book(&path, store, force) {
                indexed += 1;
            }
        }
    }
    indexed
}

fn index_book(path: &Path, store: &Store, force: bool) -> bool {
    let Some(path_str) = path.to_str() else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let size = meta.len();

    if !force && !store.needs_scan(path_str, mtime, size) {
        return false;
    }
    let Ok((book, sections)) = epub::read_metadata(path) else {
        return false;
    };
    let author = book.author_line();
    let _ = store.upsert_book(
        path_str,
        &book.title,
        &author,
        book.year,
        size,
        sections,
        mtime,
        book.series.as_deref().unwrap_or(""),
        book.series_index,
        book.publisher.as_deref().unwrap_or(""),
        book.subtitle.as_deref().unwrap_or(""),
        book.identifier.as_deref().unwrap_or(""),
        book.language.as_deref().unwrap_or(""),
    );
    // A derived file fact — always refreshed, even on a hand-edited book.
    store.set_converted(path_str, book.converted);
    true
}
