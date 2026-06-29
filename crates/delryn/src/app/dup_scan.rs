//! Thorough duplicate scan: a user-triggered pass that perceptually hashes every
//! book's cover on a worker thread, then links books whose covers match. This
//! catches duplicates the metadata pass can't — chiefly PDFs, which carry little
//! or no usable title/author/ISBN but do have a cover (their rendered first page).
//!
//! It's deliberately *off the default path*: the metadata grouping runs on every
//! refresh, but cover hashing means decoding (and, for PDFs, rasterizing) an image
//! per book, so it only runs when the reader asks. The worker computes pure data
//! and streams progress + results back; the main thread persists the discovered
//! links (the DB connection isn't `Send`) and the existing grouping folds them in.

use std::sync::mpsc::{Receiver, TryRecvError};

use super::App;

/// Cover dHashes within this many bits are treated as the same cover. Permissive
/// by design — it generates *candidates* the reader confirms in the overlay, so we
/// favour recall (catching cross-format twins through recompression/scaling noise)
/// over precision.
const COVER_HAMMING_MAX: u32 = 8;

/// State of an in-flight cover scan. `done`/`total` drive the progress message;
/// `hashes` accumulates each book's cover fingerprint as the worker reports it.
pub struct DupScan {
    rx: Receiver<(String, Option<u64>)>,
    total: usize,
    done: usize,
    hashes: Vec<(String, u64)>,
}

impl App {
    /// Kick off a thorough cover scan over the whole library. No-op (with a flash)
    /// if one is already running or there are no books. The worker thread reads
    /// each cover (cache-first, so already-built covers cost only a decode) and
    /// sends back its perceptual hash; [`App::poll_dup_scan`] drains the results.
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
                let hash =
                    super::load_cover_bytes(&path).and_then(|b| crate::media::cover_dhash(&b));
                // A send error means the receiver was dropped (scan abandoned); stop.
                if tx.send((path, hash)).is_err() {
                    break;
                }
            }
            // Dropping `tx` here disconnects the channel — the signal that the scan
            // has finished (see the `Disconnected` arm in `poll_dup_scan`).
        });
        self.lib_flash = Some(format!("scanning covers 0/{total}…"));
        self.dup_scan = Some(DupScan {
            rx,
            total,
            done: 0,
            hashes: Vec::with_capacity(total),
        });
    }

    /// True while a cover scan is running — keeps the main loop polling.
    pub fn dup_scan_pending(&self) -> bool {
        self.dup_scan.is_some()
    }

    /// Drain whatever the scan worker has produced. Updates the progress flash as
    /// hashes arrive and, once the worker finishes, persists the cover links and
    /// refreshes the library. Returns `true` if anything changed (request a redraw).
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
            self.lib_flash = Some(format!("scanning covers {}/{}…", scan.done, scan.total));
        }
        progressed
    }

    /// Compute the cover-match candidates from the collected hashes, persist them,
    /// and refresh so the Duplicates view reflects the new groups.
    fn finish_dup_scan(&mut self, scan: DupScan) {
        let pairs = crate::library::dedup::cover_link_candidates(&scan.hashes, COVER_HAMMING_MAX);
        if let Some(store) = &self.store {
            store.replace_cover_dup_links(&pairs);
        }
        self.refresh_library();
        self.lib_flash = Some(format!(
            "deep scan done: {} cover{} hashed, {} match{} — press D to resolve",
            scan.hashes.len(),
            if scan.hashes.len() == 1 { "" } else { "s" },
            pairs.len(),
            if pairs.len() == 1 { "" } else { "es" },
        ));
    }
}
