//! Off-thread worker that themes full PDF page rasters for the direct-Kitty deck.
//
// The direct-Kitty page path (see `app::page_deck`) transmits a page's PNG as-is.
// To theme a page we'd have to decode + per-pixel transform + re-encode it — tens
// of ms — which on the main thread would reintroduce the per-turn stall the deck
// was built to avoid. So pages are themed on a background thread, keyed by
// (section, policy): a theme/mode change is a cache miss that re-themes from the
// already-rasterized page rather than re-rendering it through PDFium.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use crate::recolor::{RenderPolicy, theme_page_png};

/// Identifies one themed page: the page (section), the pixel width its raster was
/// rendered at, and the theme policy it was themed for. Width is part of the key
/// so the viewport-matched crisp raster (rendered at a larger width) themes and
/// caches independently of the base raster; a theme/mode change is a plain cache
/// miss that re-themes from the already-rasterized page rather than re-rendering.
pub type PageKey = (usize, u32, RenderPolicy);

/// A request to theme one page's raster off the main thread.
struct PageThemeReq {
    key: PageKey,
    raw: Arc<Vec<u8>>,
}

/// A finished page theming: the PNG bytes the deck should transmit — the themed
/// page, or the original raster when no theming applied (Faithful / dark / photo).
pub struct ThemedPage {
    pub key: PageKey,
    pub bytes: Arc<Vec<u8>>,
}

/// Themes full PDF pages on a background thread, so a page turn never blocks the
/// render loop on the per-pixel theme transform + PNG re-encode. The direct-Kitty
/// counterpart to [`crate::ImageBuilder`] (which serves inline figures).
pub struct PageThemer {
    req_tx: Sender<PageThemeReq>,
    res_rx: Receiver<ThemedPage>,
}

impl PageThemer {
    pub fn new() -> PageThemer {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<PageThemeReq>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<ThemedPage>();
        thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                // Fall back to the raw raster when no theming applies, so the page
                // is still shown (Faithful / dark / photo page) rather than lost.
                let bytes = theme_page_png(&req.raw, req.key.2)
                    .map(Arc::new)
                    .unwrap_or(req.raw);
                if res_tx
                    .send(ThemedPage {
                        key: req.key,
                        bytes,
                    })
                    .is_err()
                {
                    break; // reader dropped
                }
            }
        });
        PageThemer { req_tx, res_rx }
    }

    /// Queue page `key.0`'s raster for theming under `key.1`'s policy.
    pub fn request(&self, key: PageKey, raw: Arc<Vec<u8>>) {
        let _ = self.req_tx.send(PageThemeReq { key, raw });
    }

    /// Collect finished themings (non-blocking).
    pub fn poll(&self) -> impl Iterator<Item = ThemedPage> + '_ {
        self.res_rx.try_iter()
    }
}

impl Default for PageThemer {
    fn default() -> PageThemer {
        PageThemer::new()
    }
}
