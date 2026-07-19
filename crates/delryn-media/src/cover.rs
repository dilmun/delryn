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

/// Longest-side pixel bound a cover is downscaled to **in the worker**, before the terminal
/// protocol is built. A publisher cover is ~1600×2400; resizing that to a grid card and
/// Kitty-encoding it at *render time* (on the main thread, for a whole screenful at once) is
/// what made the grid stutter on a fast scroll. Shrinking to this bound first makes the
/// render-time resize + encode ~20× cheaper (and corner-rounding touches ~20× fewer pixels),
/// while staying crisp for any grid card size on a hi-DPI cell. See `ratatui_image`'s own
/// guidance: never resize/encode a full-resolution image in the render path.
const COVER_THUMB_MAX: u32 = 512;

/// The decode + downscale + rounded-corner step of a cover, split out so it can run **off the
/// main thread** (its output is a plain `RgbaImage`, which is `Send` — unlike the terminal
/// protocol). Returns the rounded thumbnail RGBA plus the *source* dimensions (so the
/// aspect-ratio sizing elsewhere is unchanged), or `None` if the bytes aren't a decodable
/// image. Pair with [`wrap_cover`] on the main thread.
pub fn decode_cover(bytes: &[u8]) -> Option<(RgbaImage, (u32, u32))> {
    decode(bytes).map(|img| {
        let dims = (img.width(), img.height());
        // Downscale to a bounded thumbnail here (off the render loop) so the main-thread
        // protocol resize + Kitty encode work on a small image, not the full-res cover. A
        // fast box filter is ample — the terminal downsamples again to the cell anyway.
        let thumb = if dims.0 > COVER_THUMB_MAX || dims.1 > COVER_THUMB_MAX {
            img.thumbnail(COVER_THUMB_MAX, COVER_THUMB_MAX)
        } else {
            img
        };
        let radius = thumb.width().min(thumb.height()) / COVER_CORNER_DIV;
        (round_corners(&thumb, radius), dims)
    })
}

/// Wrap a [`decode_cover`] result into a terminal resize protocol for `picker`. Cheap (no
/// decode), so it stays on the main thread where the `picker` lives.
pub fn wrap_cover(picker: &Picker, rounded: RgbaImage, dims: (u32, u32)) -> CoverImage {
    CoverImage {
        proto: picker.new_resize_protocol(DynamicImage::ImageRgba8(rounded)),
        dims,
    }
}

/// Decode `bytes` and build a resize protocol for `picker`, capturing the source
/// dimensions. The corners are rounded (transparent) so the cover reads as a card
/// rather than a hard rectangle. `None` if the bytes aren't a decodable image. The
/// synchronous convenience form of [`decode_cover`] + [`wrap_cover`].
pub fn build_cover(picker: &Picker, bytes: &[u8]) -> Option<CoverImage> {
    decode_cover(bytes).map(|(rounded, dims)| wrap_cover(picker, rounded, dims))
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
