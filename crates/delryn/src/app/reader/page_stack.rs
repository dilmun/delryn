//! Pure vertical page-stack geometry for continuous paged (PDF) scrolling.
//!
//! Single-page and spread paged views place one whole page (or a facing pair) per
//! frame. Continuous paged reading instead stacks pages *vertically* — one page per
//! band in single-column mode, a facing pair per band in two-page mode — laid out
//! top to bottom with a small inter-page gap, and scrolls through the stack a row
//! at a time so a page boundary passes seamlessly (the SumatraPDF / Preview model).
//!
//! Pages are laid out **fit-page** — the whole page shows, sized to fit the viewport
//! (a portrait page in a wide pane comes out narrower than the pane, so it centres
//! with side padding) rather than stretched to fill the width. [`fit_page_cols`]
//! computes that width and [`place_tile_h`] the horizontal placement (centre, or a
//! pan crop once zoomed past the viewport).
//!
//! This module is the pure geometry: it never touches the terminal, the reader, or
//! the image caches. The reader hands the resolved tiles to [`stack_targets`], which
//! walks the bands **vertically** and slices each visible tile into a Kitty
//! source-crop that the direct-Kitty [`PageDeck`](crate::app::page_deck) transmits —
//! reusing the same `crop` the deck already supports for zoom/pan. The `impl Reader`
//! glue that reads caches + config, and the scroll math, live in `reader::paged` /
//! `reader::continuous`.

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

/// The display width in cells at which a page's **whole** content fits inside a slot
/// of `slot_w` cells and `vh` rows (fit-page): the full slot width when the page is
/// already short enough at that width, else shrunk so its height fits `vh`. This is
/// the "make a full single page appear" size — a portrait page in a wide slot comes
/// out narrower than the slot (so it centres with side padding). `cell` = the pixel
/// size of a terminal cell.
pub(super) fn fit_page_cols(
    content: (u32, u32, u32, u32),
    slot_w: u16,
    vh: u16,
    cell: (u16, u16),
) -> u16 {
    let rows_at_slot = page_rows(content, slot_w, cell);
    if rows_at_slot <= vh.max(1) {
        slot_w.max(1)
    } else {
        (slot_w as u32 * vh.max(1) as u32 / rows_at_slot.max(1) as u32).max(1) as u16
    }
}

/// Place a tile of display width `disp_w` cells and its source crop:
///
/// * If `disp_w` fits its slot, centre it in the slot (the slot itself is already
///   inside the padded content region), showing the whole content width.
/// * If it grew past the slot (zoomed in) but still fits the viewport, centre it in
///   the viewport (spilling into the side padding).
/// * If it exceeds the viewport, fill the viewport and show a `pan_x`-selected
///   horizontal window of the content.
///
/// Returns `(dest_x` from the body's left, `dest_w, src_x, src_w)`.
pub(super) fn place_tile_h(
    slot_x: u16,
    slot_w: u16,
    disp_w: u16,
    viewport_cols: u16,
    content_x: u32,
    content_w: u32,
    pan_x: f32,
) -> (u16, u16, u32, u32) {
    let vcols = viewport_cols.max(1);
    let disp_w = disp_w.max(1);
    if disp_w <= vcols {
        let (region_x, region_w) = if disp_w <= slot_w {
            (slot_x, slot_w)
        } else {
            (0, vcols)
        };
        let x = region_x + region_w.saturating_sub(disp_w) / 2;
        (x.min(vcols), disp_w, content_x, content_w)
    } else {
        // Overflow: fill the viewport, pan-crop the content horizontally.
        let vis_w = (content_w as u64 * vcols as u64 / disp_w as u64).max(1) as u32;
        let vis_w = vis_w.min(content_w.max(1));
        let src_x = content_x
            + (content_w.saturating_sub(vis_w) as f32 * pan_x.clamp(0.0, 1.0)).round() as u32;
        (0, vcols, src_x, vis_w)
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
                // The stack lays out against base rasters throughout (its tiles are
                // measured from the base dimensions and it never takes the crisp
                // path), so its crops index the base raster.
                raster_w: super::BASE_RASTER_WIDTH,
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

    /// Fit-page: a portrait page in a slot that's short for it shrinks so its height
    /// fits, coming out narrower than the slot (→ it will centre with padding). A
    /// page already short enough keeps the full slot width.
    #[test]
    fn fit_page_shrinks_a_tall_page_to_fit_height() {
        // 1000×1400 content at 100 cols is 70 rows tall (see page_rows test); in a
        // 40-row viewport it must shrink to ~40/70 of the width.
        let w = fit_page_cols((0, 0, 1000, 1400), 100, 40, CELL);
        assert!(w < 100, "tall page narrower than the slot: {w}");
        assert_eq!(w, 100 * 40 / 70);
        // A short (landscape) page already fits the height at full width.
        assert_eq!(fit_page_cols((0, 0, 1400, 500), 100, 40, CELL), 100);
    }

    /// A tile narrower than its slot centres within the slot and shows all content.
    #[test]
    fn place_tile_centres_within_the_slot() {
        // Slot [10, 80] (a padded region), tile 60 wide → centred at 10 + 10.
        let (x, w, sx, sw) = place_tile_h(10, 80, 60, 100, 0, 1000, 0.0);
        assert_eq!((x, w), (20, 60));
        assert_eq!((sx, sw), (0, 1000), "whole content shown");
    }

    /// A tile grown past its slot but within the viewport centres in the viewport.
    #[test]
    fn place_tile_spills_into_padding_when_grown() {
        // Slot [10, 80], tile 90 wide (> slot 80, < viewport 100) → centred at 5.
        let (x, w, ..) = place_tile_h(10, 80, 90, 100, 0, 1000, 0.0);
        assert_eq!((x, w), (5, 90));
    }

    /// A tile wider than the viewport fills it and pan-crops the content.
    #[test]
    fn place_tile_overflow_crops_and_pans() {
        let (x, w, sx0, sw) = place_tile_h(0, 100, 200, 100, 0, 1000, 0.0);
        assert_eq!((x, w), (0, 100), "fills the viewport width");
        assert_eq!(sw, 500, "shows half the content at 2× display width");
        assert_eq!(sx0, 0, "pan 0 → left edge");
        let (_, _, sx1, _) = place_tile_h(0, 100, 200, 100, 0, 1000, 1.0);
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
