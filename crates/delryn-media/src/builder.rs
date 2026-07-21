//! Off-thread worker that builds inline figure image protocols for the reader.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use delryn_infra::config::ImageFit;
use image::GenericImageView;
use ratatui_image::picker::Picker;

use crate::decode::decode;
use crate::recolor::{RenderPolicy, render_for_theme};
use crate::sizing::{FitBox, SizeHint, SizeSpec, target_cells};

/// Which per-section index space a built image belongs to, so a figure's `idx` and
/// an inline equation's `id` — both section-local and both starting at 0 — never
/// collide in the shared cache.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ImgSlot {
    /// A block-level figure / equation raster (row-reserving).
    Figure,
    /// A small inline equation drawn mid-line.
    InlineMath,
}

/// Identifies one built image so it can be cached and reused across sections
/// (revisiting a section reuses the already-uploaded image — no re-transmit).
/// Equal keys ⇒ identical build, so they share one cache entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImgKey {
    /// Which index space `idx` counts in (figures vs. inline math).
    pub kind: ImgSlot,
    pub section: usize,
    pub idx: usize,
    pub avail: u16,
    pub max_rows: u16,
    pub max_px: u16,
    /// Default figure width (% of column) — part of the key so changing the knob
    /// rebuilds at the new size instead of serving a stale one.
    pub target_pct: u16,
    /// Equation-picture size (% of auto) — part of the key so changing the knob
    /// rebuilds at the new size instead of serving a stale one.
    pub math_scale: u16,
    /// Sizing policy (normalize vs. faithful) — part of the key so toggling it
    /// rebuilds at the new size rather than serving a stale one.
    pub fit_mode: ImageFit,
    /// Theme tint + adaptation mode — part of the key so re-theming or changing
    /// the mode rebuilds the image rather than serving a stale one from cache.
    pub policy: RenderPolicy,
}

/// A built, ready-to-place inline image: the themed PNG (the transmit payload)
/// padded to whole cells, its pixel size (so a partially-visible image can be
/// shown by cropping the matching source rows), and its exact cell footprint.
/// Delivered by the [`crate::app`]-side `InlineDeck` via direct Kitty placement
/// (transmit once, place, re-place on scroll) rather than a per-cell protocol.
pub struct ImagePlan {
    /// The themed, whole-cell-padded PNG — placed 1:1 (no terminal rescaling).
    pub png: Vec<u8>,
    /// `png`'s pixel dimensions `(w, h)`, for source-cropping a clipped image.
    pub px: (u32, u32),
    pub cols: u16,
    pub rows: u16,
}

/// Persistent (disk) cache of built rasters, so reopening a book doesn't rebuild every
/// equation from scratch (the 2–4 s reopen). Content-addressed by the *full* sizing key —
/// image bytes + geometry + the measured em + theme — so a byte-identical render is served
/// straight from disk (no decode, resize, recolour, or PNG re-encode). The cache dir is
/// version-stamped ([`VERSION`]); a change to the build/sizing pipeline just misses the old
/// entries rather than serving a stale size.
mod raster_cache {
    use std::hash::{Hash, Hasher};
    use std::path::{Path, PathBuf};

    use super::{FitBox, ImagePlan, RenderPolicy, SizeHint, SizeSpec};

    /// Bump when the build/sizing pipeline changes so a raster would differ — old entries
    /// (under the previous version dir) are then simply ignored.
    pub const VERSION: u32 = 1;

