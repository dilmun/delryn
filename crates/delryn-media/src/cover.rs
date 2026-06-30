//! Book-cover decoding into a terminal protocol, with rounded card corners.

use image::{DynamicImage, RgbaImage};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::decode::decode;

/// A decoded cover plus its source pixel dimensions, so the renderer can size a
/// render rect to the cover's aspect ratio (filling it with no letterbox).
pub struct CoverImage {
    pub proto: StatefulProtocol,
    /// Source pixel dimensions (w, h).
    pub dims: (u32, u32),
}

impl CoverImage {
    /// The terminal (Kitty) image id, if any — used to delete it when this cover
    /// is evicted from the library cache, so terminal image memory stays bounded.
    pub fn image_id(&self) -> Option<u32> {
        self.proto.image_id()
    }
}

/// How much of a cover's shorter side becomes its corner radius (1/N).
const COVER_CORNER_DIV: u32 = 18;

/// Decode `bytes` and build a resize protocol for `picker`, capturing the source
/// dimensions. The corners are rounded (transparent) so the cover reads as a card
/// rather than a hard rectangle. `None` if the bytes aren't a decodable image.
pub fn build_cover(picker: &Picker, bytes: &[u8]) -> Option<CoverImage> {
    decode(bytes).map(|img| {
        let dims = (img.width(), img.height());
        let radius = dims.0.min(dims.1) / COVER_CORNER_DIV;
        let rounded = DynamicImage::ImageRgba8(round_corners(&img, radius));
        CoverImage {
            proto: picker.new_resize_protocol(rounded),
            dims,
        }
    })
}

/// Return `img` as RGBA with its four corners rounded to `radius` px: pixels
/// outside the rounded rectangle are made transparent (with a 1px soft edge), so
/// the terminal background shows through and the cover looks like a rounded card.
fn round_corners(img: &DynamicImage, radius: u32) -> RgbaImage {
    let mut rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let r = radius.min(w / 2).min(h / 2);
    if r == 0 {
        return rgba;
    }
    let rf = r as f32;
    for (x, y, px) in rgba.enumerate_pixels_mut() {
        // Distance of the pixel centre past the corner arc's centre, per axis —
        // `None` when the pixel isn't within `r` of that edge (no rounding).
        let dx = if x < r {
            Some(rf - 0.5 - x as f32)
        } else if x >= w - r {
            Some(x as f32 + 0.5 - (w - r) as f32)
        } else {
            None
        };
        let dy = if y < r {
            Some(rf - 0.5 - y as f32)
        } else if y >= h - r {
            Some(y as f32 + 0.5 - (h - r) as f32)
        } else {
            None
        };
        if let (Some(dx), Some(dy)) = (dx, dy) {
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > rf {
                px[3] = 0;
            } else if dist > rf - 1.0 {
                let edge = ((rf - dist) * 255.0) as u16;
                px[3] = px[3].min(edge.min(255) as u8);
            }
        }
    }
    rgba
}
