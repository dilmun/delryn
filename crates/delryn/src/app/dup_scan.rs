//! Thorough duplicate scan: a user-triggered pass that reads each book's *identity*
//! from its own pages — the printed title (and subtitle) and copyright year — and
//! links books whose identities agree (see `dedup::content_link_candidates`). This
//! is content, not metadata: metadata is often wrong or missing, but the title page
//! states the work plainly and reads the same across formats, so it matches every
//! combination (EPUB↔EPUB, PDF↔PDF, PDF↔EPUB).
//!
//! Per format: an EPUB's title comes from its most prominent heading
//! (`epub::extract_content_title`); a PDF's from the largest-font text on its title
//! page (`pdf::extract_title`); the year is the first copyright-style year in the
//! opening text. A scanned PDF with no text layer yields no identity and is skipped.
//!
//! It runs off the default path because reading pages means opening/decoding each
//! file. The worker computes pure data and streams progress + results back; the
//! main thread persists the links (the DB connection isn't `Send`) and the existing
//! grouping folds them in.

use std::sync::mpsc::{Receiver, TryRecvError};

use super::App;
use crate::library::dedup::ContentId;

/// Opening-text budget sampled to find the copyright year.
const YEAR_SAMPLE_CHARS: usize = 8_000;
/// PDF pages to scan for the year.
const PDF_YEAR_PAGES: usize = 5;

/// A worker result: the book's path and, if its title could be read, the extracted
/// `(title, copyright year)`.
type IdResult = (String, Option<(String, Option<i32>)>);

/// State of an in-flight content scan. `done`/`total` drive the progress message;
/// `ids` accumulates each book's extracted identity as the worker reports it (books
/// whose title couldn't be read are counted but not stored).
pub struct DupScan {
    rx: Receiver<IdResult>,
    total: usize,
    done: usize,
    ids: Vec<ContentId>,
}

impl App {
    /// Kick off a thorough content scan over the whole library. No-op (with a
    /// flash) if one is already running or there are no books. The worker thread
    /// reads each book's title/year from its pages and sends it back (or `None`);
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
                let id = extract_identity(&path);
                // A send error means the receiver was dropped (scan abandoned); stop.
                if tx.send((path, id)).is_err() {
                    break;
                }
            }
            // Dropping `tx` here disconnects the channel — the signal that the scan
            // has finished (see the `Disconnected` arm in `poll_dup_scan`).
        });
        self.lib_flash = Some(format!("reading titles 0/{total}…"));
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
                Ok((path, id)) => {
                    scan.done += 1;
                    if let Some((title, year)) = id {
                        scan.ids.push(ContentId { path, title, year });
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
            self.lib_flash = Some(format!("reading titles {}/{}…", scan.done, scan.total));
        }
        progressed
    }

    /// Pair up the collected identities, persist the links, and refresh so the
    /// Duplicates view reflects the new groups.
    fn finish_dup_scan(&mut self, scan: DupScan) {
        let pairs = crate::library::dedup::content_link_candidates(&scan.ids);
        if let Some(store) = &self.store {
            store.replace_scan_dup_links(&pairs);
        }
        self.refresh_library();
        self.lib_flash = Some(format!(
            "deep scan done: read {}/{} titles, {} match{} — press D to resolve",
            scan.ids.len(),
            scan.total,
            pairs.len(),
            if pairs.len() == 1 { "" } else { "es" },
        ));
    }
}

/// Read a book's identity — printed title (with subtitle) and copyright year —
/// from its own pages, dispatching by file type. `None` for unsupported types or
/// when no title could be read (e.g. a scanned PDF with no text layer).
fn extract_identity(path: &str) -> Option<(String, Option<i32>)> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "epub" => {
            let (title, subtitle) = crate::document::epub::extract_content_title(path)?;
            let full = match subtitle {
                Some(s) => format!("{title} {s}"),
                None => title,
            };
            let year = crate::document::epub::extract_text_sample(path, YEAR_SAMPLE_CHARS)
                .as_deref()
                .and_then(year_in_text);
            Some((full, year))
        }
        "pdf" => {
            let title = crate::document::pdf::extract_title(path)?;
            let year =
                crate::document::pdf::extract_text_sample(path, PDF_YEAR_PAGES, YEAR_SAMPLE_CHARS)
                    .as_deref()
                    .and_then(year_in_text);
            Some((title, year))
        }
        _ => None,
    }
}

/// First copyright-style year (1900–2099) in the text — the publication year that
/// appears on the title/copyright page.
fn year_in_text(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let four = &bytes[i..i + 4];
        let bounded_left = i == 0 || !bytes[i - 1].is_ascii_digit();
        let bounded_right = i + 4 == bytes.len() || !bytes[i + 4].is_ascii_digit();
        if bounded_left && bounded_right && four.iter().all(u8::is_ascii_digit) {
            // Safe: four ASCII digits.
            let year: i32 = std::str::from_utf8(four).unwrap().parse().unwrap();
            if (1900..=2099).contains(&year) {
                return Some(year);
            }
        }
        i += 1;
    }
    None
}
