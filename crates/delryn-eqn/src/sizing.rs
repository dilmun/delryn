//! Universal, text-relative sizing. One rule for every render variant: an equation is
//! sized relative to the surrounding text, never to its own pixels — so a 150-DPI GIF, a
//! 600-DPI PNG, and a re-typeset equation of the same formula all land at the prose height.
//! Pure math (no I/O, no decoding), so it is exhaustively unit-testable. See
//! `docs/MATH-RENDERING.md`.

use crate::ir::PictureSize;
use crate::render::Raster;

/// Terminal cell geometry, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub w: u16,
    pub h: u16,
}

/// A computed placement: the cell footprint plus where the text baseline sits within it
/// (rows from the top, fractional), so inline math aligns to the surrounding text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub cols: u16,
    pub rows: u16,
    pub baseline_row: f32,
}

/// The transparent padding (px) the render stage adds around a typeset raster — must match
/// `render`'s `RenderOptions.padding` so the placement math agrees with the pixels.
const RENDER_PAD_PX: f32 = 2.0;

/// The text em in pixels for a cell height: the single size everything is normalized to
/// (typeset renders at it; pictures are scaled so their ink line-height matches it).
pub fn em_text_px(cell_h: u16, factor: f32) -> u32 {
    (f32::from(cell_h.max(1)) * factor).round().max(1.0) as u32
}

/// Place a typeset raster, from its em metrics and the em it was rendered at. The raster's
/// pixel size is `width/height/depth × em_px` plus the fixed padding; the baseline sits
/// `pad + height × em_px` below the top.
pub fn size_typeset(r: &Raster, em_px: u32, cell: Cell) -> Placement {
    let em = em_px as f32;
    let ch = f32::from(cell.h.max(1));
    let cw = f32::from(cell.w.max(1));
    let px_w = (r.width * em + 2.0 * RENDER_PAD_PX).ceil().max(1.0);
    let px_h = ((r.height + r.depth) * em + 2.0 * RENDER_PAD_PX)
        .ceil()
        .max(1.0);
    Placement {
        cols: (px_w / cw).ceil().max(1.0) as u16,
        rows: (px_h / ch).ceil().max(1.0) as u16,
        baseline_row: (RENDER_PAD_PX + r.height * em) / ch,
    }
}

/// Place a publisher picture text-relative. The **display width** comes from the authored
/// CSS size (`em`/`ex`, exact and DPI-independent) or, absent that, from a measured ink
/// line-height scaled to the text em (`ink_line_px`); the picture's own pixel resolution
/// only sets the aspect ratio, so it never renders out of scale. `pic_px` is the decoded
/// `(width, height)` in pixels.
pub fn size_picture(
    size: PictureSize,
    pic_px: (u32, u32),
    em_px: u32,
    cell: Cell,
    ink_line_px: Option<f32>,
) -> Placement {
    let em = em_px as f32;
    let pw = pic_px.0.max(1) as f32;
    let ph = pic_px.1.max(1) as f32;
    let display_w = match size {
        PictureSize::Em(w) => w * em,
        PictureSize::Ex(w) => w * 0.5 * em, // 1ex ≈ 0.5em
        PictureSize::MeasureInk => match ink_line_px {
            Some(l) if l > 0.0 => pw * (em / l), // scale so one ink line == the text em
            _ => pw,                             // no measure available → native (last resort)
        },
    };
    let scale = display_w / pw;
    let display_h = ph * scale;
    let cw = f32::from(cell.w.max(1));
    let ch = f32::from(cell.h.max(1));
    let rows = (display_h / ch).ceil().max(1.0) as u16;
    Placement {
        cols: (display_w / cw).ceil().max(1.0) as u16,
        rows,
        // A picture carries no baseline metric; sit it on the row bottom.
        baseline_row: rows as f32,
    }
}

/// Shrink a placement uniformly so it fits `avail_cols`, preserving aspect (a wide equation
/// never overflows the column). A no-op when it already fits.
pub fn fit_columns(p: Placement, avail_cols: u16) -> Placement {
    if avail_cols == 0 || p.cols <= avail_cols {
        return p;
    }
    let scale = f32::from(avail_cols) / f32::from(p.cols.max(1));
    Placement {
        cols: avail_cols,
        rows: (f32::from(p.rows) * scale).ceil().max(1.0) as u16,
        baseline_row: p.baseline_row * scale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: Cell = Cell { w: 8, h: 16 };

    #[test]
    fn text_em_tracks_cell_height() {
        assert_eq!(em_text_px(16, 1.0), 16);
        assert_eq!(em_text_px(20, 0.9), 18);
    }

    #[test]
    fn typeset_placement_from_em_metrics() {
        // A ~2em-wide, 1em-tall raster at em_px=20: px ≈ 44×24 (+4 pad) → 6×2 cells.
        let r = Raster {
            png: Vec::new(),
            width: 2.0,
            height: 0.8,
            depth: 0.2,
        };
        let p = size_typeset(&r, 20, CELL);
        assert_eq!(
            (p.cols, p.rows),
            (6, 2),
            "cols/rows from em×em_px + pad: {p:?}"
        );
        assert!(p.baseline_row > 0.0, "baseline within the raster");
    }

    #[test]
    fn picture_em_width_is_dpi_independent() {
        // 4em wide at em_px=20 → 80px display width → 10 cols, regardless of the file's
        // pixel resolution (only the aspect comes from the pixels).
        let lo = size_picture(PictureSize::Em(4.0), (200, 100), 20, CELL, None);
        let hi = size_picture(PictureSize::Em(4.0), (800, 400), 20, CELL, None);
        assert_eq!(
            lo, hi,
            "same em width → same placement at any DPI: {lo:?} vs {hi:?}"
        );
        assert_eq!(lo.cols, 10, "4em × 20px/em ÷ 8px/cell = 10 cols");
    }

    #[test]
    fn picture_measured_ink_scales_to_text() {
        // Ink line = 40px, text em = 20px → scale 0.5 → a 100×50 picture displays 50×25.
        let p = size_picture(PictureSize::MeasureInk, (100, 50), 20, CELL, Some(40.0));
        assert_eq!(p.cols, (50.0f32 / 8.0).ceil() as u16);
        assert_eq!(p.rows, (25.0f32 / 16.0).ceil() as u16);
    }

    #[test]
    fn ex_width_is_half_em() {
        let e = size_picture(PictureSize::Em(2.0), (100, 50), 20, CELL, None);
        let x = size_picture(PictureSize::Ex(4.0), (100, 50), 20, CELL, None);
        assert_eq!(e.cols, x.cols, "4ex == 2em width");
    }

    #[test]
    fn wide_equation_fits_the_column() {
        let wide = Placement {
            cols: 60,
            rows: 3,
            baseline_row: 3.0,
        };
        let fit = fit_columns(wide, 20);
        assert!(fit.cols <= 20, "clamped to the column: {fit:?}");
        assert!(fit.rows <= 3, "scaled down proportionally");
    }
}
