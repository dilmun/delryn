//! Reader image lifecycle: collect finished background builds, estimate rows
//! for reflow, dispatch protocol builds, remap on section change, prefetch
//! neighbours, and report pending builds.

use std::num::NonZeroUsize;

use ratatui_image::picker::Picker;

use super::*;
use crate::app::IMAGE_CACHE_CAP;
use crate::media::{ImageBuilder, ImagePlan, ImgKey};

/// Map a block's authored width, math flag, and caption presence to the media
/// layer's sizing intent. A caption is the reliable figure/table-vs-equation
/// signal: figures and tables are captioned and normalize to the column band;
/// equation pictures are uncaptioned and stay text-proportional.
fn size_spec(width: delryn_model::ImageWidth, math: bool, captioned: bool) -> media::SizeSpec {
    let hint = match width {
        delryn_model::ImageWidth::Auto => media::SizeHint::Auto,
        delryn_model::ImageWidth::Pct(p) => media::SizeHint::Pct(p),
        delryn_model::ImageWidth::Px(px) => media::SizeHint::Px(px),
        delryn_model::ImageWidth::Full => media::SizeHint::Full,
    };
    media::SizeSpec {
        hint,
        math,
        captioned,
    }
}

/// The geometry + render policy that sizes a section's images for the current
/// frame: column width (`avail`), the row/pixel caps, the default figure width %
/// for unsized figures, and the theme render policy. Bundled so the image-
/// lifecycle methods take one argument instead of five — the same values that
/// key an [`ImgKey`].
#[derive(Clone, Copy, PartialEq)]
pub struct ImageGeom {
    pub avail: u16,
    pub max_rows: u16,
    pub max_px: u16,
    pub width_pct: u16,
    pub eq_scale: u16,
    pub fit_mode: media::ImageFit,
    pub policy: media::RenderPolicy,
}

impl Reader {
    /// Collect any finished background image builds, and — when the section or
    /// size changes — estimate each image's rows (cheaply, for reflow) and
    /// dispatch the protocol builds to the worker. Never blocks on encoding.
    /// `width_pct` is the default figure width (% of column) for unsized images.
    pub fn sync_images(&mut self, builder: &ImageBuilder, picker: &Picker, geom: ImageGeom) {
        // Pick up any sections the background loader has finished — neighbours
        // are requested on navigation but only land in the cache when drained.
        // A two-page spread needs the facing section's blocks *now* (not just on
        // the next navigation) so its page image can build; without this the
        // facing page never appears.
        self.drain_loader();

        // Tell the worker where we are so it can drop builds for far-away
        // sections (avoids a fast-scroll backlog delaying the current one).
        builder.set_current(self.section);

        // 1. Protect the images currently on screen from eviction *before* draining
        //    new builds. A figure/equation-dense neighbour can finish >IMAGE_CACHE_CAP
        //    builds in one poll drain; pushing them would walk past the cache's spare
        //    headroom and evict a *visible* image — deleting it from the terminal
        //    mid-scroll — since marking it most-recently-used only happened afterwards.
        //    Touch them first so the eviction victims are the off-screen prefetch
        //    entries. (An evicted image also rebuilds with a fresh protocol whose
        //    transmit flag is reset, causing a re-transmit flicker.)
        let visible: Vec<ImgKey> = self.images.section_images.values().copied().collect();
        for k in &visible {
            self.images.cache.get(k);
        }

        // 2. Move finished builds into the cache; evictions free the terminal image.
        for done in builder.poll() {
            self.images.requested.remove(&done.key);
            if done.stale {
                continue; // skipped as far-away; re-requested if it's needed again
            }
            match done.plan {
                Some(plan) => {
                    // Hard guarantee: a neighbour prefetch build must never evict a
                    // current-section image. If the cache is full and this build is
                    // for another section, drop it (it's re-requested once there's
                    // room). Current-section builds always cache (evicting a
                    // neighbour, which is the least-recently-used after step 1).
                    let is_current = done.key.section == self.section;
                    if !is_current && self.images.cache.len() >= self.images.cache.cap().get() {
                        continue;
                    }
                    if let Some((_, evicted)) = self.images.cache.push(done.key, plan)
                        && let Some(id) = evicted.image_id()
                    {
                        self.pending_deletes.push(id);
                    }
                }
                None => {
                    self.images.failed.insert(done.key);
                }
            }
        }

        // 3. On section/size change, remap the current section and dispatch any
        //    builds it still needs.
        let key = (
            self.section,
            geom.avail,
            geom.max_rows,
            geom.max_px,
            geom.width_pct,
            geom.eq_scale,
            geom.fit_mode,
        );
        if self.images.images_key != key || self.images.policy != geom.policy {
            self.images.images_key = key;
            self.images.policy = geom.policy;
            self.remap_section_images(builder, picker, geom);
        }

        // 4. Pre-build neighbouring sections' images once the current one is ready.
        if !self.images_pending() {
            self.prefetch_neighbor_images(builder, geom);
        }

        // 5. Continuous mode: build the images of the *following* sections stitched
        //    into the scroll buffer so the view can draw them too — a boundary
        //    figure then scrolls smoothly rather than showing a blank gap until its
        //    section becomes the anchor. Their reserved rows are sized on demand in
        //    `following_lines` (from `images.geom`/`fs` stored here), so record the
        //    geometry and dispatch builds for the visible spans plus the next.
        if self.continuous_active() {
            let fs = picker.font_size();
            self.images.geom = Some(geom);
            self.images.fs = (fs.width, fs.height);
            let mut sections: Vec<usize> = self.cont_spans.iter().map(|(s, _)| *s).collect();
            let next = self.section + 1;
            if next < self.doc.section_count() && !sections.contains(&next) {
                sections.push(next);
            }
            for s in sections {
                self.request_section_image_builds(s, builder, geom);
            }
        } else if !self.images.following.is_empty() {
            self.images.following.clear();
        }
    }

