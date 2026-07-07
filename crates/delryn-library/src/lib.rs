//! Library scanning: walk configured directories for recognized book files and
//! index their metadata into the store. Cheap-incremental — unchanged files are
//! skipped. EPUBs are read for full metadata; other recognized formats (PDF,
//! MOBI, AZW3) are indexed by filename so they appear in the library, pending a
//! reader backend (see the Phase 5 plan in `TODO.md`). See `DESIGN.md` §5.

use std::path::Path;
use std::time::UNIX_EPOCH;

use delryn_format::{BookFormat, epub, mobi};
use delryn_store::Store;

pub mod dedup;
pub mod export;
pub mod fuzzy;
pub mod query;
pub mod stats;

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

/// Normalize a user-supplied library folder path into the canonical form stored
/// in `library_paths`: expand a leading `~` (the in-app input has no shell to do
/// it), then canonicalize to an absolute path — falling back to the expanded
/// input when the folder can't be resolved (e.g. an offline drive). Keeping this
/// in one place means the CLI and the in-app Sources manager store paths
/// identically, so dedupe (`contains`) works.
pub fn normalize_root(input: &str) -> String {
    let expanded = if input == "~" {
        std::env::var("HOME").unwrap_or_else(|_| input.to_string())
    } else if let Some(rest) = input.strip_prefix("~/") {
        std::env::var("HOME")
            .map(|home| format!("{home}/{rest}"))
            .unwrap_or_else(|_| input.to_string())
    } else {
        input.to_string()
    };
    std::fs::canonicalize(&expanded)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(expanded)
}

/// Drop every indexed book that lives under `root` — used when a library source
/// folder is removed so its books leave the library instead of lingering as
/// dead entries. Returns how many were removed. (Files on disk are untouched;
/// this only forgets them.)
pub fn remove_root(root: &str, store: &Store) -> usize {
    let root = Path::new(root);
    let mut removed = 0;
    for path in store.all_book_paths() {
        if Path::new(&path).starts_with(root) {
            store.remove_book(&path);
            removed += 1;
        }
    }
    removed
}

/// Drop DB entries whose file no longer exists, so deleted/moved books don't
/// linger as dead, un-openable duplicates. A book is kept (not pruned) when it
/// lives under a configured root that's currently unreadable — e.g. an unmounted
/// drive — since the file may simply be offline rather than gone. Returns how
/// many entries were removed.
pub fn prune_missing(paths: &[String], store: &Store) -> usize {
    let mut removed = 0;
    for path in store.all_book_paths() {
        if Path::new(&path).exists() {
            continue;
        }
        let root_offline = paths
            .iter()
            .any(|r| Path::new(&path).starts_with(Path::new(r)) && !Path::new(r).is_dir());
        if root_offline {
            continue;
        }
        store.remove_book(&path);
        removed += 1;
    }
    removed
}

