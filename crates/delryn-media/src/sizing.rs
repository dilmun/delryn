//! Image display sizing: fitting figures and pages into the terminal cell grid.

/// How an image's display size was authored (mirrors `delryn_model::ImageWidth`,
/// kept here so this crate stays independent of the content model).
#[derive(Clone, Copy, PartialEq)]
pub enum SizeHint {
    /// No authored size — normalize to the target fraction of the column.
    Auto,
    /// A fraction of the column width (CSS %), 0.0–1.0.
    Pct(f32),
    /// An absolute CSS-pixel width.
    Px(u32),
    /// Fill the pane (preserving aspect), bounded only by the cols×rows box —
    /// for page-as-image content (PDF), not inline figures.
    Full,
}

/// Per-image sizing intent passed to [`target_cells`] / the build worker.
#[derive(Clone, Copy, PartialEq)]
pub struct SizeSpec {
    /// The authored display width, if any.
    pub hint: SizeHint,
    /// An equation rendered as a picture: kept at native size (proportional to
    /// the text), never normalized or enlarged.
    pub math: bool,
}

impl Default for SizeSpec {
    fn default() -> SizeSpec {
        SizeSpec {
            hint: SizeHint::Auto,
            math: false,
        }
    }
}

/// Upper bound on how far a low-resolution figure may be enlarged to reach its
/// target display width. Caps the softness from upscaling — and keeps genuinely
/// tiny images (icons, ornaments) from being blown up to fill the column.
const MAX_UPSCALE: f64 = 2.5;

/// The cell geometry and caps an image must fit into: terminal cell size
/// (`fw`×`fh` px), the available `cols`×`rows` box, the longest-side pixel cap
/// (`max_px`, 0 = none), and the default figure width (`target_pct`% of the
/// column) for images with no authored size.
#[derive(Clone, Copy)]
pub struct FitBox {
    pub fw: u16,
    pub fh: u16,
    pub cols: u16,
    pub rows: u16,
    pub max_px: u16,
    pub target_pct: u16,
}

