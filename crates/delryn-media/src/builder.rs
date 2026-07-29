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
use crate::resize::resize_exact;
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

/// Persistent (disk) cache of built raster **geometry**, so reopening a book doesn't
/// re-decode + re-resize every equation (the 2–4 s reopen), and switching theme reuses the
/// geometry instead of rebuilding. Content-addressed by the sizing key — image bytes +
/// geometry + the measured em — but deliberately **not** the theme policy, since the theme
/// recolour is the cheap [`finish_theme`] tail applied after a load. The cache dir is
/// version-stamped ([`VERSION`]); a change to the decode/sizing pipeline just misses the old
/// entries rather than serving a stale size.
mod geo_cache {
    use std::hash::{Hash, Hasher};
    use std::path::{Path, PathBuf};

    use super::{FitBox, Geometry, SizeHint, SizeSpec};

    /// Bump when the decode/sizing pipeline changes so the geometry would differ — old
    /// entries (under the previous version dir) are then simply ignored. The sizing
    /// **constants** (e.g. `EQ_TARGET_LINE_CELLS`) aren't in [`key`], so a change to them
    /// must bump this or a stale-sized raster is served from disk. (v3: display-equation
    /// target lowered from 1.15 to 0.9 cells. v4: per-equation em, not one forced median.
    /// v5: display target dropped to prose size. v6: cap equation enlargement at native
    /// size. v7: one uniform book scale + generous height cap (no per-equation sizing).)
    pub const VERSION: u32 = 7;

    /// The cache key for a build's geometry: everything that determines the *pre-theme*
    /// raster. The render policy is excluded on purpose (theme is applied after a load).
    pub fn key(bytes: &[u8], fit: &FitBox, spec: &SizeSpec) -> u64 {
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
        h.finish()
    }

    fn entry_path(dir: &Path, key: u64) -> PathBuf {
        dir.join(format!("{key:016x}.geo"))
    }

    /// Cached geometry, or `None` on miss / unreadable / truncated. Layout:
    /// `[tag u8][cols u16][rows u16][png_len u32][png…]`, little-endian; `tag` 0 = inline
    /// glyph (with `cols`/`rows`), 1 = figure. The PNG holds the pre-theme RGBA pixels.
    pub fn load(dir: &Path, key: u64) -> Option<Geometry> {
        let data = std::fs::read(entry_path(dir, key)).ok()?;
        if data.len() < 9 {
            return None;
        }
        let tag = data[0];
        let cols = u16::from_le_bytes([data[1], data[2]]);
        let rows = u16::from_le_bytes([data[3], data[4]]);
        let len = u32::from_le_bytes(data[5..9].try_into().ok()?) as usize;
        let png = data.get(9..9 + len)?;
        let img = super::decode(png)?.into_rgba8();
        match tag {
            0 => Some(Geometry::Inline {
                glyph: img,
                cols,
                rows,
            }),
            1 => Some(Geometry::Figure { img }),
            _ => None,
        }
    }

    /// Write built geometry to the cache (best-effort — a failure just means a rebuild next
    /// time). Written to a temp file then renamed, so a concurrent reader never sees a
    /// partial entry (rename is atomic on the same filesystem).
    pub fn store(dir: &Path, key: u64, geo: &Geometry) {
        let (tag, cols, rows, img) = match geo {
            Geometry::Inline { glyph, cols, rows } => (0u8, *cols, *rows, glyph),
            Geometry::Figure { img } => (1u8, 0u16, 0u16, img),
        };
        let mut png = Vec::new();
        if image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .is_err()
        {
            return;
        }
        let mut data = Vec::with_capacity(png.len() + 9);
        data.push(tag);
        data.extend(cols.to_le_bytes());
        data.extend(rows.to_le_bytes());
        data.extend((png.len() as u32).to_le_bytes());
        data.extend(&png);
        let mut tid = std::collections::hash_map::DefaultHasher::new();
        std::thread::current().id().hash(&mut tid);
        let tmp = dir.join(format!("{key:016x}.{:x}.tmp", tid.finish()));
        if std::fs::write(&tmp, &data).is_ok() {
            let _ = std::fs::rename(&tmp, entry_path(dir, key));
        }
    }
}

