//! Graphical math: render the recovered [`delryn_model::MathItem`] to themed images.
//!
//! A [`Block::Math`] is rendered down the [`delryn_eqn`] ladder — its crisp typeset raster,
//! else the publisher's own picture — and swapped for a `Block::Image { math: true }`, so it
//! flows through the existing inline-image pipeline (indexing, row reservation, async build +
//! theme recolour, draw) with no math-specific plumbing. Anything the ladder can't render
//! graphically stays a `Block::Math` shown as the centred Unicode floor — never regressed.
//!
//! Whether graphical math is on (config **and** a graphics protocol) and the target em size
//! (from the terminal cell height) live in two atomics the view keeps in sync each frame
//! ([`Reader::sync_graphical_math`]); a change re-decodes the open sections so the switch is
//! live. The conversion runs where blocks are decoded — the background section loader (off
//! the main thread) and the inline fetch.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use delryn_model::SpanMath;

use crate::document::Block;

use super::Reader;

/// Every publisher-equation em measured so far in the *current book*, keyed by section
/// so re-decoding a section replaces (never duplicates) its contribution. Pooled
/// **book-wide** because a whole section can be uniformly mis-measured — a page of
/// `1/n Σ` error metrics, every one undershot, has no well-measured neighbour to level
/// against — so the rest of the book's correctly-measured equations set the shared size
/// instead (see [`unify_book_em`]). Reset when a new book opens ([`reset_book_ems`]);
/// its rasters carry their own resolution and must not be sized by the previous book.
static BOOK_EMS: Mutex<BTreeMap<usize, Vec<f32>>> = Mutex::new(BTreeMap::new());

/// The book-wide reference em in effect (×100; `0` = unset), and a generation that ticks
/// only when it *shifts meaningfully*. Early on — before the pool has enough correctly-
/// measured equations — the reference can move as sections load; a section baked against
/// the old value is then out of scale. The generation lets the reader cheaply notice a
/// shift and re-decode its *off-screen* cached sections at the new size (see
/// [`Reader::sync_graphical_math`]) so they're already right when scrolled to. For normal
/// reading the reference is stable from the first page, so this never ticks.
static BOOK_REF: AtomicU32 = AtomicU32::new(0);
static BOOK_REF_GEN: AtomicU32 = AtomicU32::new(0);
/// The generation the reader has already reacted to, so a shift drops off-screen sections
/// exactly once (not every frame).
static BOOK_REF_SEEN: AtomicU32 = AtomicU32::new(0);
/// A meaningful reference shift: below this the generation doesn't tick, so tiny
/// measurement jitter never churns the section cache.
const BOOK_REF_SHIFT: f32 = 0.06;

/// Forget the accumulated equation ems, so one book's measurements never size another's.
/// Called when a book is opened; also re-syncs the reaction generation so a fresh book
/// doesn't spuriously drop its cache on the first frame.
pub(crate) fn reset_book_ems() {
    if let Ok(mut pool) = BOOK_EMS.lock() {
        pool.clear();
    }
    BOOK_REF.store(0, Ordering::Relaxed);
    BOOK_REF_SEEN.store(BOOK_REF_GEN.load(Ordering::Relaxed), Ordering::Relaxed);
}

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
/// size (the default); the knob scales up to 3× or down below text size, so a reader who
/// finds equations too large can tune them down. Multi-line equations are additionally
/// height-capped in the sizer (`EQ_MAX_ROWS_FRAC`) so a tall stack can't take over the page.
const DISPLAY_EM_FACTOR: f32 = 1.0;

/// The px-per-em to rasterise display equations at, for a terminal `cell_h` (px) and
/// the `scale_pct` "Math size %" knob (100 = the built-in [`DISPLAY_EM_FACTOR`]). Pure,
/// so the size math is unit-testable without a live `Reader`.
fn display_em_px(cell_h: u16, scale_pct: u16) -> u32 {
    (f32::from(cell_h.max(1)) * DISPLAY_EM_FACTOR * f32::from(scale_pct.max(1)) / 100.0).round()
        as u32
}

