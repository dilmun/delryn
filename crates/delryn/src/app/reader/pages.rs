//! Paged-image (PDF) theming lifecycle: adapt each full-page raster to the active
//! theme off the main thread, keyed by (section, policy), and feed the themed PNGs
//! to the direct-Kitty [`PageDeck`](crate::app::page_deck::PageDeck).
//!
//! Inline figures are themed by the [`ImageBuilder`](crate::media::ImageBuilder)
//! pipeline (`reader::images`); a full PDF page bypasses that for the transmit-once
//! direct path, so it carries its own small theming pass here. The expensive PDFium
//! raster is produced once by the background loader and cached by section; theming
//! runs on top of it and is cached by (section, policy), so cycling the theme or
//! image mode re-themes from the raster instead of re-rendering the page.

use std::sync::Arc;

use super::*;

/// How many pages each side of the current one to pre-theme, matching the raster
/// prefetch window so an arrived page turn finds its themed PNG already built.
pub(super) const PAGE_THEME_AHEAD: usize = 4;

impl Reader {
    /// Collect finished page themings into the cache (cheap; safe to call often).
    /// Serves both the base raster and the viewport-matched crisp rasters — they
    /// share the themer, keyed by (section, width, policy).
    pub(super) fn drain_page_themer(&mut self) {
        for done in self.pages.themer.poll() {
            self.pages.requested.remove(&done.key);
            self.pages.themed.put(done.key, done.bytes);
        }
    }

    /// Collect finished crisp re-rasterizations into the cache (cheap; safe to call
    /// often). No-op for reflowable documents (no worker).
    pub(super) fn drain_crisp(&mut self) {
        let Some(worker) = self.crisp.worker.as_ref() else {
            return;
        };
        let done: Vec<_> = worker.poll().collect();
        for page in done {
            self.crisp.requested.remove(&page.key);
            if page.bytes.is_empty() {
                self.crisp.failed.insert(page.key); // render failed → don't retry
            } else {
                self.crisp.rasters.put(page.key, page.bytes);
            }
        }
    }

    /// Per-frame page-theming pass for a paged-image (PDF) document: record the
    /// active render `policy`, collect finished themings, and dispatch theming for
    /// the visible + look-ahead pages whose raster is ready (so a turn lands on an
    /// already-themed page, no per-turn transform stall). Faithful mode shows the
    /// raw raster directly, so it only records the policy. Called by the view each
    /// frame, before it reads `page_png` to place the pages.
    pub fn sync_pages(&mut self, policy: media::RenderPolicy) {
        self.pages.policy = policy;
        self.drain_page_themer();
        self.drain_crisp();
        // The view rebuilds the per-section display width each frame (via
        // `resolve_page_width`); clear last frame's so stale sections don't linger.
        self.crisp.effective.clear();
        if policy.mode == media::ImageMode::Faithful {
            return; // the raw raster is shown as-is; nothing to theme
        }
        let n = self.doc.section_count();
        if n == 0 {
            return;
        }
        let lo = self.section.saturating_sub(PAGE_THEME_AHEAD);
        let hi = (self.section + PAGE_THEME_AHEAD).min(n - 1);
        for s in lo..=hi {
            self.theme_base_page(s, policy);
        }
        // A continuous stack can show more pages than the ±neighbour window reaches
        // (a tall / zoomed-out spread stack), so theme every visible band too — else
        // the far bands would stay blank.
        if self.continuous_paged_active() {
            for s in self.visible_stack.clone() {
                self.theme_base_page(s, policy);
            }
        }
    }

    /// Dispatch theming of `section`'s base raster under `policy` if it's rasterized
    /// and not already themed or in flight (deduped).
    fn theme_base_page(&mut self, section: usize, policy: media::RenderPolicy) {
        let key = (section, BASE_RASTER_WIDTH, policy);
        if self.raster_ready(section)
            && !self.pages.themed.contains(&key)
            && !self.pages.requested.contains(&key)
            && let Some(raw) = self.raster_png(section)
        {
            self.pages.requested.insert(key);
            self.pages.themer.request(key, Arc::new(raw));
        }
    }
}
