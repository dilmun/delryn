//! Viewport-matched crisp re-rasterization for paged (PDF) documents.
//!
//! The base section loader rasterizes each page once at a generous fixed width
//! ([`delryn_format::pdf::PAGE_RASTER_WIDTH`]) for immediate display. At fit-page in
//! a normal terminal that raster is *downscaled* to the pane — already crisp. But
//! when a page is **zoomed in** (a small source window blown up to fill the
//! viewport) or shown on a **large / hi-DPI viewport**, the base raster is
//! *upscaled* and softens.
//!
//! This module re-renders the page through PDFium at a larger, viewport-matched
//! width on a background thread ([`PageRasterWorker`]), so the crisp raster arrives
//! a frame later without ever blocking a page turn: the base raster is shown until
//! the crisp one is ready (see the `effective_*` methods on `Reader`). The width is
//! chosen by [`crisp_target_width`] — pure and unit-tested — and bucketed + capped
//! so panning (same width, different crop) never re-rasters and zoom re-renders at
//! most a handful of distinct widths.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use delryn_format::PageRasterizer;

/// The largest width a crisp re-raster targets. Beyond ~2× the base the transmit
/// cost and memory grow while the perceived gain flattens, so heavy zoom is
/// capped here (still far sharper than the base upscaled the same amount).
pub const CRISP_MAX_WIDTH: u32 = 4096;

/// Round crisp widths to this bucket so a small viewport/zoom change reuses the
/// cached raster instead of re-rendering at a fractionally different width.
const CRISP_BUCKET: u32 = 512;

/// Only re-raster when it buys at least this many extra pixels of width over the
/// base — a near-1× placement isn't worth a PDFium round-trip.
const CRISP_MIN_GAIN: u32 = 400;

/// The crisp raster width to render `section` at, or `None` when the base raster
/// (`base_width` px) already has enough resolution (the placement downscales it,
/// or the gain would be negligible). `want_width` is the width at which the
/// current placement maps ~1 raster pixel per screen pixel (see the caller). The
/// result is bucketed and capped so distinct widths stay few.
pub fn crisp_target_width(want_width: u32, base_width: u32) -> Option<u32> {
    let capped = want_width.min(CRISP_MAX_WIDTH);
    if capped <= base_width.saturating_add(CRISP_MIN_GAIN) {
        return None; // base is already crisp enough (or barely worse)
    }
    let bucketed = capped.div_ceil(CRISP_BUCKET) * CRISP_BUCKET;
    Some(bucketed.min(CRISP_MAX_WIDTH))
}

/// Identifies one crisp raster: the page (section) and the width it's rendered at.
pub type RasterKey = (usize, u32);

/// A finished crisp rasterization: the raw (un-themed) PNG at the requested width,
/// or empty `bytes` when the render failed (so the reader clears the in-flight
/// marker and stops retrying / spinning rather than waiting forever).
pub struct RasteredPage {
    pub key: RasterKey,
    pub bytes: Arc<Vec<u8>>,
}

/// Re-rasterizes PDF pages at viewport-matched widths on a background thread, so a
/// zoom / resize never blocks the render loop on a PDFium re-render. The raw crisp
/// PNGs it produces are then themed by the existing [`PageThemer`](delryn_media::
/// PageThemer), exactly like the base raster.
pub struct PageRasterWorker {
    req_tx: Sender<RasterKey>,
    res_rx: Receiver<RasteredPage>,
}

impl PageRasterWorker {
    /// Spawn a worker driving `rasterizer` (the document's own PDFium handle,
    /// reopened on the worker thread).
    pub fn new(mut rasterizer: Box<dyn PageRasterizer>) -> PageRasterWorker {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<RasterKey>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<RasteredPage>();
        thread::spawn(move || {
            while let Ok((section, width)) = req_rx.recv() {
                // Always reply (empty on failure) so the reader clears the in-flight
                // marker; otherwise the redraw loop would spin waiting for a page
                // that never arrives.
                let bytes = rasterizer.rasterize(section, width).unwrap_or_default();
                let page = RasteredPage {
                    key: (section, width),
                    bytes: Arc::new(bytes),
                };
                if res_tx.send(page).is_err() {
                    break; // reader dropped
                }
            }
        });
        PageRasterWorker { req_tx, res_rx }
    }

    /// Queue page `key.0` for re-rasterization at `key.1` px.
    pub fn request(&self, key: RasterKey) {
        let _ = self.req_tx.send(key);
    }

    /// Collect finished rasterizations (non-blocking).
    pub fn poll(&self) -> impl Iterator<Item = RasteredPage> + '_ {
        self.res_rx.try_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_crisp_when_the_base_already_downscales() {
        // A placement that shows the page at or below the base resolution needs no
        // re-raster.
        assert_eq!(crisp_target_width(1500, 2000), None);
        assert_eq!(crisp_target_width(2000, 2000), None);
    }

    #[test]
    fn a_negligible_gain_is_not_worth_a_re_raster() {
        // Just above base but within the min-gain slack → still the base.
        assert_eq!(crisp_target_width(2000 + CRISP_MIN_GAIN - 1, 2000), None);
    }

    #[test]
    fn zoom_requests_a_bucketed_larger_raster() {
        // ~1.5× upscale → a crisp raster, rounded up to the bucket.
        let w = crisp_target_width(3000, 2000).expect("a zoomed page re-rasters");
        assert!(
            w >= 3000 && w.is_multiple_of(CRISP_BUCKET),
            "bucketed ≥ want: {w}"
        );
    }

    #[test]
    fn heavy_zoom_is_capped() {
        assert_eq!(crisp_target_width(100_000, 2000), Some(CRISP_MAX_WIDTH));
    }
}
