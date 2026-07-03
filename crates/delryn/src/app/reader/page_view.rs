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

/// Zoom multiplier step per keypress, and the bounds (relative to the fit scale).
/// The 1400 px raster stays crisp to roughly 2× on a typical viewport; past that
/// it softens (a crisp re-raster-at-DPI is a planned follow-up), so the cap is
/// conservative.
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

/// Map a page raster + viewport + [`PageView`] to a destination cell box and a
/// source crop. Pure: no terminal, no reader.
///
/// * `img_w`/`img_h` — raster pixel dimensions.
/// * `cell_w`/`cell_h` — font cell pixel size (from the image picker).
/// * `vcols`/`vrows` — viewport size in cells.
pub fn place_page(
    img_w: u32,
    img_h: u32,
    cell_w: u16,
    cell_h: u16,
    vcols: u16,
    vrows: u16,
    view: &PageView,
) -> PagePlacement {
    let img_w = img_w.max(1);
    let img_h = img_h.max(1);
    let cw = cell_w.max(1) as f32;
    let ch = cell_h.max(1) as f32;
    let vcols = vcols.max(1);
    let vrows = vrows.max(1);
    let vpx = vcols as f32 * cw; // viewport width in pixels
    let vpy = vrows as f32 * ch; // viewport height in pixels

    // The base fit scale (page px → screen px), then the manual zoom on top.
    let fit_scale = match view.fit {
        PageFit::Page => (vpx / img_w as f32).min(vpy / img_h as f32),
        PageFit::Width => vpx / img_w as f32,
        PageFit::Height => vpy / img_h as f32,
    };
    let s = (fit_scale * view.zoom).max(f32::MIN_POSITIVE);

    // The source window that fills the viewport at scale `s`, bounded by the page.
    let crop_w = ((vpx / s).round() as u32).clamp(1, img_w);
    let crop_h = ((vpy / s).round() as u32).clamp(1, img_h);
    let range_x = img_w - crop_w;
    let range_y = img_h - crop_h;
    let crop_x = (view.pan_x.clamp(0.0, 1.0) * range_x as f32).round() as u32;
    let crop_y = (view.pan_y.clamp(0.0, 1.0) * range_y as f32).round() as u32;

    // Destination cell box: the crop scaled by `s`, clamped to the viewport.
    let cols = (((crop_w as f32 * s) / cw).round() as u16).clamp(1, vcols);
    let rows = (((crop_h as f32 * s) / ch).round() as u16).clamp(1, vrows);

    let whole = crop_w == img_w && crop_h == img_h;
    let crop = (!whole).then_some((crop_x, crop_y, crop_w, crop_h));

    let room = PanRoom {
        left: crop_x > 0,
        right: crop_x < range_x,
        up: crop_y > 0,
        down: crop_y < range_y,
    };
    // One screenful per press: the visible window as a fraction of the range.
    let step_x = if range_x > 0 {
        (crop_w as f32 / range_x as f32).min(1.0)
    } else {
        0.0
    };
    let step_y = if range_y > 0 {
        (crop_h as f32 / range_y as f32).min(1.0)
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

    fn place(view: &PageView) -> PagePlacement {
        place_page(IMG_W, IMG_H, CW, CH, VCOLS, VROWS, view)
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
}