    /// The cache key for a build: everything that determines the resulting raster.
    pub fn key(bytes: &[u8], fit: &FitBox, spec: &SizeSpec, policy: &RenderPolicy) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        (
            fit.fw,
            fit.fh,
            fit.cols,
            fit.rows,
            fit.max_px,
            fit.target_pct,
            fit.math_scale,
        )
            .hash(&mut h);
        fit.fit_mode.hash(&mut h);
        (spec.math, spec.captioned, spec.alt_math, spec.inline).hash(&mut h);
        match spec.hint {
            SizeHint::Auto => 0u8.hash(&mut h),
            SizeHint::Pct(p) => (1u8, p.to_bits()).hash(&mut h),
            SizeHint::Px(px) => (2u8, px).hash(&mut h),
            SizeHint::Em(e) => (3u8, e.to_bits()).hash(&mut h),
            SizeHint::Full => 4u8.hash(&mut h),
        }
        if let Some(p) = &spec.ink {
            (p.x0, p.y0, p.x1, p.y1, p.line_count).hash(&mut h);
            p.line_px.to_bits().hash(&mut h); // f32 → bits so the measured em keys exactly
        }
        policy.hash(&mut h); // theme tint + adaptation mode
        h.finish()
    }

    fn entry_path(dir: &Path, key: u64) -> PathBuf {
        dir.join(format!("{key:016x}.raster"))
    }

    /// A cached raster, or `None` on miss / unreadable / truncated.
    /// Layout: `[cols u16][rows u16][pw u32][ph u32][png_len u32][png…]`, little-endian.
    pub fn load(dir: &Path, key: u64) -> Option<ImagePlan> {
        let data = std::fs::read(entry_path(dir, key)).ok()?;
        if data.len() < 16 {
            return None;
        }
        let cols = u16::from_le_bytes([data[0], data[1]]);
        let rows = u16::from_le_bytes([data[2], data[3]]);
        let pw = u32::from_le_bytes(data[4..8].try_into().ok()?);
        let ph = u32::from_le_bytes(data[8..12].try_into().ok()?);
        let len = u32::from_le_bytes(data[12..16].try_into().ok()?) as usize;
        let png = data.get(16..16 + len)?.to_vec();
        Some(ImagePlan {
            png,
            px: (pw, ph),
            cols,
            rows,
        })
    }

    /// Write a built raster to the cache (best-effort — a failure just means a rebuild next
    /// time). Written to a temp file then renamed, so a concurrent reader never sees a
    /// partial entry (rename is atomic on the same filesystem).
    pub fn store(dir: &Path, key: u64, plan: &ImagePlan) {
        let mut data = Vec::with_capacity(plan.png.len() + 16);
        data.extend(plan.cols.to_le_bytes());
        data.extend(plan.rows.to_le_bytes());
        data.extend(plan.px.0.to_le_bytes());
        data.extend(plan.px.1.to_le_bytes());
        data.extend((plan.png.len() as u32).to_le_bytes());
        data.extend(&plan.png);
        let mut tid = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut tid);
        let tmp = dir.join(format!("{key:016x}.{:x}.tmp", tid.finish()));
        if std::fs::write(&tmp, &data).is_ok() {
            let _ = std::fs::rename(&tmp, entry_path(dir, key));
        }
    }
}

