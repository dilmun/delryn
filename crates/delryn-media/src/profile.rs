//! Ink profiling for publisher equation images.
//!
//! Books ship display equations two ways: as recoverable LaTeX (which delryn
//! re-renders crisply at the right size) or as a fixed **raster** — a PNG whose
//! pixel resolution varies wildly between publishers, so the same equation is 40px
//! tall in one book and 200px in another. Sizing such a raster by its raw pixels is
//! the Kindle mistake: too huge in one book, too small in the next.
//!
//! [`ink_profile`] measures the raster's *ink* instead — its tight bounding box and
//! the height of one equation line — so the reader can normalise it to a
//! text-relative size (DPI-independent), exactly like the re-rendered LaTeX. It runs
//! once, off-thread, when a section is decoded, and the result rides along on the
//! block (see `delryn_model::InkProfile`). Photographs and dense diagrams return
//! `None` — they are figures, sized to the column, not equations.

use image::DynamicImage;

use crate::recolor::{INK_CHROMA_MAX, analyze_background, ink_coverage, opaque_chroma};

/// Per-pixel ink coverage below this is treated as background (antialiasing haze),
/// so faint edges don't inflate the bounding box or the density estimate.
const INK_PIXEL_MIN: f32 = 0.15;

/// Overall ink fraction above which a graphic is too *dense* to be an equation
/// (photos, filled diagrams, colour charts) — equations are sparse strokes. Gates
/// out figures even when they happen to be greyscale (so chroma alone can't tell).
const INK_DENSITY_MAX: f32 = 0.45;

/// A row/column counts as "ink" when its ink mass reaches this fraction of the
/// peak row/column — low enough to keep ascenders and superscripts in the bbox,
/// high enough to ignore stray antialiasing.
const INK_LINE_FRAC: f32 = 0.04;

/// Two ink runs belong to the same equation line when the blank gap between them is
/// under this fraction of the typical line height (bridges intra-line gaps — a
/// fraction bar, a stacked limit — without merging separate array rows).
const INTERLINE_FRAC: f32 = 0.55;

/// A gap must be at least this many pixels to ever split a line, so 1px
/// antialiasing gaps in a single glyph row never fragment a line.
const MIN_BREAK_PX: f32 = 3.0;

/// Connected components shorter than this are specks / thin rules (fraction bars,
/// minus signs), not glyphs — excluded from the glyph-em estimate.
const MIN_GLYPH_PX: f32 = 3.0;

/// The measured ink geometry of an equation raster (mirrors `delryn_model::InkProfile`
/// — this crate stays independent of the content model). Produced by [`ink_profile`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkProfile {
    /// Tight ink bounding box in source pixels (1px margin), `[x0,x1) × [y0,y1)`.
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    /// The text em in px — the base-glyph cluster height, robust to tall operators
    /// (`Σ`, fractions) and small subscripts, so a plain line and a busy one measure
    /// the same size.
    pub line_px: f32,
    /// Ink-line count (≈ rows of a multi-line array); 1 for a single equation.
    pub line_count: u16,
}

impl InkProfile {
    /// The ink bounding-box size in pixels (`f64` for the sizing math). This is the
    /// *effective* image size — the whitespace margins are cropped away.
    pub fn bbox_dims(&self) -> (f64, f64) {
        (
            f64::from(self.x1.saturating_sub(self.x0)).max(1.0),
            f64::from(self.y1.saturating_sub(self.y0)).max(1.0),
        )
    }
}

/// Measure the ink of an equation raster, or `None` when it isn't one — a colourful
/// or dense graphic (a photo / filled figure), or a blank image. Reuses the
/// `recolor` background/ink model so the ink notion matches the theme recolour.
pub fn ink_profile(img: &DynamicImage) -> Option<InkProfile> {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    // Colourful strokes ⇒ a picture, not monochrome equation ink.
    if opaque_chroma(&rgba) >= INK_CHROMA_MAX {
        return None;
    }

    let bg = analyze_background(&rgba);
    let (wu, hu) = (w as usize, h as usize);
    let mut mask = vec![false; wu * hu];
    let (mut row_ink, mut col_ink) = (vec![0f32; hu], vec![0f32; wu]);
    let mut total = 0f32;
    for y in 0..hu {
        for x in 0..wu {
            let c = ink_coverage(rgba.get_pixel(x as u32, y as u32), &bg);
            if c > INK_PIXEL_MIN {
                mask[y * wu + x] = true;
                row_ink[y] += c;
                col_ink[x] += c;
                total += c;
            }
        }
    }
    // Sparse line-art only: a dense graphic is a figure, sized to the column.
    if total / (w as f32 * h as f32) > INK_DENSITY_MAX {
        return None;
    }

    let (x0, x1) = ink_span(&col_ink)?;
    let (y0, y1) = ink_span(&row_ink)?;

    // The text em ≈ the densest cluster of glyph (connected-component) heights — the
    // base body glyphs, apart from the small sub/superscripts and the tall operators —
    // so it tracks the body text size, not the full bbox, and a plain line and a busy
    // one measure the same. Fall back to the bbox height when no sizeable glyph found.
    let line_px = glyph_em(&mask, wu, x0, x1, y0, y1).unwrap_or((y1 - y0) as f32);
    let line_count = count_lines(&row_ink, y0, y1);

    // Pad the bbox by 1px (clamped) so the crisp crop keeps antialiased edges.
    Some(InkProfile {
        x0: x0.saturating_sub(1) as u32,
        y0: y0.saturating_sub(1) as u32,
        x1: (x1 + 1).min(wu) as u32,
        y1: (y1 + 1).min(hu) as u32,
        line_px: line_px.max(1.0),
        line_count,
    })
}

