//! Image display sizing: fitting figures and pages into the terminal cell grid.

use delryn_infra::config::ImageFit;

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
    /// Real math rendered as a picture (from LaTeX/MathML): always kept at native
    /// size, proportional to the text, never normalized or enlarged.
    pub math: bool,
    /// Whether the image carries a caption. Captions are the reliable
    /// figure/table-vs-equation signal in books: figures and tables are captioned
    /// (and normalize to the column band), while equation pictures are uncaptioned
    /// (and stay text-proportional). Only consulted in [`ImageFit::Fit`].
    pub captioned: bool,
}

impl Default for SizeSpec {
    fn default() -> SizeSpec {
        SizeSpec {
            hint: SizeHint::Auto,
            math: false,
            captioned: false,
        }
    }
}

/// Upper bound on how far a low-resolution figure may be enlarged to reach its
/// target display width. Bounds the softness from upscaling (paired with a
/// quality resampling filter in the build step) while still letting a low-res
/// figure grow enough to be readable and consistent with its neighbours.
const MAX_UPSCALE: f64 = 4.0;

/// Target display height (in text lines) an equation picture is auto-boosted *up*
/// to when it's rendering smaller than this — so a low-resolution equation (whose
/// glyphs are packed too small to read) grows to a legible size. Taller equations
/// (multi-line arrays) already exceed it and keep native size; the user's
/// `eq_scale` knob tunes on top for the rest.
const EQUATION_MIN_LINES: f64 = 2.0;

/// Upper bound on the *automatic* low-resolution boost (quality guard). The user's
/// `eq_scale` knob can still enlarge past this deliberately (bounded only by the
/// column/viewport).
const EQUATION_AUTO_MAX: f64 = 2.5;

/// The cell geometry and caps an image must fit into: terminal cell size
/// (`fw`×`fh` px), the available `cols`×`rows` box, the longest-side pixel cap
/// (`max_px`, 0 = none), the default/normalized figure width (`target_pct`% of
/// the column), the equation-picture size knob (`eq_scale`%), and the sizing
/// policy (`fit_mode`: normalize vs. faithful).
#[derive(Clone, Copy)]
pub struct FitBox {
    pub fw: u16,
    pub fh: u16,
    pub cols: u16,
    pub rows: u16,
    pub max_px: u16,
    pub target_pct: u16,
    pub eq_scale: u16,
    pub fit_mode: ImageFit,
}