    /// The images of `section` as `(cache key, reserved rows)` by section-local
    /// index, from its cached blocks at the current geometry. The reserved rows are
    /// the **decode-time estimate** (`target_cells`), deliberately *not* the built
    /// plan's height — a stable, up-front reservation so a following section never
    /// re-wraps (and shifts the scroll) when its images finish building. The drawn
    /// image is ≤ the estimate, so it fits within the reserved rows. Read-only
    /// (dispatches no builds). Empty if the section's blocks aren't loaded.
    pub(super) fn section_image_info(
        &self,
        section: usize,
        geom: ImageGeom,
        fw: u16,
        fh: u16,
    ) -> Vec<(ImgKey, u16)> {
        let Some(blocks) = self.sections.sections.get(&section) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut idx = 0;
        for block in blocks {
            if let Block::Image {
                data,
                math,
                width,
                caption,
                ..
            } = block
            {
                let spec = size_spec(*width, *math, !caption.is_empty());
                let key = ImgKey {
                    section,
                    idx,
                    avail: geom.avail,
                    max_rows: geom.max_rows,
                    max_px: geom.max_px,
                    target_pct: geom.width_pct,
                    eq_scale: geom.eq_scale,
                    fit_mode: geom.fit_mode,
                    policy: geom.policy,
                };
                let fit = media::FitBox {
                    fw,
                    fh,
                    cols: geom.avail,
                    rows: geom.max_rows,
                    max_px: geom.max_px,
                    target_pct: geom.width_pct,
                    eq_scale: geom.eq_scale,
                    fit_mode: geom.fit_mode,
                };
                let rows = if data.is_empty() {
                    0
                } else {
                    media::image_dimensions(data)
                        .map(|(w, h)| media::target_cells(w, h, fit, spec).1)
                        .unwrap_or(0)
                };
                out.push((key, rows));
                idx += 1;
            }
        }
        out
    }

    /// Map the current section's images to cache keys, estimate their rows for
    /// reflow, and request builds for any not already cached/in-flight/failed.
    fn remap_section_images(&mut self, builder: &ImageBuilder, picker: &Picker, geom: ImageGeom) {
        // A failed build is only blacklisted until the next remap (section change,
        // resize, theme/mode toggle): clear it so a *transient* failure (e.g. the
        // protocol upload losing under load) recovers on its own instead of
        // staying blank until the app is restarted.
        self.images.failed.clear();

        let fs = picker.font_size();
        let (fw, fh) = (fs.width, fs.height);
        let mut section_images = HashMap::new();
        let mut estimates = Vec::new();
        let mut requests: Vec<(ImgKey, Vec<u8>, media::SizeSpec)> = Vec::new();
        let mut idx = 0;
        for block in &self.blocks {
            if let Block::Image {
                data,
                math,
                width,
                caption,
                ..
            } = block
            {
                let spec = size_spec(*width, *math, !caption.is_empty());
                let key = ImgKey {
                    section: self.section,
                    idx,
                    avail: geom.avail,
                    max_rows: geom.max_rows,
                    max_px: geom.max_px,
                    target_pct: geom.width_pct,
                    eq_scale: geom.eq_scale,
                    fit_mode: geom.fit_mode,
                    policy: geom.policy,
                };
                let fit = media::FitBox {
                    fw,
                    fh,
                    cols: geom.avail,
                    rows: geom.max_rows,
                    max_px: geom.max_px,
                    target_pct: geom.width_pct,
                    eq_scale: geom.eq_scale,
                    fit_mode: geom.fit_mode,
                };
                // Reserve the stable decode estimate — deliberately not the built
                // plan's height — so the reservation never changes when the image
                // finishes building (no re-wrap, no scroll shift) and matches how a
                // *following* continuous section reserves the same content, so the
                // anchor rolling into it doesn't re-flow. The drawn image is ≤ the
                // estimate, so it fits within the reserved rows.
                let rows = if data.is_empty() {
                    0
                } else {
                    media::image_dimensions(data)
                        .map(|(w, h)| media::target_cells(w, h, fit, spec).1)
                        .unwrap_or(0)
                };
                estimates.push(rows);
                section_images.insert(idx, key);
                if rows > 0
                    && !self.images.cache.contains(&key)
                    && !self.images.requested.contains(&key)
                    && !self.images.failed.contains(&key)
                {
                    requests.push((key, data.clone(), spec));
                }
                idx += 1;
            }
        }
        self.images.section_images = section_images;
        self.images.rows_estimate = estimates;
        // Make sure the whole current section fits in the cache (math chapters
        // are one big section with dozens of equations); otherwise neighbour
        // prefetch evicts on-screen equations and they render as blank gaps. Grow
        // only — keep `IMAGE_CACHE_CAP` spare slots for that prefetch.
        let needed = self
            .images
            .section_images
            .len()
            .saturating_add(IMAGE_CACHE_CAP);
        if self.images.cache.cap().get() < needed
            && let Some(cap) = NonZeroUsize::new(needed)
        {
            self.images.cache.resize(cap);
        }
        for (k, bytes, spec) in requests {
            self.images.requested.insert(k);
            builder.request(k, bytes, spec);
        }
    }

