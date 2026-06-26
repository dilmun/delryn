//! Reader image lifecycle: collect finished background builds, estimate rows
//! for reflow, dispatch protocol builds, remap on section change, prefetch
//! neighbours, and report pending builds.

use super::*;

/// Map a block's authored width + math flag to the media layer's sizing intent.
fn size_spec(width: delryn_model::ImageWidth, math: bool) -> media::SizeSpec {
    let hint = match width {
        delryn_model::ImageWidth::Auto => media::SizeHint::Auto,
        delryn_model::ImageWidth::Pct(p) => media::SizeHint::Pct(p),
        delryn_model::ImageWidth::Px(px) => media::SizeHint::Px(px),
        delryn_model::ImageWidth::Full => media::SizeHint::Full,
    };
    media::SizeSpec { hint, math }
}

impl Reader {
    /// Collect any finished background image builds, and — when the section or
    /// size changes — estimate each image's rows (cheaply, for reflow) and
    /// dispatch the protocol builds to the worker. Never blocks on encoding.
    /// `width_pct` is the default figure width (% of column) for unsized images.
    #[allow(clippy::too_many_arguments)]
    pub fn sync_images(
        &mut self,
        builder: &ImageBuilder,
        picker: &Picker,
        avail: u16,
        max_rows: u16,
        max_px: u16,
        width_pct: u16,
        policy: media::RenderPolicy,
    ) {
        // Pick up any sections the background loader has finished — neighbours
        // are requested on navigation but only land in the cache when drained.
        // A two-page spread needs the facing section's blocks *now* (not just on
        // the next navigation) so its page image can build; without this the
        // facing page never appears.
        self.drain_loader();

        // Tell the worker where we are so it can drop builds for far-away
        // sections (avoids a fast-scroll backlog delaying the current one).
        builder.set_current(self.section);

        // 1. Move finished builds into the cache; evictions free the terminal image.
        for done in builder.poll() {
            self.img_requested.remove(&done.key);
            if done.stale {
                continue; // skipped as far-away; re-requested if it's needed again
            }
            match done.plan {
                Some(plan) => {
                    if let Some((_, evicted)) = self.image_cache.push(done.key, plan)
                        && let Some(id) = evicted.image_id()
                    {
                        self.pending_deletes.push(id);
                    }
                }
                None => {
                    self.img_failed.insert(done.key);
                }
            }
        }

        // 2. On section/size change, remap the current section and dispatch any
        //    builds it still needs.
        let key = (self.section, avail, max_rows, max_px, width_pct);
        if self.images_key != key || self.images_policy != policy {
            self.images_key = key;
            self.images_policy = policy;
            self.remap_section_images(builder, picker, avail, max_rows, max_px, width_pct, policy);
        }

        // 2b. In a two-page spread the facing page (next section) is on screen,
        //     so build it at the same priority as the current page — not deferred
        //     like an off-screen prefetch — so the right page appears with the left
        //     instead of lagging a beat behind it.
        if self.spread {
            self.request_section_image_builds(
                self.section + 1,
                builder,
                avail,
                max_rows,
                max_px,
                width_pct,
                policy,
            );
        }

        // 3. Keep the visible images most-recently-used so they aren't evicted
        //    while on screen: the current section's images, plus the facing page
        //    of a spread (which lives outside `section_images`, so it would
        //    otherwise churn and flicker as look-ahead builds land).
        let keys: Vec<ImgKey> = self.section_images.values().copied().collect();
        for k in keys {
            self.image_cache.get(&k);
        }
        if self.spread {
            let facing = self.page_key(self.section + 1);
            self.image_cache.get(&facing);
        }

        // 4. Pre-build neighbouring sections' images once the current one is ready
        //    (one section further ahead in a spread, so the next turn is instant).
        if !self.images_pending() {
            self.prefetch_neighbor_images(builder, avail, max_rows, max_px, width_pct, policy);
        }
    }

