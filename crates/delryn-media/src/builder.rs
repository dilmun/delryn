//! Off-thread worker that builds inline figure image protocols for the reader.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use delryn_infra::config::ImageFit;
use image::GenericImageView;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::SlicedProtocol;

use crate::decode::decode;
use crate::recolor::{RenderPolicy, render_for_theme};
use crate::sizing::{FitBox, SizeSpec, target_cells};

/// Identifies one built image so it can be cached and reused across sections
/// (revisiting a section reuses the already-uploaded image — no re-transmit).
/// Equal keys ⇒ identical build, so they share one cache entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImgKey {
    pub section: usize,
    pub idx: usize,
    pub avail: u16,
    pub max_rows: u16,
    pub max_px: u16,
    /// Default figure width (% of column) — part of the key so changing the knob
    /// rebuilds at the new size instead of serving a stale one.
    pub target_pct: u16,
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

    // Resize to exactly the target cell box in pixels so the protocol fills
    // (cols, rows) precisely. Lanczos3 is the highest-quality resampling filter
    // for the text, equations, and line-art common in book figures — sharp on
    // both the up-scaling of low-res figures and the down-scaling of oversized
    // ones, where the old bilinear (Triangle) filter left everything soft. The
    // cost is paid once, off-thread, and the result is cached.
    let img = img.resize(
        cols as u32 * fs.width.max(1) as u32,
        rows as u32 * fs.height.max(1) as u32,
        image::imageops::FilterType::Lanczos3,
    );
    // Adapt the graphic to the theme (recolour ink / flatten / invert) per mode.
    let img = render_for_theme(&img, policy.tint, policy.mode);
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

/// Builds image protocols on a background thread so decoding/encoding never
/// stalls scrolling. Send requests with [`request`], collect ready ones with
/// [`poll`]. Keep the worker informed of the viewport via [`set_current`] so it
/// can drop stale work.
pub struct ImageBuilder {
    req_tx: Sender<BuildReq>,
    res_rx: Receiver<BuiltImage>,
    current: Arc<AtomicUsize>,
}

impl ImageBuilder {
    pub fn new(picker: Picker) -> ImageBuilder {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<BuildReq>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<BuiltImage>();
        let current = Arc::new(AtomicUsize::new(0));
        let worker_current = Arc::clone(&current);
        thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                let k = req.key;
                // Skip builds for sections the reader has already scrolled away
                // from — they only delay the section now in view.
                let cur = worker_current.load(Ordering::Relaxed);
                if k.section.abs_diff(cur) > KEEP_RADIUS {
                    if res_tx
                        .send(BuiltImage {
                            key: k,
                            plan: None,
                            stale: true,
                        })
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                // `fw`/`fh` are filled in by `build_plan` from the live font size.
                let fit = FitBox {
                    fw: 0,
                    fh: 0,
                    cols: k.avail,
                    rows: k.max_rows,
                    max_px: k.max_px,
                    target_pct: k.target_pct,
                    fit_mode: k.fit_mode,
                };
                let plan = build_plan(&picker, &req.bytes, fit, k.policy, req.spec);
                if res_tx
                    .send(BuiltImage {
                        key: k,
                        plan,
                        stale: false,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        ImageBuilder {
            req_tx,
            res_rx,
            current,
        }
    }

    /// Tell the worker which section is in view, so it can drop stale builds.
    pub fn set_current(&self, section: usize) {
        self.current.store(section, Ordering::Relaxed);
    }

    pub fn request(&self, key: ImgKey, bytes: Vec<u8>, spec: SizeSpec) {
        let _ = self.req_tx.send(BuildReq { key, bytes, spec });
    }

    pub fn poll(&self) -> impl Iterator<Item = BuiltImage> + '_ {
        self.res_rx.try_iter()
    }
}
