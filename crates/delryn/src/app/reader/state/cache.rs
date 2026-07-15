//! Section block cache: the decoded-section map, the background loader's worker
//! thread + channels, and the set of in-flight requests.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use crate::document::{Block, SectionLoader};

/// How far from the reader's current section the background loader still honours
/// a request before treating it as stale. Must cover the neighbour-prefetch
/// window (±4) so the look-ahead pages aren't dropped, with a little margin.
const LOADER_RADIUS: usize = 6;

/// Decoded section blocks plus the background loader, owned by `Reader` as
/// `reader.sections`.
pub struct SectionCache {
    /// Decoded section blocks, keyed by section index (bounded by the reader).
    pub sections: HashMap<usize, Vec<Block>>,
    /// Sections requested from the loader but not yet returned.
    pub requested: HashSet<usize>,
    /// Channel to ask the background loader for a section.
    pub req_tx: Sender<usize>,
    /// Channel of decoded sections from the background loader. `None` blocks mean
    /// the loader dropped a request as stale (the reader scrolled far past it),
    /// so it's left uncached and re-requestable.
    pub res_rx: Receiver<(usize, Option<Vec<Block>>)>,
    /// The section the reader is currently on, shared with the loader thread so it
    /// can skip rasterizing pages flown past during a fast `j`/`k` scroll.
    pub loader_current: Arc<AtomicUsize>,
}

impl SectionCache {
    /// Build the cache and spawn the background loader worker. The worker decodes
    /// sections on request, tracking where the reader is (`loader_current`) and
    /// dropping requests for pages scrolled far past, so a fast `j`/`k` burst
    /// reaches the page you actually stopped on instead of grinding through every
    /// page in between. `start` seeds the reader's current section.
    pub fn new(mut loader: Box<dyn SectionLoader>, start: usize) -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<usize>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<(usize, Option<Vec<Block>>)>();
        let loader_current = Arc::new(AtomicUsize::new(start));
        let worker_current = Arc::clone(&loader_current);
        thread::spawn(move || {
            while let Ok(index) = req_rx.recv() {
                let cur = worker_current.load(Ordering::Relaxed);
                let msg = if index.abs_diff(cur) > LOADER_RADIUS {
                    (index, None) // stale: reader moved on; leave it re-requestable
                } else {
                    // Render display math to images here, off the main thread
                    // (disk-cached), so the section arrives ready to wrap.
                    let mut blocks = loader.load(index);
                    crate::app::reader::math::convert_math_blocks(&mut blocks);
                    crate::app::reader::math::convert_inline_math(&mut blocks);
                    crate::app::reader::math::profile_equation_images(&mut blocks, index);
                    (index, Some(blocks))
                };
                if res_tx.send(msg).is_err() {
                    break; // reader dropped
                }
            }
        });
        Self {
            sections: HashMap::new(),
            requested: HashSet::new(),
            req_tx,
            res_rx,
            loader_current,
        }
    }
}
