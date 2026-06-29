//! Thorough duplicate scan: a user-triggered pass that fingerprints every book's
//! *content* on a worker thread, then links books whose text matches closely (see
//! `dedup::text_simhash` / `dedup::content_link_candidates`). Content is the ground
//! truth — independent of the messy titles, authors, and ISBNs the default pass
//! relies on — so this catches same-work files (notably EPUB↔PDF) whatever their
//! metadata says.
//!
//! Each book is sampled from the *front* — the printed title page, author,
//! copyright year, and table of contents (see `epub`/`pdf::extract_text_sample`),
//! which is where the book states its own identity and reads the same across
//! formats. That text is reduced to bare letters/digits and SimHashed. Image-only/
//! scanned PDFs (no text layer) yield no fingerprint and are skipped, never falsely
//! matched.
//!
//! It's deliberately *off the default path*: sampling means opening and decoding
//! each file, so it only runs when the reader asks. The worker computes pure data
//! and streams progress + results back; the main thread persists the discovered
//! links (the DB connection isn't `Send`) and the existing grouping folds them in.

use std::sync::mpsc::{Receiver, TryRecvError};

use super::App;

/// Plain-text budget sampled per book — a chapter or two, never the whole book.
const SAMPLE_CHARS: usize = 16_000;
/// Most PDF pages to pull text from (from the middle outward).
const PDF_SAMPLE_PAGES: usize = 10;

/// State of an in-flight content scan. `done`/`total` drive the progress message;
/// `hashes` accumulates each book's content fingerprint as the worker reports it
/// (books with too little text to fingerprint are counted but not stored).
pub struct DupScan {
    rx: Receiver<(String, Option<u64>)>,
    total: usize,
    done: usize,
    hashes: Vec<(String, u64)>,
}

impl App {
    /// Kick off a thorough content scan over the whole library. No-op (with a
    /// flash) if one is already running or there are no books. The worker thread
    /// samples each book's text and sends back its SimHash (or `None`);
    /// [`App::poll_dup_scan`] drains the results.
    pub(crate) fn start_dup_scan(&mut self) {
        if self.dup_scan.is_some() {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        let paths: Vec<String> = store.all_books().into_iter().map(|b| b.path).collect();
        let total = paths.len();
        if total == 0 {
            self.lib_flash = Some("no books to scan".into());
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for path in paths {
                let hash = sample_text(&path).and_then(|t| crate::library::dedup::text_simhash(&t));
                // A send error means the receiver was dropped (scan abandoned); stop.
                if tx.send((path, hash)).is_err() {
                    break;
                }
            }
            // Dropping `tx` here disconnects the channel — the signal that the scan
            // has finished (see the `Disconnected` arm in `poll_dup_scan`).
        });
        self.lib_flash = Some(format!("fingerprinting 0/{total}…"));
        self.dup_scan = Some(DupScan {
            rx,
            total,
            done: 0,
            hashes: Vec::with_capacity(total),
        });
    }

    /// True while a content scan is running — keeps the main loop polling.
    pub fn dup_scan_pending(&self) -> bool {
        self.dup_scan.is_some()
    }

    /// Drain whatever the scan worker has produced. Updates the progress flash as
    /// fingerprints arrive and, once the worker finishes, persists the content links
    /// and refreshes. Returns `true` if anything changed (request a redraw).
    pub fn poll_dup_scan(&mut self) -> bool {
        let Some(scan) = self.dup_scan.as_mut() else {
            return false;
        };
        let mut progressed = false;
        let mut finished = false;
        loop {
            match scan.rx.try_recv() {
                Ok((path, hash)) => {
                    scan.done += 1;
                    if let Some(h) = hash {
                        scan.hashes.push((path, h));
                    }
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
            self.lib_flash = Some(format!("fingerprinting {}/{}…", scan.done, scan.total));
        }
        progressed
    }

    /// Pair up the collected content fingerprints, persist the links, and refresh so
    /// the Duplicates view reflects the new groups.
    fn finish_dup_scan(&mut self, scan: DupScan) {
        let pairs = crate::library::dedup::content_link_candidates(
            &scan.hashes,
            crate::library::dedup::CONTENT_HAMMING_MAX,
        );
        if let Some(store) = &self.store {
            store.replace_scan_dup_links(&pairs);
        }
        self.refresh_library();
        self.lib_flash = Some(format!(
            "deep scan done: fingerprinted {}/{}, {} match{} — press D to resolve",
            scan.hashes.len(),
            scan.total,
            pairs.len(),
            if pairs.len() == 1 { "" } else { "es" },
        ));
    }
}

/// Sample a bounded chunk of a book's text for fingerprinting, dispatching by file
/// type. `None` for unsupported types or files with no extractable text.
fn sample_text(path: &str) -> Option<String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "epub" => crate::document::epub::extract_text_sample(path, SAMPLE_CHARS),
        "pdf" => crate::document::pdf::extract_text_sample(path, PDF_SAMPLE_PAGES, SAMPLE_CHARS),
        _ => None,
    }
}
