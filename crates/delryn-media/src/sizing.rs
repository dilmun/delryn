//! Image display sizing: fitting figures and pages into the terminal cell grid.

use delryn_infra::config::ImageFit;

use crate::profile::InkProfile;

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
    /// A font-relative width in CSS `em`. The publisher's *text-relative* size — the
    /// reliable, DPI-independent hint for an equation raster: the source pixels are
    /// ignored and the raster is scaled so one authored em is a fixed number of text
    /// cells ([`MATH_EM_CELLS`]), so every equation flows at the prose size.
    Em(f32),
    /// Fill the pane (preserving aspect), bounded only by the cols×rows box —
    /// for page-as-image content (PDF), not inline figures.
    Full,
}

/// Per-image sizing intent passed to [`target_cells`] / the build worker.
#[derive(Clone, Copy, PartialEq)]
pub struct SizeSpec {
    /// The authored display width, if any.
    pub hint: SizeHint,
    /// Real math re-rendered from LaTeX/MathML: sized by its render em at render
    /// time, shown native (only shrunk to fit the column).
    pub math: bool,
    /// Whether the image carries a caption — a figure/table signal (figures and
    /// tables are captioned; equations are not). Consulted by the classifier.
    pub captioned: bool,
    /// The image's alt text parses as math (`$…$`, `\(`, MathML, …). The strongest
    /// positive "this raster is an equation" signal, independent of the pixels.
    pub alt_math: bool,
    /// The measured equation ink profile, when this is a publisher equation raster
    /// (filled once off-thread; see [`crate::ink_profile`]). Drives DPI-independent,
    /// text-relative equation sizing; `None` for figures, photos, and rendered math.
    pub ink: Option<InkProfile>,
    /// A small inline equation drawn mid-line (delryn's own LaTeX render at ~text
    /// size): shown near-native in a single text row (only shrunk to fit one cell
    /// tall / the column). Takes precedence over `math`; `ink` is not consulted.
    pub inline: bool,
}

impl Default for SizeSpec {
    fn default() -> SizeSpec {
        SizeSpec {
            hint: SizeHint::Auto,
            math: false,
            captioned: false,
            alt_math: false,
            ink: None,
            inline: false,
        }
    }
}

/// Upper bound on how far a low-resolution figure may be enlarged to reach its
/// target display width. Bounds the softness from upscaling (paired with a
/// quality resampling filter in the build step) while still letting a low-res
/// figure grow enough to be readable and consistent with its neighbours.
const MAX_UPSCALE: f64 = 4.0;

/// Target displayed height, in text cells, of one equation's **text em** — the
/// text-relative size a publisher equation raster is normalised to (per the measured
/// glyph em [`InkProfile::line_px`]), independent of the file's DPI. Every equation on
/// a page is scaled so its body glyphs hit this size, so they look consistent; tall
/// operators (Σ, fractions) then extend above/below proportionally. The `math_scale`
/// knob tunes on top (100% = this value). Adjust here if the default reads large/small.
/// Set a touch above text (1.15×) so an equation reads as math — flowing with the prose but
/// a hair larger — per the "natural, a little bigger like 1.1–1.2×" target. Must equal
/// [`MATH_EM_CELLS`] so the ink-measured and authored-em paths render the same size.
const EQ_TARGET_LINE_CELLS: f64 = 1.15;

/// On-screen height, in text cells, of one authored CSS `em` for an equation raster
/// sized by its publisher [`SizeHint::Em`] width. Set to the **text** em (matching the
/// reader's `DISPLAY_EM_FACTOR`) so a publisher raster and a LaTeX equation delryn
/// re-renders come out the same size — and the same size as the surrounding prose. The
/// `math_scale` knob scales up from there. The *reliable* path: source pixels ignored,
/// so a raster never renders out of scale regardless of its DPI. Kept equal to
/// [`EQ_TARGET_LINE_CELLS`] (1.15×) so every equation — authored-em or ink-measured — flows
/// at the same natural, slightly-larger-than-text size across the whole book.
const MATH_EM_CELLS: f64 = 1.15;

/// A display equation is capped to this fraction of the available rows, so a tall /
/// multi-line one shrinks uniformly instead of dominating the page (a wide multi-line
/// equation otherwise fills the column width and, by its aspect, runs many rows tall).
/// Single-line equations sit far under the cap and are untouched. Floored by
/// [`EQ_MAX_ROWS_FLOOR`] so a short pane still shows a readable equation.
const EQ_MAX_ROWS_FRAC: f64 = 0.28;

