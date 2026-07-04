//! Pure vertical page-stack geometry for continuous paged (PDF) scrolling.
//!
//! Single-page and spread paged views place one whole page (or a facing pair) per
//! frame. Continuous paged reading instead stacks pages *vertically* — one page per
//! band in single-column mode, a facing pair per band in two-page mode — laid out
//! top to bottom with a small inter-page gap, and scrolls through the stack a row
//! at a time so a page boundary passes seamlessly (the SumatraPDF / Preview model).
//!
//! This module is the pure geometry: it never touches the terminal, the reader, or
//! the image caches. The **horizontal** placement of each tile (its zoom scale,
//! centring, and pan crop) is resolved by [`tile_h`]; the reader hands the resolved
//! tiles to [`stack_targets`], which walks the bands **vertically** and slices each
//! visible tile into a Kitty source-crop that the direct-Kitty
//! [`PageDeck`](crate::app::page_deck) transmits. Slicing a page to its visible
//! window reuses the same `crop` the deck already supports for zoom/pan. The
//! `impl Reader` glue that reads caches + config, and the scroll math, live in
//! `reader::paged` / `reader::continuous`.

use ratatui::layout::Rect;

use crate::app::PageTarget;

/// Rows of blank space between consecutive stacked bands, so the pages read as
/// distinct sheets rather than one continuous strip. Kept small (the continuous
/// feel wants pages close) and constant.
pub(super) const STACK_GAP: u16 = 1;

/// One page tile in the stack, with its horizontal placement already resolved:
/// which section, its content box `(x, y, w, h)` (margin-trimmed raster region),
/// the destination cell column offset `x` (from the body's left) and width `w`, the
/// horizontal source crop (`src_x`, `src_w`, in raster px — a sub-window when zoomed
/// past the viewport width, else the whole content width), and its full display
/// height in cells at the current zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StackTile {
    pub section: usize,
    pub content: (u32, u32, u32, u32),
    pub x: u16,
    pub w: u16,
    pub src_x: u32,
    pub src_w: u32,
    pub rows: u16,
}

/// A stack band: the tiles sharing one vertical slot — one for single-column, one
/// or two for a two-page spread. `rows` is the band height (the max over its tiles),
/// which sets where the next band begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StackBand {
    pub tiles: Vec<StackTile>,
    pub rows: u16,
}

/// A page's display height in cells when its content-box width is scaled to fill
/// `disp_w` cells. `cell` is the pixel size `(w, h)` of one terminal cell. Always
/// ≥ 1. This is the one place page aspect → display rows, shared by the layout and
/// the scroll math.
pub(super) fn page_rows(content: (u32, u32, u32, u32), disp_w: u16, cell: (u16, u16)) -> u16 {
    let (_, _, cw, ch) = content;
    let cw = cw.max(1) as u64;
    let ch = ch.max(1) as u64;
    let disp_w_px = disp_w.max(1) as u64 * cell.0.max(1) as u64;
    // displayed height in px = ch * (disp_w_px / cw); rows = that / cell height.
    let disp_h_px = ch * disp_w_px / cw;
    let rows = disp_h_px.div_ceil(cell.1.max(1) as u64);
    rows.clamp(1, u16::MAX as u64) as u16
}

