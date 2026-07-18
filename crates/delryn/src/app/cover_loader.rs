//! Off-thread book-cover loading, so navigating the library never blocks on I/O + decode.
//!
//! Reading a cover out of an EPUB zip (or rasterising a PDF's first page) and decoding +
//! resizing the image is slow enough — tens of ms each, and much more for PDF — that doing
//! it inline on the render loop froze grid/list navigation until every visible cover
//! finished. A worker thread does the load + decode (its output is a plain `RgbaImage`,
//! which is `Send`); the main thread only does the cheap picker-wrap when a result arrives.
//!
//! The caller [`request`](CoverLoader::request)s paths (visible **and** a prefetch margin in
//! the scroll direction) and [`drain`](CoverLoader::drain)s finished decodes each frame,
//! wrapping and caching them. In-flight paths are tracked so a held key never re-queues the
//! same cover, and the request channel is unbounded so a burst of prefetch requests never
//! blocks the UI.

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use image::RgbaImage;

/// A finished decode: the rounded RGBA cover and its source `(w, h)` pixels — or `None` when
/// the book had no decodable cover (cached as a negative so it isn't retried every frame).
pub type DecodedCover = (RgbaImage, (u32, u32));

/// Loads + decodes book covers on a background thread; the main loop requests paths and
/// drains finished decodes.
pub struct CoverLoader {
    req_tx: Sender<String>,
    res_rx: Receiver<(String, Option<DecodedCover>)>,
    /// Paths handed to the worker but not yet drained — so a repeated request (a held j/k)
    /// never re-queues the same cover.
    inflight: HashSet<String>,
}

impl CoverLoader {
    /// Spawn the worker. It lives for the process; dropping the loader closes the request
    /// channel, so the worker's `recv` returns `Err` and the thread exits.
    pub fn new() -> CoverLoader {
        let (req_tx, req_rx) = channel::<String>();
        let (res_tx, res_rx) = channel::<(String, Option<DecodedCover>)>();
        thread::spawn(move || {
            while let Ok(path) = req_rx.recv() {
                let decoded = super::load_cover_bytes(&path)
                    .as_deref()
                    .and_then(crate::media::decode_cover);
                if res_tx.send((path, decoded)).is_err() {
                    break; // main side gone
                }
            }
        });
        CoverLoader {
            req_tx,
            res_rx,
            inflight: HashSet::new(),
        }
    }

    /// Queue `path` for loading if it isn't already in flight. Cheap and non-blocking.
    pub fn request(&mut self, path: &str) {
        if self.inflight.contains(path) {
            return;
        }
        if self.req_tx.send(path.to_string()).is_ok() {
            self.inflight.insert(path.to_string());
        }
    }

    /// Whether any requested cover is still being loaded (keeps the loop redrawing so a
    /// finished cover pops in without needing a keypress).
    pub fn pending(&self) -> bool {
        !self.inflight.is_empty()
    }

    /// Take all covers finished since the last call, clearing them from the in-flight set.
    pub fn drain(&mut self) -> Vec<(String, Option<DecodedCover>)> {
        let done: Vec<_> = self.res_rx.try_iter().collect();
        for (path, _) in &done {
            self.inflight.remove(path);
        }
        done
    }
}
