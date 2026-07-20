//! Reader image lifecycle: collect finished background builds, estimate rows
//! for reflow, dispatch protocol builds, remap on section change, prefetch
//! neighbours, and report pending builds.

use std::num::NonZeroUsize;

use ratatui_image::picker::Picker;

use super::*;
use crate::app::IMAGE_CACHE_CAP;
use crate::media::{ImageBuilder, ImagePlan, ImgKey};

/// Cap on how many **distinct** inline-math images one section may upload. Identical images
/// are deduped (a repeated symbol uploads once), so this only bounds genuinely-different
/// inline equations. A dense math chapter legitimately has hundreds (this book runs ~320–500
/// distinct per chapter, every symbol a separate image), so the cap is generous — high enough
/// that a real book never loses equations, still a backstop against a truly pathological file.
/// Off-screen atoms build in the background pool and only upload when scrolled into view, so
/// the cost scales with what's read, not the raw count. Extras past the cap fall back to text.
const MAX_INLINE_ATOMS: usize = 1024;

/// A content-address for an inline atom: a hash of its rendered bytes, so identical images
/// (the same symbol repeated across a section) share one build, one upload, and one cache
/// slot instead of one per occurrence.
fn inline_content_key(png: &[u8]) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    png.hash(&mut h);
    h.finish() as usize
}

/// How far a single inline equation's measured em may sit from the section median before it
/// is clamped toward it. Tighter than the display-equation band ([`EM_CLAMP_*`]): the inline
/// ink measurement is noisier (subscripts/superscripts/fractions throw the per-image
/// cap-height off), and this book — like most — sets all its math at one size, so pulling
/// outliers close to the median is what makes inline equations flow at a consistent size.
const INLINE_EM_CLAMP_LO: f32 = 0.82;
const INLINE_EM_CLAMP_HI: f32 = 1.20;

/// The median of `vals` (sorted in place); `None` if empty. Robust to the per-image em
/// measurement noise, so one bad reading can't move the section's normalisation size.
fn median(vals: &mut [f32]) -> Option<f32> {
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(vals[vals.len() / 2])
}