/// Decode, size, theme, and PNG-encode one image into a placeable raster. This is the
/// expensive step (decode + PNG re-encode), so it runs on the [`ImageBuilder`] worker.
fn build_plan(
    picker: &Picker,
    bytes: &[u8],
    fit: FitBox,
    policy: RenderPolicy,
    spec: SizeSpec,
) -> Option<ImagePlan> {
    let img = decode(bytes)?;
    let (w, h) = img.dimensions();
    let fs = picker.font_size();
    let fit = FitBox {
        fw: fs.width,
        fh: fs.height,
        ..fit
    };
    let (cols, rows) = target_cells(w, h, fit, spec);

    // Crop an equation raster to its measured ink bbox before scaling, so the file's
    // whitespace margins don't shrink the glyphs. `target_cells` sized (cols, rows)
    // on this same bbox, so the crop fills them exactly (estimate == build).
    let img = match spec.ink {
        Some(p) => {
            let x0 = p.x0.min(w);
            let y0 = p.y0.min(h);
            let cw = p.x1.min(w).saturating_sub(x0).max(1);
            let ch = p.y1.min(h).saturating_sub(y0).max(1);
            img.crop_imm(x0, y0, cw, ch)
        }
        None => img,
    };

    let (fw, fh) = (fs.width.max(1) as u32, fs.height.max(1) as u32);
    let img = match (spec.inline, spec.ink) {
        // Inline picture: draw the ink at its **exact** text-relative size on a transparent
        // whole-cell canvas, centred on the text row's optical axis. This is the one path that
        // must not fill the ceil'd cell box — doing so fattens a single glyph like `ℝ` (its
        // 1.4-cell ink ceils to 2 cells). Instead the ink is scaled to the same pixels the
        // layout reserved and centred on the line, so every inline raster flows at the prose
        // size and sits level with the text. The theme recolour is applied to the glyph before
        // compositing so the canvas stays transparent.
        (true, Some(ink)) => {
            let f = crate::sizing::inline_fit(fit, ink, f64::from(fit.math_scale) / 100.0);
            let glyph = resize_simd(&img, f.draw_w.max(1), f.draw_h.max(1));
            let glyph = render_for_theme(&glyph, policy.tint, policy.mode).to_rgba8();
            let (cw, ch) = (u32::from(cols) * fw, u32::from(rows) * fh);
            let mut canvas = image::RgbaImage::from_pixel(cw, ch, image::Rgba([0, 0, 0, 0]));
            // Place the ink's centre on the **text row's** optical axis so it sits level with
            // the prose (a symbol on the line, a fraction's bar straddling it) rather than
            // hanging low. The text row is the middle of the canvas: `(rows-1)/2` spacer rows
            // sit above it (and as many below), reserved by the wrapper. Horizontally centre
            // so a symbol like ∈ doesn't butt against the preceding character (the book glues
            // a symbol image straight onto text: `… b ∈⟦ℝ⟧`).
            let half = i64::from((rows - 1) / 2); // spacer rows above the text row
            let text_top = half * i64::from(fh);
            let axis = text_top + (f64::from(fh) * INLINE_AXIS).round() as i64;
            let x = (i64::from(cw) - i64::from(f.draw_w)).max(0) / 2;
            let y = (axis - i64::from(f.draw_h) / 2)
                .clamp(0, (i64::from(ch) - i64::from(f.draw_h)).max(0));
            image::imageops::overlay(&mut canvas, &glyph, x, y);
            image::DynamicImage::ImageRgba8(canvas)
        }
        // Everything else (figures, display equations, pages): scale to fit the target cell
        // box (aspect-preserving) so the protocol fills (cols, rows). The Lanczos3 kernel is
        // the highest quality for the text, equations, and line-art common in book figures —
        // sharp on both up-scaling low-res figures and down-scaling oversized ones — and the
        // SIMD path keeps it cheap. The cost is paid once, off-thread, and the result cached.
        _ => {
            let (bw, bh) = (u32::from(cols) * fw, u32::from(rows) * fh);
            let (sw, sh) = img.dimensions();
            // Aspect-preserving fit (what `image::resize` did), then an exact SIMD resize.
            let scale =
                (f64::from(bw) / f64::from(sw.max(1))).min(f64::from(bh) / f64::from(sh.max(1)));
            let tw = (f64::from(sw) * scale).round().max(1.0) as u32;
            let th = (f64::from(sh) * scale).round().max(1.0) as u32;
            let img = resize_simd(&img, tw, th);
            // Adapt the graphic to the theme (recolour ink / flatten / invert) per mode.
            render_for_theme(&img, policy.tint, policy.mode)
        }
    };
    // Place at the image's *fitted* cell size — ceil(px / cell), NOT the target box
    // (cols, rows). The aspect-preserving resize above fits the image *within* the box,
    // so it is generally shorter/narrower than the box in one axis; placing at the box
    // (`a=p c=cols,r=rows`) would scale it to fill and stretch that axis. Pad the raster
    // up to the exact whole-cell canvas (transparent margin at right/bottom) so the deck
    // places it 1:1 — the terminal does no sub-cell rescaling, and an edge clip lands on
    // whole cell rows. The inline path's canvas is already whole-cell, so this is a no-op
    // there; the figure path's aspect-fit raster gets padded to its tight cell box.
    let (pw, ph) = img.dimensions();
    let (cell_w, cell_h) = (u32::from(fs.width.max(1)), u32::from(fs.height.max(1)));
    let fit_cols = pw.div_ceil(cell_w).max(1);
    let fit_rows = ph.div_ceil(cell_h).max(1);
    let (pad_w, pad_h) = (fit_cols * cell_w, fit_rows * cell_h);
    let rgba = if (pw, ph) == (pad_w, pad_h) {
        img.into_rgba8()
    } else {
        let mut canvas = image::RgbaImage::from_pixel(pad_w, pad_h, image::Rgba([0, 0, 0, 0]));
        image::imageops::replace(&mut canvas, &img.into_rgba8(), 0, 0);
        canvas
    };
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(ImagePlan {
        png,
        px: (pad_w, pad_h),
        cols: fit_cols as u16,
        rows: fit_rows as u16,
    })
}