/// Render every display equation in `blocks` down the engine ladder to a themed image (in
/// place). A no-op when graphical math is off, and per-equation best-effort: a rung that
/// can't render leaves that `Block::Math` untouched (Unicode floor). Runs off the main
/// thread (the section loader) or once inline.
pub(crate) fn convert_math_blocks(blocks: &mut [Block]) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let em_px = EM_PX.load(Ordering::Relaxed);
    if em_px == 0 {
        return;
    }
    for b in blocks.iter_mut() {
        let Block::Math { item } = b else {
            continue;
        };
        // A `Block::Math` reaching here is typeset-able markup (the parser routed picture-
        // backed equations it couldn't typeset to `Block::Image` already). Lay it out to a
        // crisp raster; on the rare engine failure it stays `Block::Math` and centres its
        // Unicode floor — never blank.
        if let delryn_eqn::Rendered::Typeset(raster) = delryn_eqn::render(item, em_px, |_| None) {
            *b = Block::Image {
                src: String::new(),
                alt: std::mem::take(&mut item.text),
                data: raster.png,
                caption: Vec::new(),
                math: true,
                width: delryn_model::ImageWidth::Auto,
                // Path A: sized by its render em, not a measured ink profile.
                ink: None,
            };
        }
    }
}

/// Render each **inline** math run in `blocks` to a small themed image drawn mid-line,
/// assigning each a section-local id and swapping it in place for [`SpanMath::Raster`]. Two
/// sources feed it:
/// - [`SpanMath::Picture`] — a publisher inline **image** (a tiny `<img>` symbol/equation the
///   loader resolved bytes for). The image *is* the render, so it's drawn whenever graphical
///   math is on, replacing the ugly placeholder these books would otherwise show mid-prose.
/// - [`SpanMath::Source`] — recovered LaTeX/MathML, typeset down the engine rung. This stays
///   behind the `graphical_inline_math` opt-in (off by default), since its Unicode floor
///   lines up with prose better than a raster when the source is recoverable.
///
/// A no-op when graphical math is off. Per-run best-effort — an unresolved/undecodable/too-tall
/// picture, or a typeset failure, leaves the run as-is so the wrapper shows its floor (never
/// regressed). Only top-level `Para`/`Heading` runs are rendered — the same runs the wrapper
/// reserves atom cells for; nested (callout/table/caption) inline math keeps its own id space
/// the reader doesn't address. Runs off the main thread at every decode site.
pub(crate) fn convert_inline_math(blocks: &mut [Block]) {
    if !ENABLED.load(Ordering::Relaxed) {
        return; // graphical math off entirely — inline math stays its Unicode floor
    }
    let inline_typeset = INLINE_ENABLED.load(Ordering::Relaxed);
    let em_px = INLINE_EM_PX.load(Ordering::Relaxed);
    let mut next_id = 0usize;
    for b in blocks.iter_mut() {
        let spans = match b {
            Block::Para { spans, .. } | Block::Heading { spans, .. } => spans,
            _ => continue,
        };
        for span in spans.iter_mut() {
            // Take the math out so we can produce the raster without a borrow tangle; restore
            // the source when we don't convert, so the floor is never lost.
            let png: Option<Vec<u8>> = match span.math.take() {
                Some(SpanMath::Picture { src, data }) => {
                    // The publisher marked this image inline (`class="inline"`) and the loader
                    // resolved its bytes — so the image *is* the render: draw it. No aspect
                    // gate — a narrow tall fraction (`1/n`) is exactly what we want, and the
                    // ink-based sizing normalises it to the prose size (a centred multi-row
                    // atom, bounded by `INLINE_MAX_ROWS`). An unresolved picture (no bytes)
                    // keeps its `Picture` floor so its placeholder text shows. Identical images
                    // (the same glyph repeated) are deduped downstream by content.
                    if data.is_empty() {
                        span.math = Some(SpanMath::Picture { src, data });
                        None
                    } else {
                        Some(data)
                    }
                }
                Some(SpanMath::Source(item)) => {
                    // Typeset rung, opt-in only. A too-tall stack (a fraction / ∫∑ with limits)
                    // that would smear in one row, or any failure, keeps the Unicode floor.
                    let png = (inline_typeset && em_px != 0)
                        .then(|| match delryn_eqn::render(&item, em_px, |_| None) {
                            delryn_eqn::Rendered::Typeset(raster)
                                if crate::media::image_dimensions(&raster.png).is_some_and(
                                    |(_, h)| (h as f32) <= INLINE_MAX_H_EM * em_px as f32,
                                ) =>
                            {
                                Some(raster.png)
                            }
                            _ => None,
                        })
                        .flatten();
                    if png.is_none() {
                        span.math = Some(SpanMath::Source(item));
                    }
                    png
                }
                other => {
                    span.math = other;
                    None
                }
            };
            if let Some(png) = png {
                span.math = Some(SpanMath::Raster { id: next_id, png });
                next_id += 1;
            }
        }
    }
}