/// The `[lo, hi)` span of indices whose mass reaches [`INK_LINE_FRAC`] of the peak —
/// the ink bounding box along one axis. `None` when there is no ink.
fn ink_span(mass: &[f32]) -> Option<(usize, usize)> {
    let peak = mass.iter().copied().fold(0.0f32, f32::max);
    if peak <= 0.0 {
        return None;
    }
    let thresh = INK_LINE_FRAC * peak;
    let lo = mass.iter().position(|&m| m >= thresh)?;
    let hi = mass.iter().rposition(|&m| m >= thresh)? + 1;
    Some((lo, hi))
}

/// A robust estimate of the text em: the centre of the densest cluster of connected
/// ink-component (glyph) heights (see [`cluster_centre`]). The base body glyphs are
/// the tightest, most numerous cluster, so this ignores the small sub/superscripts and
/// the tall operators (Σ, fractions) *however many* there are — every equation recovers
/// the same font size and normalises consistently. Specks and thin rules (below
/// [`MIN_GLYPH_PX`]) are dropped. 4-connected flood fill; `None` when nothing sizeable
/// is found.
fn glyph_em(mask: &[bool], w: usize, x0: usize, x1: usize, y0: usize, y1: usize) -> Option<f32> {
    let mut visited = vec![false; mask.len()];
    let mut heights: Vec<f32> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for sy in y0..y1 {
        for sx in x0..x1 {
            let start = sy * w + sx;
            if !mask[start] || visited[start] {
                continue;
            }
            visited[start] = true;
            stack.push(start);
            let (mut top, mut bot) = (sy, sy);
            while let Some(i) = stack.pop() {
                let (x, y) = (i % w, i / w);
                top = top.min(y);
                bot = bot.max(y);
                let visit = |ni: usize, visited: &mut [bool], stack: &mut Vec<usize>| {
                    if mask[ni] && !visited[ni] {
                        visited[ni] = true;
                        stack.push(ni);
                    }
                };
                if x > x0 {
                    visit(i - 1, &mut visited, &mut stack);
                }
                if x + 1 < x1 {
                    visit(i + 1, &mut visited, &mut stack);
                }
                if y > y0 {
                    visit(i - w, &mut visited, &mut stack);
                }
                if y + 1 < y1 {
                    visit(i + w, &mut visited, &mut stack);
                }
            }
            let ch = (bot - top + 1) as f32;
            if ch >= MIN_GLYPH_PX {
                heights.push(ch);
            }
        }
    }
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(cluster_centre(&heights))
}

/// The centre of the densest cluster in an ascending, non-empty slice — the
/// "half-sample mode". Finds the shortest window holding half the values (the tightest,
/// most numerous group — the base glyphs) and returns its middle, so sparse outliers on
/// either side (subscripts below, tall operators above) don't move it, regardless of
/// how many there are.
fn cluster_centre(sorted: &[f32]) -> f32 {
    let n = sorted.len();
    if n <= 2 {
        return sorted[n / 2];
    }
    let half = n.div_ceil(2);
    let mut best = (f32::INFINITY, 0usize);
    for i in 0..=(n - half) {
        let range = sorted[i + half - 1] - sorted[i];
        if range < best.0 {
            best = (range, i);
        }
    }
    sorted[best.1 + half / 2]
}

/// Count the equation's ink lines: raw ink-row runs merged across small (intra-line)
/// gaps and split on large (inter-line) ones. Used only to tell a sparse equation
/// from a many-row diagram in the classifier — the sizing uses the glyph em above.
fn count_lines(row_ink: &[f32], y0: usize, y1: usize) -> u16 {
    let peak = row_ink[y0..y1].iter().copied().fold(0.0f32, f32::max);
    let is_ink = |y: usize| row_ink[y] >= INK_LINE_FRAC * peak;

    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for y in y0..y1 {
        match (is_ink(y), start) {
            (true, None) => start = Some(y),
            (false, Some(s)) => {
                runs.push((s, y));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, y1));
    }
    if runs.len() <= 1 {
        return 1;
    }

    // Merge runs separated by a gap smaller than a line-height fraction (intra-line
    // gaps: fraction bars, stacked limits) so only genuine row breaks split a line.
    let line_h = percentile(runs.iter().map(|&(a, b)| (b - a) as f32), 0.5);
    let break_gap = (INTERLINE_FRAC * line_h).max(MIN_BREAK_PX);
    let mut bands = 1u16;
    let mut prev_end = runs[0].1;
    for (a, b) in runs.into_iter().skip(1) {
        if (a as f32 - prev_end as f32) >= break_gap {
            bands += 1;
        }
        prev_end = b;
    }
    bands
}

