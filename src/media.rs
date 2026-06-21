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

/// A built, ready-to-render inline image: its protocol plus the exact cell size
/// it occupies (so the reflow can reserve precisely that many rows — no gap).
pub struct ImagePlan {
    pub proto: Protocol,
    pub cols: u16,
    pub rows: u16,
}

/// Build fixed-size protocols for a section's images, each fitted within
/// `avail_cols`×`max_rows` cells (preserving aspect). Keyed by image index.
/// Returns the actual cell size of each via [`ImagePlan`].
pub fn plan_images<'a>(
    picker: &Picker,
    images: impl Iterator<Item = (usize, &'a [u8])>,
    avail_cols: u16,
    max_rows: u16,
) -> HashMap<usize, ImagePlan> {
    // Target pixel box for the column. We upscale small images to fill it,
    // because the protocol's own Fit only ever shrinks — on HiDPI terminals a
    // modest-resolution figure would otherwise render tiny.
    let fs = picker.font_size();
    let box_w = avail_cols as u32 * fs.width.max(1) as u32;
    let box_h = max_rows as u32 * fs.height.max(1) as u32;
    let size = ratatui::layout::Size::new(avail_cols, max_rows);

    let mut plans = HashMap::new();
    for (idx, bytes) in images {
        let Some(mut img) = decode(bytes) else { continue };
        if box_w > 0 && box_h > 0 {
            // Scales up or down to fit the box, preserving aspect ratio.
            img = img.resize(box_w, box_h, image::imageops::FilterType::Lanczos3);
        }
        if let Ok(proto) = picker.new_protocol(img, size, ratatui_image::Resize::Fit(None)) {
            let s = proto.size();
            plans.insert(idx, ImagePlan { proto, cols: s.width, rows: s.height });
        }
    }
    plans
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