/// Absolute floor (in text rows) for the [`EQ_MAX_ROWS_FRAC`] cap, so a small viewport
/// doesn't shrink a multi-line equation to an illegible sliver.
const EQ_MAX_ROWS_FLOOR: f64 = 6.0;

/// Ink-line count at or below which a monochrome line-art raster is taken to be an
/// equation rather than a diagram (a genuine figure with many text rows reads as a
/// figure). Only used by the classifier when there's no stronger signal. Set generously so
/// a **multi-line equation** (a stacked matrix / cases / big column vector) still sizes on
/// the text-relative, book-unified equation path instead of falling to the figure path
/// (a fraction of the column), which is what made equation sizes jump between sections.
const ARRAY_MAX_LINES: u16 = 16;

/// The most text rows an **inline** equation may occupy. A script-only equation is one
/// row; a fraction / limit-stack (allowed through the reader's height gate) spans two,
/// hanging into a blank spacer row the wrapper inserts below its line. Anything taller
/// stays the Unicode fallback.
const INLINE_MAX_ROWS: u16 = 2;

/// Natural height (in text cells) at or below which an inline equation stays **one**
/// row — a script-only equation (`xᵢ²`, `√2`) or a compact stack that shrinks cleanly
/// into a single cell. Above it (a real two-line fraction) the equation keeps native
/// size across two rows instead of being squashed. Calibrated against RaTeX's compact
/// inline fractions (`\frac{1}{2}` ≈ 1.2 cells, a busy fraction ≈ 1.5 cells).
const INLINE_ONE_ROW_MAX: f64 = 1.35;

/// Fallback (unprofiled raster) only: the height in text lines a low-resolution
/// equation is boosted *up* toward. Used when no [`InkProfile`] is available, so the
/// DPI-independent path can't run; keeps the old boost-only behaviour as a floor.
const EQUATION_MIN_LINES: f64 = 2.0;

/// Fallback upper bound on the unprofiled low-resolution boost (quality guard).
const EQUATION_AUTO_MAX: f64 = 2.5;

/// The cell geometry and caps an image must fit into: terminal cell size
/// (`fw`×`fh` px), the available `cols`×`rows` box, the longest-side pixel cap
/// (`max_px`, 0 = none), the default/normalized figure width (`target_pct`% of
/// the column), the equation-picture size knob (`math_scale`%), and the sizing
/// policy (`fit_mode`: normalize vs. faithful).
#[derive(Clone, Copy)]
pub struct FitBox {
    pub fw: u16,
    pub fh: u16,
    pub cols: u16,
    pub rows: u16,
    pub max_px: u16,
    pub target_pct: u16,
    pub math_scale: u16,
    pub fit_mode: ImageFit,
}

/// How a graphic is sized, decided by [`classify`] from the sizing signals.
#[derive(Clone, Copy, PartialEq, Debug)]
enum GraphicKind {
    /// A full-bleed page image (PDF): fills the pane.
    Page,
    /// A small inline equation drawn mid-line: shown at its native render-em size
    /// (so its glyphs match the surrounding text) in a single row — only shrunk if
    /// it would overflow one cell tall or the column.
    InlineMath,
    /// Real math re-rendered from LaTeX: sized by its render em; shown native.
    RenderedMath,
    /// A publisher equation shipped as a raster: normalised to a text-relative size.
    EquationRaster,
    /// A figure / table / photo / diagram: normalised to the column band.
    Figure,
}