/// Map a block's authored width, math flag, and caption presence to the media
/// layer's sizing intent. A caption is the reliable figure/table-vs-equation
/// signal: figures and tables are captioned and normalize to the column band;
/// equation pictures are uncaptioned and stay text-proportional.
fn size_spec(
    width: delryn_model::ImageWidth,
    math: bool,
    captioned: bool,
    alt_math: bool,
    ink: Option<delryn_model::InkProfile>,
) -> media::SizeSpec {
    let hint = match width {
        delryn_model::ImageWidth::Auto => media::SizeHint::Auto,
        delryn_model::ImageWidth::Pct(p) => media::SizeHint::Pct(p),
        delryn_model::ImageWidth::Px(px) => media::SizeHint::Px(px),
        delryn_model::ImageWidth::Em(em) => media::SizeHint::Em(em),
        delryn_model::ImageWidth::Full => media::SizeHint::Full,
    };
    media::SizeSpec {
        hint,
        math,
        captioned,
        alt_math,
        // Block figures/equations are never the inline (mid-line) kind; inline math
        // builds its own `SizeSpec` in `remap_inline_math`.
        inline: false,
        // Cross the crate boundary: the content model's dependency-free profile
        // becomes the media crate's (mirrors ImageWidth -> SizeHint above).
        ink: ink.map(|p| media::InkProfile {
            x0: p.x0,
            y0: p.y0,
            x1: p.x1,
            y1: p.y1,
            line_px: p.line_px,
            line_count: p.line_count,
        }),
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
    pub math_scale: u16,
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
        let mut visible: Vec<ImgKey> = self.images.section_images.values().copied().collect();
        visible.extend(self.images.section_inline.values().copied());
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
                    // The `InlineDeck` owns the terminal image lifecycle now: it frees a
                    // resident image when its key leaves the screen. So an eviction from
                    // this LRU only drops the PNG payload — no `d=I` needed here.
                    self.images.cache.push(done.key, plan);
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
            geom.math_scale,
            geom.fit_mode,
        );
        if self.images.images_key != key || self.images.policy != geom.policy {
            self.images.images_key = key;
            self.images.policy = geom.policy;
            self.remap_section_images(builder, picker, geom);
            self.remap_inline_math(builder, picker, geom);
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
        if self.reflow_flows() {
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
                alt,
                math,
                width,
                caption,
                ink,
                ..
            } = block
            {
                let spec = size_spec(
                    *width,
                    *math,
                    !caption.is_empty(),
                    delryn_model::math::is_math(alt),
                    *ink,
                );
                let key = ImgKey {
                    kind: media::ImgSlot::Figure,
                    section,
                    idx,
                    avail: geom.avail,
                    max_rows: geom.max_rows,
                    max_px: geom.max_px,
                    target_pct: geom.width_pct,
                    math_scale: geom.math_scale,
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
                    math_scale: geom.math_scale,
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
                alt,
                math,
                width,
                caption,
                ink,
                ..
            } = block
            {
                let spec = size_spec(
                    *width,
                    *math,
                    !caption.is_empty(),
                    delryn_model::math::is_math(alt),
                    *ink,
                );
                let key = ImgKey {
                    kind: media::ImgSlot::Figure,
                    section: self.section,
                    idx,
                    avail: geom.avail,
                    max_rows: geom.max_rows,
                    max_px: geom.max_px,
                    target_pct: geom.width_pct,
                    math_scale: geom.math_scale,
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
                    math_scale: geom.math_scale,
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

    /// Map the current section's **inline** math runs (the atoms `convert_inline_math`
    /// rendered) to cache keys, estimate each atom's reserved cell width for the
    /// wrapper (`inline_cols`, keyed by the run's section-local id), and request any
    /// builds not already cached / in-flight. The inline analogue of
    /// [`remap_section_images`], in its own [`media::ImgSlot::InlineMath`] index space
    /// so a figure's `idx` and an equation's `id` never collide. The `id` is the one
    /// `convert_inline_math` stamped on the run, so the width estimate here, the
    /// wrapper's atom reservation, and the draw all agree. Called right after
    /// `remap_section_images` (which sized the cache to the figures); this grows it
    /// again to fit the inline equations too.
    fn remap_inline_math(&mut self, builder: &ImageBuilder, picker: &Picker, geom: ImageGeom) {
        let fs = picker.font_size();
        let (fw, fh) = (fs.width, fs.height);
        let fit = media::FitBox {
            fw,
            fh,
            cols: geom.avail,
            rows: geom.max_rows,
            max_px: geom.max_px,
            target_pct: geom.width_pct,
            math_scale: geom.math_scale,
            fit_mode: geom.fit_mode,
        };
        let mut section_inline = HashMap::new();
        let mut cols_by_id: Vec<u16> = Vec::new();
        let mut rows_by_id: Vec<u16> = Vec::new();
        // Inline builds carry the already-decoded image (decoded once here for ink), so the
        // worker doesn't re-decode the JPEG.
        let mut requests: Vec<(ImgKey, media::DynamicImage, media::SizeSpec)> = Vec::new();
        // Content-address the atoms: an inline image is keyed by *what it is*, not where it
        // occurs, so the same symbol repeated across the section (a book that ships ℝ as its
        // own tiny `<img>` uses it hundreds of times) is built and uploaded **once** and every
        // occurrence places that single image. `distinct` also bounds how many unique inline
        // images a section may upload — beyond the cap the extra ones fall back to their text
        // floor (cols = 0), so a pathological page can never flood the terminal.
        // Collect every atom run (recursing into callout/footnote bodies, so a note's inline
        // math is sized/built too). The borrow of `self.blocks` (via the slices) stays
        // disjoint from the `self.images` we read/mutate below.
        let mut runs: Vec<&[delryn_model::Span]> = Vec::new();
        for block in &self.blocks {
            block.collect_span_runs(&mut runs);
        }
        let section = self.section;
        let key_for = |ck: usize| ImgKey {
            kind: media::ImgSlot::InlineMath,
            section,
            idx: ck,
            avail: geom.avail,
            max_rows: geom.max_rows,
            max_px: geom.max_px,
            target_pct: geom.width_pct,
            math_scale: geom.math_scale,
            fit_mode: geom.fit_mode,
            policy: geom.policy,
        };

        // Pass 1 — measure each **distinct** atom's ink once (decoding the JPEG at most once,
        // cached by content-key), collect the section's measured ems, and keep the decoded
        // image for any atom that still needs building (so the worker never re-decodes it).
        // `order` is the first-seen order, for the distinct-upload cap.
        let mut ems: Vec<f32> = Vec::new();
        let mut order: Vec<usize> = Vec::new();
        let mut pending: HashMap<usize, (ImgKey, media::DynamicImage)> = HashMap::new();
        let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for spans in &runs {
            for span in *spans {
                let Some(delryn_model::SpanMath::Raster { png, .. }) = &span.math else {
                    continue;
                };
                let ck = inline_content_key(png);
                if !seen.insert(ck) {
                    continue; // distinct only
                }
                order.push(ck);
                let key = key_for(ck);
                let need_build = !self.images.cache.contains(&key)
                    && !self.images.requested.contains(&key)
                    && !self.images.failed.contains(&key);
                let need_ink = !self.images.inline_ink.contains_key(&ck);
                let decoded = if need_ink || need_build {
                    media::decode(png)
                } else {
                    None
                };
                let ink = *self
                    .images
                    .inline_ink
                    .entry(ck)
                    .or_insert_with(|| decoded.as_ref().and_then(media::ink_profile));
                if let Some(p) = ink {
                    ems.push(p.line_px);
                }
                if need_build && let Some(img) = decoded {
                    pending.insert(ck, (key, img));
                }
            }
        }
        // Robust section em: the median of the distinct atoms' measured ems. The whole book is
        // set at one font size, so one normalisation em is correct; the median shrugs off the
        // per-image ink-measurement noise (a `lim` stack measures too tall, a fraction too
        // short) that otherwise makes lone equations render huge or tiny. Each atom's own em is
        // then *clamped* toward it (Pass 2), so a well-measured equation keeps its exact size
        // while an outlier is reined in. Needs a few samples to be meaningful.
        let section_em = (ems.len() >= 3).then(|| median(&mut ems)).flatten();

        // Pass 2 — reserve cells and dispatch builds, sizing each atom against the section em.
        let cap: std::collections::HashSet<usize> =
            order.iter().take(MAX_INLINE_ATOMS).copied().collect();
        for spans in &runs {
            for span in *spans {
                let Some(delryn_model::SpanMath::Raster { id, png }) = &span.math else {
                    continue;
                };
                let ck = inline_content_key(png);
                let capped = !cap.contains(&ck);
                // Size on the measured ink, but with the em clamped toward the section median
                // so every inline equation flows at the same prose size regardless of the
                // per-image measurement noise. Keep each raster's own ink bbox (a fraction is
                // still taller than a symbol) and DPI (the clamp only reins in outliers).
                let ink = if capped {
                    None
                } else {
                    self.images
                        .inline_ink
                        .get(&ck)
                        .copied()
                        .flatten()
                        .map(|mut p| {
                            if let Some(em) = section_em {
                                p.line_px = p
                                    .line_px
                                    .clamp(em * INLINE_EM_CLAMP_LO, em * INLINE_EM_CLAMP_HI);
                            }
                            p
                        })
                };
                let spec = media::SizeSpec {
                    inline: true,
                    math: true,
                    ink,
                    ..Default::default()
                };
                // The atom's reserved width *and height* — the same `target_cells` (and thus
                // `inline_fit`) the build uses, so the wrapper's reservation (columns, and the
                // spacer rows for a centred fraction) matches the drawn raster exactly.
                let (cols, rows) = if capped {
                    (0, 1)
                } else {
                    media::image_dimensions(png)
                        .map(|(w, h)| media::target_cells(w, h, fit, spec))
                        .unwrap_or((0, 1))
                };
                if *id >= cols_by_id.len() {
                    cols_by_id.resize(*id + 1, 0);
                    rows_by_id.resize(*id + 1, 1);
                }
                cols_by_id[*id] = cols;
                rows_by_id[*id] = rows;
                if cols > 0 {
                    section_inline.insert(*id, key_for(ck));
                    // Hand the worker the already-decoded image kept in Pass 1 (once per
                    // distinct atom — later occurrences find it already taken).
                    if let Some((k, img)) = pending.remove(&ck) {
                        requests.push((k, img, spec));
                    }
                }
            }
        }
        let distinct_count = cap.len();
        self.images.section_inline = section_inline;
        self.images.inline_cols = cols_by_id;
        self.images.inline_rows = rows_by_id;
        // Grow the cache so the section's figures *and* its **distinct** inline equations all
        // fit, keeping `IMAGE_CACHE_CAP` spare for neighbour prefetch (grow only).
        let needed = self
            .images
            .section_images
            .len()
            .saturating_add(distinct_count)
            .saturating_add(IMAGE_CACHE_CAP);
        if self.images.cache.cap().get() < needed
            && let Some(cap) = NonZeroUsize::new(needed)
        {
            self.images.cache.resize(cap);
        }
        for (k, img, spec) in requests {
            self.images.requested.insert(k);
            builder.request_decoded(k, img, spec);
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
                alt,
                math,
                width,
                caption,
                ink,
                ..
            } = block
            {
                if !data.is_empty() {
                    let key = ImgKey {
                        kind: media::ImgSlot::Figure,
                        section,
                        idx,
                        avail: geom.avail,
                        max_rows: geom.max_rows,
                        max_px: geom.max_px,
                        target_pct: geom.width_pct,
                        math_scale: geom.math_scale,
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
                            size_spec(
                                *width,
                                *math,
                                !caption.is_empty(),
                                delryn_model::math::is_math(alt),
                                *ink,
                            ),
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

    /// The cache key for the current section's figure `idx` (its deck identity).
    pub fn image_key(&self, idx: usize) -> Option<ImgKey> {
        self.images.section_images.get(&idx).copied()
    }

    /// Look up a built plan for the current section's inline-math atom `id` — the
    /// small equation raster the reader paints over the atom's reserved cells.
    pub fn inline_math_plan(&self, id: usize) -> Option<&ImagePlan> {
        let key = self.images.section_inline.get(&id)?;
        self.images.cache.peek(key)
    }

    /// The cache key for the current section's inline-math atom `id` (its deck identity).
    pub fn inline_math_key(&self, id: usize) -> Option<ImgKey> {
        self.images.section_inline.get(&id).copied()
    }

    /// The current section's inline-math atom widths (by id), for the wrapper.
    pub fn inline_math_cols(&self) -> &[u16] {
        &self.images.inline_cols
    }

    /// The built plan for a specific cache key — a *following* section's image in
    /// continuous mode (see [`Reader::continuous_following_images`]). Peek (no LRU
    /// touch); `None` until it's built.
    pub fn image_plan_by_key(&self, key: &ImgKey) -> Option<&ImagePlan> {
        self.images.cache.peek(key)
    }

    /// The built PNG for a cache key — the `InlineDeck`'s transmit payload. `None`
    /// until the background build lands (the deck retries next frame). Peek (no LRU
    /// touch), cloned since the deck writes it to the terminal outside the borrow.
    pub fn image_png(&self, key: ImgKey) -> Option<Vec<u8>> {
        self.images.cache.peek(&key).map(|p| p.png.clone())
    }

    /// Record one inline-image placement target for this frame (the view collects them
    /// during render; `App::inline_escapes` drains and reconciles them via the deck).
    pub fn push_inline_target(&self, target: crate::app::inline_deck::InlineTarget) {
        self.inline_targets.borrow_mut().push(target);
    }

    /// Drain this frame's collected inline-image targets.
    pub fn take_inline_targets(&self) -> Vec<crate::app::inline_deck::InlineTarget> {
        std::mem::take(&mut self.inline_targets.borrow_mut())
    }

    /// Start a fresh frame's inline-target collection (clears the previous frame's).
    pub fn begin_inline_frame(&self) {
        self.inline_targets.borrow_mut().clear();
    }

    /// Whether the inline deck must be fully cleared before this frame's placements
    /// (a restage dropped the built PNGs, so the terminal images must re-transmit).
    pub fn take_inline_clear(&self) -> bool {
        self.inline_needs_clear.replace(false)
    }

    /// Drain terminal image ids that should be deleted (evicted from cache).
    pub fn take_image_deletes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending_deletes)
    }

    /// Returning from the full-screen image viewer: drop the current section's
    /// cached image protocols (freeing their terminal ids) and invalidate the remap
    /// key so `sync_images` rebuilds them with **fresh** ids and re-transmits.
    ///
    /// The viewer's large figure can push the terminal past its graphics budget and
    /// evict the reader's inline images from the terminal's own store; transmit-once
    /// would then never re-send them, leaving them blank until an unrelated redraw
    /// churned the cache. Rebuilding under a new id is the only recovery that works
    /// on terminals that ignore a re-transmit of the same id (Ghostty #6711), and
    /// `images_pending()` keeps the loop drawing until they land — no keypress needed.
    pub fn restage_visible_images(&mut self) {
        let keys: Vec<ImgKey> = self
            .images
            .section_images
            .values()
            .chain(self.images.section_inline.values())
            .copied()
            .collect();
        for k in keys {
            self.images.cache.pop(&k);
            self.images.requested.remove(&k);
            self.images.failed.remove(&k);
        }
        // Free every terminal-resident image next frame (deck `clear`) so the rebuilt
        // images re-transmit under fresh ids instead of being assumed still resident.
        self.inline_needs_clear.set(true);
        // Force the next `sync_images` to re-remap this section (re-dispatching the
        // builds that were just dropped) even when nothing else changed.
        self.images.images_key.0 = usize::MAX;
    }

    /// Drop every built image and its remap state — for leaving the reader. The
    /// terminal-resident images are freed by the deck (it sees no targets / a clear).
    pub fn evict_all_images(&mut self) {
        self.images.cache.clear();
        self.images.section_images.clear();
        self.images.section_inline.clear();
        self.images.following.clear();
        self.images.requested.clear();
        self.images.failed.clear();
        self.images.images_key.0 = usize::MAX;
    }

    /// Are any of the current section's images (figures or inline equations) still
    /// building (so the loop should keep redrawing until they pop in)?
    pub fn images_pending(&self) -> bool {
        let unbuilt =
            |k: &ImgKey| !self.images.cache.contains(k) && !self.images.failed.contains(k);
        let figures = self
            .images
            .rows_estimate
            .iter()
            .enumerate()
            .any(|(i, &rows)| rows > 0 && self.images.section_images.get(&i).is_some_and(unbuilt));
        let inline = self
            .images
            .inline_cols
            .iter()
            .enumerate()
            .any(|(id, &cols)| {
                cols > 0 && self.images.section_inline.get(&id).is_some_and(unbuilt)
            });
        figures || inline
    }
}