/// Exact-size resize of `img` to `dw`×`dh` with a **SIMD** Lanczos3 convolution
/// (`fast_image_resize` auto-detects SSE4.1/AVX2/NEON at runtime). Resize is the #2
/// cost of a build after decode — measured 51–260µs with `image`'s scalar Lanczos3 on
/// the book's rasters — and this is ~5× faster for the same kernel, so it stays sharp.
/// Alpha is handled (premultiplied) so transparent inline glyphs resize cleanly. Falls
/// back to `image`'s scalar resize if the SIMD path can't accept the buffer.
fn resize_simd(img: &image::DynamicImage, dw: u32, dh: u32) -> image::DynamicImage {
    use fast_image_resize::images::Image as FirImage;
    use fast_image_resize::{PixelType, Resizer};

    let (dw, dh) = (dw.max(1), dh.max(1));
    let rgba = img.to_rgba8();
    let (sw, sh) = rgba.dimensions();
    let fallback = || img.resize_exact(dw, dh, image::imageops::FilterType::Lanczos3);

    let Ok(src) = FirImage::from_vec_u8(sw, sh, rgba.into_raw(), PixelType::U8x4) else {
        return fallback();
    };
    let mut dst = FirImage::new(dw, dh, PixelType::U8x4);
    // Default `ResizeOptions` is `Convolution(Lanczos3)` with alpha handling on.
    if Resizer::new().resize(&src, &mut dst, None).is_err() {
        return fallback();
    }
    match image::RgbaImage::from_raw(dw, dh, dst.into_vec()) {
        Some(buf) => image::DynamicImage::ImageRgba8(buf),
        None => fallback(),
    }
}

/// A request to build one image's protocol off the main thread.
struct BuildReq {
    key: ImgKey,
    bytes: Vec<u8>,
    spec: SizeSpec,
}

/// A completed image build. `plan` is `None` if the image failed to build (so
/// the reader stops waiting); `stale` means it was skipped because the reader
/// had scrolled far away by the time the worker reached it (re-request later).
pub struct BuiltImage {
    pub key: ImgKey,
    pub plan: Option<ImagePlan>,
    pub stale: bool,
}

/// Sections farther than this from the current one are skipped by the worker, so
/// a fast-scroll backlog of flown-past sections doesn't delay the current one.
const KEEP_RADIUS: usize = 3;

/// The text's optical centre within a cell, as a fraction of the cell height from the top —
/// an inline equation's own vertical centre is placed here (on the text row) so the symbol,
/// or a fraction's bar, sits level with the prose instead of hanging low. Text caps sit
/// slightly above the geometric cell centre, so this is a touch under 0.5. **Tune this one
/// value** if inline math reads a hair high (lower it) or low (raise it) against the text.
const INLINE_AXIS: f64 = 0.40;

/// Worker-pool size: one per core less one (keep a core for the UI), at least two so
/// the inline and block lanes can build concurrently, capped so a many-core machine
/// doesn't spawn a wasteful number of encode threads.
fn worker_count() -> usize {
    thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).clamp(2, 6))
        .unwrap_or(3)
}

