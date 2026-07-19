//! Off-thread worker that builds inline figure image protocols for the reader.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use delryn_infra::config::ImageFit;
use image::GenericImageView;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::SlicedProtocol;

use crate::decode::decode;
use crate::recolor::{RenderPolicy, render_for_theme};
use crate::sizing::{FitBox, SizeSpec, target_cells};

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

/// A built, ready-to-render inline image: a sliced protocol (so partial rows
/// can be drawn as it scrolls past an edge) plus its exact cell size.
pub struct ImagePlan {
    pub proto: SlicedProtocol,
    pub cols: u16,
    pub rows: u16,
}

impl ImagePlan {
    /// The terminal image id (Kitty), if any — used to delete it on eviction.
    pub fn image_id(&self) -> Option<u32> {
        self.proto.image_id()
    }

    /// The Kitty upload sequence if this image hasn't been transmitted yet, so a
    /// look-ahead page can be uploaded to the terminal *ahead* of display (no
    /// first-render upload flash). `None` for non-Kitty protocols or once sent.
    /// The caller MUST write the returned bytes to the terminal.
    pub fn pretransmit(&self) -> Option<String> {
        self.proto.pretransmit()
    }

    /// Whether this image still needs uploading to the terminal (non-consuming),
    /// so the reader can keep the loop alive until look-ahead pages are uploaded.
    pub fn needs_pretransmit(&self) -> bool {
        self.proto.needs_pretransmit()
    }
}

/// Decode, upscale-to-fill, and encode one image into a sliced protocol. This
/// is the expensive step (RGBA encode), so it runs on the [`ImageBuilder`]
/// worker.
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
    let lanczos = image::imageops::FilterType::Lanczos3;
    let img = match (spec.inline, spec.ink) {
        // Inline picture: draw the ink at its **exact** text-relative size on a transparent
        // whole-cell canvas, baseline-aligned. This is the one path that must not fill the
        // ceil'd cell box — doing so fattens a single glyph like `ℝ` (its 1.4-cell ink ceils
        // to 2 cells) and floats it at the top of the row. Instead the ink is scaled to the
        // same pixels the layout reserved and seated on the text baseline, so every inline
        // raster flows at the prose size and sits on the line. The theme recolour is applied
        // to the glyph before compositing so the canvas stays transparent.
        (true, Some(ink)) => {
            let f = crate::sizing::inline_fit(fit, ink, f64::from(fit.math_scale) / 100.0);
            let glyph = img.resize_exact(f.draw_w.max(1), f.draw_h.max(1), lanczos);
            let glyph = render_for_theme(&glyph, policy.tint, policy.mode).to_rgba8();
            let (cw, ch) = (u32::from(cols) * fw, u32::from(rows) * fh);
            let mut canvas = image::RgbaImage::from_pixel(cw, ch, image::Rgba([0, 0, 0, 0]));
            // Bottom-align with a small descender gap so caps land on the text baseline.
            let descender = (0.14 * f64::from(fh)).round() as i64;
            let y = (i64::from(ch) - i64::from(f.draw_h) - descender).max(0);
            // Centre the glyph in its (ceil'd) cell box so it doesn't butt against the
            // preceding character — the book glues a symbol image straight onto the text
            // (`… b ∈⟦ℝ⟧`), so a left-aligned glyph touched the `∈`. The slack is the
            // rounding of one glyph's width to whole cells (a few px), split evenly.
            let x = (i64::from(cw) - i64::from(f.draw_w)).max(0) / 2;
            image::imageops::overlay(&mut canvas, &glyph, x, y);
            image::DynamicImage::ImageRgba8(canvas)
        }
        // Everything else (figures, display equations, pages): resize to exactly the target
        // cell box so the protocol fills (cols, rows) precisely. Lanczos3 is the highest-
        // quality resampling filter for the text, equations, and line-art common in book
        // figures — sharp on both the up-scaling of low-res figures and the down-scaling of
        // oversized ones. The cost is paid once, off-thread, and the result is cached.
        _ => {
            let img = img.resize(u32::from(cols) * fw, u32::from(rows) * fh, lanczos);
            // Adapt the graphic to the theme (recolour ink / flatten / invert) per mode.
            render_for_theme(&img, policy.tint, policy.mode)
        }
    };
    let size = ratatui::layout::Size::new(cols, rows);
    let proto =
        SlicedProtocol::new_with_resize(picker, img, size, ratatui_image::Resize::Fit(None))
            .ok()?;
    let s = proto.size();
    Some(ImagePlan {
        proto,
        cols: s.width,
        rows: s.height,
    })
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

impl ImageBuilder {
    pub fn new(picker: Picker) -> ImageBuilder {
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
            // Alternate which lane each worker favours, so at least one always prefers
            // inline and one prefers block — neither starves while the other drains.
            let prefer_inline = i % 2 == 0;
            thread::spawn(move || build_loop(&shared, &tx, &cur, &pk, prefer_inline));
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
            // `fw`/`fh` are filled in by `build_plan` from the live font size.
            let fit = FitBox {
                fw: 0,
                fh: 0,
                cols: k.avail,
                rows: k.max_rows,
                max_px: k.max_px,
                target_pct: k.target_pct,
                math_scale: k.math_scale,
                fit_mode: k.fit_mode,
            };
            BuiltImage {
                key: k,
                plan: build_plan(picker, &req.bytes, fit, k.policy, req.spec),
                stale: false,
            }
        };
        if tx.send(built).is_err() {
            return; // reader gone
        }
    }
}
