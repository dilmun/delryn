//! Terminal image support: protocol detection and decoding. Wraps
//! `ratatui-image` so the rest of the app doesn't depend on it directly.
//! See `DESIGN.md` §0 (graphics protocols).

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

/// A built, ready-to-render inline image: a sliced protocol (so partial rows
/// can be drawn as it scrolls past an edge) plus its exact cell size.
pub struct ImagePlan {
    pub proto: SlicedProtocol,
    pub cols: u16,
    pub rows: u16,
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
    token: u64,
    idx: usize,
    bytes: Vec<u8>,
    avail_cols: u16,
    max_rows: u16,
    max_px: u16,
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
                let plan = build_plan(&picker, &req.bytes, req.avail_cols, req.max_rows, req.max_px);
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

    pub fn request(
        &self,
        token: u64,
        idx: usize,
        bytes: Vec<u8>,
        avail_cols: u16,
        max_rows: u16,
        max_px: u16,
    ) {
        let _ = self
            .req_tx
            .send(BuildReq { token, idx, bytes, avail_cols, max_rows, max_px });
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