/// Two work lanes so a mid-line glyph never waits behind the section's display
/// equations: `remap_section_images` queues every block figure/equation *before*
/// `remap_inline_math` queues the inline atoms, so on a single FIFO the tiny inline
/// `ℝ` sat at the back behind dozens of big rasters. Separate lanes (with workers
/// biased to each) let inline atoms build in parallel with block math and pop in first.
struct Lanes {
    /// Mid-line inline-math atoms (`ImgSlot::InlineMath`) — small, fast, latency-
    /// sensitive (they gate reading the sentence).
    inline: VecDeque<BuildReq>,
    /// Block figures and display equations (`ImgSlot::Figure`) — larger, less urgent.
    block: VecDeque<BuildReq>,
    /// Set on drop so each worker's `wait` returns and the thread exits.
    closed: bool,
}

impl Lanes {
    /// Pull the next request, preferring `own` lane but stealing from the other when
    /// idle so no worker sits idle while there's work anywhere.
    fn take(&mut self, own_inline: bool) -> Option<BuildReq> {
        if own_inline {
            self.inline.pop_front().or_else(|| self.block.pop_front())
        } else {
            self.block.pop_front().or_else(|| self.inline.pop_front())
        }
    }
}

/// Builds image protocols on a pool of background threads so decoding/encoding never
/// stalls scrolling. Send requests with [`request`], collect ready ones with
/// [`poll`]. Keep the workers informed of the viewport via [`set_current`] so they
/// can drop stale work.
pub struct ImageBuilder {
    shared: Arc<(Mutex<Lanes>, Condvar)>,
    res_rx: Receiver<BuiltImage>,
    current: Arc<AtomicUsize>,
}

/// The default persistent raster-cache root, `<config>/rasters`, for [`ImageBuilder::new`].
pub fn raster_cache_dir() -> Option<PathBuf> {
    Some(delryn_infra::paths::config_dir().join("rasters"))
}

impl ImageBuilder {
    /// `cache_root` is the persistent raster-cache directory (typically
    /// `<config>/rasters`); `None` disables disk caching. A version subdirectory is
    /// created under it so a pipeline change invalidates old entries cleanly.
    pub fn new(picker: Picker, cache_root: Option<PathBuf>) -> ImageBuilder {
        let cache_dir = cache_root.and_then(|root| {
            let dir = root.join(format!("v{}", raster_cache::VERSION));
            std::fs::create_dir_all(&dir).ok().map(|()| dir)
        });
        let shared = Arc::new((
            Mutex::new(Lanes {
                inline: VecDeque::new(),
                block: VecDeque::new(),
                closed: false,
            }),
            Condvar::new(),
        ));
        let (res_tx, res_rx) = channel::<BuiltImage>();
        let current = Arc::new(AtomicUsize::new(0));
        for i in 0..worker_count() {
            let shared = Arc::clone(&shared);
            let tx = res_tx.clone();
            let cur = Arc::clone(&current);
            let pk = picker.clone(); // Picker is cheap to clone (font metrics + protocol id)
            let cache = cache_dir.clone();
            // Alternate which lane each worker favours, so at least one always prefers
            // inline and one prefers block — neither starves while the other drains.
            let prefer_inline = i % 2 == 0;
            thread::spawn(move || build_loop(&shared, &tx, &cur, &pk, prefer_inline, cache));
        }
        ImageBuilder {
            shared,
            res_rx,
            current,
        }
    }

    /// Tell the workers which section is in view, so they can drop stale builds.
    pub fn set_current(&self, section: usize) {
        self.current.store(section, Ordering::Relaxed);
    }