/// The theme-independent result of decoding + sizing one image: the expensive work (JPEG
/// decode, ink crop, Lanczos resize) done, but held **before** the theme recolour. A theme
/// change reuses this and re-runs only the cheap [`finish_theme`] recolour + encode instead
/// of rebuilding from scratch — so switching theme (and re-inverting figures) doesn't stall
/// on a full rebuild of every equation. Cached (disk) keyed *without* the render policy.
enum Geometry {
    /// Inline atom: the resized ink glyph (pre-recolour) plus the whole-cell canvas it is
    /// centred into (`cols`×`rows`; the current font size fixes the pixels at finish time).
    Inline {
        glyph: image::RgbaImage,
        cols: u16,
        rows: u16,
    },
    /// Figure / display equation / page: the aspect-fitted raster (pre-recolour, pre-pad).
    Figure { img: image::RgbaImage },
}

/// Decode + size one image up to (but not including) the theme recolour — the expensive part
/// of a build (decode + Lanczos resize). Theme-independent, so its result is cached and
/// shared across every theme.
fn build_geometry(picker: &Picker, bytes: &[u8], fit: FitBox, spec: SizeSpec) -> Option<Geometry> {
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

    match (spec.inline, spec.ink) {
        // Inline picture: scale the ink to its **exact** text-relative pixels (the same
        // pixels the layout reserved) so a single glyph like `ℝ` is never fattened to a
        // ceil'd cell. Kept un-recoloured here; [`finish_theme`] tints it and centres it on
        // the text row's optical axis of a transparent whole-cell canvas.
        (true, Some(ink)) => {
            let f = crate::sizing::inline_fit(fit, ink, f64::from(fit.math_scale) / 100.0);
            let glyph = resize_exact(&img, f.draw_w.max(1), f.draw_h.max(1)).into_rgba8();
            Some(Geometry::Inline { glyph, cols, rows })
        }
        // Everything else (figures, display equations, pages): aspect-preserving fit into the
        // target cell box. The Lanczos3 SIMD kernel is the highest quality for the text,
        // equations, and line-art common in book figures. [`finish_theme`] applies the theme
        // adaptation (recolour / flatten / invert) and the whole-cell pad.
        _ => {
            let (fw, fh) = (fs.width.max(1) as u32, fs.height.max(1) as u32);
            let (bw, bh) = (u32::from(cols) * fw, u32::from(rows) * fh);
            let (sw, sh) = img.dimensions();
            let scale =
                (f64::from(bw) / f64::from(sw.max(1))).min(f64::from(bh) / f64::from(sh.max(1)));
            let tw = (f64::from(sw) * scale).round().max(1.0) as u32;
            let th = (f64::from(sh) * scale).round().max(1.0) as u32;
            let img = resize_exact(&img, tw, th).into_rgba8();
            Some(Geometry::Figure { img })
        }
    }
}

