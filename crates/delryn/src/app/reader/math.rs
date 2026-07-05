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

use crate::document::Block;

use super::Reader;

/// Whether display equations are rendered graphically (config on + graphics
/// available). Read on every decoded section.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// The em size in px to rasterise equations at (`0` = unset), so a display equation
/// shows at ~[`DISPLAY_EM_FACTOR`] text lines — delryn-media shows math at native px.
static EM_PX: AtomicU32 = AtomicU32::new(0);

/// A single-line display equation is ~1.2 em tall; rendering at ~1.7× the cell
/// height lands it at roughly two text lines — prominent without dominating.
const DISPLAY_EM_FACTOR: f32 = 1.7;

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
        };
    }
}

impl Reader {
    /// Mirror the graphical-math state from the view each frame: `on` = the config
    /// toggle AND a graphics protocol; `cell_h` = the terminal cell height in px;
    /// `scale_pct` = the "Math size %" setting (100 = the built-in size). When the
    /// effective state changes (startup, toggle, size, or a cell-size change) the open
    /// sections are re-decoded so equations switch / resize live, without reopening.
    pub fn sync_graphical_math(&mut self, on: bool, cell_h: u16, scale_pct: u16) {
        let em = (cell_h.max(1) as f32 * DISPLAY_EM_FACTOR * scale_pct.max(1) as f32 / 100.0)
            .round() as u32;
        let changed =
            ENABLED.swap(on, Ordering::Relaxed) != on || EM_PX.swap(em, Ordering::Relaxed) != em;
        // Only reflowable docs have math to (un)convert; never drop a paged doc's
        // rasterized-page cache (it would blank the page and force a reload).
        if changed && !self.is_paged_image() {
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
}
