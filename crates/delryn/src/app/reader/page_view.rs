//! Zoom / pan / fit state for a paged (PDF) page, plus the pure placement math
//! that turns it into a destination cell box and a source-pixel crop.
//!
//! A paged page is a full-bleed image driven directly through the Kitty protocol
//! by the [`PageDeck`](crate::app::page_deck). At fit-page the whole raster is
//! placed aspect-fit; zooming in shows a **cropped sub-region** of the raster
//! scaled to fill the viewport, and panning slides that region — so zoom & pan
//! reduce to "which pixels of the page fill the viewport this frame".
//!
//! [`place_page`] is pure (no terminal, no reader) so it's unit-tested here. It's
//! also used for the un-zoomed spread pages (with a default [`PageView`]), so
//! fit-page placement has one implementation.

/// How a page is scaled to the viewport before any manual zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageFit {
    /// The whole page fits in the viewport (the default, no overflow).
    #[default]
    Page,
    /// Page width fills the viewport width; a tall page overflows → vertical pan.
    Width,
    /// Page height fills the viewport height; a wide page overflows → horizontal pan.
    Height,
}

impl PageFit {
    fn label(self) -> &'static str {
        match self {
            PageFit::Page => "fit page",
            PageFit::Width => "fit width",
            PageFit::Height => "fit height",
        }
    }
}

/// Zoom multiplier step per keypress, and the bounds **relative to the fit scale**
/// the active [`PageFit`] picked — so `1.0` is always "as large as this fit mode
/// shows the page", whatever the viewport.
///
/// The floor is that fit scale, not something smaller: a single page shown below
/// its own fit is just a smaller page in the same empty viewport, and the reader
/// who wants to see more of it has `W` (fit page / width / height) for that.
/// [`is_zoomed`](PageView::is_zoomed) leans on the same floor to mean "a crop is
/// in play". The continuous stack's zoom (`reader::continuous`) reads differently
/// and *does* go below 1.0 — there, zooming out fits more pages on screen, which
/// is a thing worth doing.
///
/// Pages are re-rastered to match the display width when the base raster would
/// upscale (`reader::crisp`), so the ceiling is a limit on useful magnification
/// rather than on sharpness.
const ZOOM_STEP: f32 = 1.25;
const ZOOM_MIN: f32 = 1.0;
const ZOOM_MAX: f32 = 5.0;

/// Per-page zoom / pan / fit for paged documents. `zoom` multiplies the fit
/// scale (1.0 = the fit mode's own scale); `pan` is a fraction `[0, 1]` of the
/// pannable range in each axis (0 = top / left). `Copy` so the view can snapshot
/// it cheaply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageView {
    pub fit: PageFit,
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for PageView {
    fn default() -> Self {
        Self {
            fit: PageFit::Page,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }
}

impl PageView {
    /// Whether the page is scaled beyond its fit-page size — i.e. a crop/pan is
    /// in play (so nav keys pan rather than flip, and the deck shows a sub-region).
    pub fn is_zoomed(&self) -> bool {
        self.fit != PageFit::Page || self.zoom > 1.0
    }

    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * ZOOM_STEP).min(ZOOM_MAX);
    }

    /// Zoom out toward the fit scale; snaps to exactly 1.0 near the bottom so
    /// `is_zoomed` cleanly reports fit-page again.
    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / ZOOM_STEP).max(ZOOM_MIN);
        if (self.zoom - 1.0).abs() < 1e-3 {
            self.zoom = 1.0;
        }
    }

    /// Reset to the fit-page default (also clears any pan).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Cycle fit mode Page → Width → Height → Page. Manual zoom is kept (it
    /// multiplies whichever fit is active).
    pub fn cycle_fit(&mut self) {
        self.fit = match self.fit {
            PageFit::Page => PageFit::Width,
            PageFit::Width => PageFit::Height,
            PageFit::Height => PageFit::Page,
        };
    }

    /// A short status label for the current zoom/fit, e.g. `fit width · 150%`.
    pub fn label(&self) -> String {
        if (self.zoom - 1.0).abs() < 1e-3 {
            self.fit.label().to_string()
        } else {
            format!("{} · {:.0}%", self.fit.label(), self.zoom * 100.0)
        }
    }
}