    /// Build adjacent sections' images ahead of time (from already-prefetched
    /// blocks) so a page turn / chapter crossing is instant. Never forces a load.
    fn prefetch_neighbor_images(&mut self, builder: &ImageBuilder, geom: ImageGeom) {
        for sec in [self.section + 1, self.section.wrapping_sub(1)] {
            self.request_section_image_builds(sec, builder, geom);
        }
    }

    /// Request background builds for `section`'s images that aren't already
    /// built / in-flight / failed, from its cached blocks, at the given geometry.
    /// A no-op when the section is out of range, is the current one (handled by
    /// `remap_section_images`), or its blocks aren't loaded yet. Shared by the
    /// facing-page eager build and neighbour/look-ahead prefetch.
    pub(super) fn request_section_image_builds(
        &mut self,
        section: usize,
        builder: &ImageBuilder,
        geom: ImageGeom,
    ) {
        if section >= self.doc.section_count() || section == self.section {
            return;
        }
        let Some(blocks) = self.sections.sections.get(&section) else {
            return;
        };
        // Bound neighbour prefetch to the cache's spare room. The cache is sized to
        // the current section + `IMAGE_CACHE_CAP` spare; an *image-dense* neighbour
        // (a stats textbook can have 60+ figures/equations per section) would
        // otherwise flood the cache and evict the current section's *visible*
        // images — they'd then never rebuild until a section change, leaving blanks.
        // The current section's own builds go through `remap_section_images`, which
        // is never bounded. A neighbour build that doesn't fit is simply requested
        // later, once the cache frees room / that section becomes current.
        let cap = self.images.cache.cap().get();
        let mut budget = cap.saturating_sub(self.images.cache.len() + self.images.requested.len());
        if budget == 0 {
            return;
        }
        let mut requests: Vec<(ImgKey, Vec<u8>, media::SizeSpec)> = Vec::new();
        let mut idx = 0;
        for block in blocks {
            if let Block::Image {
                data,
                math,
                width,
                caption,
                ..
            } = block
            {
                if !data.is_empty() {
                    let key = ImgKey {
                        section,
                        idx,
                        avail: geom.avail,
                        max_rows: geom.max_rows,
                        max_px: geom.max_px,
                        target_pct: geom.width_pct,
                        eq_scale: geom.eq_scale,
                        fit_mode: geom.fit_mode,
                        policy: geom.policy,
                    };
                    if !self.images.cache.contains(&key)
                        && !self.images.requested.contains(&key)
                        && !self.images.failed.contains(&key)
                    {
                        requests.push((
                            key,
                            data.clone(),
                            size_spec(*width, *math, !caption.is_empty()),
                        ));
                        budget -= 1;
                        if budget == 0 {
                            break; // spare exhausted — prefetch the rest on reveal
                        }
                    }
                }
                idx += 1;
            }
        }
        for (k, bytes, spec) in requests {
            self.images.requested.insert(k);
            builder.request(k, bytes, spec);
        }
    }

    /// Look up a built plan for the current section's image `idx`.
    pub fn image_plan(&self, idx: usize) -> Option<&ImagePlan> {
        let key = self.images.section_images.get(&idx)?;
        self.images.cache.peek(key)
    }

    /// The built plan for a specific cache key — a *following* section's image in
    /// continuous mode (see [`Reader::continuous_following_images`]). Peek (no LRU
    /// touch); `None` until it's built.
    pub fn image_plan_by_key(&self, key: &ImgKey) -> Option<&ImagePlan> {
        self.images.cache.peek(key)
    }

    /// Drain terminal image ids that should be deleted (evicted from cache).
    pub fn take_image_deletes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending_deletes)
    }

    /// Are any of the current section's images still building (so the loop
    /// should keep redrawing until they pop in)?
    pub fn images_pending(&self) -> bool {
        self.images
            .rows_estimate
            .iter()
            .enumerate()
            .any(|(i, &rows)| {
                rows > 0
                    && self.images.section_images.get(&i).is_some_and(|k| {
                        !self.images.cache.contains(k) && !self.images.failed.contains(k)
                    })
            })
    }
}
