//! Terminal image support: protocol detection and decoding. Wraps
//! `ratatui-image` so the rest of the app doesn't depend on it directly.
//! See `DESIGN.md` §0 (graphics protocols).

use std::collections::HashMap;

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

/// Read just an image's pixel dimensions (cheap — header only).
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Cell dimensions (cols, rows) to display a `w`×`h` px image within at most
/// `max_cols`×`max_rows` cells, preserving aspect ratio. `fw`/`fh` are the
/// terminal's cell size in pixels.
pub fn fit_cells(w: u32, h: u32, fw: u16, fh: u16, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if w == 0 || h == 0 || fw == 0 || fh == 0 || max_cols == 0 || max_rows == 0 {
        return (1, 1);
    }
    let max_w_px = max_cols as f64 * fw as f64;
    let max_h_px = max_rows as f64 * fh as f64;
    let scale = (max_w_px / w as f64).min(max_h_px / h as f64);
    let disp_w = (w as f64 * scale).max(1.0);
    let disp_h = (h as f64 * scale).max(1.0);
    let cols = ((disp_w / fw as f64).ceil() as u16).clamp(1, max_cols);
    let rows = ((disp_h / fh as f64).ceil() as u16).clamp(1, max_rows);
    (cols, rows)
}

/// Cached fixed-size protocols for inline images, keyed by image index within
/// a section. Rebuilt when the section or content width changes.
#[derive(Default)]
pub struct ImgCache {
    /// (section, content-width) the cache was built for.
    pub key: (usize, usize),
    pub map: HashMap<usize, Protocol>,
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