/// Build the full-text index for every known book. Returns how many were
/// indexed. Heavier than metadata scanning (parses every section), so it's a
/// separate, explicit step (`delryn --index`).
pub fn index_fulltext(store: &Store) -> usize {
    let mut n = 0;
    for path in store.all_book_paths() {
        if let Ok(text) = epub::read_fulltext(&path)
            && store.index_text(&path, &text).is_ok()
        {
            n += 1;
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
        } else if BookFormat::from_path(&path) != BookFormat::Unknown
            && index_book(&path, store, force)
        {
            indexed += 1;
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

    match BookFormat::from_path(path) {
        BookFormat::Epub => index_meta(epub::read_metadata(path), path_str, size, mtime, store),
        BookFormat::Mobi | BookFormat::Azw3 => {
            index_meta(mobi::read_metadata(path), path_str, size, mtime, store)
        }
        // PDF is page-image only (no embedded text metadata to read): index by
        // filename so the book is visible/organizable in the library.
        BookFormat::Pdf => {
            let title = title_from_filename(path);
            let _ = store.upsert_book(
                path_str, &title, "", None, size, 0, mtime, "", None, "", "", "", "",
            );
            store.set_converted(path_str, false);
            true
        }
        BookFormat::Unknown => false,
    }
}

/// Index a book from its parsed metadata (shared by EPUB and MOBI/AZW3). A parse
/// failure leaves the book un-indexed (a later rescan retries).
fn index_meta(
    parsed: anyhow::Result<(delryn_format::Metadata, usize)>,
    path_str: &str,
    size: u64,
    mtime: i64,
    store: &Store,
) -> bool {
    let Ok((book, sections)) = parsed else {
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

/// A readable title from a file's stem, for formats we can't yet read metadata
/// from: drop the extension and turn separators into spaces.
fn title_from_filename(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled");
    let title = stem.replace(['_', '.'], " ");
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_root, prune_missing, remove_root, scan, title_from_filename};
    use delryn_store::Store;
    use std::path::Path;

    fn upsert(store: &Store, path: &str) {
        store
            .upsert_book(path, "T", "A", None, 1, 1, 1, "", None, "", "", "", "")
            .unwrap();
    }

    #[test]
    fn prune_removes_missing_keeps_present() {
        let _env = delryn_infra::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_prune_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let books = tmp.join("books");
        std::fs::create_dir_all(&books).unwrap();
        let real = books.join("real.epub");
        std::fs::write(&real, b"x").unwrap();
        let gone = books.join("gone.epub");

        let store = Store::open_default().unwrap();
        upsert(&store, &real.to_string_lossy());
        upsert(&store, &gone.to_string_lossy());

        let roots = vec![books.to_string_lossy().into_owned()];
        assert_eq!(prune_missing(&roots, &store), 1);
        let paths = store.all_book_paths();
        assert!(
            paths.iter().any(|p| p.ends_with("real.epub")),
            "present kept"
        );
        assert!(
            !paths.iter().any(|p| p.ends_with("gone.epub")),
            "missing pruned"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prune_skips_offline_root() {
        let _env = delryn_infra::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_prune2_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        std::fs::create_dir_all(&tmp).unwrap();

        let store = Store::open_default().unwrap();
        // A book under a root that doesn't currently exist (e.g. an unmounted
        // drive) is kept, not pruned.
        let offline = "/delryn_nonexistent_mount/sub/book.epub";
        upsert(&store, offline);
        assert_eq!(
            prune_missing(&["/delryn_nonexistent_mount".into()], &store),
            0
        );
        assert!(store.all_book_paths().iter().any(|p| p == offline));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn remove_root_drops_only_books_under_it() {
        let _env = delryn_infra::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_rmroot_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        std::fs::create_dir_all(&tmp).unwrap();

        let store = Store::open_default().unwrap();
        upsert(&store, "/lib_a/one.epub");
        upsert(&store, "/lib_a/sub/two.epub");
        upsert(&store, "/lib_b/three.epub");

        assert_eq!(
            remove_root("/lib_a", &store),
            2,
            "both under /lib_a removed"
        );
        let paths = store.all_book_paths();
        assert_eq!(paths, vec!["/lib_b/three.epub".to_string()], "sibling kept");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn normalize_root_falls_back_when_unresolvable() {
        // A path that can't be canonicalized (doesn't exist) is returned as-is,
        // so `--add`ing an offline drive still registers it.
        assert_eq!(
            normalize_root("/delryn_nonexistent_mount/books"),
            "/delryn_nonexistent_mount/books"
        );
    }

    #[test]
    fn title_from_filename_cleans_separators() {
        assert_eq!(
            title_from_filename(Path::new("/x/The_Selfish_Gene.pdf")),
            "The Selfish Gene"
        );
        assert_eq!(
            title_from_filename(Path::new("dune.part.one.mobi")),
            "dune part one"
        );
        assert_eq!(title_from_filename(Path::new("a.azw3")), "a");
    }

    #[test]
    fn scan_indexes_non_epub_by_filename() {
        let _env = delryn_infra::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_scanfmt_{}", std::process::id()));
        // SAFETY: serialized by `_env`; scopes the config dir to this process.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let books = tmp.join("books");
        std::fs::create_dir_all(&books).unwrap();
        // A PDF (recognized, not yet readable) and a non-book file (ignored).
        std::fs::write(books.join("Great_Paper.pdf"), b"%PDF-1.4").unwrap();
        std::fs::write(books.join("notes.txt"), b"hello").unwrap();

        let store = Store::open_default().unwrap();
        let roots = vec![books.to_string_lossy().into_owned()];
        let n = scan(&roots, &store);

        assert_eq!(n, 1, "only the PDF is indexed; the .txt is ignored");
        let paths = store.all_book_paths();
        assert!(paths.iter().any(|p| p.ends_with("Great_Paper.pdf")));
        assert!(!paths.iter().any(|p| p.ends_with("notes.txt")));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
