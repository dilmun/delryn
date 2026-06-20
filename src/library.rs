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
        indexed += scan_dir(Path::new(root), store);
    }
    indexed
}

fn scan_dir(dir: &Path, store: &Store) -> usize {
    let mut indexed = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            indexed += scan_dir(&path, store);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("epub")) {
            if index_book(&path, store) {
                indexed += 1;
            }
        }
    }
    indexed
}

fn index_book(path: &Path, store: &Store) -> bool {
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

    if !store.needs_scan(path_str, mtime, size) {
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
    );
    true
}
