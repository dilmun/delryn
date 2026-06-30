//! Thorough duplicate scan: a user-triggered pass that reads each book's
//! table-of-contents — the chapter titles — from its own structure, and links books
//! whose chapter lists match (see `dedup::content_link_candidates`). This is
//! content, not metadata: the TOC is distinctive, already-clean text (no page
//! numbers, images, or symbols) and reads the same work across formats, so it
//! matches every combination (EPUB↔EPUB, PDF↔PDF, PDF↔EPUB) without colliding on
//! shared topics or publisher templates.
//!
//! It scans *every* book (not just those the metadata tiers missed): the grouping
//! unions all signals — ISBN, title+author, and these TOC links — so a content
//! match can still join, say, a metadata-less third copy to an already-paired book.
//!
//! Per format: an EPUB's TOC is its navigation document; a PDF's is its bookmark
//! outline (`epub`/`pdf::toc_labels`). A book with no real TOC — a PDF with no
//! bookmarks, a bare EPUB — yields no labels and is skipped.
//!
//! It runs off the default path because reading the TOC means opening each file.
//! The worker computes pure data and streams progress + results back; the main
//! thread persists the links (the DB connection isn't `Send`) and the existing
//! grouping folds them in.

use std::sync::mpsc::{Receiver, TryRecvError};

use super::App;
use crate::library::dedup::ContentId;

/// A worker result: the book's path and its table-of-contents chapter labels
/// (empty when the book has no usable TOC).
type IdResult = (String, Vec<String>);

/// State of an in-flight content scan. `done`/`total` drive the progress message;
/// `ids` accumulates each book's table-of-contents as the worker reports it.
pub struct DupScan {
    rx: Receiver<IdResult>,
    total: usize,
    done: usize,
    ids: Vec<ContentId>,
}

impl App {
    /// Kick off a thorough content scan over *every* book. The exact-metadata tiers
    /// already group what they can on each refresh, but the content scan still reads
    /// every book so a TOC match can join copies metadata missed — the grouping
    /// unions all signals, so no tier's matches are lost. No-op (with a flash) if
    /// one is already running or there are no books. The worker reads each book's
    /// table-of-contents and sends it back; [`App::poll_dup_scan`] drains it.
    pub(crate) fn start_dup_scan(&mut self) {
        if self.dup_scan.is_some() {
            return;
        }
        let Some(store) = &self.session.store else {
            return;
        };
        let paths: Vec<String> = store.all_books().into_iter().map(|b| b.path).collect();
        let total = paths.len();
        if total == 0 {
            self.library.flash = Some("no books to scan".into());
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
        self.library.flash = Some(format!("reading contents 0/{total}…"));
        self.dup_scan = Some(DupScan {
            rx,
            total,
            done: 0,
            ids: Vec::with_capacity(total),
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
            self.library.flash = Some(format!("reading contents {}/{}…", scan.done, scan.total));
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
        if let Some(store) = &self.session.store {
            store.replace_scan_dup_links(&pairs);
        }
        self.refresh_library();
        self.library.flash = Some(format!(
            "deep scan done: read {}/{} contents, {} content match{} — press D to resolve",
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
