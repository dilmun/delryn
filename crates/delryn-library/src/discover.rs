//! Finding the folders where someone already keeps their books.
//!
//! Registering a library source means remembering a path and typing it exactly.
//! Most people keep books in two or three places — a Calibre folder, a papers
//! directory, a Downloads pile — and can't necessarily name them from memory.
//! This walks the home directory once and reports the folders that actually hold
//! books, with counts, so adding them is a matter of ticking a list.
//!
//! It *proposes*; it never adds. The walk is a heuristic over someone's entire
//! home directory, and quietly indexing a folder they didn't ask for is worse
//! than one keystroke of confirmation.
//!
//! The rule that keeps the result short: a folder is reported only when no
//! ancestor of it is. [`scan`](crate::scan) is recursive, so the topmost folder
//! already covers everything beneath it — a collection split across a dozen
//! genre subfolders collapses to the one row that stands for it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use delryn_format::BookFormat;

/// A proposed library folder and how many books its subtree holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Absolute path, in the form [`normalize_root`](crate::normalize_root)
    /// produces, so it dedupes against folders added by hand or from the CLI.
    pub path: String,
    /// Books found anywhere beneath it — what the proposal is judged on.
    pub books: usize,
}

/// Books a folder's subtree needs before it's worth proposing. One stray PDF in
/// `~/Documents` must not offer to index a whole working directory; a shelf of
/// them should.
const MIN_BOOKS: usize = 3;

/// How far below the search root to look. Collections sit a few levels down
/// (`~/Documents/Books/Fiction/…`); past that it's someone's source tree.
const MAX_DEPTH: usize = 6;

/// Directories read before the walk gives up, so a pathological home directory —
/// a huge monorepo, a network mount — can't turn a background search into a
/// permanent one. Generous: an ordinary home is a few thousand.
const VISIT_BUDGET: usize = 50_000;

/// Directory names never worth descending into: application data and build
/// trees, which are deep, large, and never someone's bookshelf. Hidden
/// directories (`.cache`, `.git`, `.local`) are skipped by their leading dot and
/// so need no entry here.
const SKIP: &[&str] = &[
    "Library", // macOS application support
    "Applications",
    "node_modules",
    "target",
    "vendor",
    "build",
    "dist",
    "__pycache__",
];

/// The home directory to search, if the environment names one.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Walk `root` and return the folders worth adding as library sources, the
/// fullest first.
///
/// `existing` is the already-configured source list: anything at or beneath one
/// is left unread, so a second run proposes only what's new. `root` itself is
/// never proposed — it's the home directory, and indexing a whole home is not
/// what anyone means by "find my books".
pub fn find_book_folders(root: &Path, existing: &[String]) -> Vec<Found> {
    let mut walk = Walk {
        existing: existing.iter().map(PathBuf::from).collect(),
        hits: Vec::new(),
        budget: VISIT_BUDGET,
    };
    walk.visit(root, 0);
    let mut found = topmost(walk.hits);
    // Fullest first: the folder with 400 books is the one being looked for, and
    // the tie-break by path keeps the order stable between runs.
    found.sort_by(|a, b| b.books.cmp(&a.books).then_with(|| a.path.cmp(&b.path)));
    found
}

/// One walk's state: what to avoid, what's been found, and how much budget is left.
struct Walk {
    existing: Vec<PathBuf>,
    hits: Vec<Found>,
    budget: usize,
}