/// Recolour a [`Geometry`] to the active theme and PNG-encode it into a placeable raster —
/// the cheap tail of a build (no decode, no Lanczos), re-run alone on a theme change. `cell`
/// is the terminal cell size `(w, h)` in px. The recolour + composite here is byte-for-byte
/// what the old single-pass build did (same order: recolour the glyph, then composite); only
/// the decode + resize is split out ahead of it into the cached [`Geometry`], and a PNG
/// round-trip through the disk cache is lossless for RGBA — so the split is behaviour-
/// preserving.
fn finish_theme(geo: Geometry, policy: RenderPolicy, cell: (u16, u16)) -> Option<ImagePlan> {
    let (cell_w, cell_h) = (u32::from(cell.0.max(1)), u32::from(cell.1.max(1)));
    let themed = match geo {
        Geometry::Inline { glyph, cols, rows } => {
            // Recolour the glyph to the theme ink, then centre it on the text row's optical
            // axis of a transparent whole-cell canvas: the text row is the canvas middle,
            // with `(rows-1)/2` spacer rows above it (and as many below) reserved by the
            // wrapper, so a symbol sits on the line and a fraction's bar straddles it rather
            // than hanging low. Horizontally centred so a symbol like ∈ doesn't butt against
            // the preceding character.
            let glyph = render_for_theme(
                &image::DynamicImage::ImageRgba8(glyph),
                policy.tint,
                policy.mode,
            )
            .to_rgba8();
            let (draw_w, draw_h) = glyph.dimensions();
            let (cw, ch) = (u32::from(cols) * cell_w, u32::from(rows) * cell_h);
            let mut canvas = image::RgbaImage::from_pixel(cw, ch, image::Rgba([0, 0, 0, 0]));
            let half = i64::from((rows - 1) / 2); // spacer rows above the text row
            let text_top = half * i64::from(cell_h);
            let axis = text_top + (f64::from(cell_h) * INLINE_AXIS).round() as i64;
            let x = (i64::from(cw) - i64::from(draw_w)).max(0) / 2;
            let y =
                (axis - i64::from(draw_h) / 2).clamp(0, (i64::from(ch) - i64::from(draw_h)).max(0));
            image::imageops::overlay(&mut canvas, &glyph, x, y);
            canvas
        }
        // Adapt the graphic to the theme (recolour ink / flatten / invert) per mode.
        Geometry::Figure { img } => render_for_theme(
            &image::DynamicImage::ImageRgba8(img),
            policy.tint,
            policy.mode,
        )
        .to_rgba8(),
    };
    // Place at the raster's *fitted* cell size — ceil(px / cell). The aspect-preserving fit
    // above leaves the figure shorter/narrower than its box in one axis; placing at the box
    // would stretch it. Pad up to the exact whole-cell canvas (transparent margin at
    // right/bottom) so the deck places it 1:1 — the terminal does no sub-cell rescaling. The
    // inline canvas is already whole-cell, so this is a no-op there.
    let (pw, ph) = themed.dimensions();
    let fit_cols = pw.div_ceil(cell_w).max(1);
    let fit_rows = ph.div_ceil(cell_h).max(1);
    let (pad_w, pad_h) = (fit_cols * cell_w, fit_rows * cell_h);
    let rgba = if (pw, ph) == (pad_w, pad_h) {
        themed
    } else {
        let mut canvas = image::RgbaImage::from_pixel(pad_w, pad_h, image::Rgba([0, 0, 0, 0]));
        image::imageops::replace(&mut canvas, &themed, 0, 0);
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

/// Decode, size, theme, and PNG-encode one image into a placeable raster — [`build_geometry`]
/// then [`finish_theme`], for the non-cached path and tests.
fn build_plan(
    picker: &Picker,
    bytes: &[u8],
    fit: FitBox,
    policy: RenderPolicy,
    spec: SizeSpec,
) -> Option<ImagePlan> {
    let fs = picker.font_size();
    let geo = build_geometry(picker, bytes, fit, spec)?;
    finish_theme(geo, policy, (fs.width, fs.height))
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
            let dir = root.join(format!("v{}", geo_cache::VERSION));
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
    /// equations, inline math, page images. `priority` puts it at the **front** of its lane
    /// (for images in the current viewport), so on a theme change the on-screen equations
    /// re-tint before the section's off-screen rest (this book ships every symbol as its own
    /// image, so a section holds thousands of tiny rasters).
    pub fn request(&self, key: ImgKey, bytes: Vec<u8>, spec: SizeSpec) {
        self.enqueue(key, bytes, spec, false);
    }

    /// Queue a viewport build ahead of the section's off-screen backlog (see [`request`]).
    pub fn request_priority(&self, key: ImgKey, bytes: Vec<u8>, spec: SizeSpec) {
        self.enqueue(key, bytes, spec, true);
    }

    fn enqueue(&self, key: ImgKey, bytes: Vec<u8>, spec: SizeSpec, priority: bool) {
        let (lock, cv) = &*self.shared;
        let mut lanes = lock.lock().unwrap_or_else(|e| e.into_inner());
        let req = BuildReq { key, bytes, spec };
        let lane = match key.kind {
            ImgSlot::InlineMath => &mut lanes.inline,
            ImgSlot::Figure => &mut lanes.block,
        };
        if priority {
            lane.push_front(req);
        } else {
            lane.push_back(req);
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
            // Serve the geometry (decode + resize) from the persistent cache if it was built
            // before — for this theme, another theme, or another book — else build and cache
            // it; then apply the cheap theme recolour. A theme change thus reuses the cached
            // geometry and only re-runs `finish_theme`, instead of rebuilding every equation.
            let plan = if let Some(dir) = cache.as_deref() {
                let gkey = geo_cache::key(&req.bytes, &fit, &req.spec);
                let geo = geo_cache::load(dir, gkey).or_else(|| {
                    let built = build_geometry(picker, &req.bytes, fit, req.spec);
                    if let Some(ref g) = built {
                        geo_cache::store(dir, gkey, g);
                    }
                    built
                });
                geo.and_then(|g| finish_theme(g, k.policy, (fs.width, fs.height)))
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
    use crate::recolor::Ink;
    use delryn_infra::config::ImageMode;

    fn plain_policy() -> RenderPolicy {
        RenderPolicy {
            tint: Ink {
                ink: [0, 0, 0],
                paper: [255, 255, 255],
            },
            mode: ImageMode::default(),
        }
    }

    #[test]
    fn geo_cache_round_trips() {
        // A temp dir, never the real cache (test hygiene).
        let dir = std::env::temp_dir().join(format!("delryn_gc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Inline glyph geometry: a tiny RGBA image plus its whole-cell canvas footprint.
        let glyph = image::RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([x as u8 * 40, y as u8 * 40, 7, 200])
        });
        let geo = Geometry::Inline {
            glyph: glyph.clone(),
            cols: 4,
            rows: 3,
        };
        geo_cache::store(&dir, 0xABCD_1234, &geo);
        match geo_cache::load(&dir, 0xABCD_1234).expect("geometry loads back") {
            Geometry::Inline {
                glyph: got,
                cols,
                rows,
            } => {
                assert_eq!((cols, rows), (4, 3), "cell footprint round-trips");
                assert_eq!(
                    got.dimensions(),
                    glyph.dimensions(),
                    "glyph dims round-trip"
                );
                assert_eq!(
                    got.into_raw(),
                    glyph.into_raw(),
                    "pixels round-trip (PNG lossless)"
                );
            }
            Geometry::Figure { .. } => panic!("expected an inline geometry"),
        }

        // Figure geometry round-trips under the figure tag.
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        geo_cache::store(&dir, 0x5, &Geometry::Figure { img });
        assert!(
            matches!(geo_cache::load(&dir, 0x5), Some(Geometry::Figure { .. })),
            "figure tag round-trips"
        );
        assert!(
            geo_cache::load(&dir, 0x999).is_none(),
            "a different key misses"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finish_theme_inline_canvas_is_whole_cell() {
        // An inline glyph is centred onto a transparent canvas sized exactly to its reserved
        // cell footprint (cols×cell_w by rows×cell_h) — no sub-cell padding, placed 1:1.
        let glyph = image::RgbaImage::from_pixel(10, 15, image::Rgba([0, 0, 0, 255]));
        let geo = Geometry::Inline {
            glyph,
            cols: 3,
            rows: 3,
        };
        let plan = finish_theme(geo, plain_policy(), (8, 16)).expect("finishes");
        assert_eq!(
            plan.px,
            (3 * 8, 3 * 16),
            "canvas is cols*cell_w by rows*cell_h"
        );
        assert_eq!(
            (plan.cols, plan.rows),
            (3, 3),
            "reports its whole-cell footprint"
        );
    }
}
