//! Graphical math: turn display equations into themed images.
//!
//! A [`Block::Math`] that kept its LaTeX source (see `delryn-format`) is rendered to
//! a PNG by [`delryn_math`] and swapped for a `Block::Image { math: true }`, so it
//! flows through the existing inline-image pipeline (indexing, row reservation,
//! async build + theme recolour, draw) with no math-specific plumbing. Anything
//! without a LaTeX source, or that fails to render, stays a `Block::Math` shown as
//! the centred Unicode approximation — the fallback is never regressed.
//!
//! Whether graphical math is on (config **and** a graphics protocol) and the target
//! em size (from the terminal cell height) live in two atomics the view keeps in
//! sync each frame ([`Reader::sync_graphical_math`]); a change re-decodes the open
//! sections so the switch is live. The conversion runs where blocks are decoded —
//! the background section loader (off the main thread) and the inline fetch — and
//! delryn-math disk-caches every render, so only the first-ever render costs anything.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use delryn_model::SpanMath;

use crate::document::Block;

use super::Reader;

/// Whether display equations are rendered graphically (config on + graphics
/// available). Read on every decoded section.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether *inline* math is rasterised too (the [`ENABLED`] master toggle **and**
/// the `graphical_inline_math` opt-in). Off by default: inline math then stays the
/// natural terminal-font Unicode approximation, which lines up with the prose far
/// better mid-sentence than a raster. Gates [`convert_inline_math`] alone.
static INLINE_ENABLED: AtomicBool = AtomicBool::new(false);

/// The em size in px to rasterise equations at (`0` = unset), so a display equation
/// shows at a text-relative type size — delryn-media shows math at native px.
static EM_PX: AtomicU32 = AtomicU32::new(0);

/// The em size in px to rasterise *inline* equations at (`0` = unset). Fixed at a
/// small multiple of the cell height (independent of the `math_scale` knob — inline
/// math tracks the surrounding text, always drawn one cell tall); oversampled so the
/// fit-to-one-cell downscale stays crisp.
static INLINE_EM_PX: AtomicU32 = AtomicU32::new(0);

/// Inline equations rasterise at a **text-relative** em (~0.9× the cell height) and
/// are shown near-native, so their base glyphs match the surrounding text and every
/// inline equation is the same size — a script-free `A⊂B` no bigger than a scripted
/// `xᵢ²` (the earlier "fit the whole bbox to a cell" made script-free equations fill
/// the cell). KaTeX cap-height ≈ 0.68 em, so ~0.9 em ≈ the text cap-height.
const INLINE_EM_FACTOR: f32 = 0.9;

/// Height gate for inline math, in render ems: an equation whose rendered raster is
/// taller than this keeps the Unicode fallback, since even two text rows can't hold it.
/// Calibrated against real RaTeX output (fractions render compactly): `\frac{a}{b}`
/// ≈ 1.2 em, a busy `\frac{…}{…}` ≈ 1.7 em, a triple-nested fraction ≈ 1.8 em all
/// render (across up to two rows, see `INLINE_MAX_ROWS`), while a 2×2 matrix (~2.5 em)
/// and taller stacks fall back to Unicode.
const INLINE_MAX_H_EM: f32 = 2.2;

/// The px-per-em to rasterise inline equations at, for a terminal `cell_h` (px).
fn inline_em_px(cell_h: u16) -> u32 {
    (f32::from(cell_h.max(1)) * INLINE_EM_FACTOR).round() as u32
}

/// Rasterise display math at the **text em** — equation glyphs match the surrounding
/// prose, so a single-line display equation sits at text height and reads as part of
/// the page rather than dominating it. `100%` `math_scale` is therefore exactly text
/// size (the floor — math is never smaller than the text); the knob scales up from
/// there. Multi-line equations are additionally height-capped in the sizer
/// (`EQ_MAX_ROWS_FRAC`) so a tall stack can't take over the page.
const DISPLAY_EM_FACTOR: f32 = 1.0;

/// The px-per-em to rasterise display equations at, for a terminal `cell_h` (px) and
/// the `scale_pct` "Math size %" knob (100 = the built-in [`DISPLAY_EM_FACTOR`]). Pure,
/// so the size math is unit-testable without a live `Reader`.
fn display_em_px(cell_h: u16, scale_pct: u16) -> u32 {
    (f32::from(cell_h.max(1)) * DISPLAY_EM_FACTOR * f32::from(scale_pct.max(1)) / 100.0).round()
        as u32
}

