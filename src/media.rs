//! Terminal image support: protocol detection and decoding. Wraps
//! `ratatui-image` so the rest of the app doesn't depend on it directly.
//! See `DESIGN.md` §0 (graphics protocols).

use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use image::DynamicImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::{Protocol, StatefulProtocol};

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

/// Read just an image's pixel dimensions (header only — cheap).
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Rows a `w`×`h` px image will occupy when fitted within `avail_cols`×`max_rows`
/// cells (preserving aspect), given the terminal cell size `fw`×`fh` px. Used to
/// reserve reflow rows up front, before the protocol is built in the background.
pub fn fit_rows(w: u32, h: u32, fw: u16, fh: u16, avail_cols: u16, max_rows: u16) -> u16 {
    if w == 0 || h == 0 || fw == 0 || fh == 0 {
        return 1;
    }
    let scale = (avail_cols as f64 * fw as f64 / w as f64)
        .min(max_rows as f64 * fh as f64 / h as f64);
    ((h as f64 * scale / fh as f64).ceil() as u16).clamp(1, max_rows)
}

/// A built, ready-to-render inline image: its protocol plus the exact cell size
/// it occupies.
pub struct ImagePlan {
    pub proto: Protocol,
    pub cols: u16,
    pub rows: u16,
}

/// Decode, upscale-to-fill, and encode one image into a protocol. This is the
/// expensive step (RGBA encode), so it runs on the [`ImageBuilder`] worker.
fn build_plan(picker: &Picker, bytes: &[u8], avail_cols: u16, max_rows: u16) -> Option<ImagePlan> {
    let mut img = decode(bytes)?;
    let fs = picker.font_size();
    let box_w = avail_cols as u32 * fs.width.max(1) as u32;
    let box_h = max_rows as u32 * fs.height.max(1) as u32;
    if box_w > 0 && box_h > 0 {
        // Triangle (bilinear) is much faster than Lanczos3 and fine for figures;
        // scales up or down to the box, preserving aspect.
        img = img.resize(box_w, box_h, image::imageops::FilterType::Triangle);
    }
    let size = ratatui::layout::Size::new(avail_cols, max_rows);
    let proto = picker
        .new_protocol(img, size, ratatui_image::Resize::Fit(None))
        .ok()?;
    let s = proto.size();
    Some(ImagePlan { proto, cols: s.width, rows: s.height })
}

/// A request to build one image's protocol off the main thread.
struct BuildReq {
    token: u64,
    idx: usize,
    bytes: Vec<u8>,
    avail_cols: u16,
    max_rows: u16,
}

/// A completed image build, tagged so stale results (from a previous section or
/// size) can be discarded. `plan` is `None` if the image failed to build, so
/// the reader can stop waiting on it.
pub struct BuiltImage {
    pub token: u64,
    pub idx: usize,
    pub plan: Option<ImagePlan>,
}

/// Builds image protocols on a background thread so decoding/encoding never
/// stalls scrolling. Send requests with [`request`], collect ready ones with
/// [`poll`].
pub struct ImageBuilder {
    req_tx: Sender<BuildReq>,
    res_rx: Receiver<BuiltImage>,
}

impl ImageBuilder {
    pub fn new(picker: Picker) -> ImageBuilder {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<BuildReq>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<BuiltImage>();
        thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                // Always reply (even on failure) so the reader stops waiting.
                let plan = build_plan(&picker, &req.bytes, req.avail_cols, req.max_rows);
                if res_tx
                    .send(BuiltImage { token: req.token, idx: req.idx, plan })
                    .is_err()
                {
                    break;
                }
            }
        });
        ImageBuilder { req_tx, res_rx }
    }

    pub fn request(&self, token: u64, idx: usize, bytes: Vec<u8>, avail_cols: u16, max_rows: u16) {
        let _ = self.req_tx.send(BuildReq { token, idx, bytes, avail_cols, max_rows });
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