/// Measure the ink profile of every equation image in `blocks` (in place), so it can be
/// sized to a text-relative height by the general pixel algorithm — regardless of the
/// file's DPI or how it was produced (see [`delryn_media::ink_profile`]). Runs right after
/// [`convert_math_blocks`] at every decode site — off the main thread in the section loader
/// — so the profile rides along on the block and both the up-front row estimate and the
/// async build read it (they must agree). A no-op when graphical rendering is off (images
/// then show as placeholders, so size is moot) or once a block is already profiled.
///
/// This measures delryn's **own** typeset renders (from [`convert_math_blocks`], empty
/// `src`) the same as publisher rasters: both are normalised so one glyph line hits the
/// text-relative target, so a re-typeset equation and a publisher raster come out the same
/// size (and a tall matrix is scaled down per-line instead of rendering at its native,
/// oversized layout). A figure or photo that slips through the candidate gate profiles to
/// `None` and is then sized as a figure.
/// The file name of an image `src`, without directory or extension:
/// `"images/Figure-3.1.jpg"` → `"Figure-3.1"`, so a figure-named file is recognised even
/// when the alt is empty.
fn file_stem(src: &str) -> &str {
    let base = src.rsplit(['/', '\\']).next().unwrap_or(src);
    base.rsplit_once('.').map_or(base, |(stem, _)| stem)
}

pub(crate) fn profile_equation_images(blocks: &mut [Block], section: usize) {
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
        // A candidate equation: flagged display-math (a publisher raster *or* delryn's own
        // typeset render), or alt text that parses as math, or an uncaptioned auto-width
        // image whose alt *and* filename are not a figure's. A descriptive alt ("Figure 3.1:
        // Two vectors…") or a figure-named file ("Figure-3.1.jpg" — this book ships diagrams
        // with an empty alt, so the filename is the only signal) marks a diagram: left
        // unprofiled so it sizes as a figure, not blown up on the equation path (its large
        // bbox has few ink lines, so it would otherwise be mistaken for a short equation).
        let figure = delryn_model::math::looks_like_figure_caption(alt.as_str())
            || delryn_model::math::looks_like_figure_caption(file_stem(src));
        let candidate = *math
            || delryn_model::math::is_math(alt.as_str())
            || (caption.is_empty() && matches!(*width, delryn_model::ImageWidth::Auto) && !figure);
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
    unify_book_em(blocks, section);
}

/// Clamp each equation's measured em toward the book's typical em, so a badly-measured
/// outlier can't render far off — while every normally-measured equation keeps its **own**
/// measurement and so normalises to exactly the text-relative target. This section's ems
/// join the book-wide pool ([`BOOK_EMS`]); the typical em is drawn from **every** equation
/// seen so far, then used as the clamp centre for this section.
///
/// Sizing each equation by its own em is what makes it DPI-independent — an equation shipped
/// at a higher pixel resolution must not render larger. (An earlier version instead *forced*
/// every equation to one shared em; that made displayed size track the raw raster resolution,
/// which varies per image, so equations at different DPI came out at visibly different sizes.)
///
/// The clamp centre is the pool's **upper-quartile (p75) em, not the median**, because the
/// measurement only ever errs *low*: a fraction-heavy equation (`1/n Σ`, `x̄ ± Z(σ/√n)`) is
/// mostly small numerator/denominator/limit glyphs, so its handful of full-height cap letters
/// are a minority the cap-height estimate undershoots. Biasing the centre high means a whole
/// page of such undershot equations is clamped up toward the book's correctly-measured size
/// rather than left oversized. Below a few samples the pool isn't meaningful yet, so each
/// equation keeps its own measurement unclamped.
fn unify_book_em(blocks: &mut [Block], section: usize) {
    let section_ems: Vec<f32> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Image { ink: Some(p), .. } => Some(p.line_px),
            _ => None,
        })
        .collect();
    let Ok(mut pool) = BOOK_EMS.lock() else {
        return;
    };
    if section_ems.is_empty() {
        pool.remove(&section);
        return;
    }
    pool.insert(section, section_ems);
    let all: Vec<f32> = pool.values().flatten().copied().collect();
    drop(pool); // release before touching the blocks
    let Some(reference) = book_reference(&all) else {
        return;
    };
    // Publish the reference; tick the generation only on a meaningful move, so the reader
    // re-decodes off-screen sections it baked against a now-stale size (not on jitter).
    let prev = BOOK_REF.swap((reference * 100.0) as u32, Ordering::Relaxed);
    if prev != 0 && (reference * 100.0 - prev as f32).abs() > prev as f32 * BOOK_REF_SHIFT {
        BOOK_REF_GEN.fetch_add(1, Ordering::Relaxed);
    }
    for b in blocks.iter_mut() {
        if let Block::Image { ink: Some(p), .. } = b {
            // Per-equation normalisation: each raster is sized by its OWN measured em, so a
            // well-measured equation lands at *exactly* the text-relative target — this is
            // what keeps sizing DPI-independent (an equation shipped at a higher pixel
            // resolution isn't rendered larger). Forcing every equation to one shared em did
            // the opposite: it made displayed size track the raw raster resolution, so
            // equations at different DPI came out at visibly different sizes. We only *clamp*
            // a wildly out-of-range em toward the book's typical, so a single badly-measured
            // outlier can't render far off; everything in-band keeps its own measurement.
            p.line_px = p
                .line_px
                .clamp(reference * EM_CLAMP_LO, reference * EM_CLAMP_HI);
        }
    }
}

