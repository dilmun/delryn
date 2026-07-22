//! Off-thread book-cover loading, so navigating the library never blocks on I/O + decode.
//!
//! Reading a cover out of an EPUB zip (or rasterising a PDF's first page) and decoding +
//! resizing the image is slow enough — tens of ms each, and much more for PDF — that doing
//! it inline on the render loop froze grid/list navigation until every visible cover
//! finished. A worker thread does the load + decode (its output is a plain `RgbaImage`,
//! which is `Send`); the main thread only does the cheap picker-wrap when a result arrives.
//!
//! ## Priority queue, not a FIFO backlog
//!
//! The caller [`set_wanted`](CoverLoader::set_wanted)s the covers it wants *this frame*,
//! highest priority first (the visible rows, then a prefetch margin in the scroll
//! direction), and [`drain`](CoverLoader::drain)s finished decodes each frame. Crucially the
//! wanted list **replaces** the queue wholesale rather than appending: a held `j`/`k` that
//! flies past a hundred rows does not queue a hundred covers the worker must grind through
//! before it reaches the row you actually stopped on. Each frame the queue is rebuilt from
//! the current viewport, so rows scrolled past are dropped before they are ever decoded, and
//! the covers on screen are always at the front. One decode thread keeps it simple and
//! PDFium-safe (PDFium is a single per-process binding, not safe to call concurrently).

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use image::RgbaImage;

/// A finished decode: the rounded RGBA cover and its source `(w, h)` pixels — or `None` when
/// the book had no decodable cover (cached as a negative so it isn't retried every frame).
pub type DecodedCover = (RgbaImage, (u32, u32));

/// Decode threads in the pool. Reading + decoding + downscaling a cover is CPU-bound, so a
/// few threads clear a screenful far faster than one. Kept small so the pool never starves
/// the render thread. PDF cover rasterisation is serialised inside the PDF backend (PDFium
/// isn't safe to call concurrently); EPUB covers decode fully in parallel.
fn worker_count() -> usize {
    thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).clamp(1, 4))
        .unwrap_or(2)
}

/// Work shared with the decode threads.
struct Queue {
    /// Paths still to decode, highest priority first (visible rows ahead of prefetch).
    /// Rebuilt every frame from the current viewport by [`CoverLoader::set_wanted`], so a
    /// held key never builds a backlog: rows scrolled past simply fall off the list.
    wanted: VecDeque<String>,
    /// Paths a worker is decoding right now, so a queue rebuild neither re-queues one nor
    /// loses track of it (each still counts as pending).
    decoding: HashSet<String>,
    /// Set on drop so the workers' waits return and the threads exit.
    closed: bool,
}

/// Loads + decodes book covers on a pool of background threads; the main loop sets the
/// wanted set each frame and drains finished decodes.
pub struct CoverLoader {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    res_rx: Receiver<(String, Option<DecodedCover>)>,
}

impl CoverLoader {
    /// Spawn the worker pool. Threads live for the process; dropping the loader sets
    /// `closed`, so each worker's `wait` returns and the thread exits.
    pub fn new() -> CoverLoader {
        let shared = Arc::new((
            Mutex::new(Queue {
                wanted: VecDeque::new(),
                decoding: HashSet::new(),
                closed: false,
            }),
            Condvar::new(),
        ));
        let (res_tx, res_rx) = channel::<(String, Option<DecodedCover>)>();
        for _ in 0..worker_count() {
            let worker = Arc::clone(&shared);
            let tx = res_tx.clone();
            thread::spawn(move || decode_loop(&worker, &tx));
        }
        CoverLoader { shared, res_rx }
    }

    /// Set the covers wanted this frame, highest priority first (visible rows, then the
    /// prefetch margin), already filtered to those not yet cached. Replaces the queue
    /// wholesale — rows no longer on/near screen are dropped — while preserving the
    /// in-flight decodes. Cheap and non-blocking (holds the lock only to swap a small list).
    pub fn set_wanted(&self, paths: &[String]) {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.wanted.clear();
        for p in paths {
            // Already being decoded — don't queue it twice.
            if q.decoding.contains(p.as_str()) {
                continue;
            }
            q.wanted.push_back(p.clone());
        }
        if !q.wanted.is_empty() {
            cv.notify_all();
        }
    }

    /// Whether any wanted cover is still queued or decoding (keeps the loop redrawing so a
    /// finished cover pops in without needing a keypress).
    pub fn pending(&self) -> bool {
        let (lock, _) = &*self.shared;
        lock.lock()
            .is_ok_and(|q| !q.wanted.is_empty() || !q.decoding.is_empty())
    }

    /// Take all covers finished since the last call.
    pub fn drain(&self) -> Vec<(String, Option<DecodedCover>)> {
        self.res_rx.try_iter().collect()
    }
}

impl Drop for CoverLoader {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut q) = lock.lock() {
            q.closed = true;
            cv.notify_all();
        }
    }
}

/// A worker: take the highest-priority wanted path (waiting while the queue is empty),
/// decode it off the lock, and send the result. Exits when the loader is dropped.
fn decode_loop(
    shared: &Arc<(Mutex<Queue>, Condvar)>,
    res_tx: &Sender<(String, Option<DecodedCover>)>,
) {
    let (lock, cv) = &**shared;
    loop {
        // Claim the next path under the lock, then release it for the slow decode.
        let path = {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            let path = loop {
                if q.closed {
                    return;
                }
                if let Some(p) = q.wanted.pop_front() {
                    break p;
                }
                q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
            };
            q.decoding.insert(path.clone());
            path
        };

        let decoded = super::load_cover_bytes(&path)
            .as_deref()
            .and_then(crate::media::decode_cover);

        {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.decoding.remove(&path);
        }
        if res_tx.send((path, decoded)).is_err() {
            return; // main side gone
        }
    }
}
