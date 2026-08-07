//! Background whole-book figure scan, for the image viewer's book scope (`w`).
//!
//! Gathering every figure in a book means parsing every section — and, because a
//! `Block::Math` becomes a `Block::Image` when it typesets, rendering its display maths
//! too, or the figures' section-local image indices would not match the ones the reader
//! wraps and `⏎ go` would land on the wrong figure. That is far too much work for the
//! render thread: on a 550-figure book it froze the UI for seconds.
//!
//! So it runs here instead, on its own thread with its own document handle (the same
//! `SectionLoader` seam the section prefetcher uses), and reports **one section at a
//! time**. The viewer opens immediately on the chapter it already has and grows as
//! sections land.
//!
//! Deliberately *not* the reader's own [`SectionCache`](super::reader::state) loader:
//! that one is tuned for reading — it drops requests more than a few sections from the
//! viewport as stale and its cache evicts by distance — so a whole-book sweep would both
//! fight it and evict the sections being read.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use super::image_view::{Figure, collect_figures};
use crate::document::SectionLoader;

/// One section's figures, reported as the worker finishes it.
pub struct ScannedSection {
    pub section: usize,
    pub figures: Vec<Figure>,
}

/// A running whole-book figure scan. Dropping it cancels the worker at the next section
/// boundary, so closing the viewer (or switching back to chapter scope) doesn't leave a
/// thread grinding through the rest of the book.
pub struct FigureScan {
    res_rx: Receiver<ScannedSection>,
    cancel: Arc<AtomicBool>,
    total: usize,
    done: usize,
}

impl FigureScan {
    /// Scan `order`'s sections in that order — the caller passes them nearest-first, so
    /// the chapters around the reader fill in before the far ends of the book.
    pub fn start(loader: Box<dyn SectionLoader>, order: Vec<usize>) -> FigureScan {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, res_rx) = channel::<ScannedSection>();
        let total = order.len();
        let stop = Arc::clone(&cancel);
        thread::spawn(move || {
            let mut loader = loader;
            for section in order {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let mut blocks = loader.load(section);
                // Typeset display maths, because that turns a `Block::Math` into a
                // `Block::Image` and so shifts the section-local image index every figure
                // is addressed by. The *inline* pass is skipped — it only rewrites spans,
                // never the block sequence, so it cannot move an index, and rasterising
                // every inline glyph in the book would cost far more than the scan.
                //
                // `profile_equation_images` is skipped too, and must stay skipped: it
                // feeds the book-wide equation em, which is global mutable state the
                // reader owns. It only annotates blocks with an ink profile, so it cannot
                // move an image index either.
                crate::app::reader::math::convert_math_blocks(&mut blocks);
                let mut figures = Vec::new();
                collect_figures(&blocks, section, &mut figures);
                if tx.send(ScannedSection { section, figures }).is_err() {
                    return; // viewer closed
                }
            }
        });
        FigureScan {
            res_rx,
            cancel,
            total,
            done: 0,
        }
    }

    /// Take the sections finished since the last call.
    pub fn drain(&mut self) -> Vec<ScannedSection> {
        let out: Vec<ScannedSection> = self.res_rx.try_iter().collect();
        self.done = self.total.min(self.done + out.len());
        out
    }

    /// Whether sections are still to come (keeps the loop redrawing so figures appear
    /// without needing a keypress, and drives the progress readout).
    pub fn pending(&self) -> bool {
        self.done < self.total
    }

    /// Sections scanned so far, and the total to scan.
    pub fn progress(&self) -> (usize, usize) {
        (self.done, self.total)
    }
}

impl Drop for FigureScan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Every section except `current`, ordered by distance from it — so the chapters either
/// side of the reader are scanned before the far ends of the book. `current`'s own
/// figures are gathered synchronously from the already-decoded blocks, so it is excluded.
pub fn scan_order(count: usize, current: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).filter(|&s| s != current).collect();
    order.sort_by_key(|&s| (s.abs_diff(current), s));
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_order_works_outward_from_the_current_section() {
        // Nearest first, and the current section is excluded (already gathered).
        assert_eq!(scan_order(7, 3), vec![2, 4, 1, 5, 0, 6]);
    }

    #[test]
    fn the_scan_order_handles_an_edge_section() {
        assert_eq!(scan_order(4, 0), vec![1, 2, 3]);
        assert_eq!(scan_order(1, 0), Vec::<usize>::new());
    }

    /// The worker reports each section separately and finishes; a scan of nothing is
    /// immediately complete (so the viewer never waits on an empty book).
    #[test]
    fn an_empty_order_is_immediately_complete() {
        struct NoLoader;
        impl SectionLoader for NoLoader {
            fn load(&mut self, _: usize) -> Vec<crate::document::Block> {
                Vec::new()
            }
        }
        let mut scan = FigureScan::start(Box::new(NoLoader), Vec::new());
        assert!(!scan.pending());
        assert!(scan.drain().is_empty());
        assert_eq!(scan.progress(), (0, 0));
    }
}