/// Classify a graphic from its sizing signals — the one place equations are told
/// apart from figures. `spec.math` marks *any* display-math image: delryn's own
/// LaTeX render (no ink profile — sized by its render em, shown native) versus a
/// **publisher** equation raster (carries an [`InkProfile`] — normalised to a
/// text-relative size like every other raster, so the size knob and DPI-independence
/// apply). In [`ImageFit::Faithful`] every remaining graphic is a figure (the
/// authored width is honoured). In `Fit` (the default) the signals, strongest first:
/// alt text that parses as math ⇒ equation; a caption ⇒ figure; else the measured
/// ink — sparse line-art with few lines ⇒ equation, anything denser or unprofiled ⇒
/// figure.
fn classify(spec: SizeSpec, fit_mode: ImageFit) -> GraphicKind {
    if matches!(spec.hint, SizeHint::Full) {
        return GraphicKind::Page;
    }
    if spec.inline {
        return GraphicKind::InlineMath;
    }
    if spec.math {
        // A publisher equation raster (profiled ink, or an authored `em` width — either
        // gives a text-relative size) is normalised; an unprofiled one with no size hint
        // is delryn's own crisp LaTeX render (native, sized by its render em).
        return match spec.ink {
            Some(_) => GraphicKind::EquationRaster,
            None if matches!(spec.hint, SizeHint::Em(_)) => GraphicKind::EquationRaster,
            None => GraphicKind::RenderedMath,
        };
    }
    if fit_mode != ImageFit::Fit {
        return GraphicKind::Figure;
    }
    if spec.alt_math {
        return GraphicKind::EquationRaster;
    }
    if spec.captioned {
        return GraphicKind::Figure;
    }
    match spec.ink {
        Some(p) if p.line_count <= ARRAY_MAX_LINES => GraphicKind::EquationRaster,
        _ => GraphicKind::Figure,
    }
}