/// Render every display equation with a recovered LaTeX source in `blocks` to a
/// themed image (in place). A no-op when graphical math is off, and per-equation
/// best-effort: a render failure leaves that `Block::Math` untouched (Unicode
/// fallback). Runs off the main thread (the section loader) or once inline; cheap
/// after the first render thanks to delryn-math's disk cache.
pub(crate) fn convert_math_blocks(blocks: &mut [Block]) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let em_px = EM_PX.load(Ordering::Relaxed);
    if em_px == 0 {
        return;
    }
    for b in blocks.iter_mut() {
        let Block::Math {
            unicode,
            latex: Some(latex),
        } = b
        else {
            continue;
        };
        let Some(png) = delryn_math::render(latex, delryn_math::Style::Display, em_px) else {
            continue; // unrenderable → keep the Unicode `Block::Math`
        };
        let alt = std::mem::take(unicode);
        *b = Block::Image {
            src: String::new(),
            alt,
            data: png,
            caption: Vec::new(),
            math: true,
            width: delryn_model::ImageWidth::Auto,
            // Rendered LaTeX is Path A — sized by its render em, not a measured
            // ink profile — so it needs no profiling.
            ink: None,
        };
    }
}

/// Rasterise each **inline** math run in `blocks` that kept a LaTeX source
/// ([`SpanMath::Latex`]) to a small themed image drawn mid-line, assigning each a
/// section-local id and swapping it in place for [`SpanMath::Raster`]. A no-op
/// unless graphical inline math is enabled (the master toggle **and** the
/// `graphical_inline_math` opt-in); per-run best-effort — a render failure, a too-tall
/// equation (a fraction / limit stack that would smear in one text row), or an
/// undecodable raster leaves the run as `Latex`, so the wrapper shows its Unicode
/// approximation (the fallback is never regressed).
///
/// Only top-level `Para`/`Heading` runs are rendered — the same runs the wrapper
/// can reserve atom cells for; nested (callout/footnote/table/caption) inline math
/// keeps its own id space the reader doesn't address, so it stays Unicode. Runs off
/// the main thread at every decode site (after [`convert_math_blocks`]); cheap after
/// the first render thanks to delryn-math's disk cache.
pub(crate) fn convert_inline_math(blocks: &mut [Block]) {
    if !INLINE_ENABLED.load(Ordering::Relaxed) {
        return; // inline math stays the natural Unicode approximation
    }
    let em_px = INLINE_EM_PX.load(Ordering::Relaxed);
    if em_px == 0 {
        return;
    }
    let mut next_id = 0usize;
    for b in blocks.iter_mut() {
        let spans = match b {
            Block::Para { spans, .. } | Block::Heading { spans, .. } => spans,
            _ => continue,
        };
        for span in spans.iter_mut() {
            let Some(SpanMath::Latex(latex)) = &span.math else {
                continue;
            };
            let Some(png) = delryn_math::render(latex, delryn_math::Style::Inline, em_px) else {
                continue; // unrenderable → keep the Unicode fallback
            };
            // Height gate: a single-line inline equation fits one text row; a tall
            // stack (fraction, ∫/∑ with limits) would smear, so keep it Unicode.
            let too_tall = crate::media::image_dimensions(&png)
                .is_none_or(|(_, h)| h as f32 > INLINE_MAX_H_EM * em_px as f32);
            if too_tall {
                continue;
            }
            span.math = Some(SpanMath::Raster { id: next_id, png });
            next_id += 1;
        }
    }
}

