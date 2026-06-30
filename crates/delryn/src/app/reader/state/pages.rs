//! Paged-image (PDF) theming state: the off-thread page themer, the themed-PNG
//! cache, in-flight themings, and the policy the visible pages are shown under.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;

use crate::media::{self, PageKey, PageThemer};

/// How many themed PDF pages to keep. Covers the look-ahead window (±4 pages ⇒ 9
/// pages) for the current policy with a little margin for recently-visited pages.
/// A theme/mode toggle re-themes from the cached raster; the on-screen page is
/// held by the deck independently of this cache, so old-policy entries can be
/// evicted freely. Each entry is a full-page PNG, so this is the main bound on
/// page-theme memory.
const PAGE_THEME_CACHE_CAP: usize = 16;

/// All paged-image theming state, owned by `Reader` as `reader.pages`.
pub struct PageThemeState {
    /// Themes full PDF pages off the main thread (the direct-Kitty page path),
    /// so a page turn never blocks on the per-pixel transform + PNG re-encode.
    pub themer: PageThemer,
    /// Themed page PNGs, keyed by (section, policy) so a theme/mode change re-
    /// themes from the cached raster rather than re-rasterizing. LRU-bounded.
    pub themed: LruCache<PageKey, Arc<Vec<u8>>>,
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
            themed: LruCache::new(NonZeroUsize::new(PAGE_THEME_CACHE_CAP).unwrap()),
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
