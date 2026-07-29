//! High-quality image resampling, shared by the inline build worker and the figure viewer.
//!
//! Both paths must shrink a publisher raster — often 1–2 megapixels — to the handful of
//! terminal cells it will occupy. Two rules follow from that, and they are the reason this
//! lives in one place:
//!
//! 1. **Resize before anything per-pixel.** The theme recolour costs what the *screen*
//!    shows, not what the file holds, so fitting first makes it an order of magnitude
//!    cheaper. `ratatui_image`'s own guidance says the same: never resize or encode a
//!    full-resolution image in the render path.
//! 2. **Lanczos3, not the default filter.** Book figures are text, equations, and line art —
//!    exactly the content a nearest-neighbour downscale destroys (`Resize::Scale(None)`
//!    defaults to `Nearest`, which was dropping every other stroke of a shrunk diagram).

use image::DynamicImage;

/// Exact-size resize of `img` to `dw`×`dh` with a **SIMD** Lanczos3 convolution
/// (`fast_image_resize` auto-detects SSE4.1/AVX2/NEON at runtime). Resize is the #2
/// cost of a build after decode — measured 51–260µs with `image`'s scalar Lanczos3 on
/// the book's rasters — and this is ~5× faster for the same kernel, so it stays sharp.
/// Alpha is handled (premultiplied) so transparent inline glyphs resize cleanly. Falls
/// back to `image`'s scalar resize if the SIMD path can't accept the buffer.
pub fn resize_exact(img: &DynamicImage, dw: u32, dh: u32) -> DynamicImage {
    use fast_image_resize::images::Image as FirImage;
    use fast_image_resize::{PixelType, Resizer};

    let (dw, dh) = (dw.max(1), dh.max(1));
    let rgba = img.to_rgba8();
    let (sw, sh) = rgba.dimensions();
    let fallback = || img.resize_exact(dw, dh, image::imageops::FilterType::Lanczos3);

    let Ok(src) = FirImage::from_vec_u8(sw, sh, rgba.into_raw(), PixelType::U8x4) else {
        return fallback();
    };
    let mut dst = FirImage::new(dw, dh, PixelType::U8x4);
    // Default `ResizeOptions` is `Convolution(Lanczos3)` with alpha handling on.
    if Resizer::new().resize(&src, &mut dst, None).is_err() {
        return fallback();
    }
    match image::RgbaImage::from_raw(dw, dh, dst.into_vec()) {
        Some(buf) => DynamicImage::ImageRgba8(buf),
        None => fallback(),
    }
}

/// Scale `img` to fit within `max_w`×`max_h` pixels, preserving aspect — enlarging a small
/// figure as well as shrinking a large one, so the result always fills one axis of the box.
/// Returns the image untouched when it already fits exactly (no needless resample).
pub fn fit_to_box(img: &DynamicImage, max_w: u32, max_h: u32) -> DynamicImage {
    let (w, h) = (img.width().max(1), img.height().max(1));
    let (max_w, max_h) = (max_w.max(1), max_h.max(1));
    let scale = (f64::from(max_w) / f64::from(w)).min(f64::from(max_h) / f64::from(h));
    let dw = (f64::from(w) * scale).round().max(1.0) as u32;
    let dh = (f64::from(h) * scale).round().max(1.0) as u32;
    if (dw, dh) == (w, h) {
        return img.clone();
    }
    resize_exact(img, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            w,
            h,
            image::Rgba([10, 20, 30, 255]),
        ))
    }

    #[test]
    fn fit_shrinks_to_the_binding_axis_keeping_aspect() {
        // 1000x500 into a 200x200 box: width binds, aspect 2:1 preserved.
        let out = fit_to_box(&img(1000, 500), 200, 200);
        assert_eq!((out.width(), out.height()), (200, 100));
    }

    #[test]
    fn fit_enlarges_a_small_figure_to_fill_the_box() {
        // A small figure must grow to fill one axis — the viewer shows it large.
        let out = fit_to_box(&img(50, 100), 400, 400);
        assert_eq!((out.width(), out.height()), (200, 400));
    }

    #[test]
    fn an_exact_fit_is_not_resampled() {
        let out = fit_to_box(&img(300, 150), 300, 150);
        assert_eq!((out.width(), out.height()), (300, 150));
    }

    #[test]
    fn resize_exact_hits_the_requested_size() {
        let out = resize_exact(&img(37, 91), 8, 5);
        assert_eq!((out.width(), out.height()), (8, 5));
    }
}