/// Measure the ink profile of each **publisher** equation image in `blocks` (in
/// place), so it can be sized to a text-relative height regardless of the file's DPI
/// (see [`delryn_media::ink_profile`]). Runs right after [`convert_math_blocks`] at
/// every decode site — off the main thread in the section loader — so the profile
/// rides along on the block and both the up-front row estimate and the async build
/// read it (they must agree). A no-op when graphical rendering is off (images then
/// show as placeholders, so size is moot) or once a block is already profiled.
///
/// delryn's *own* LaTeX renders (from [`convert_math_blocks`]) carry an empty `src`
/// and are sized by their render em — they are skipped. A figure or photo that slips
/// through the candidate gate profiles to `None` and is then sized as a figure.
pub(crate) fn profile_equation_images(blocks: &mut [Block]) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    for b in blocks.iter_mut() {
        let Block::Image {
            math,
            src,
            data,
            alt,
            caption,
            width,
            ink,
        } = b
        else {
            continue;
        };
        if data.is_empty() || ink.is_some() {
            continue; // no bytes, or already measured
        }
        // delryn's own LaTeX render is flagged math with an empty src and is sized by
        // its render em — leave it native, don't ink-normalise it.
        if *math && src.is_empty() {
            continue;
        }
        // A candidate publisher equation: flagged display-math, or alt text that
        // parses as math, or an uncaptioned auto-width image (captioned / explicitly
        // sized graphics are figures). A photo that slips through profiles to `None`
        // and is then sized as a figure.
        let candidate = *math
            || delryn_model::math::is_math(alt.as_str())
            || (caption.is_empty() && matches!(*width, delryn_model::ImageWidth::Auto));
        if !candidate {
            continue;
        }
        let Some(img) = crate::media::decode(data) else {
            continue;
        };
        *ink = crate::media::ink_profile(&img).map(|p| delryn_model::InkProfile {
            x0: p.x0,
            y0: p.y0,
            x1: p.x1,
            y1: p.y1,
            line_px: p.line_px,
            line_count: p.line_count,
        });
    }
    unify_section_em(blocks);
}

/// Give every publisher equation in the section one shared text em — the median of
/// their measured ems — so they all scale to the same on-screen size. A single
/// equation whose glyph measurement misreads (e.g. one so subscript-dominated that the
/// tiny subscripts form the densest height cluster) then can't blow up out of line: the
/// median across the section's equations rejects it. All display equations in a book
/// share one font size, so unifying them is correct. Needs a few equations for the
/// median to reject an outlier; below that, each keeps its own measurement.
fn unify_section_em(blocks: &mut [Block]) {
    let mut ems: Vec<f32> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Image { ink: Some(p), .. } => Some(p.line_px),
            _ => None,
        })
        .collect();
    if ems.len() < 3 {
        return;
    }
    ems.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let reference = ems[ems.len() / 2];
    for b in blocks.iter_mut() {
        if let Block::Image { ink: Some(p), .. } = b {
            p.line_px = reference;
        }
    }
}

impl Reader {
    /// Mirror the graphical-math state from the view each frame: `on` = the config
    /// toggle AND a graphics protocol; `inline_on` = that **and** the
    /// `graphical_inline_math` opt-in (off ⇒ inline math stays Unicode); `cell_h` =
    /// the terminal cell height in px; `scale_pct` = the "Math size %" setting (100 =
    /// the built-in size). When the effective state changes (startup, toggle, size, or
    /// a cell-size change) the open sections are re-decoded so equations switch /
    /// resize live, without reopening.
    pub fn sync_graphical_math(&mut self, on: bool, inline_on: bool, cell_h: u16, scale_pct: u16) {
        let em = display_em_px(cell_h, scale_pct);
        let inline_em = inline_em_px(cell_h);
        // Update ALL atomics unconditionally, then OR the results: a `||` between the
        // `swap`s would short-circuit and skip a later write whenever an earlier value
        // had changed, stranding an em at a stale size — so e.g. a "Math size %" change
        // (which only moves the display `em`) would never reach the renderer.
        let enabled_changed = ENABLED.swap(on, Ordering::Relaxed) != on;
        let inline_enabled_changed = INLINE_ENABLED.swap(inline_on, Ordering::Relaxed) != inline_on;
        let em_changed = EM_PX.swap(em, Ordering::Relaxed) != em;
        let inline_em_changed = INLINE_EM_PX.swap(inline_em, Ordering::Relaxed) != inline_em;
        // Only reflowable docs have math to (un)convert; never drop a paged doc's
        // rasterized-page cache (it would blank the page and force a reload).
        if (enabled_changed || inline_enabled_changed || em_changed || inline_em_changed)
            && !self.is_paged_image()
        {
            self.invalidate_sections();
        }
    }