/// Cell size (cols, rows) for a `w`×`h` px image.
///
/// Figures are sized to a *consistent display width* so they look the same across
/// books, regardless of the publisher's authored width or the file's resolution
/// (both unreliable). In [`ImageFit::Fit`] (the default) that width is
/// `fit.target_pct`% of the column — the authored width is deliberately ignored;
/// in [`ImageFit::Faithful`] the authored width (`spec.hint`) is honored, else the
/// same `target_pct` default. Either way a low-res figure is enlarged up to
/// [`MAX_UPSCALE`] so it isn't tiny, but never past the `fit.cols`×`fit.rows` box.
///
/// Equations are sized proportional to the text, not stretched to the column.
/// Real math (`spec.math`) shows native (its size comes from `math_scale` at
/// render time). In `Fit` mode an *uncaptioned* graphic is an equation picture
/// (captioned graphics are figures/tables — captions are the reliable
/// figure-vs-equation signal, where pixel shape cannot tell a wide table from a
/// wide equation or a tall array from a tall figure): it shows native, but a
/// low-resolution one (glyphs too small to read) is auto-boosted up toward
/// [`EQUATION_MIN_LINES`] tall (quality-capped by [`EQUATION_AUTO_MAX`]), then
/// scaled by the user's `eq_scale` knob. Full-bleed pages (`SizeHint::Full`) fill
/// the pane.
///
/// The longest displayed side is finally capped to `fit.max_px` px to bound the
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

    // An uncaptioned graphic (in Fit mode) is a display equation shipped as a
    // picture — captioned graphics are figures/tables. It reads best proportional
    // to the text, not stretched to the column.
    let is_equation_pic = fit.fit_mode == ImageFit::Fit && !spec.captioned && !spec.math;

    let mut scale = if matches!(spec.hint, SizeHint::Full) {
        // A full-bleed page (PDF): fill the pane box, preserving aspect —
        // enlarging a small page or shrinking a large one to the cols×rows box.
        cap
    } else if spec.math {
        // Real math (LaTeX/MathML) is already sized by `math_scale` at render
        // time — show it native, only shrinking to fit.
        cap.min(1.0)
    } else if is_equation_pic {
        // An equation picture. Keep it proportional to the text, but auto-boost a
        // low-resolution one (whose glyphs are packed too small to read) up toward
        // a readable height, then apply the user's `eq_scale` knob. Bounded by the
        // box; the automatic part is additionally quality-capped.
        let auto = (EQUATION_MIN_LINES * fhf / hf).clamp(1.0, EQUATION_AUTO_MAX);
        let knob = f64::from(fit.eq_scale) / 100.0;
        (auto * knob).min(cap)
    } else {
        // A figure/table/diagram. The display width we want it to occupy: in Fit
        // mode a consistent fraction of the column (authored width ignored — it's
        // as unreliable as the pixel resolution); in Faithful mode the authored
        // width, else the same normalized default.
        let want_px = if fit.fit_mode == ImageFit::Fit {
            f64::from(fit.cols) * fwf * f64::from(fit.target_pct) / 100.0
        } else {
            match spec.hint {
                SizeHint::Pct(p) => f64::from(fit.cols) * fwf * f64::from(p).clamp(0.0, 1.0),
                SizeHint::Px(px) => f64::from(px),
                SizeHint::Auto => f64::from(fit.cols) * fwf * f64::from(fit.target_pct) / 100.0,
                SizeHint::Full => unreachable!("full-bleed handled above"),
            }
        };
        // Reach it (up- or down-scaling), but never blow up low-res art past the
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

    /// A cell box `cols` wide (8×16px cells, `rows` tall, no px cap, 85% target)
    /// in the default `Fit` (normalizing) mode.
    fn fit(cols: u16, rows: u16) -> FitBox {
        FitBox {
            fw: 8,
            fh: 16,
            cols,
            rows,
            max_px: 0,
            target_pct: 85,
            eq_scale: 100,
            fit_mode: ImageFit::Fit,
        }
    }

    /// The same box in `Faithful` mode (honors the authored width).
    fn faithful(cols: u16, rows: u16) -> FitBox {
        FitBox {
            fit_mode: ImageFit::Faithful,
            ..fit(cols, rows)
        }
    }

    /// A captioned figure/table (normalizes to the column band), no authored size.
    fn fig() -> SizeSpec {
        SizeSpec {
            hint: SizeHint::Auto,
            math: false,
            captioned: true,
        }
    }

    /// An uncaptioned equation picture (stays text-proportional), no authored size.
    fn eq() -> SizeSpec {
        SizeSpec::default()
    }

    #[test]
    fn figure_never_wider_than_the_text_column() {
        // A very wide figure must be scaled to fit — its cell width can never
        // exceed the available text width, in single-page or two-page layout.
        for avail in [20u16, 48, 96, 200] {
            let (cols, _rows) = target_cells(4000, 600, fit(avail, 40), fig());
            assert!(
                cols <= avail,
                "avail={avail}: cols={cols} must not exceed it"
            );
        }
    }

    #[test]
    fn low_res_captioned_figures_normalize_up_but_bounded() {
        // A small captioned figure is enlarged toward the target width (so figures
        // look consistent), not left tiny — but bounded by the upscale cap so it is
        // never blown up absurdly (an 80px image at most MAX_UPSCALE×).
        let (cols, _) = target_cells(80, 40, fit(200, 40), fig());
        assert!(
            cols > 10,
            "low-res figure is upscaled past native ~10 cols: {cols}"
        );
        assert!(cols <= 40, "but bounded by the upscale cap (~4×): {cols}");
    }

    #[test]
    fn uncaptioned_graphics_stay_text_proportional() {
        // Uncaptioned graphics (equation pictures) keep native size in Fit mode,
        // whatever their shape — a wide/short strip AND a tall multi-line array
        // both stay near native, never stretched or blown up to the column like a
        // captioned figure of the same pixels is.
        let (strip_cols, _) = target_cells(400, 50, fit(200, 60), eq());
        let (fig_strip_cols, _) = target_cells(400, 50, fit(200, 60), fig());
        assert!(
            fig_strip_cols > strip_cols,
            "wide strip: figure enlarges ({fig_strip_cols}), equation stays native ({strip_cols})"
        );

        // A tall array (the case the old aspect heuristic blew up as a "figure").
        let (_eq_cols, eq_rows) = target_cells(400, 300, fit(200, 60), eq());
        let (_fig_cols, fig_rows) = target_cells(400, 300, fit(200, 60), fig());
        assert!(
            fig_rows > eq_rows,
            "tall array: figure enlarges ({fig_rows} rows), equation stays native ({eq_rows})"
        );
    }

    #[test]
    fn fit_mode_overrides_small_authored_width_for_figures() {
        // In Fit mode a captioned figure's tiny authored width is ignored: it is
        // normalized to the target band, not left small (~5 cols if honored).
        let small = SizeSpec {
            hint: SizeHint::Px(40),
            math: false,
            captioned: true,
        };
        let (cols, _) = target_cells(400, 300, fit(200, 200), small);
        assert!(
            cols > 40,
            "small authored width is overridden in Fit: {cols}"
        );
    }

    #[test]
    fn low_res_equations_auto_boost_to_readable() {
        // A low-resolution single-line equation (~1 text line tall at native) is
        // auto-enlarged toward EQUATION_MIN_LINES so its glyphs are legible, even
        // at the default eq_scale (no manual tuning needed).
        let (_c, rows) = target_cells(300, 16, fit(200, 60), eq());
        assert!(rows >= 2, "tiny equation auto-boosted to >=2 rows: {rows}");
        // A taller multi-line array is already legible and stays native.
        let (_c2, tall_rows) = target_cells(300, 96, fit(200, 60), eq());
        assert_eq!(tall_rows, 6, "tall array keeps native ~6 rows: {tall_rows}");
    }

    #[test]
    fn eq_scale_knob_scales_equations() {
        // The eq_scale knob enlarges equation pictures on top of the auto size.
        let base = fit(200, 200);
        let big = FitBox {
            eq_scale: 200,
            ..base
        };
        let (_c1, r1) = target_cells(300, 60, base, eq());
        let (_c2, r2) = target_cells(300, 60, big, eq());
        assert!(r2 > r1, "200% eq_scale renders larger: {r2} vs {r1}");
    }

    #[test]
    fn math_images_stay_native_size() {
        // Real math (LaTeX/MathML pictures) keeps native size regardless of caption
        // or mode — proportional to the text, never normalized up to the column.
        let math = SizeSpec {
            hint: SizeHint::Auto,
            math: true,
            captioned: false,
        };
        let (cols, _) = target_cells(80, 40, fit(200, 40), math);
        assert!(
            cols <= 10,
            "equation at native ~10 cols, not stretched: {cols}"
        );
    }

    #[test]
    fn faithful_mode_honors_authored_width() {
        // In Faithful mode a 50% CSS width targets half the column regardless of
        // pixel resolution or caption (in Fit mode the authored width is ignored).
        let half = SizeSpec {
            hint: SizeHint::Pct(0.5),
            math: false,
            captioned: false,
        };
        let (cols, _) = target_cells(4000, 2000, faithful(100, 200), half);
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
            captioned: false,
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