/// Which directions still have pan room, so navigation can pan while there's room
/// and flip the page (edge-flip) once there isn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanRoom {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

/// The computed placement of a page for one frame: where to draw it and which
/// source pixels to show.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePlacement {
    /// Destination size in cells (the caller centres this in the viewport).
    pub cols: u16,
    pub rows: u16,
    /// Source-pixel crop `(x, y, w, h)` of the raster to display; `None` means the
    /// whole page (fit-page — no crop params emitted, byte-identical to before).
    pub crop: Option<(u32, u32, u32, u32)>,
    /// Remaining pan room, for edge-flip navigation.
    pub room: PanRoom,
    /// Pan step per keypress as a fraction of the pan range (~one screenful).
    pub step_x: f32,
    pub step_y: f32,
}

/// The terminal viewport a page is placed into: its size in cells plus the pixel
/// size of one cell (from the image picker).
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u16,
    pub cell_h: u16,
}

/// Map a page raster + viewport + [`PageView`] to a destination cell box and a
/// source crop. Pure: no terminal, no reader.
///
/// * `raster` — the page raster's `(width, height)` in pixels.
/// * `content` — the sub-region `(x, y, w, h)` of the raster to treat as the page
///   (margin trim); pass `(0, 0, w, h)` for the whole raster. Fit / zoom / pan all
///   operate within this region, and the emitted crop is in raster coords.
/// * `vp` — the viewport (cells + cell pixel size).
pub fn place_page(
    raster: (u32, u32),
    content: (u32, u32, u32, u32),
    vp: Viewport,
    view: &PageView,
) -> PagePlacement {
    let img_w = raster.0.max(1);
    let img_h = raster.1.max(1);
    // The content region, clamped inside the raster.
    let (cx, cy, cw_px, ch_px) = content;
    let cx = cx.min(img_w - 1);
    let cy = cy.min(img_h - 1);
    let cw_px = cw_px.clamp(1, img_w - cx);
    let ch_px = ch_px.clamp(1, img_h - cy);
    let cw = vp.cell_w.max(1) as f32;
    let ch = vp.cell_h.max(1) as f32;
    let vcols = vp.cols.max(1);
    let vrows = vp.rows.max(1);
    let vpx = vcols as f32 * cw; // viewport width in pixels
    let vpy = vrows as f32 * ch; // viewport height in pixels

    // The base fit scale (content px → screen px), then the manual zoom on top.
    let fit_scale = match view.fit {
        PageFit::Page => (vpx / cw_px as f32).min(vpy / ch_px as f32),
        PageFit::Width => vpx / cw_px as f32,
        PageFit::Height => vpy / ch_px as f32,
    };
    let s = (fit_scale * view.zoom).max(f32::MIN_POSITIVE);

    // The source window that fills the viewport at scale `s`, within the content.
    let win_w = ((vpx / s).round() as u32).clamp(1, cw_px);
    let win_h = ((vpy / s).round() as u32).clamp(1, ch_px);
    let range_x = cw_px - win_w;
    let range_y = ch_px - win_h;
    let off_x = (view.pan_x.clamp(0.0, 1.0) * range_x as f32).round() as u32;
    let off_y = (view.pan_y.clamp(0.0, 1.0) * range_y as f32).round() as u32;
    let crop_x = cx + off_x; // absolute raster coordinates
    let crop_y = cy + off_y;

    // Destination cell box: the window scaled by `s`, clamped to the viewport.
    let cols = (((win_w as f32 * s) / cw).round() as u16).clamp(1, vcols);
    let rows = (((win_h as f32 * s) / ch).round() as u16).clamp(1, vrows);

    // A crop is needed unless the window is the whole (untrimmed) raster.
    let whole = win_w == img_w && win_h == img_h && cx == 0 && cy == 0;
    let crop = (!whole).then_some((crop_x, crop_y, win_w, win_h));

    let room = PanRoom {
        left: off_x > 0,
        right: off_x < range_x,
        up: off_y > 0,
        down: off_y < range_y,
    };
    // One screenful per press: the visible window as a fraction of the range.
    let step_x = if range_x > 0 {
        (win_w as f32 / range_x as f32).min(1.0)
    } else {
        0.0
    };
    let step_y = if range_y > 0 {
        (win_h as f32 / range_y as f32).min(1.0)
    } else {
        0.0
    };

    PagePlacement {
        cols,
        rows,
        crop,
        room,
        step_x,
        step_y,
    }
}