/// Cell size (cols, rows) for a `w`×`h` px image. Figures are sized to a
/// *consistent display width* — the authored width (`spec.hint`) when known, else
/// `fit.target_pct`% of the column — enlarging low-res figures up to
/// [`MAX_UPSCALE`] so they aren't tiny, but never past the `fit.cols`×`fit.rows`
/// box. Equation images (`spec.math`) keep native size and only ever shrink to
/// fit. The longest displayed side is then capped to `fit.max_px` px to bound the
/// terminal transfer. Used by both the up-front row estimate and the background
/// build, so the two always agree (no gap).
pub fn target_cells(w: u32, h: u32, fit: FitBox, spec: SizeSpec) -> (u16, u16) {
    if w == 0 || h == 0 || fit.fw == 0 || fit.fh == 0 {
        return (1, 1);
    }
    let (wf, hf) = (w as f64, h as f64);
    let (fwf, fhf) = (f64::from(fit.fw), f64::from(fit.fh));
    // The most the aspect-preserving image can scale before it overflows the
    // column width or the viewport height.
    let cap = (f64::from(fit.cols) * fwf / wf).min(f64::from(fit.rows) * fhf / hf);

    let mut scale = if spec.math {
        // Equations read best at native size, proportional to the surrounding
        // text; only shrink to fit, never enlarge.
        cap.min(1.0)
    } else if matches!(spec.hint, SizeHint::Full) {
        // A full-bleed page (PDF): fill the pane box, preserving aspect —
        // enlarging a small page or shrinking a large one to the cols×rows box.
        cap
    } else {
        // The display width we want this figure to occupy, in pixels.
        let want_px = match spec.hint {
            SizeHint::Pct(p) => f64::from(fit.cols) * fwf * f64::from(p).clamp(0.0, 1.0),
            SizeHint::Px(px) => f64::from(px),
            SizeHint::Auto => f64::from(fit.cols) * fwf * f64::from(fit.target_pct) / 100.0,
            SizeHint::Full => unreachable!("full-bleed handled above"),
        };
        // Reach it (up- or down-scaling), but never blow up tiny art past the
        // upscale cap and never exceed the box.
        (want_px / wf).min(cap).min(MAX_UPSCALE)
    };
    if scale <= 0.0 {
        scale = cap.min(1.0);
    }

    // A full-bleed page is bounded by the pane itself; the per-figure pixel cap
    // (which bounds inline-figure transfers) would only letterbox it, so skip it.
    let longest = (wf * scale).max(hf * scale);
    if fit.max_px > 0 && longest > f64::from(fit.max_px) && !matches!(spec.hint, SizeHint::Full) {
        scale *= f64::from(fit.max_px) / longest;
    }
    let cols = ((wf * scale / fwf).ceil() as u16).clamp(1, fit.cols.max(1));
    let rows = ((hf * scale / fhf).ceil() as u16).clamp(1, fit.rows.max(1));
    (cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cell box `cols` wide (8×16px cells, `rows` tall, no px cap, 85% target).
    fn fit(cols: u16, rows: u16) -> FitBox {
        FitBox {
            fw: 8,
            fh: 16,
            cols,
            rows,
            max_px: 0,
            target_pct: 85,
        }
    }

    #[test]
    fn image_never_wider_than_the_text_column() {
        // A very wide image must be scaled to fit — its cell width can never
        // exceed the available text width, in single-page or two-page layout.
        for avail in [20u16, 48, 96, 200] {
            let (cols, _rows) = target_cells(4000, 600, fit(avail, 40), SizeSpec::default());
            assert!(
                cols <= avail,
                "avail={avail}: cols={cols} must not exceed it"
            );
        }
    }

    #[test]
    fn low_res_figures_normalize_up_but_bounded() {
        // A small figure is enlarged toward the target width (so figures look
        // consistent), not left tiny — but bounded by the upscale cap so it is
        // never blown up absurdly (an 80px image at most MAX_UPSCALE×).
        let (cols, _) = target_cells(80, 40, fit(200, 40), SizeSpec::default());
        assert!(
            cols > 10,
            "low-res figure is upscaled past native ~10 cols: {cols}"
        );
        assert!(cols <= 25, "but bounded by the upscale cap: {cols}");
    }

    #[test]
    fn equation_images_stay_native_size() {
        // Equation images keep native size (proportional to the text), never
        // normalized up to fill the column.
        let math = SizeSpec {
            hint: SizeHint::Auto,
            math: true,
        };
        let (cols, _) = target_cells(80, 40, fit(200, 40), math);
        assert!(
            cols <= 10,
            "equation at native ~10 cols, not stretched: {cols}"
        );
    }

    #[test]
    fn authored_width_is_honored() {
        // A 50% CSS width targets half the column regardless of pixel resolution.
        let half = SizeSpec {
            hint: SizeHint::Pct(0.5),
            math: false,
        };
        let (cols, _) = target_cells(4000, 2000, fit(100, 200), half);
        assert!(
            (i32::from(cols) - 50).abs() <= 2,
            "≈50% of 100 cols: {cols}"
        );
    }

    #[test]
    fn full_bleed_page_fills_the_pane() {
        // A full-bleed page (PDF) fills the pane, unlike a figure: it is bounded
        // only by the cols×rows box, ignoring the upscale cap and the px cap.
        let page = SizeSpec {
            hint: SizeHint::Full,
            math: false,
        };
        // A portrait (A4-ish) page in a wide-enough pane fills the column width.
        let (cols, _) = target_cells(1240, 1750, fit(100, 200), page);
        assert!(
            (i32::from(cols) - 100).abs() <= 1,
            "page fills width: {cols}"
        );

        // A small page is enlarged to fill — no MAX_UPSCALE cap (a figure of the
        // same size would stay near native size).
        let (small, _) = target_cells(80, 113, fit(100, 200), page);
        assert!(small >= 90, "small page upscales to fill: {small}");

        // The per-figure pixel cap must not letterbox a page (the pane bounds it).
        let capped = FitBox {
            max_px: 100,
            ..fit(100, 200)
        };
        let (cols_capped, _) = target_cells(1240, 1750, capped, page);
        assert!(
            (i32::from(cols_capped) - 100).abs() <= 1,
            "max_px does not shrink a full-bleed page: {cols_capped}"
        );
    }
}
