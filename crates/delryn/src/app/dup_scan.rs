//! Thorough duplicate scan: a user-triggered pass that reads each book's
//! table-of-contents — the chapter titles — from its own structure, and links books
//! whose chapter lists match (see `dedup::content_link_candidates`). This is
//! content, not metadata: the TOC is distinctive, already-clean text (no page
//! numbers, images, or symbols) and reads the same work across formats, so it
//! matches every combination (EPUB↔EPUB, PDF↔PDF, PDF↔EPUB) without colliding on
//! shared topics or publisher templates.
//!
//! Per format: an EPUB's TOC is its navigation document; a PDF's is its bookmark
//! outline (`epub`/`pdf::toc_labels`). A book with no real TOC — a PDF with no
//! bookmarks, a bare EPUB — yields no labels and is skipped.
//!
//! It runs off the default path because reading the TOC means opening each file.
//! The worker computes pure data and streams progress + results back; the main
//! thread persists the links (the DB connection isn't `Send`) and the existing
//! grouping folds them in.

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, TryRecvError};

use super::App;
use crate::library::dedup::ContentId;

/// A worker result: the book's path and its table-of-contents chapter labels
/// (empty when the book has no usable TOC).
type IdResult = (String, Vec<String>);

/// State of an in-flight content scan. `done`/`total` drive the progress message;
/// `ids` accumulates each book's table-of-contents as the worker reports it.
/// `skipped` is the count already matched by metadata (not scanned).
pub struct DupScan {
    rx: Receiver<IdResult>,
    total: usize,
    done: usize,
    ids: Vec<ContentId>,
    skipped: usize,
}

impl App {
    /// Kick off a thorough content scan. The cheap exact-metadata tiers (ISBN, then
    /// title + any author) already group what they can on every refresh, so the
    /// content scan only opens files for the *leftovers* — books metadata couldn't
    /// match. No-op (with a flash) if one is already running or there are no books.
    /// The worker reads each remaining book's table-of-contents and sends it back;
    /// [`App::poll_dup_scan`] drains the results.
    pub(crate) fn start_dup_scan(&mut self) {
        if self.dup_scan.is_some() {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        let all = store.all_books();
        if all.is_empty() {
            self.lib_flash = Some("no books to scan".into());
            return;
        }
        // Tier 1+2 (exact metadata): books already in a metadata duplicate group are
        // resolved — skip opening their files.
        let grouped: HashSet<String> = crate::library::dedup::duplicate_groups(&all)
            .into_iter()
            .flatten()
            .map(|i| all[i].path.clone())
            .collect();
        let skipped = grouped.len();
        let paths: Vec<String> = all
            .into_iter()
            .map(|b| b.path)
            .filter(|p| !grouped.contains(p))
            .collect();
        let total = paths.len();
        if total == 0 {
            // Everything is matched by metadata; drop any stale content links.
            if let Some(store) = &self.store {
                store.replace_scan_dup_links(&[]);
            }
            self.refresh_library();
            self.lib_flash = Some(format!(
                "{skipped} matched by metadata — no content scan needed"
            ));
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for path in paths {
                let labels = extract_toc(&path);
                // A send error means the receiver was dropped (scan abandoned); stop.
                if tx.send((path, labels)).is_err() {
                    break;
                }
            }
            // Dropping `tx` here disconnects the channel — the signal that the scan
            // has finished (see the `Disconnected` arm in `poll_dup_scan`).
        });
        self.lib_flash = Some(format!("reading contents 0/{total}…"));
        self.dup_scan = Some(DupScan {
            rx,
            total,
            done: 0,
            ids: Vec::with_capacity(total),
            skipped,
        });
    }

    /// True while a content scan is running — keeps the main loop polling.
    pub fn dup_scan_pending(&self) -> bool {
        self.dup_scan.is_some()
    }

    /// Drain whatever the scan worker has produced. Updates the progress flash as
    /// identities arrive and, once the worker finishes, persists the content links
    /// and refreshes. Returns `true` if anything changed (request a redraw).
    pub fn poll_dup_scan(&mut self) -> bool {
        let Some(scan) = self.dup_scan.as_mut() else {
            return false;
        };
        let mut progressed = false;
        let mut finished = false;
        loop {
            match scan.rx.try_recv() {
                Ok((path, toc_labels)) => {
                    scan.done += 1;
                    scan.ids.push(ContentId { path, toc_labels });
                    progressed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            let scan = self.dup_scan.take().expect("scan present");
            self.finish_dup_scan(scan);
            return true;
        }
        if progressed {
            self.lib_flash = Some(format!("reading contents {}/{}…", scan.done, scan.total));
        }
        progressed
    }

    /// Pair up the collected tables of contents, persist the links, and refresh so
    /// the Duplicates view reflects the new groups.
    fn finish_dup_scan(&mut self, scan: DupScan) {
        let with_toc = scan
            .ids
            .iter()
            .filter(|id| !id.toc_labels.is_empty())
            .count();
        let pairs = crate::library::dedup::content_link_candidates(&scan.ids);
        if let Some(store) = &self.store {
            store.replace_scan_dup_links(&pairs);
        }
        self.refresh_library();
        self.lib_flash = Some(format!(
            "deep scan done: {} matched by metadata, read {}/{} contents, {} more match{} — D to resolve",
            scan.skipped,
            with_toc,
            scan.total,
            pairs.len(),
            if pairs.len() == 1 { "" } else { "es" },
        ));
    }
}

/// Read a book's table-of-contents chapter labels from its own structure (EPUB nav
/// or PDF bookmarks), dispatching by file type. Empty for unsupported types or a
/// book with no usable TOC.
fn extract_toc(path: &str) -> Vec<String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "epub" => crate::document::epub::toc_labels(path),
        "pdf" => crate::document::pdf::toc_labels(path),
        _ => Vec::new(),
    }
}