    /// Map the current section's images to cache keys, estimate their rows for
    /// reflow, and request builds for any not already cached/in-flight/failed.
    #[allow(clippy::too_many_arguments)]
    fn remap_section_images(
        &mut self,
        builder: &ImageBuilder,
        picker: &Picker,
        avail: u16,
        max_rows: u16,
        max_px: u16,
        width_pct: u16,
        policy: media::RenderPolicy,
    ) {
        // A failed build is only blacklisted until the next remap (section change,
        // resize, theme/mode toggle): clear it so a *transient* failure (e.g. the
        // protocol upload losing under load) recovers on its own instead of
        // staying blank until the app is restarted.
        self.img_failed.clear();

        let fs = picker.font_size();
        let (fw, fh) = (fs.width, fs.height);
        let mut section_images = HashMap::new();
        let mut estimates = Vec::new();
        let mut requests: Vec<(ImgKey, Vec<u8>, media::SizeSpec)> = Vec::new();
        let mut idx = 0;
        for block in &self.blocks {
            if let Block::Image {
                data, math, width, ..
            } = block
            {
                let spec = size_spec(*width, *math);
                let key = ImgKey {
                    section: self.section,
                    idx,
                    avail,
                    max_rows,
                    max_px,
                    target_pct: width_pct,
                    policy,
                };
                let fit = media::FitBox {
                    fw,
                    fh,
                    cols: avail,
                    rows: max_rows,
                    max_px,
                    target_pct: width_pct,
                };
                let rows = if let Some(plan) = self.image_cache.peek(&key) {
                    plan.rows
                } else if data.is_empty() {
                    0
                } else {
                    media::image_dimensions(data)
                        .map(|(w, h)| media::target_cells(w, h, fit, spec).1)
                        .unwrap_or(0)
                };
                estimates.push(rows);
                section_images.insert(idx, key);
                if rows > 0
                    && !self.image_cache.contains(&key)
                    && !self.img_requested.contains(&key)
                    && !self.img_failed.contains(&key)
                {
                    requests.push((key, data.clone(), spec));
                }
                idx += 1;
            }
        }
        self.section_images = section_images;
        self.image_rows_estimate = estimates;
        // Make sure the whole current section fits in the cache (math chapters
        // are one big section with dozens of equations); otherwise neighbour
        // prefetch evicts on-screen equations and they render as blank gaps. Grow
        // only — keep `IMAGE_CACHE_CAP` spare slots for that prefetch.
        let needed = self.section_images.len().saturating_add(IMAGE_CACHE_CAP);
        if self.image_cache.cap().get() < needed
            && let Some(cap) = NonZeroUsize::new(needed)
        {
            self.image_cache.resize(cap);
        }
        for (k, bytes, spec) in requests {
            self.img_requested.insert(k);
            builder.request(k, bytes, spec);
        }
    }

    /// Build adjacent sections' images ahead of time (from already-prefetched
    /// blocks) so a page turn / chapter crossing is instant. Never forces a load.
    /// In a spread the facing page (section + 1) is built eagerly elsewhere, so
    /// look one section *further* ahead here, so the next turn is already warm.
    fn prefetch_neighbor_images(
        &mut self,
        builder: &ImageBuilder,
        avail: u16,
        max_rows: u16,
        max_px: u16,
        width_pct: u16,
        policy: media::RenderPolicy,
    ) {
        let ahead = if self.spread {
            self.section + 2
        } else {
            self.section + 1
        };
        for sec in [ahead, self.section.wrapping_sub(1)] {
            self.request_section_image_builds(
                sec, builder, avail, max_rows, max_px, width_pct, policy,
            );
        }
    }

    /// Request background builds for `section`'s images that aren't already
    /// built / in-flight / failed, from its cached blocks, at the given geometry.
    /// A no-op when the section is out of range, is the current one (handled by
    /// `remap_section_images`), or its blocks aren't loaded yet. Shared by the
    /// facing-page eager build and neighbour/look-ahead prefetch.
    // The geometry args mirror the `sync_images` frame parameters; bundling them
    // into a struct is tracked as tech debt (TODO.md) across this image pipeline.
    #[allow(clippy::too_many_arguments)]
    fn request_section_image_builds(
        &mut self,
        section: usize,
        builder: &ImageBuilder,
        avail: u16,
        max_rows: u16,
        max_px: u16,
        width_pct: u16,
        policy: media::RenderPolicy,
    ) {
        if section >= self.doc.section_count() || section == self.section {
            return;
        }
        let Some(blocks) = self.cache.get(&section) else {
            return;
        };
        let mut requests: Vec<(ImgKey, Vec<u8>, media::SizeSpec)> = Vec::new();
        let mut idx = 0;
        for block in blocks {
            if let Block::Image {
                data, math, width, ..
            } = block
            {
                if !data.is_empty() {
                    let key = ImgKey {
                        section,
                        idx,
                        avail,
                        max_rows,
                        max_px,
                        target_pct: width_pct,
                        policy,
                    };
                    if !self.image_cache.contains(&key)
                        && !self.img_requested.contains(&key)
                        && !self.img_failed.contains(&key)
                    {
                        requests.push((key, data.clone(), size_spec(*width, *math)));
                    }
                }
                idx += 1;
            }
        }
        for (k, bytes, spec) in requests {
            self.img_requested.insert(k);
            builder.request(k, bytes, spec);
        }
    }

    /// Look up a built plan for the current section's image `idx`.
    pub fn image_plan(&self, idx: usize) -> Option<&ImagePlan> {
        let key = self.section_images.get(&idx)?;
        self.image_cache.peek(key)
    }

    /// Drain terminal image ids that should be deleted (evicted from cache).
    pub fn take_image_deletes(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending_deletes)
    }

    /// Are any of the current section's images still building (so the loop
    /// should keep redrawing until they pop in)?
    pub fn images_pending(&self) -> bool {
        self.image_rows_estimate
            .iter()
            .enumerate()
            .any(|(i, &rows)| {
                rows > 0
                    && self.section_images.get(&i).is_some_and(|k| {
                        !self.image_cache.contains(k) && !self.img_failed.contains(k)
                    })
            })
    }
}