/// The raster width at which the base placement `p` would map ~1 raster pixel per
/// screen pixel — the width a crisp re-raster should target. `base_dims` is the
/// base raster's size; `vp` the viewport. When the placement *downscales* the base
/// (the common fit-page case) this is ≤ `base_dims.0`, so no crisp raster is
/// warranted; it exceeds the base only when the page is zoomed / shown large enough
/// that the base upscales. Pure — unit-tested below.
pub fn raster_width_for_crispness(p: &PagePlacement, base_dims: (u32, u32), vp: Viewport) -> u32 {
    // The source window the placement samples (the whole base raster when there's
    // no crop), and the screen pixels it's scaled into.
    let (win_w, win_h) = match p.crop {
        Some((_, _, w, h)) => (w.max(1), h.max(1)),
        None => (base_dims.0.max(1), base_dims.1.max(1)),
    };
    let dest_w = p.cols as u32 * vp.cell_w.max(1) as u32;
    let dest_h = p.rows as u32 * vp.cell_h.max(1) as u32;
    let upscale = (dest_w as f32 / win_w as f32).max(dest_h as f32 / win_h as f32);
    // Only an *upscale* (>1) needs more raster resolution; a downscale keeps base.
    (base_dims.0 as f32 * upscale.max(1.0)).ceil() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    // A portrait page and a landscape-ish viewport (cells are ~1:2 w:h px).
    const IMG_W: u32 = 1000;
    const IMG_H: u32 = 1400;
    const CW: u16 = 8;
    const CH: u16 = 16;
    const VCOLS: u16 = 100; // 800 px
    const VROWS: u16 = 40; //  640 px

    fn vp() -> Viewport {
        Viewport {
            cols: VCOLS,
            rows: VROWS,
            cell_w: CW,
            cell_h: CH,
        }
    }

    fn place(view: &PageView) -> PagePlacement {
        place_page((IMG_W, IMG_H), (0, 0, IMG_W, IMG_H), vp(), view)
    }

    #[test]
    fn trimmed_content_always_crops_within_the_content() {
        // Margin trim: fit-page on an inset content region shows just that region
        // (filling the viewport) and always emits a crop, hiding the margins.
        let content = (150u32, 100u32, 700u32, 1200u32);
        let p = place_page((IMG_W, IMG_H), content, vp(), &PageView::default());
        let (x, y, w, h) = p.crop.expect("a trimmed page always crops");
        assert!(
            x >= 150 && y >= 100,
            "crop starts within the content origin"
        );
        assert!(
            x + w <= 150 + 700 && y + h <= 100 + 1200,
            "crop stays within the content"
        );
        assert!(p.rows <= VROWS && p.cols <= VCOLS);
    }

    #[test]
    fn fit_page_shows_the_whole_page_with_no_crop() {
        let p = place(&PageView::default());
        assert_eq!(p.crop, None, "fit-page places the whole raster (no crop)");
        assert!(p.cols <= VCOLS && p.rows <= VROWS);
        // Height-limited here (tall page), so it fills the rows.
        assert_eq!(p.rows, VROWS);
        assert_eq!(p.room, PanRoom::default(), "nothing to pan at fit-page");
    }

    #[test]
    fn fit_width_overflows_vertically_and_pans_down() {
        let v = PageView {
            fit: PageFit::Width,
            ..Default::default()
        };
        let p = place(&v);
        // Fills the full width; a crop appears because the page is now taller
        // than the viewport.
        assert_eq!(p.cols, VCOLS);
        let (_, _, cw, ch) = p.crop.expect("fit-width crops the tall page");
        assert_eq!(cw, IMG_W, "the whole width is visible");
        assert!(ch < IMG_H, "only part of the height is visible");
        assert!(
            p.room.down && !p.room.up,
            "at the top: can pan down, not up"
        );
    }

    #[test]
    fn panning_down_moves_the_crop_and_flips_room() {
        let mut v = PageView {
            fit: PageFit::Width,
            ..Default::default()
        };
        let top = place(&v);
        let (_, top_y, _, _) = top.crop.unwrap();
        v.pan_y = 1.0; // all the way down
        let bottom = place(&v);
        let (_, bot_y, _, ch) = bottom.crop.unwrap();
        assert!(bot_y > top_y, "the crop moved down the page");
        assert_eq!(bot_y + ch, IMG_H, "the crop sits flush against the bottom");
        assert!(bottom.room.up && !bottom.room.down, "at the bottom now");
    }

    #[test]
    fn zooming_in_crops_and_enables_pan_both_axes() {
        let mut v = PageView::default();
        // Enough zoom that even the width overflows a height-limited fit-page.
        v.zoom_in();
        v.zoom_in();
        v.zoom_in(); // ~1.95×
        v.pan_x = 0.5;
        v.pan_y = 0.5;
        let p = place(&v);
        let (x, y, cw, ch) = p.crop.expect("a zoomed page is cropped");
        assert!(cw < IMG_W && ch < IMG_H, "both axes overflow when zoomed");
        assert!(x > 0 && y > 0, "centred pan sits inside the page");
        assert!(p.room.left && p.room.right && p.room.up && p.room.down);
        assert_eq!(p.cols, VCOLS, "the crop fills the viewport");
        assert_eq!(p.rows, VROWS);
    }

    #[test]
    fn zoom_out_snaps_back_to_fit_page() {
        let mut v = PageView::default();
        v.zoom_in();
        assert!(v.is_zoomed());
        v.zoom_out();
        assert!(!v.is_zoomed(), "back to exactly fit-page");
        assert_eq!(place(&v).crop, None);
    }

    #[test]
    fn zoom_is_bounded() {
        let mut v = PageView::default();
        for _ in 0..50 {
            v.zoom_in();
        }
        assert!(v.zoom <= ZOOM_MAX);
        for _ in 0..50 {
            v.zoom_out();
        }
        assert_eq!(v.zoom, 1.0);
    }

    #[test]
    fn fit_page_downscale_wants_no_more_than_base() {
        // A tall page fit into a smaller viewport downscales → base is already
        // crisp, so the wanted width doesn't exceed the base width.
        let p = place(&PageView::default());
        let want = raster_width_for_crispness(&p, (IMG_W, IMG_H), vp());
        assert!(want <= IMG_W, "downscaling keeps the base width: {want}");
    }

    #[test]
    fn zooming_in_wants_a_larger_raster() {
        // Zoom far enough that a small crop is blown up past the base resolution
        // (with this 1000 px base and small viewport the page still downscales at
        // low zoom) → the base upscales, so a crisper (wider) raster is wanted.
        let mut v = PageView::default();
        for _ in 0..6 {
            v.zoom_in();
        }
        let p = place(&v);
        let want = raster_width_for_crispness(&p, (IMG_W, IMG_H), vp());
        assert!(want > IMG_W, "a zoomed page wants a wider raster: {want}");
    }
}
