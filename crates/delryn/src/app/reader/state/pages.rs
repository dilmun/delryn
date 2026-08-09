//! Paged-image (PDF) theming state: the off-thread page themer, the themed-PNG
//! cache, in-flight themings, and the policy the visible pages are shown under;
//! plus the viewport-matched crisp re-raster state ([`PageRasterState`]).

use std::collections::{HashMap, HashSet};

use super::byte_lru::ByteLru;
use crate::app::reader::raster::{PageRasterWorker, RasterKey};
use crate::media::{self, PageKey, PageThemer};

/// Memory allowed for themed PDF pages. Comfortably covers the base look-ahead
/// window (±4 pages ⇒ 9) for the current policy plus recently-visited and a couple
/// of crisp (zoomed) entries, at any page size — which is the point of budgeting
/// bytes rather than entries. A theme/mode toggle re-themes from the cached raster;
/// the on-screen page is held by the deck independently of this cache, so
/// old-policy entries can be evicted freely.
const PAGE_THEME_BUDGET: usize = 32 * 1024 * 1024;

/// Memory allowed for raw crisp rasters. A crisp raster is only rendered for the
/// current single (zoomed / large-viewport) page at a handful of distinct widths,
/// so a small budget covers the working set; the base raster is held separately by
/// the section cache and is always available as the fallback.
const CRISP_RASTER_BUDGET: usize = 16 * 1024 * 1024;

/// All paged-image theming state, owned by `Reader` as `reader.pages`.
pub struct PageThemeState {
    /// Themes full PDF pages off the main thread (the direct-Kitty page path),
    /// so a page turn never blocks on the per-pixel transform + PNG re-encode.
    pub themer: PageThemer,
    /// Themed page PNGs, keyed by (section, policy) so a theme/mode change re-
    /// themes from the cached raster rather than re-rasterizing. LRU-bounded.
    pub themed: ByteLru<PageKey>,
    /// Page themings currently in flight (avoid dispatching duplicates).
    pub requested: HashSet<PageKey>,
    /// The render policy the visible page(s) are themed/shown under, set each
    /// frame by `Reader::sync_pages`; the source of truth for `page_png` and the
    /// page-readiness checks the deck gates on.
    pub policy: media::RenderPolicy,
}

impl Default for PageThemeState {
    fn default() -> Self {
        Self {
            themer: PageThemer::new(),
            themed: ByteLru::new(PAGE_THEME_BUDGET),
            requested: HashSet::new(),
            policy: media::RenderPolicy {
                tint: media::Ink {
                    ink: [0, 0, 0],
                    paper: [255, 255, 255],
                },
                mode: media::ImageMode::default(),
            },
        }
    }
}

/// Viewport-matched crisp re-raster state for paged (PDF) documents: the
/// off-thread rasterizer, the raw crisp-PNG cache, in-flight requests, and the
/// per-section width the view chose to display at this frame. See
/// [`crate::app::reader::raster`]. All empty/absent for reflowable documents.
pub struct PageRasterState {
    /// Re-rasterizes pages at viewport-matched widths off the main thread; `None`
    /// for reflowable documents (no fixed page image to re-render).
    pub worker: Option<PageRasterWorker>,
    /// Raw (un-themed) crisp-raster PNGs keyed by (section, width). LRU-bounded;
    /// the base raster lives in the section cache and is the always-present
    /// fallback, so crisp entries can be evicted freely.
    pub rasters: ByteLru<RasterKey>,
    /// Crisp rasterizations currently in flight (avoid dispatching duplicates).
    pub requested: HashSet<RasterKey>,
    /// Crisp rasterizations that failed to render, so they aren't retried every
    /// frame (a permanently-soft page falls back to the base without spinning).
    pub failed: HashSet<RasterKey>,
    /// Cached base-raster pixel dimensions per section, so the per-frame want-width
    /// computation and placement don't re-read the base PNG header each frame.
    pub base_dims: HashMap<usize, (u32, u32)>,
    /// The raster width the view chose to display each visible section at this
    /// frame — the base width, or a crisp width once its raster + theming are
    /// ready. `page_png` (served to the deck after the frame) reads this so it
    /// returns bytes matching the crop the view computed. Rebuilt each frame.
    pub effective: HashMap<usize, u32>,
}

impl Default for PageRasterState {
    fn default() -> Self {
        Self {
            worker: None,
            rasters: ByteLru::new(CRISP_RASTER_BUDGET),
            requested: HashSet::new(),
            failed: HashSet::new(),
            base_dims: HashMap::new(),
            effective: HashMap::new(),
        }
    }
}