impl Walk {
    /// Count the books under `dir`, recording it as a hit when there are enough.
    /// Returns that count so the caller folds it into its own.
    fn visit(&mut self, dir: &Path, depth: usize) -> usize {
        if self.budget == 0 {
            return 0;
        }
        self.budget -= 1;
        let Ok(entries) = std::fs::read_dir(dir) else {
            // Unreadable — permissions, or a mount that went away. Nothing to
            // propose here, and no reason to fail the rest of the walk.
            return 0;
        };
        let mut books = 0;
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            // Symlinks are followed by neither this walk nor `scan`: they can
            // form cycles, and a link into an already-counted tree doubles it.
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                if depth < MAX_DEPTH && !self.skip(&entry.file_name(), &path) {
                    books += self.visit(&path, depth + 1);
                }
            } else if BookFormat::from_path(&path).is_readable() {
                books += 1;
            }
        }
        // The root stands for the search itself, not for a shelf inside it.
        if depth > 0 && books >= MIN_BOOKS {
            self.hits.push(Found {
                path: dir.to_string_lossy().into_owned(),
                books,
            });
        }
        books
    }

    /// Whether to leave a directory unread: hidden, a known application or build
    /// tree, or already covered by a configured source.
    fn skip(&self, name: &OsStr, path: &Path) -> bool {
        let name = name.to_string_lossy();
        name.starts_with('.')
            || SKIP.contains(&name.as_ref())
            || self.existing.iter().any(|e| path.starts_with(e))
    }
}

/// Keep only the folders with no reported ancestor. Scanning is recursive, so an
/// ancestor already covers everything below it and listing both is noise.
fn topmost(mut hits: Vec<Found>) -> Vec<Found> {
    // An ancestor's path is a strict prefix and therefore shorter, so shortest
    // first puts every ancestor ahead of the descendants it subsumes.
    hits.sort_by_key(|f| f.path.len());
    let mut kept: Vec<Found> = Vec::new();
    for found in hits {
        if kept
            .iter()
            .any(|k| Path::new(&found.path).starts_with(&k.path))
        {
            continue;
        }
        kept.push(found);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private tree under the temp dir; the walk only ever reads, so nothing
    /// here can touch the real home directory.
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("delryn_discover_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `n` book files in `dir` (created along with it).
    fn shelve(dir: &Path, n: usize) {
        std::fs::create_dir_all(dir).unwrap();
        for i in 0..n {
            std::fs::write(dir.join(format!("book{i}.epub")), b"x").unwrap();
        }
    }

    fn paths(found: &[Found]) -> Vec<&str> {
        found.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn a_shelf_is_proposed_with_its_count() {
        let root = scratch("shelf");
        shelve(&root.join("Books"), 4);

        let found = find_book_folders(&root, &[]);

        assert_eq!(paths(&found), vec![root.join("Books").to_str().unwrap()]);
        assert_eq!(found[0].books, 4);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A collection split across genre subfolders is one proposal, not five —
    /// adding the parent scans all of it.
    #[test]
    fn only_the_topmost_folder_of_a_collection_is_proposed() {
        let root = scratch("topmost");
        shelve(&root.join("Books/Fiction"), 3);
        shelve(&root.join("Books/Tech/Rust"), 5);

        let found = find_book_folders(&root, &[]);

        assert_eq!(paths(&found), vec![root.join("Books").to_str().unwrap()]);
        assert_eq!(found[0].books, 8, "the count covers the whole subtree");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The reason for a threshold: one datasheet in a working directory must not
    /// offer to index the working directory.
    #[test]
    fn a_stray_book_does_not_drag_in_a_working_directory() {
        let root = scratch("stray");
        shelve(&root.join("Work/Project"), 1);

        assert!(find_book_folders(&root, &[]).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Run it twice and the second run has nothing to say.
    #[test]
    fn folders_already_configured_are_not_proposed_again() {
        let root = scratch("existing");
        shelve(&root.join("Books"), 4);
        shelve(&root.join("Papers"), 3);

        let existing = vec![root.join("Books").to_string_lossy().into_owned()];
        let found = find_book_folders(&root, &existing);

        assert_eq!(paths(&found), vec![root.join("Papers").to_str().unwrap()]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hidden_and_build_directories_are_never_read() {
        let root = scratch("skipped");
        shelve(&root.join(".cache/covers"), 5);
        shelve(&root.join("proj/node_modules/pkg/docs"), 5);
        shelve(&root.join("proj/target/doc"), 5);

        assert!(find_book_folders(&root, &[]).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Proposing the search root would mean offering to index a whole home
    /// directory — the one answer that's never useful.
    #[test]
    fn the_search_root_itself_is_never_proposed() {
        let root = scratch("root");
        shelve(&root, 6);

        assert!(find_book_folders(&root, &[]).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }
}