/// How far a single equation's measured em may sit from the book's typical em before it is
/// clamped toward it — wide, so only a clearly mis-measured outlier is pulled in and every
/// normally-measured equation keeps its own em (and so normalises to exactly the target).
const EM_CLAMP_LO: f32 = 0.7;
const EM_CLAMP_HI: f32 = 1.5;

/// The shared equation em from a pool of measured ems: the **upper quartile** — biased
/// high because the ink measurement only ever errs *low* (see [`unify_book_em`]) — so
/// undershooting equations are levelled *up* to the size the correctly-measured majority
/// agree on. `None` below a few samples, where the pool isn't yet meaningful.
fn book_reference(ems: &[f32]) -> Option<f32> {
    if ems.len() < 3 {
        return None;
    }
    let mut sorted = ems.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(sorted[(sorted.len() * 3) / 4])
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
        } else {
            self.resize_offscreen_on_ref_shift();
        }
    }

    /// When the book-wide equation em shifts (early reading, before enough equations are
    /// pooled — see [`BOOK_REF_GEN`]), drop the cached **off-screen** sections so they
    /// re-decode at the new shared size when scrolled to. The visible section is left
    /// untouched, so there is no resize on screen; it self-corrects on the next revisit.
    /// A no-op for normal reading, where the reference is stable and never ticks — and on
    /// a paged doc, whose rasterized-page cache must never be dropped mid-view.
    fn resize_offscreen_on_ref_shift(&mut self) {
        let current = BOOK_REF_GEN.load(Ordering::Relaxed);
        if current == BOOK_REF_SEEN.swap(current, Ordering::Relaxed) || self.is_paged_image() {
            return;
        }
        let cur = self.section;
        self.sections.sections.retain(|&s, _| s == cur);
        self.sections.requested.retain(|&s| s == cur);
        self.cont_cache.clear();
        self.prefetch_neighbors();
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

    /// The rasterise em is the text em at the 100% default (equation glyphs match the
    /// prose), scaling linearly with the "Math size %" knob — down toward text size and
    /// below, or up to the 300% max.
    #[test]
    fn display_em_px_is_text_relative() {
        assert_eq!(display_em_px(20, 100), 20); // default: exactly the text (cell) em
        assert_eq!(display_em_px(20, 200), 40); // knob doubles it
        assert_eq!(display_em_px(20, 300), 60); // …up to the 300% max
        assert_eq!(display_em_px(20, 70), 14); // …and down below text size to tune large math down
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
            item: delryn_model::MathItem {
                display: true,
                typeset: Some(delryn_model::MarkupSource::Latex("x^2".to_string())),
                picture: None,
                text: "x²".to_string(),
            },
        };
        let no_graphics = || Block::Math {
            item: delryn_model::MathItem {
                display: true,
                typeset: None,
                picture: None,
                text: "α".to_string(),
            },
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
        let mut on = vec![math(), no_graphics()];
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
                delryn_model::Span::math(delryn_model::MathItem {
                    display: false,
                    typeset: Some(delryn_model::MarkupSource::Latex(latex.to_string())),
                    picture: None,
                    text: "approx".to_string(),
                }),
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
        ENABLED.store(true, Ordering::Relaxed); // graphical math on (the master gate)

        // Off → no conversion (stays a LaTeX source span). Typeset inline math is gated on
        // its own opt-in (`INLINE_ENABLED`) on top of the master toggle.
        INLINE_ENABLED.store(false, Ordering::Relaxed);
        let mut off = vec![para("x^2")];
        convert_inline_math(&mut off);
        assert!(
            matches!(math_of(&off[0]), Some(SpanMath::Source(_))),
            "off: inline math stays a recovered source"
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
            matches!(math_of(&tall[0]), Some(SpanMath::Source(_))),
            "a too-tall stack keeps its Unicode fallback"
        );

        INLINE_ENABLED.store(false, Ordering::Relaxed); // don't leak to other tests
        ENABLED.store(false, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A publisher inline picture (a tiny `<img>` the loader resolved bytes for) is drawn
    /// mid-line whenever graphical math is on — no typeset opt-in needed, because the image
    /// *is* the render. Off, or unresolved, it stays a `Picture` so its floor text shows.
    /// (Identical images are deduped by content in the atom pipeline, so a repeated symbol
    /// uploads once.)
    #[test]
    fn converts_inline_picture_to_raster() {
        // Serialised with the other tests that flip the global `ENABLED`/`INLINE_ENABLED`
        // atomics, so they can't race and flip the flag out from under our assertions.
        let _env = crate::test_env_guard();
        let para_pic = |data: Vec<u8>| Block::Para {
            spans: vec![
                delryn_model::Span::plain("see "),
                delryn_model::Span {
                    text: "▢".to_string(),
                    style: delryn_model::Inline {
                        math: true,
                        ..Default::default()
                    },
                    anchor: None,
                    math: Some(SpanMath::Picture {
                        src: "images/r.jpg".to_string(),
                        data,
                    }),
                },
            ],
            indent: 0,
            quote: false,
            marker: None,
        };
        let math_of = |b: &Block| match b {
            Block::Para { spans, .. } => spans[1].math.clone(),
            _ => unreachable!(),
        };

        // Master graphical math OFF → stays a Picture (the floor shows).
        ENABLED.store(false, Ordering::Relaxed);
        let mut off = vec![para_pic(equation_png())];
        convert_inline_math(&mut off);
        assert!(
            matches!(math_of(&off[0]), Some(SpanMath::Picture { .. })),
            "off: inline picture stays a picture"
        );

        // ON, typeset opt-in OFF → the resolved picture still rasterises (it needs no source).
        ENABLED.store(true, Ordering::Relaxed);
        INLINE_ENABLED.store(false, Ordering::Relaxed);
        let mut on = vec![para_pic(equation_png())];
        convert_inline_math(&mut on);
        assert!(
            matches!(math_of(&on[0]), Some(SpanMath::Raster { .. })),
            "on: resolved inline picture rasterises to an atom"
        );

        // An unresolved picture (loader found no bytes) stays a Picture → floor.
        let mut unresolved = vec![para_pic(Vec::new())];
        convert_inline_math(&mut unresolved);
        assert!(
            matches!(math_of(&unresolved[0]), Some(SpanMath::Picture { .. })),
            "unresolved inline picture keeps its floor"
        );

        ENABLED.store(false, Ordering::Relaxed);
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

    /// The profiling pass measures every equation image — an uncaptioned raster, a
    /// `math`-flagged publisher `<img>` (real `src`), *and* delryn's own typeset render
    /// (`math`, empty `src`) — so all are sized text-relative by the same pixel algorithm;
    /// only captioned figures are left untouched.
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
        profile_equation_images(&mut blocks, 0);
        assert!(
            matches!(&blocks[0], Block::Image { ink: None, .. }),
            "off: not profiled"
        );

        // On → every equation is measured (publisher rasters *and* delryn's own render),
        // so all size text-relative by the same pixel algorithm; only the figure is left.
        ENABLED.store(true, Ordering::Relaxed);
        profile_equation_images(&mut blocks, 0);
        let ink = |b: &Block| matches!(b, Block::Image { ink: Some(_), .. });
        assert!(ink(&blocks[0]), "uncaptioned equation image is profiled");
        assert!(
            ink(&blocks[1]),
            "publisher math image (real src) is profiled"
        );
        assert!(!ink(&blocks[2]), "captioned figure left unprofiled");
        assert!(
            ink(&blocks[3]),
            "delryn's own render (empty src) is profiled too — sized like every equation"
        );

        ENABLED.store(false, Ordering::Relaxed); // don't leak to other tests
    }

    /// The shared reference is the pool's upper quartile, so a mis-measured (undershot)
    /// equation is levelled *up* to what the correctly-measured majority agree on — not
    /// the median, which a page of uniformly-undershot equations would drag down.
    #[test]
    fn book_reference_biases_to_the_upper_quartile() {
        // Three well-measured (~20px) equations and one undershot outlier (6px).
        assert_eq!(book_reference(&[20.0, 21.0, 19.0, 6.0]), Some(21.0));
        // A whole page of undershot metrics next to the book's correct ones snaps up.
        assert_eq!(
            book_reference(&[17.0, 17.0, 18.0, 24.0, 24.0, 24.0, 24.0]),
            Some(24.0)
        );
        // Below a few samples the pool isn't meaningful yet.
        assert_eq!(book_reference(&[20.0, 20.0]), None);
    }
}