    /// Drop cached section blocks so they re-decode (and re-run the math conversion)
    /// with the current settings, and force a re-wrap.
    fn invalidate_sections(&mut self) {
        self.sections.sections.clear();
        self.sections.requested.clear();
        self.cont_cache.clear();
        self.blocks = self.fetch_blocks(self.section);
        self.wrapped.width = usize::MAX; // force a re-wrap next draw
        self.prefetch_neighbors();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rasterise em is the text em at the 100% floor (equation glyphs match the
    /// prose), scaling linearly with the "Math size %" knob. The knob only enlarges
    /// (the config floors it at 100), so display math is never smaller than the text.
    #[test]
    fn display_em_px_is_text_relative() {
        assert_eq!(display_em_px(20, 100), 20); // floor: exactly the text (cell) em
        assert_eq!(display_em_px(20, 200), 40); // knob doubles it
        assert_eq!(display_em_px(20, 300), 60); // …up to the 300% max
        // At the 100% floor a display equation is text-sized — never smaller.
        assert_eq!(display_em_px(20, 100), 20, "the floor is exactly text size");
    }

    /// With graphical math on, a display equation that kept its LaTeX source becomes
    /// a themed image; one without a source, and everything when off, stays Unicode.
    #[test]
    fn converts_display_math_with_latex_to_image() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_gmath_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // SAFETY: serialised by `_env`; scopes the math cache dir to this test.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };

        let math = || Block::Math {
            unicode: "x²".to_string(),
            latex: Some("x^2".to_string()),
        };
        let mathml_only = || Block::Math {
            unicode: "α".to_string(),
            latex: None,
        };
        EM_PX.store(40, Ordering::Relaxed);

        // Off → no conversion.
        ENABLED.store(false, Ordering::Relaxed);
        let mut off = vec![math()];
        convert_math_blocks(&mut off);
        assert!(
            matches!(off[0], Block::Math { .. }),
            "off: stays Unicode math"
        );

        // On → LaTeX math becomes a themed image; a source-less one stays Unicode.
        ENABLED.store(true, Ordering::Relaxed);
        let mut on = vec![math(), mathml_only()];
        convert_math_blocks(&mut on);
        match &on[0] {
            Block::Image {
                math, data, alt, ..
            } => {
                assert!(*math, "flagged as math");
                assert!(!data.is_empty(), "carries rendered PNG bytes");
                assert_eq!(alt, "x²", "keeps the Unicode fallback as the alt");
            }
            _ => panic!("LaTeX display math should become an image"),
        }
        assert!(
            matches!(on[1], Block::Math { .. }),
            "no LaTeX: stays Unicode"
        );

        ENABLED.store(false, Ordering::Relaxed); // don't leak to other tests
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Inline math with a LaTeX source is rasterised to a `SpanMath::Raster` when
    /// short enough for one text row; a tall stack (a fraction) keeps its Unicode
    /// fallback; and everything stays Unicode when graphical math is off.
    #[test]
    fn converts_short_inline_math_and_skips_tall_stacks() {
        let _env = crate::test_env_guard();
        let tmp = std::env::temp_dir().join(format!("delryn_imath_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // SAFETY: serialised by `_env`; scopes the math cache dir to this test.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };

        // A paragraph "see <inline math>" — only the second span is math.
        let para = |latex: &str| Block::Para {
            spans: vec![
                delryn_model::Span::plain("see "),
                delryn_model::Span::math("approx", latex),
            ],
            indent: 0,
            quote: false,
            marker: None,
        };
        let math_of = |b: &Block| match b {
            Block::Para { spans, .. } => spans[1].math.clone(),
            _ => unreachable!(),
        };
        INLINE_EM_PX.store(32, Ordering::Relaxed); // ~2× a 16px cell (the calib em)

        // Off → no conversion (stays a LaTeX source span). Inline rasterising is
        // gated on its own opt-in (`INLINE_ENABLED`), not the display master toggle.
        INLINE_ENABLED.store(false, Ordering::Relaxed);
        let mut off = vec![para("x^2")];
        convert_inline_math(&mut off);
        assert!(
            matches!(math_of(&off[0]), Some(SpanMath::Latex(_))),
            "off: inline math stays a LaTeX source"
        );

        // On → a short equation rasterises to an atom; a tall fraction stays Unicode.
        INLINE_ENABLED.store(true, Ordering::Relaxed);
        let mut short = vec![para("x^2")];
        convert_inline_math(&mut short);
        match math_of(&short[0]) {
            Some(SpanMath::Raster { id, png }) => {
                assert_eq!(id, 0, "first inline equation gets id 0");
                assert!(!png.is_empty(), "carries the rendered PNG bytes");
            }
            other => panic!("short inline math should rasterise, got {other:?}"),
        }

        // A fraction now rasterises (was Unicode) — it fits within two rows.
        let mut frac = vec![para("\\frac{a}{b}")];
        convert_inline_math(&mut frac);
        assert!(
            matches!(math_of(&frac[0]), Some(SpanMath::Raster { .. })),
            "a fraction rasterises (two-row inline math)"
        );

        // A genuinely tall equation (a 2×2 matrix, ~2.5 em) exceeds even two rows →
        // Unicode fallback (never smashed into the line).
        let mut tall = vec![para("\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}")];
        convert_inline_math(&mut tall);
        assert!(
            matches!(math_of(&tall[0]), Some(SpanMath::Latex(_))),
            "a too-tall stack keeps its Unicode fallback"
        );

        INLINE_ENABLED.store(false, Ordering::Relaxed); // don't leak to other tests
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A transparent PNG with one opaque ink band — a minimal publisher equation.
    fn equation_png() -> Vec<u8> {
        let mut img = image::RgbaImage::from_pixel(120, 60, image::Rgba([0, 0, 0, 0]));
        for y in 24..36 {
            for x in 20..100 {
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    /// The profiling pass measures publisher equation images — an uncaptioned raster
    /// or a `math`-flagged `<img>` (real `src`) — while leaving captioned figures and
    /// delryn's own LaTeX renders (`math`, empty `src`) untouched.
    #[test]
    fn profiles_publisher_equation_images_only() {
        let _env = crate::test_env_guard();
        let img = |src: &str, caption: Vec<delryn_model::Span>, math: bool| Block::Image {
            src: src.to_string(),
            alt: String::new(),
            data: equation_png(),
            caption,
            math,
            width: delryn_model::ImageWidth::Auto,
            ink: None,
        };
        let mut blocks = vec![
            img("", Vec::new(), false),      // uncaptioned equation image
            img("eq.png", Vec::new(), true), // publisher math image (has src)
            img("", vec![delryn_model::Span::plain("Fig 1")], false), // captioned figure
            img("", Vec::new(), true),       // delryn's own render (empty src)
        ];

        // Off → nothing profiled.
        ENABLED.store(false, Ordering::Relaxed);
        profile_equation_images(&mut blocks);
        assert!(
            matches!(&blocks[0], Block::Image { ink: None, .. }),
            "off: not profiled"
        );

        // On → both publisher equations are measured; the figure and delryn's own
        // render are left alone.
        ENABLED.store(true, Ordering::Relaxed);
        profile_equation_images(&mut blocks);
        let ink = |b: &Block| matches!(b, Block::Image { ink: Some(_), .. });
        assert!(ink(&blocks[0]), "uncaptioned equation image is profiled");
        assert!(
            ink(&blocks[1]),
            "publisher math image (real src) is profiled"
        );
        assert!(!ink(&blocks[2]), "captioned figure left unprofiled");
        assert!(
            !ink(&blocks[3]),
            "delryn's own render (empty src) left unprofiled"
        );

        ENABLED.store(false, Ordering::Relaxed); // don't leak to other tests
    }

    /// The section pass levels a mis-measured outlier to the shared (median) em, so one
    /// subscript-dominated equation can't render out of scale with its neighbours.
    #[test]
    fn unify_section_em_levels_an_outlier() {
        let eq = |line_px: f32| Block::Image {
            src: "e.png".into(),
            alt: String::new(),
            data: vec![1],
            caption: Vec::new(),
            math: true,
            width: delryn_model::ImageWidth::Auto,
            ink: Some(delryn_model::InkProfile {
                x0: 0,
                y0: 0,
                x1: 100,
                y1: 20,
                line_px,
                line_count: 1,
            }),
        };
        // Three well-measured equations (~20px em) and one that misread far too small.
        let mut blocks = vec![eq(20.0), eq(21.0), eq(19.0), eq(6.0)];
        unify_section_em(&mut blocks);
        for b in &blocks {
            let Block::Image { ink: Some(p), .. } = b else {
                unreachable!()
            };
            assert_eq!(
                p.line_px, 20.0,
                "all share the median em; the outlier is levelled"
            );
        }
    }
}