/// The `p`-quantile (0.0–1.0) of `vals` by nearest rank (0.0 if empty). `p = 0.5` is
/// the median; a higher `p` biases toward the taller values.
fn percentile(vals: impl Iterator<Item = f32>, p: f32) -> f32 {
    let mut v: Vec<f32> = vals.collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = (p.clamp(0.0, 1.0) * (v.len() - 1) as f32).round() as usize;
    v[idx.min(v.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// A transparent image with opaque black horizontal ink `bands` (each `[y0,y1)`),
    /// inked across the middle half of the width — the shape of a real equation PNG.
    fn banded(w: u32, h: u32, bands: &[(u32, u32)]) -> DynamicImage {
        let mut img = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        for &(y0, y1) in bands {
            for y in y0..y1 {
                for x in (w / 4)..(3 * w / 4) {
                    img.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                }
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn single_band_is_one_line() {
        let p = ink_profile(&banded(200, 100, &[(40, 60)])).expect("an equation");
        assert_eq!(p.line_count, 1);
        assert!((p.line_px - 20.0).abs() <= 4.0, "line_px≈20: {}", p.line_px);
        assert!(p.y0 <= 40 && p.y1 >= 60, "bbox wraps the ink: {p:?}");
        assert!(p.x0 >= 40 && p.x1 <= 160, "bbox tight in x: {p:?}");
    }

    /// A tall operator (Σ, big parens) is a minority component the median ignores —
    /// so the em tracks the body glyphs, keeping a busy line the same size as a plain
    /// one. This is what makes the on-screen size consistent.
    #[test]
    fn cluster_centre_finds_base_glyphs() {
        // Small subscripts (6), a dense base cluster (12), tall operators (30): the
        // base size is recovered whether operators or subscripts dominate the extremes.
        let subs = [6.0, 6.0, 6.0, 12.0, 12.0, 12.0, 12.0, 12.0, 30.0];
        assert_eq!(cluster_centre(&subs), 12.0);
        let ops = [12.0, 12.0, 12.0, 12.0, 30.0, 30.0, 30.0];
        assert_eq!(cluster_centre(&ops), 12.0);
    }

    #[test]
    fn glyph_em_ignores_tall_operators() {
        let mut img = RgbaImage::from_pixel(120, 60, Rgba([0, 0, 0, 0]));
        let mut fill = |x0: u32, y0: u32, x1: u32, y1: u32| {
            for y in y0..y1 {
                for x in x0..x1 {
                    img.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                }
            }
        };
        // Five body glyphs 10px tall on the line…
        for i in 0..5u32 {
            let x = 10 + i * 15;
            fill(x, 25, x + 8, 35);
        }
        // …and one 40px-tall operator spike.
        fill(100, 10, 108, 50);
        let p = ink_profile(&DynamicImage::ImageRgba8(img)).expect("an equation");
        assert!(
            (p.line_px - 10.0).abs() <= 3.0,
            "em tracks the glyphs (~10px), not the 40px spike: {}",
            p.line_px
        );
    }

    #[test]
    fn gapped_bands_count_as_lines() {
        // Three bands separated by wide gaps → three equation lines.
        let p =
            ink_profile(&banded(200, 200, &[(10, 22), (46, 58), (82, 94)])).expect("an equation");
        assert_eq!(p.line_count, 3, "three ink bands → 3 lines");
    }

    #[test]
    fn close_runs_merge_into_one_line() {
        // A 2px gap (a fraction bar) is bridged — one line, not two.
        let p = ink_profile(&banded(200, 100, &[(40, 50), (52, 62)])).expect("an equation");
        assert_eq!(p.line_count, 1, "tiny gap bridged into one line");
    }

    #[test]
    fn blank_image_has_no_profile() {
        assert!(ink_profile(&banded(100, 100, &[])).is_none());
    }

    #[test]
    fn colourful_image_is_not_an_equation() {
        // A saturated gradient (a photo / colour chart) → None, sized as a figure.
        let mut img = RgbaImage::new(64, 64);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            *px = Rgba([(x * 4) as u8, 40, 200, 255]);
        }
        assert!(ink_profile(&DynamicImage::ImageRgba8(img)).is_none());
    }

    #[test]
    fn dense_ink_is_not_an_equation() {
        // Ink over more than INK_DENSITY_MAX of the frame is a figure, not a sparse
        // equation — even in greyscale (chroma alone can't tell).
        let mut img = RgbaImage::from_pixel(64, 64, Rgba([0, 0, 0, 0]));
        for y in 0..48 {
            for x in 0..64 {
                img.put_pixel(x, y, Rgba([0, 0, 0, 255])); // 48/64 = 75% inked
            }
        }
        assert!(ink_profile(&DynamicImage::ImageRgba8(img)).is_none());
    }
}