/// Resolve a tile's horizontal destination + source crop. The band's natural width
/// at zoom 1.0 fills the viewport; a slot `(slot_x, slot_w)` (cells, at zoom 1.0)
/// positions this tile within that band. Returns `(dest_x, dest_w, src_x, src_w)`:
///
/// * When the scaled band fits the viewport (`scale` ≤ 1, or a narrow band), the
///   whole band is centred and the tile shows its full content width.
/// * When the scaled band overflows (zoomed in past the viewport width), the tile
///   fills the viewport and shows a horizontal sub-window of its content chosen by
///   `pan_x` ∈ [0, 1] — the reader only lets this happen for a full-width single
///   tile, so a spread never has to split a page across the fold.
pub(super) fn tile_h(
    slot_x: u16,
    slot_w: u16,
    viewport_cols: u16,
    content_x: u32,
    content_w: u32,
    scale: f32,
    pan_x: f32,
) -> (u16, u16, u32, u32) {
    let vcols = viewport_cols.max(1) as u32;
    let scaled_band = (vcols as f32 * scale).round().max(1.0) as u32;
    if scaled_band <= vcols {
        // Centre the whole band, place the tile within it at the scaled slot.
        let band_off = (vcols - scaled_band) / 2;
        let x = band_off + (slot_x as f32 * scale).round() as u32;
        let w = (slot_w as f32 * scale).round().max(1.0) as u32;
        (x.min(vcols) as u16, w as u16, content_x, content_w)
    } else {
        // Overflow: fill the viewport, show a pan-selected horizontal window.
        let vis_w = (content_w as f32 * vcols as f32 / scaled_band as f32)
            .round()
            .clamp(1.0, content_w.max(1) as f32) as u32;
        let src_x = content_x
            + (content_w.saturating_sub(vis_w) as f32 * pan_x.clamp(0.0, 1.0)).round() as u32;
        (0, viewport_cols, src_x, vis_w)
    }
}

