//! Terminal image support: protocol detection and decoding. Wraps
//! `ratatui-image` so the rest of the app doesn't depend on it directly.
//! See `DESIGN.md` §0 (graphics protocols).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use image::DynamicImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::sliced::SlicedProtocol;

/// Detect the terminal's image protocol + cell size by querying stdio. Returns
/// `None` if there's no tty or detection fails (then images are unavailable).
/// Call before entering the alternate screen / raw mode.
pub fn detect_picker() -> Option<Picker> {
    Picker::from_query_stdio().ok()
}

pub fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

/// A decoded cover plus its source pixel dimensions, so the renderer can size a
/// render rect to the cover's aspect ratio (filling it with no letterbox).
pub struct CoverImage {
    pub proto: StatefulProtocol,
    /// Source pixel dimensions (w, h).
    pub dims: (u32, u32),
}

/// Decode `bytes` and build a resize protocol for `picker`, capturing the source
/// dimensions. `None` if the bytes aren't a decodable image.
pub fn build_cover(picker: &Picker, bytes: &[u8]) -> Option<CoverImage> {
    decode(bytes).map(|img| {
        let dims = (img.width(), img.height());
        CoverImage {
            proto: picker.new_resize_protocol(img),
            dims,
        }
    })
}

/// Read just an image's pixel dimensions (header only — cheap).
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Cell size (cols, rows) for a `w`×`h` px image: fit its aspect within
/// `avail_cols`×`max_rows` cells, then cap the displayed longest side to
/// `max_px` pixels so the data transmitted to the terminal stays bounded.
/// `fw`×`fh` is the terminal cell size in px. Used by both the up-front row
/// estimate and the background build, so the two always agree (no gap).
pub fn target_cells(
    w: u32,
    h: u32,
    fw: u16,
    fh: u16,
    avail_cols: u16,
    max_rows: u16,
    max_px: u16,
) -> (u16, u16) {
    if w == 0 || h == 0 || fw == 0 || fh == 0 {
        return (1, 1);
    }
    let (wf, hf, fwf, fhf) = (w as f64, h as f64, fw as f64, fh as f64);
    let mut scale = (avail_cols as f64 * fwf / wf).min(max_rows as f64 * fhf / hf);
    let longest = (wf * scale).max(hf * scale);
    if max_px > 0 && longest > max_px as f64 {
        scale *= max_px as f64 / longest;
    }
    let cols = ((wf * scale / fwf).ceil() as u16).clamp(1, avail_cols.max(1));
    let rows = ((hf * scale / fhf).ceil() as u16).clamp(1, max_rows.max(1));
    (cols, rows)
}

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
}

/// Kitty escape sequence to delete an image (and free its data) by id.
pub fn delete_image_seq(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id}\x1b\\")
}

/// Decode, upscale-to-fill, and encode one image into a sliced protocol. This
/// is the expensive step (RGBA encode), so it runs on the [`ImageBuilder`]
/// worker.
fn build_plan(
    picker: &Picker,
    bytes: &[u8],
    avail_cols: u16,
    max_rows: u16,
    max_px: u16,
) -> Option<ImagePlan> {
    use image::GenericImageView;
    let img = decode(bytes)?;
    let (w, h) = img.dimensions();
    let fs = picker.font_size();
    let (cols, rows) = target_cells(w, h, fs.width, fs.height, avail_cols, max_rows, max_px);

    // Resize to exactly the target cell box in pixels (Triangle: fast, fine for
    // figures; up- or down-scales), so the protocol fills (cols, rows) precisely.
    let img = img.resize(
        cols as u32 * fs.width.max(1) as u32,
        rows as u32 * fs.height.max(1) as u32,
        image::imageops::FilterType::Triangle,
    );
    let size = ratatui::layout::Size::new(cols, rows);
    let proto = SlicedProtocol::new_with_resize(picker, img, size, ratatui_image::Resize::Fit(None))
        .ok()?;
    let s = proto.size();
    Some(ImagePlan { proto, cols: s.width, rows: s.height })
}

/// A request to build one image's protocol off the main thread.
struct BuildReq {
    key: ImgKey,
    bytes: Vec<u8>,
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
                    if res_tx.send(BuiltImage { key: k, plan: None, stale: true }).is_err() {
                        break;
                    }
                    continue;
                }
                let plan = build_plan(&picker, &req.bytes, k.avail, k.max_rows, k.max_px);
                if res_tx.send(BuiltImage { key: k, plan, stale: false }).is_err() {
                    break;
                }
            }
        });
        ImageBuilder { req_tx, res_rx, current }
    }

    /// Tell the worker which section is in view, so it can drop stale builds.
    pub fn set_current(&self, section: usize) {
        self.current.store(section, Ordering::Relaxed);
    }

    pub fn request(&self, key: ImgKey, bytes: Vec<u8>) {
        let _ = self.req_tx.send(BuildReq { key, bytes });
    }

    pub fn poll(&self) -> impl Iterator<Item = BuiltImage> + '_ {
        self.res_rx.try_iter()
    }
}

/// An open image viewer: a set of decoded images (as resize protocols) for the
/// current section, with a selected index.
pub struct ImageView {
    pub protocols: Vec<StatefulProtocol>,
    pub sel: usize,
}

impl ImageView {
    /// Build a viewer from raw image bytes; `None` if nothing decodes.
    pub fn new(picker: &Picker, images: &[Vec<u8>]) -> Option<ImageView> {
        let protocols: Vec<StatefulProtocol> = images
            .iter()
            .filter_map(|b| decode(b))
            .map(|img| picker.new_resize_protocol(img))
            .collect();
        if protocols.is_empty() {
            None
        } else {
            Some(ImageView { protocols, sel: 0 })
        }
    }

    pub fn len(&self) -> usize {
        self.protocols.len()
    }

    pub fn next(&mut self) {
        if !self.protocols.is_empty() {
            self.sel = (self.sel + 1) % self.protocols.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.protocols.is_empty() {
            self.sel = (self.sel + self.protocols.len() - 1) % self.protocols.len();
        }
    }
}