    /// Queue a build from an encoded file (the worker decodes it) — figures, display
    /// equations, inline math, page images.
    pub fn request(&self, key: ImgKey, bytes: Vec<u8>, spec: SizeSpec) {
        let (lock, cv) = &*self.shared;
        let mut lanes = lock.lock().unwrap_or_else(|e| e.into_inner());
        let req = BuildReq { key, bytes, spec };
        match key.kind {
            ImgSlot::InlineMath => lanes.inline.push_back(req),
            ImgSlot::Figure => lanes.block.push_back(req),
        }
        drop(lanes);
        cv.notify_one();
    }

    pub fn poll(&self) -> impl Iterator<Item = BuiltImage> + '_ {
        self.res_rx.try_iter()
    }
}

impl Drop for ImageBuilder {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        if let Ok(mut lanes) = lock.lock() {
            lanes.closed = true;
            cv.notify_all();
        }
    }
}

/// One pool worker: pull the next request (favouring `prefer_inline`'s lane), decode/
/// encode it off the lock, and send the result. Exits when the builder is dropped.
fn build_loop(
    shared: &Arc<(Mutex<Lanes>, Condvar)>,
    tx: &Sender<BuiltImage>,
    current: &Arc<AtomicUsize>,
    picker: &Picker,
    prefer_inline: bool,
    cache: Option<PathBuf>,
) {
    let (lock, cv) = &**shared;
    loop {
        // Wait for work, then take one request (releasing the lock before building).
        let req = {
            let mut lanes = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if lanes.closed {
                    return;
                }
                if let Some(req) = lanes.take(prefer_inline) {
                    break req;
                }
                lanes = cv.wait(lanes).unwrap_or_else(|e| e.into_inner());
            }
        };

        let k = req.key;
        // Skip builds for sections the reader has already scrolled away from — they
        // only delay the section now in view.
        let cur = current.load(Ordering::Relaxed);
        let built = if k.section.abs_diff(cur) > KEEP_RADIUS {
            BuiltImage {
                key: k,
                plan: None,
                stale: true,
            }
        } else {
            // Fill the cell size from the live font *here* (build_plan re-fills it too), so
            // the disk-cache key captures it — a font/zoom change must not serve a raster
            // built for a different cell size.
            let fs = picker.font_size();
            let fit = FitBox {
                fw: fs.width,
                fh: fs.height,
                cols: k.avail,
                rows: k.max_rows,
                max_px: k.max_px,
                target_pct: k.target_pct,
                math_scale: k.math_scale,
                fit_mode: k.fit_mode,
            };
            // Serve from the persistent cache if a byte-identical render was built before
            // (any prior open of this or another book); else build it and cache the result.
            let plan = if let Some(dir) = cache.as_deref() {
                let ckey = raster_cache::key(&req.bytes, &fit, &req.spec, &k.policy);
                if let Some(hit) = raster_cache::load(dir, ckey) {
                    Some(hit)
                } else {
                    let built = build_plan(picker, &req.bytes, fit, k.policy, req.spec);
                    if let Some(ref pl) = built {
                        raster_cache::store(dir, ckey, pl);
                    }
                    built
                }
            } else {
                build_plan(picker, &req.bytes, fit, k.policy, req.spec)
            };
            BuiltImage {
                key: k,
                plan,
                stale: false,
            }
        };
        if tx.send(built).is_err() {
            return; // reader gone
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_cache_round_trips() {
        // A temp dir, never the real cache (test hygiene).
        let dir = std::env::temp_dir().join(format!("delryn_rc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plan = ImagePlan {
            png: vec![9, 8, 7, 6, 5, 4],
            px: (48, 22),
            cols: 5,
            rows: 2,
        };
        raster_cache::store(&dir, 0xDEAD_BEEF, &plan);
        let hit = raster_cache::load(&dir, 0xDEAD_BEEF).expect("cached raster loads back");
        assert_eq!(hit.png, plan.png, "png round-trips");
        assert_eq!(hit.px, plan.px, "pixel size round-trips");
        assert_eq!(
            (hit.cols, hit.rows),
            (plan.cols, plan.rows),
            "cell size round-trips"
        );
        assert!(
            raster_cache::load(&dir, 0x1234).is_none(),
            "a different key misses"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