/// Lay out the visible bands into [`PageTarget`]s. `scroll` is how many rows of the
/// top band have scrolled above the viewport top; bands are separated by
/// [`STACK_GAP`]. Each tile is top-aligned within its band and sliced by the
/// viewport edges into a source-pixel crop of its content box (the horizontal crop
/// was already resolved into `tile.src_x` / `tile.src_w`). Pure — the caller filters
/// to tiles whose pixels are ready.
pub(super) fn stack_targets(body: Rect, scroll: usize, bands: &[StackBand]) -> Vec<PageTarget> {
    let vh = body.height as i64;
    let mut targets = Vec::new();
    // Top of the current band relative to the viewport top, in cells.
    let mut cursor: i64 = -(scroll as i64);
    for band in bands {
        if cursor >= vh {
            break;
        }
        for tile in &band.tiles {
            let th = tile.rows.max(1) as i64;
            let top = cursor;
            let bot = cursor + th;
            if bot <= 0 || top >= vh {
                continue;
            }
            let vis_top = top.max(0);
            let vis_bot = bot.min(vh);
            let dest_rows = (vis_bot - vis_top) as u16;
            if dest_rows == 0 {
                continue;
            }
            let skip = (vis_top - top) as u64; // rows clipped off this tile's top
            let (_, cy, _, ch) = tile.content;
            let ch64 = ch.max(1) as u64;
            let th64 = th as u64;
            let src_y = cy + (skip * ch64 / th64) as u32;
            let src_h = ((dest_rows as u64 * ch64 / th64) as u32).max(1);
            let src_h = src_h.min((cy + ch).saturating_sub(src_y)).max(1);
            targets.push(PageTarget {
                section: tile.section,
                rect: Rect::new(body.x + tile.x, body.y + vis_top as u16, tile.w, dest_rows),
                crop: Some((tile.src_x, src_y, tile.src_w, src_h)),
            });
        }
        cursor += band.rows.max(1) as i64 + STACK_GAP as i64;
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (u16, u16) = (10, 20);

    /// A US-letter-ish portrait content box scaled to a 100-col viewport is taller
    /// than it is wide (in cells), and the height tracks the aspect ratio.
    #[test]
    fn page_rows_tracks_aspect_at_fit_width() {
        // 1000×1400 content, 100 cols. disp width = 1000px; scale = 1.0; height
        // 1400px / 20px per cell = 70 rows.
        let rows = page_rows((0, 0, 1000, 1400), 100, CELL);
        assert_eq!(rows, 70);
        // A wider (landscape) page of the same width is shorter.
        let land = page_rows((0, 0, 1400, 1000), 100, CELL);
        assert!(land < rows, "landscape page is shorter: {land} vs {rows}");
        // Halving the display width halves the height.
        assert_eq!(page_rows((0, 0, 1000, 1400), 50, CELL), 35);
    }

    /// At fit-width (scale 1) a single full-width tile fills the viewport with the
    /// whole content and no crop offset.
    #[test]
    fn tile_h_fit_width_fills_and_shows_all() {
        let (x, w, sx, sw) = tile_h(0, 100, 100, 0, 1000, 1.0, 0.0);
        assert_eq!((x, w), (0, 100));
        assert_eq!((sx, sw), (0, 1000), "whole content width visible");
    }

    /// Zooming out (scale < 1) shrinks the tile and centres it; the whole content
    /// width still shows.
    #[test]
    fn tile_h_zoom_out_centres() {
        let (x, w, sx, sw) = tile_h(0, 100, 100, 0, 1000, 0.5, 0.0);
        assert_eq!(w, 50, "half width");
        assert_eq!(x, 25, "centred in the 100-col viewport");
        assert_eq!((sx, sw), (0, 1000), "still the whole content");
    }

    /// Zooming in (scale > 1) fills the viewport and crops the content horizontally;
    /// pan selects which slice.
    #[test]
    fn tile_h_zoom_in_crops_and_pans() {
        let (x, w, sx0, sw) = tile_h(0, 100, 100, 0, 1000, 2.0, 0.0);
        assert_eq!((x, w), (0, 100), "fills the viewport width");
        assert_eq!(sw, 500, "shows half the content width at 2×");
        assert_eq!(sx0, 0, "pan 0 → left edge");
        // Pan right → the window moves right.
        let (_, _, sx1, _) = tile_h(0, 100, 100, 0, 1000, 2.0, 1.0);
        assert_eq!(sx1, 500, "pan 1 → flush right");
    }

    fn single_band(section: usize, rows: u16) -> StackBand {
        StackBand {
            tiles: vec![StackTile {
                section,
                content: (0, 0, 1000, 800),
                x: 0,
                w: 100,
                src_x: 0,
                src_w: 1000,
                rows,
            }],
            rows,
        }
    }

    fn body() -> Rect {
        Rect::new(0, 2, 100, 50)
    }

    /// At scroll 0 the top band sits flush, the next follows after the gap, clipped
    /// by the viewport bottom.
    #[test]
    fn stack_places_top_band_then_next() {
        let bands = vec![single_band(3, 40), single_band(4, 40)];
        let t = stack_targets(body(), 0, &bands);
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].section, t[0].rect.y, t[0].rect.height), (3, 2, 40));
        assert_eq!(t[1].section, 4);
        assert_eq!(t[1].rect.y, 2 + 40 + STACK_GAP);
        assert_eq!(t[1].rect.height, 50 - 40 - STACK_GAP);
    }

    /// Scrolling into the top band clips its top: the crop moves down its content.
    #[test]
    fn stack_scroll_clips_the_top_band() {
        let bands = vec![single_band(3, 40), single_band(4, 40)];
        let t = stack_targets(body(), 10, &bands);
        assert_eq!(t[0].rect.height, 30, "10 of 40 rows scrolled off");
        let (_, sy, _, sh) = t[0].crop.unwrap();
        assert_eq!(sy, 10 * 800 / 40);
        assert_eq!(sh, 30 * 800 / 40);
    }

    /// A two-tile band (a spread) places both pages side by side at the same y,
    /// and the next band starts below the taller tile.
    #[test]
    fn two_tile_band_places_both_then_advances_past_the_tallest() {
        let band = StackBand {
            tiles: vec![
                StackTile {
                    section: 4,
                    content: (0, 0, 500, 700),
                    x: 0,
                    w: 48,
                    src_x: 0,
                    src_w: 500,
                    rows: 34,
                },
                StackTile {
                    section: 5,
                    content: (0, 0, 500, 800),
                    x: 52,
                    w: 48,
                    src_x: 0,
                    src_w: 500,
                    rows: 40, // the taller of the pair
                },
            ],
            rows: 40,
        };
        let next = single_band(6, 20);
        let t = stack_targets(body(), 0, &[band, next]);
        assert_eq!(t.len(), 3);
        assert_eq!((t[0].section, t[0].rect.x), (4, 0));
        assert_eq!((t[1].section, t[1].rect.x), (5, 52), "right page offset");
        // Third tile is the next band, below the taller (40-row) page + gap.
        assert_eq!(t[2].section, 6);
        assert_eq!(t[2].rect.y, 2 + 40 + STACK_GAP);
    }
}
