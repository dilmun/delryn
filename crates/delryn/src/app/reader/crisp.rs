//! Resolving the pixels for a paged-image (PDF) page: constant-margin crop,
//! viewport-matched crisp re-raster, and the theming request.
//!
//! The background loader rasterizes each page once at a base width. That base is
//! fine when the page *downscales* to fit, but a page shown larger than its base
//! (zoomed in, or on a big / hi-DPI viewport) would look soft. This module bridges
//! that: [`page_content_box`](Reader::page_content_box) applies the constant edge
//! crop, [`resolve_page_width`](Reader::resolve_page_width) picks the display width
//! (and requests a sharper re-raster through the [`PageRasterWorker`](super::raster)
//! when the base would upscale), and the themer is asked to adapt whichever raster
//! is chosen. The pure width math lives in `reader::raster` / `reader::page_view`;
//! this is the `impl Reader` glue that reads config, caches, and dispatches.

use super::*;

impl Reader {
    /// Mirror the margin-trim settings from config (called each render).
    pub fn set_trim(&mut self, on: bool, pct: u16) {
        self.trim_margins = on;
        self.trim_pct = pct;
    }

    /// The content box `(x, y, w, h)` of a PDF page for the margin trim, in the
    /// pixel coordinates of a raster of size `full`: a **constant** crop of
    /// `trim_pct` % off each edge. Because the crop is the same fraction on every
    /// page, the displayed region is a constant fraction of the (uniform) page, so
    /// the page width stays identical when flipping between pages — regardless of
    /// each page's own baked-in margins. The whole page when trimming is off or the
    /// percent is 0. Section-independent by design (the parameter is kept for the
    /// per-page call site and any future per-page override).
    pub fn page_content_box(&self, _section: usize, full: (u32, u32)) -> (u32, u32, u32, u32) {
        let pct = if self.trim_margins {
            self.trim_pct.min(crate::config::MAX_PDF_MARGIN_PCT) as u32
        } else {
            0
        };
        if pct == 0 {
            return (0, 0, full.0, full.1);
        }
        let mx = full.0 * pct / 100;
        let my = full.1 * pct / 100;
        let w = full.0.saturating_sub(mx * 2).max(1);
        let h = full.1.saturating_sub(my * 2).max(1);
        (mx, my, w, h)
    }

    /// Resolve which raster width to display `section` at this frame and record it
    /// for [`page_png`](Self::page_png). Given the base raster dimensions and the
    /// width the current placement wants (≥1 raster px per screen px), request a
    /// crisp re-raster (and its theming) when the base would upscale, and return the
    /// effective `(width, dimensions)` to place — the crisp raster only once it's
    /// fully ready (rasterized *and* themed for the active policy), otherwise the
    /// base. Single-page only; spreads sit at fit-page and keep the base raster.
    pub fn resolve_page_width(
        &mut self,
        section: usize,
        base_dims: (u32, u32),
        want_width: u32,
    ) -> (u32, (u32, u32)) {
        let base_w = base_dims.0;
        let chosen = match raster::crisp_target_width(want_width, base_w) {
            // No worker (reflowable) means no crisp path — keep the base.
            Some(cw) if self.crisp.worker.is_some() => {
                if self.crisp_ready(section, cw) {
                    cw
                } else {
                    // Drive the two-step async: request the raster, then (once it's
                    // cached) its theming. Both are deduped no-ops when pending.
                    // Show the base this frame; the crisp page pops in when ready.
                    self.request_crisp(section, cw);
                    self.request_page_theme(section, cw);
                    base_w
                }
            }
            _ => base_w,
        };
        self.crisp.effective.insert(section, chosen);
        if chosen == base_w {
            return (base_w, base_dims);
        }
        let dims = self
            .raw_raster_at(section, chosen)
            .and_then(|p| media::image_dimensions(&p))
            .unwrap_or(base_dims);
        (chosen, dims)
    }

    /// The pixel dimensions of `section`'s base raster, or `None` until it's
    /// rasterized. Cached so repeated frames don't re-read the PNG header.
    pub fn base_raster_dims(&mut self, section: usize) -> Option<(u32, u32)> {
        if let Some(d) = self.crisp.base_dims.get(&section) {
            return Some(*d);
        }
        let png = self.raster_png(section)?;
        let d = media::image_dimensions(&png)?;
        self.crisp.base_dims.insert(section, d);
        Some(d)
    }

    /// Whether `section`'s crisp raster at `width` is ready to place: the raw
    /// raster is cached and, unless in Faithful mode, its theming is built too.
    fn crisp_ready(&self, section: usize, width: u32) -> bool {
        if !self.crisp.rasters.contains(&(section, width)) {
            return false;
        }
        self.pages.policy.mode == media::ImageMode::Faithful
            // The *same* lookup `page_png` will make, so the width chosen here and
            // the bytes served there can never disagree.
            || self
                .pages
                .themed
                .peek(&(section, width, self.pages.policy))
                .is_some()
    }

    /// Request the crisp re-raster of `section` at `width` if not already cached,
    /// in flight, or previously failed (deduped). No-op without a worker.
    fn request_crisp(&mut self, section: usize, width: u32) {
        let key = (section, width);
        if self.crisp.rasters.contains(&key)
            || self.crisp.requested.contains(&key)
            || self.crisp.failed.contains(&key)
        {
            return;
        }
        if let Some(worker) = self.crisp.worker.as_ref() {
            self.crisp.requested.insert(key);
            worker.request(key);
        }
    }

    /// Whether any viewport-matched crisp work is in flight — a re-raster, or the
    /// theming of one — so the render loop keeps drawing until the crisp page pops
    /// in (rather than settling on the base after a zoom and waiting for a keypress).
    /// Self-limiting: a failed raster clears its request and won't be retried.
    pub fn crisp_awaiting(&self) -> bool {
        !self.crisp.requested.is_empty()
            || self
                .pages
                .requested
                .iter()
                .any(|&(_, w, _)| w != BASE_RASTER_WIDTH)
    }

    /// Request theming of an already-rasterized crisp page at `width` under the
    /// active policy (deduped). No-op in Faithful mode (the raw raster is shown).
    fn request_page_theme(&mut self, section: usize, width: u32) {
        if self.pages.policy.mode == media::ImageMode::Faithful {
            return;
        }
        let key = (section, width, self.pages.policy);
        if self.pages.themed.contains(&key) || self.pages.requested.contains(&key) {
            return;
        }
        if let Some(raw) = self.raw_raster_at(section, width) {
            self.pages.requested.insert(key);
            self.pages.themer.request(key, std::sync::Arc::new(raw));
        }
    }
}
