//! Inline-image lifecycle state: built protocols, the per-section index→key map,
//! row estimates for reflow, and in-flight / failed build tracking.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;

use lru::LruCache;

use crate::app::IMAGE_CACHE_CAP;
use crate::media::{self, ImagePlan, ImgKey};

/// All inline-image state, owned by `Reader` as `reader.images`.
pub struct ImageState {
    /// Built image protocols, reused across sections (revisiting a section reuses
    /// the already-uploaded image instead of re-transmitting). LRU.
    pub cache: LruCache<ImgKey, ImagePlan>,
    /// Current section's image index → cache key.
    pub section_images: HashMap<usize, ImgKey>,
    /// Current section's inline-math id → cache key (a separate index space from
    /// `section_images`; see [`media::ImgSlot`]). Built alongside the section's
    /// figures, drawn mid-line by the reader's inline-math pass.
    pub section_inline: HashMap<usize, ImgKey>,
    /// Reserved cell width per inline-math id, estimated up front (like
    /// `rows_estimate` for figures) so the wrapper can reserve the atom's columns
    /// before its raster finishes building. Passed to the wrapper as
    /// `WrapOpts::inline_math_cols`.
    pub inline_cols: Vec<u16>,
    /// Reserved cell **height** per inline-math id (parallel to [`inline_cols`]). A
    /// value > 1 (a fraction) makes the wrapper reserve a blank spacer row below its
    /// line. Passed to the wrapper as `WrapOpts::inline_math_rows`.
    pub inline_rows: Vec<u16>,
    /// Reserved rows per image index, estimated up front so reflow doesn't wait
    /// on the background build.
    pub rows_estimate: Vec<u16>,
    /// (section, avail-cols, max-rows, max-px, width-pct, eq-scale, fit-mode) the
    /// estimates are for — a change re-remaps so a live sizing-config change takes
    /// effect without leaving the section.
    pub images_key: (usize, u16, u16, u16, u16, u16, media::ImageFit),
    /// Theme tint + mode the current image builds used; a change re-requests them
    /// so images re-render when the theme cycles or the image mode changes.
    pub policy: media::RenderPolicy,
    /// Image builds currently in flight (avoid dispatching duplicates).
    pub requested: HashSet<ImgKey>,
    /// Image builds that failed (so we stop waiting / re-requesting).
    pub failed: HashSet<ImgKey>,
    /// Continuous mode only: for each *following* section joined into the scroll
    /// buffer, its images as `(cache key, reserved rows)` by section-local index.
    /// Lets the view draw those images (not just the anchor section's) so a figure
    /// near a section boundary scrolls smoothly instead of leaving a blank gap
    /// until its section becomes the anchor. Rebuilt each frame from the sections
    /// on screen.
    pub following: HashMap<usize, Vec<(ImgKey, u16)>>,
    /// The image geometry + font cell size (`(fw, fh)`) captured by `sync_images`,
    /// so `following_lines` can size a newly-shown continuous section's figures on
    /// demand (no frame lag). `None` until the first `sync_images`.
    pub geom: Option<crate::app::reader::ImageGeom>,
    pub fs: (u16, u16),
}

impl Default for ImageState {
    fn default() -> Self {
        Self {
            cache: LruCache::new(NonZeroUsize::new(IMAGE_CACHE_CAP).unwrap()),
            section_images: HashMap::new(),
            section_inline: HashMap::new(),
            inline_cols: Vec::new(),
            inline_rows: Vec::new(),
            rows_estimate: Vec::new(),
            images_key: (usize::MAX, 0, 0, 0, 0, 0, media::ImageFit::default()),
            policy: media::RenderPolicy {
                tint: media::Ink {
                    ink: [0, 0, 0],
                    paper: [255, 255, 255],
                },
                mode: media::ImageMode::default(),
            },
            requested: HashSet::new(),
            failed: HashSet::new(),
            following: HashMap::new(),
            geom: None,
            fs: (0, 0),
        }
    }
}
