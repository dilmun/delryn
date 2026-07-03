//! Detect a page raster's content bounding box, so the reader can trim the
//! baked-in whitespace margins that make PDF text look small when fit to the
//! viewport. Theme-independent: run on the raw raster (paper is light), and the
//! resulting pixel box applies equally to the themed PNG (theming never moves
//! pixels).
//!
//! Uses row/column **ink projections** rather than a raw min/max, so an isolated
//! speck in the margin (common in scans) doesn't defeat the trim — a margin line
//! only counts as content when a real fraction of it is ink.

use crate::decode::decode;

/// Luma at or below this (0–255) counts as ink against a light page.
const INK_MAX_LUMA: u8 = 200;
/// Reject a trim that keeps less than this fraction of the page area — a likely
/// misdetection or near-blank page. The caller then shows the whole page.
const MIN_KEEP_FRAC: f32 = 0.15;

/// A page box `(x, y, w, h)` that **halves** each of the page's whitespace
/// margins around the content (not a tight crop — the page keeps half its
/// original breathing room). `None` when it can't decode, the page is (near)
/// blank, the box is implausibly small, or there's essentially nothing to trim —
/// in every case the caller should fall back to the whole page.
pub fn content_bbox(png: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let gray = decode(png)?.to_luma8();
    let (w, h) = (gray.width(), gray.height());
    if w == 0 || h == 0 {
        return None;
    }

    // Sample ~500 lines per axis — bbox precision to a step is ample (we pad),
    // and it keeps the scan cheap on a high-res raster.
    let step_x = (w / 500).max(1);
    let step_y = (h / 500).max(1);
    let mut col_ink = vec![0u32; w as usize];
    let mut row_ink = vec![0u32; h as usize];
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            if gray.get_pixel(x, y).0[0] <= INK_MAX_LUMA {
                col_ink[x as usize] += 1;
                row_ink[y as usize] += 1;
            }
            x += step_x;
        }
        y += step_y;
    }

    // A column/row is content when ≥1% of its sampled pixels are ink (≥2 min),
    // which rejects isolated specks while keeping any real text line.
    let rows_sampled = (h / step_y).max(1);
    let cols_sampled = (w / step_x).max(1);
    let col_thresh = (rows_sampled / 100).max(2);
    let row_thresh = (cols_sampled / 100).max(2);
    let min_x = (0..w).find(|&x| col_ink[x as usize] >= col_thresh)?;
    let max_x = (0..w).rev().find(|&x| col_ink[x as usize] >= col_thresh)?;
    let min_y = (0..h).find(|&y| row_ink[y as usize] >= row_thresh)?;
    let max_y = (0..h).rev().find(|&y| row_ink[y as usize] >= row_thresh)?;

    // Keep half of each original margin: the box edge sits halfway between the
    // content and the page edge, so the page loses half its whitespace, not all.
    let x0 = min_x / 2;
    let y0 = min_y / 2;
    let x1 = max_x + (w - 1 - max_x) / 2;
    let y1 = max_y + (h - 1 - max_y) / 2;
    let bw = x1 - x0 + 1;
    let bh = y1 - y0 + 1;

    // Nothing meaningful to trim, or an implausibly small box → whole page.
    if bw >= w && bh >= h {
        return None;
    }
    if (bw as f32 * bh as f32) < (w as f32 * h as f32) * MIN_KEEP_FRAC {
        return None;
    }
    Some((x0, y0, bw, bh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, GrayImage, Luma};

    fn png_of(img: GrayImage) -> Vec<u8> {
        let mut buf = Vec::new();
        DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn halves_the_margin_around_the_content() {
        // A 200×200 white page with a black block in [60,140) each axis (content),
        // so every margin is 60 px.
        let mut img = GrayImage::from_pixel(200, 200, Luma([255]));
        for y in 60..140 {
            for x in 60..140 {
                img.put_pixel(x, y, Luma([0]));
            }
        }
        let (x, y, w, h) = content_bbox(&png_of(img)).expect("content found");
        // Each 60px margin is halved (~30px kept) — not removed, not tight.
        assert!(
            x > 0 && x < 60,
            "left margin halved, not removed/tight: {x}"
        );
        assert!(y > 0 && y < 60, "top margin halved: {y}");
        // The content itself stays fully inside the box.
        assert!(x <= 60 && x + w >= 140, "content preserved horizontally");
        assert!(y <= 60 && y + h >= 140, "content preserved vertically");
    }

    #[test]
    fn blank_page_is_not_trimmed() {
        let img = GrayImage::from_pixel(200, 200, Luma([255]));
        assert_eq!(content_bbox(&png_of(img)), None, "nothing to trim");
    }

    #[test]
    fn a_speck_does_not_defeat_the_trim() {
        // A large content block (well over the min-keep area) plus a single
        // stray speck out in the margin corner.
        let mut img = GrayImage::from_pixel(300, 300, Luma([255]));
        for y in 60..260 {
            for x in 60..260 {
                img.put_pixel(x, y, Luma([0]));
            }
        }
        img.put_pixel(5, 5, Luma([0])); // lone speck
        let (x, y, _, _) = content_bbox(&png_of(img)).expect("content found");
        assert!(x > 5 && y > 5, "the speck didn't extend the box: {x},{y}");
    }

    #[test]
    fn full_bleed_content_is_not_trimmed() {
        // An all-dark page: everything is "ink", so there's nothing to trim.
        let img = GrayImage::from_pixel(200, 200, Luma([10]));
        assert_eq!(content_bbox(&png_of(img)), None);
    }
}
