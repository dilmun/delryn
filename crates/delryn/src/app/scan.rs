//! Background library scan: (re)indexing the configured source folders on a
//! worker thread, so a large scan never freezes the UI. The worker opens its own
//! store connection (SQLite in WAL mode — see [`crate::store::Store::open_default`]
//! — so it writes while the UI reads), scans, prunes, and reports the indexed
//! count back; the main loop refreshes on completion. Sibling of `dup_scan`.

use std::sync::mpsc::{Receiver, TryRecvError};

use super::{App, Mode};

/// An in-flight background scan: the worker's result channel and the label shown
/// while it runs (and, with the count, on completion).
pub struct ScanJob {
    rx: Receiver<usize>,
    label: String,
}

impl App {
    /// Spawn a background (re)scan of the configured library folders. `force`
    /// re-reads every book (a full rescan) instead of the incremental default;
    /// `prune_orphans` also drops books no longer under any configured folder.
    /// Dead-file pruning always runs. No-op if a scan is already in flight (the
    /// caller's config change still persists and is picked up next time).
    pub(crate) fn start_scan(&mut self, force: bool, prune_orphans: bool, label: String) {
        if self.scan.is_some() {
            return;
        }
        let paths = self.config.library_paths.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // A fresh connection for this thread (the UI's `Store` isn't shared);
            // WAL makes the concurrent write/read safe.
            let indexed = match crate::store::Store::open_default() {
                Ok(store) => {
                    let n = if force {
                        crate::library::rescan(&paths, &store)
                    } else {
                        crate::library::scan(&paths, &store)
                    };
                    crate::library::prune_missing(&paths, &store);
                    if prune_orphans {
                        crate::library::prune_outside_roots(&paths, &store);
                    }
                    n
                }
                Err(_) => 0,
            };
            // A send error just means the app closed mid-scan; nothing to do.
            let _ = tx.send(indexed);
        });
        self.library.flash = Some(format!("{label}…"));
        self.scan = Some(ScanJob { rx, label });
    }

    /// Kick off the launch scan when opening into a non-empty library: an
    /// incremental scan plus a dead-file prune, with orphans preserved (a one-off
    /// `delryn <file>` open stays in Recent). No-op in the reader or with no folders
    /// configured.
    pub fn start_scan_startup(&mut self) {
        if self.mode == Mode::Library && !self.config.library_paths.is_empty() {
            self.start_scan(false, false, "Scanning library".to_string());
        }
    }

    /// True while a background scan is running — keeps the main loop polling.
    pub fn scan_pending(&self) -> bool {
        self.scan.is_some()
    }

    /// Pick up a finished background scan: refresh the library and flash the
    /// indexed count. Returns `true` when it completes (request a redraw).
    pub fn poll_scan(&mut self) -> bool {
        let Some(job) = self.scan.as_ref() else {
            return false;
        };
        match job.rx.try_recv() {
            Ok(indexed) => {
                let label = job.label.clone();
                self.scan = None;
                self.refresh_library();
                self.library.flash = Some(format!("{label} · indexed {indexed} book(s)"));
                true
            }
            Err(TryRecvError::Empty) => false,
            // The worker died before sending (store open failed): drop the job and
            // refresh so the UI stops waiting.
            Err(TryRecvError::Disconnected) => {
                self.scan = None;
                self.refresh_library();
                true
            }
        }
    }

    /// Block until the background scan finishes (tests only — the real UI polls).
    #[cfg(test)]
    pub(crate) fn await_scan(&mut self) {
        while self.scan.is_some() {
            if !self.poll_scan() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}