/// Cell size (cols, rows) for a `w`×`h` px image.
///
/// One text-relative model, four kinds (see [`classify`]):
/// - **Page** (`SizeHint::Full`): fills the `cols`×`rows` pane, preserving aspect.
/// - **Rendered math** (`spec.math`): already sized by its render em — shown native,
///   only shrunk to fit the column.
/// - **Equation raster**: normalised so one ink-line is [`EQ_TARGET_LINE_CELLS`]
///   text cells tall (from the measured [`InkProfile`]) — *bidirectional*, so a
///   high-DPI raster shrinks and a low-DPI one grows (enlargement quality-capped),
///   the same text-relative size in every book. An unprofiled one falls back to the
///   legacy low-res boost. A multi-line array scales proportionally; all are bounded
///   to fit the column.
/// - **Figure**: normalised to `target_pct`% of the column in `Fit`, or the authored
///   width in `Faithful` — enlarged up to [`MAX_UPSCALE`], never past the box.
///
/// Equations are measured on their **ink** (whitespace margins cropped away, via
/// [`InkProfile::bbox_dims`]) so the file's padding never inflates the size, and the
/// build crops to the same bbox so displayed pixels match. The longest displayed side
/// is finally capped to `fit.max_px`. Used by both the up-front row estimate and the
/// background build, so the two always agree (no gap).
pub fn target_cells(w: u32, h: u32, fit: FitBox, spec: SizeSpec) -> (u16, u16) {
    if w == 0 || h == 0 || fit.fw == 0 || fit.fh == 0 {
        return (1, 1);
    }
    let (fwf, fhf) = (f64::from(fit.fw), f64::from(fit.fh));
    // Effective size: an equation is sized on its ink bbox (margins cropped away);
    // every other graphic on its full pixels.
    let (bw, bh) = match spec.ink {
        Some(p) => p.bbox_dims(),
        None => (w as f64, h as f64),
    };
    // The most the aspect-preserving image can scale before it overflows the column
    // width or the viewport height.
    let cap = (f64::from(fit.cols) * fwf / bw).min(f64::from(fit.rows) * fhf / bh);
    let knob = f64::from(fit.math_scale) / 100.0;

    let kind = classify(spec, fit.fit_mode);
    let mut scale = match kind {
        GraphicKind::Page => cap,
        // Sized by its render em, shown **native** (like RenderedMath) so the base
        // glyphs match the surrounding text and every inline equation is the same
        // Native (never upscaled), so base glyphs match the surrounding text: a
        // short equation shrinks into one cell; a two-line fraction keeps native size
        // across INLINE_MAX_ROWS rows. Only shrunk to fit the column width or that row
        // budget. (Fitting the whole bbox to a cell would make script-free equations
        // fill the cell, bigger than scripted ones.)
        GraphicKind::InlineMath => {
            let budget = if bh / fhf <= INLINE_ONE_ROW_MAX {
                1.0
            } else {
                f64::from(INLINE_MAX_ROWS)
            };
            (f64::from(fit.cols) * fwf / bw)
                .min(budget * fhf / bh)
                .min(1.0)
        }
        GraphicKind::RenderedMath => cap.min(1.0),
        GraphicKind::EquationRaster => {
            // The general pixel algorithm first — no publisher metadata: measure the raster's
            // own ink and bring one glyph line to the text-relative target, scaling a too-big
            // raster *down* and a too-small one *up*. Every equation — a publisher raster or
            // delryn's own typeset render — is measured the same way, so both land at the
            // same size and a tall matrix is normalised per-line instead of rendering at its
            // native, oversized layout. DPI drops out (the measurement is on the pixels).
            let s = match spec.ink {
                Some(p) => fhf * EQ_TARGET_LINE_CELLS * knob / f64::from(p.line_px),
                // No measurable ink (rare): fall back to the authored em width if the
                // publisher gave one, else the legacy low-res boost (never shrinks).
                None => match spec.hint {
                    SizeHint::Em(em_w) => {
                        f64::from(em_w) * MATH_EM_CELLS * fhf * knob / f64::from(w)
                    }
                    _ => (EQUATION_MIN_LINES * fhf / bh).clamp(1.0, EQUATION_AUTO_MAX) * knob,
                },
            };
            let s = s.min(cap); // fit the column / viewport
            if s > 1.0 { s.min(MAX_UPSCALE) } else { s } // quality-cap enlargement only
        }
        GraphicKind::Figure => {
            // The display width the figure should occupy: a consistent fraction of
            // the column in Fit (authored width ignored — as unreliable as the
            // resolution); the authored width in Faithful, else the same default.
            let want_px = if fit.fit_mode == ImageFit::Fit {
                f64::from(fit.cols) * fwf * f64::from(fit.target_pct) / 100.0
            } else {
                match spec.hint {
                    SizeHint::Pct(p) => f64::from(fit.cols) * fwf * f64::from(p).clamp(0.0, 1.0),
                    SizeHint::Px(px) => f64::from(px),
                    // A font-relative width on a non-math figure (rare): 1 em ≈ the cell
                    // height, so the figure occupies that many ems of column width.
                    SizeHint::Em(em) => f64::from(em) * fhf,
                    SizeHint::Auto => f64::from(fit.cols) * fwf * f64::from(fit.target_pct) / 100.0,
                    SizeHint::Full => unreachable!("full-bleed is GraphicKind::Page"),
                }
            };
            (want_px / bw).min(cap).min(MAX_UPSCALE)
        }
    };
    if scale <= 0.0 {
        scale = cap.min(1.0);
    }

    // Height-cap a display equation so a tall / multi-line one can't dominate the page:
    // shrink the whole equation uniformly to fit a fraction of the available rows. A
    // single-line equation sits far under the cap and is untouched; inline math (one
    // row), figures, and pages are exempt.
    if matches!(
        kind,
        GraphicKind::EquationRaster | GraphicKind::RenderedMath
    ) {
        let max_h_px = (f64::from(fit.rows) * EQ_MAX_ROWS_FRAC).max(EQ_MAX_ROWS_FLOOR) * fhf;
        if bh * scale > max_h_px {
            scale = max_h_px / bh;
        }
    }

    // A full-bleed page is bounded by the pane itself; the per-figure pixel cap
    // (which bounds inline-figure transfers) would only letterbox it, so skip it.
    let longest = (bw * scale).max(bh * scale);
    if fit.max_px > 0 && longest > f64::from(fit.max_px) && !matches!(spec.hint, SizeHint::Full) {
        scale *= f64::from(fit.max_px) / longest;
    }
    let cols = ((bw * scale / fwf).ceil() as u16).clamp(1, fit.cols.max(1));
    // Inline math reserves one row for a short equation, two for a taller fraction —
    // the same `INLINE_ONE_ROW_MAX` split as the scale above, so the reserved rows and
    // the drawn raster always agree. Every other kind ceils to its fitted height.
    let rows = if kind == GraphicKind::InlineMath {
        if bh / fhf <= INLINE_ONE_ROW_MAX {
            1
        } else {
            INLINE_MAX_ROWS
        }
    } else {
        ((bh * scale / fhf).ceil() as u16).clamp(1, fit.rows.max(1))
    };
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
            math_scale: 100,
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
            captioned: true,
            ..SizeSpec::default()
        }
    }

    /// A profiled equation raster whose ink fills a `w`×`h` bbox as `line_count`
    /// lines of `line_px` each — the signal a real publisher equation carries.
    fn eq(w: u32, h: u32, line_px: f32, line_count: u16) -> SizeSpec {
        SizeSpec {
            ink: Some(InkProfile {
                x0: 0,
                y0: 0,
                x1: w,
                y1: h,
                line_px,
                line_count,
            }),
            ..SizeSpec::default()
        }
    }

    /// The headline fix: a single-line equation lands at the *same* text-relative
    /// height regardless of the file's DPI — a 4× range of ink-line heights all
    /// render to the same rows (the boost-only code left a hi-DPI raster huge).
    #[test]
    fn equation_size_is_dpi_independent() {
        let rows: Vec<u16> = [8.0f32, 16.0, 32.0, 64.0]
            .into_iter()
            .map(|line_px| {
                let h = line_px as u32;
                target_cells(100, h, fit(400, 400), eq(100, h, line_px, 1)).1
            })
            .collect();
        assert!(
            rows.iter().all(|&r| r == rows[0]),
            "same equation at 4 DPIs → same height, got {rows:?}"
        );
        // …and far below the native 4 rows a 64px raster would occupy at 1:1.
        assert!(
            rows[0] < 64 / 16,
            "hi-DPI raster shrinks below its native height: {}",
            rows[0]
        );
    }

    /// The #17 fix: an equation shipped as a raster with an authored `em` width is
    /// sized from that width, so two rasters of wildly different pixel resolution but
    /// the *same* em width render to the SAME cells — the DPI drops out entirely. (The
    /// pixel-based ink measurement was unreliable at low DPI; the em width is exact.)
    #[test]
    fn em_width_equation_is_dpi_independent() {
        let em = |w: u32, h: u32| {
            let spec = SizeSpec {
                math: true,
                hint: SizeHint::Em(8.0),
                ..SizeSpec::default()
            };
            target_cells(w, h, fit(400, 400), spec)
        };
        // The same 8em-wide, 2:1 equation at 4× DPI: 200×100 vs 800×400 px.
        let (lo, hi) = (em(200, 100), em(800, 400));
        assert_eq!(
            lo, hi,
            "same em width → same cells at any DPI: {lo:?} vs {hi:?}"
        );
        // …and it lands text-relative (8em × 1.0 cells/em × fh/fw ≈ ~16 cols), not
        // blown up by the raw pixels.
        assert!(
            (12..=20).contains(&lo.0),
            "8em ≈ ~16 cols wide (text-relative), got {}",
            lo.0
        );
    }

    /// The `math_scale` knob still scales an em-sized equation (it enlarges from the
    /// text-relative floor, like every other equation).
    #[test]
    fn em_width_equation_respects_math_scale() {
        let spec = SizeSpec {
            math: true,
            hint: SizeHint::Em(8.0),
            ..SizeSpec::default()
        };
        let base = target_cells(300, 120, fit(400, 400), spec);
        let big = target_cells(
            300,
            120,
            FitBox {
                math_scale: 200,
                ..fit(400, 400)
            },
            spec,
        );
        assert!(big.0 > base.0, "200% enlarges: {big:?} vs {base:?}");
    }

    /// A tall / multi-line display equation is height-capped so it can't dominate the
    /// page — shrunk to a fraction of the available rows — while a single-line one is
    /// far under the cap and untouched.
    #[test]
    fn tall_equation_is_height_capped() {
        // A ~30-row pane. A big, near-square multi-line equation (wide → fills the
        // column, tall by aspect) must be capped well under its uncapped height.
        let pane = fit(45, 30);
        let big = SizeSpec {
            math: true,
            hint: SizeHint::Em(30.0),
            ..SizeSpec::default()
        };
        let (_c, rows) = target_cells(1270, 850, pane, big);
        let cap = (30.0 * EQ_MAX_ROWS_FRAC).max(EQ_MAX_ROWS_FLOOR).ceil() as u16;
        assert!(
            rows <= cap,
            "multi-line equation capped to ~{cap} rows, got {rows}"
        );
        // A single-line equation (short, wide) is nowhere near the cap.
        let line = SizeSpec {
            math: true,
            hint: SizeHint::Em(20.0),
            ..SizeSpec::default()
        };
        let (_c, r1) = target_cells(800, 90, pane, line);
        assert!(r1 < cap, "single-line equation untouched by the cap: {r1}");
    }

    /// A multi-line array normalises *per line*, so it stays tall (proportional),
    /// never squashed to a single line like a naive height-normalisation would.
    #[test]
    fn multiline_array_scales_proportionally() {
        let single = target_cells(100, 16, fit(400, 400), eq(100, 16, 16.0, 1)).1;
        let triple = target_cells(100, 48, fit(400, 400), eq(100, 48, 16.0, 3)).1;
        assert!(
            triple >= 2 * single,
            "three lines stay ~3× tall (proportional): {triple} vs {single}"
        );
    }

    /// A wide equation is shrunk uniformly to fit the column — its cells never
    /// exceed the available width, in single- or two-page layout.
    #[test]
    fn wide_equation_fits_the_column() {
        for avail in [20u16, 48, 96] {
            let (cols, _) = target_cells(4000, 16, fit(avail, 40), eq(4000, 16, 16.0, 1));
            assert!(cols <= avail, "avail={avail}: cols={cols} must fit");
        }
    }

    /// The `math_scale` knob scales profiled equations on top of the target.
    #[test]
    fn math_scale_knob_scales_equations() {
        let base = fit(400, 400);
        let big = FitBox {
            math_scale: 200,
            ..base
        };
        let r1 = target_cells(100, 32, base, eq(100, 32, 32.0, 1)).1;
        let r2 = target_cells(100, 32, big, eq(100, 32, 32.0, 1)).1;
        assert!(r2 > r1, "200% math_scale renders larger: {r2} vs {r1}");
    }

    /// An equation is sized on its **ink** bbox, so the file's whitespace margins
    /// never inflate it (the build crops to the same bbox).
    #[test]
    fn equation_sized_on_ink_not_margins() {
        // A 400×400 image whose ink is only a 100×16 strip in the corner sizes as a
        // single small line, not as a big 400px-tall graphic.
        let mut spec = eq(100, 16, 16.0, 1);
        if let Some(p) = spec.ink.as_mut() {
            (p.x1, p.y1) = (100, 16); // ink bbox ≪ the 400×400 image
        }
        let (_c, rows) = target_cells(400, 400, fit(400, 400), spec);
        assert!(
            rows <= 3,
            "sized on the 16px ink line, not the 400px canvas: {rows}"
        );
    }

    /// An unprofiled equation (`ink == None` but flagged by its alt text) falls back
    /// to the legacy low-resolution boost, so nothing regresses when profiling is
    /// unavailable.
    #[test]
    fn unprofiled_equation_falls_back_to_boost() {
        let spec = SizeSpec {
            alt_math: true,
            ..SizeSpec::default()
        };
        let (_c, rows) = target_cells(300, 16, fit(200, 60), spec);
        assert!(rows >= 2, "low-res unprofiled equation boosted: {rows}");
    }

    /// The classifier maps every signal combination to the right kind.
    #[test]
    fn classify_covers_every_kind() {
        let page = SizeSpec {
            hint: SizeHint::Full,
            ..SizeSpec::default()
        };
        let math = SizeSpec {
            math: true,
            ..SizeSpec::default()
        };
        let alt = SizeSpec {
            alt_math: true,
            ..SizeSpec::default()
        };
        // Math flag, no ink = delryn's own LaTeX render (native, sized by its em).
        // Math flag WITH ink = a publisher equation raster (normalise it + knob).
        let pub_eq = SizeSpec {
            math: true,
            ..eq(100, 16, 16.0, 1)
        };
        assert_eq!(classify(page, ImageFit::Fit), GraphicKind::Page);
        assert_eq!(classify(math, ImageFit::Fit), GraphicKind::RenderedMath);
        assert_eq!(classify(pub_eq, ImageFit::Fit), GraphicKind::EquationRaster);
        assert_eq!(classify(alt, ImageFit::Fit), GraphicKind::EquationRaster);
        assert_eq!(classify(fig(), ImageFit::Fit), GraphicKind::Figure);
        // Ink line-art with few lines is an equation; many lines reads as a figure.
        assert_eq!(
            classify(eq(100, 16, 16.0, 1), ImageFit::Fit),
            GraphicKind::EquationRaster
        );
        assert_eq!(
            classify(eq(100, 400, 16.0, 20), ImageFit::Fit),
            GraphicKind::Figure
        );
        // Faithful mode never treats a graphic as an equation (authored width wins).
        assert_eq!(
            classify(eq(100, 16, 16.0, 1), ImageFit::Faithful),
            GraphicKind::Figure
        );
        // Unprofiled + uncaptioned + no math signal ⇒ a figure, not an equation.
        assert_eq!(
            classify(SizeSpec::default(), ImageFit::Fit),
            GraphicKind::Figure
        );
    }

    /// An uncaptioned graphic with no ink profile (a photo the profiler declined) is
    /// sized as a figure — identical to a captioned figure of the same pixels.
    #[test]
    fn unprofiled_uncaptioned_graphic_is_a_figure() {
        let a = target_cells(400, 300, fit(200, 200), SizeSpec::default());
        let b = target_cells(400, 300, fit(200, 200), fig());
        assert_eq!(a, b, "unprofiled uncaptioned graphic sizes like a figure");
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
    fn fit_mode_overrides_small_authored_width_for_figures() {
        // In Fit mode a captioned figure's tiny authored width is ignored: it is
        // normalized to the target band, not left small (~5 cols if honored).
        let small = SizeSpec {
            hint: SizeHint::Px(40),
            captioned: true,
            ..SizeSpec::default()
        };
        let (cols, _) = target_cells(400, 300, fit(200, 200), small);
        assert!(
            cols > 40,
            "small authored width is overridden in Fit: {cols}"
        );
    }

    #[test]
    fn math_images_stay_native_size() {
        // Real math (LaTeX/MathML pictures) keeps native size regardless of caption
        // or mode — proportional to the text, never normalized up to the column.
        let math = SizeSpec {
            math: true,
            ..SizeSpec::default()
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
            ..SizeSpec::default()
        };
        let (cols, _) = target_cells(4000, 2000, faithful(100, 200), half);
        assert!(
            (i32::from(cols) - 50).abs() <= 2,
            "≈50% of 100 cols: {cols}"
        );
    }

    /// Inline math is shown **native** (never upscaled) and occupies one or two text
    /// rows: a script-only equation stays one row; a fraction spans two (hanging into a
    /// spacer row); anything taller shrinks to fit two rows.
    #[test]
    fn inline_math_is_one_or_two_rows() {
        let inline = SizeSpec {
            inline: true,
            math: true,
            ..SizeSpec::default()
        };
        // 8×16px cells. A one-cell-tall (script-only) equation → one row, native width.
        let (cols, rows) = target_cells(64, 16, fit(400, 40), inline);
        assert_eq!(
            (cols, rows),
            (8, 1),
            "one-cell equation: one row, native width"
        );
        // A two-cell-tall fraction → two rows, still native (never upscaled).
        let (cols, rows) = target_cells(64, 32, fit(400, 40), inline);
        assert_eq!(
            (cols, rows),
            (8, 2),
            "two-cell fraction: two rows, native width"
        );
        // Never more than two rows: a very tall raster shrinks to fit INLINE_MAX_ROWS.
        let (_cols, rows) = target_cells(64, 64, fit(400, 40), inline);
        assert_eq!(rows, 2, "capped at two rows (shrunk to fit)");
        // A short, wide equation is drawn at NATIVE width — ceil(px/cell), not enlarged
        // to fill the cell height (that enlargement made script-free equations look big).
        let (cols, rows) = target_cells(40, 10, fit(400, 40), inline);
        assert_eq!(
            (cols, rows),
            (5, 1),
            "40px wide at native = ceil(40/8) = 5 cols"
        );
    }

    /// Inline sizing takes precedence over the `math`/`ink` signals: a spec flagged
    /// both inline and (publisher) equation still classifies as one-row inline math.
    #[test]
    fn inline_precedes_equation_raster() {
        let spec = SizeSpec {
            inline: true,
            ..eq(100, 32, 32.0, 1)
        };
        assert_eq!(classify(spec, ImageFit::Fit), GraphicKind::InlineMath);
    }

    #[test]
    fn full_bleed_page_fills_the_pane() {
        // A full-bleed page (PDF) fills the pane, unlike a figure: it is bounded
        // only by the cols×rows box, ignoring the upscale cap and the px cap.
        let page = SizeSpec {
            hint: SizeHint::Full,
            ..SizeSpec::default()
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
