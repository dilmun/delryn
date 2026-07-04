//! Pure vertical page-stack geometry for continuous paged (PDF) scrolling.
//!
//! Single-page and spread paged views place one whole page (or a facing pair) per
//! frame. Continuous paged reading instead stacks pages *vertically* — each page
//! scaled so its content-box width fills the viewport, laid out top to bottom with
//! a small inter-page gap — and scrolls through that stack a row at a time, so a
//! page boundary passes seamlessly (the tail of one page and the head of the next
//! share the viewport, the SumatraPDF / Preview model).
//!
//! This module is the pure geometry: it never touches the terminal, the reader, or
//! the image caches. Given each visible page's content box (margin-trimmed raster
//! region) and its display height in cells, plus the anchor scroll offset, it emits
//! the [`PageTarget`]s — a destination cell rect and a **source-pixel crop** of the
//! raster — that the direct-Kitty [`PageDeck`](crate::app::page_deck) then
//! transmits. Slicing a page to its visible band reuses the same `crop` the deck
//! already supports for zoom/pan. The `impl Reader` glue that reads the caches and
//! the scroll math live in `reader::paged` / `reader::continuous`.

use ratatui::layout::Rect;

use crate::app::PageTarget;

/// Rows of blank space between consecutive stacked pages, so the pages read as
/// distinct sheets rather than one continuous strip. Kept small (the continuous
/// feel wants pages close) and constant (not a config knob for v1).
pub(super) const STACK_GAP: u16 = 1;

/// One page's metrics for the stack: its section, the content box `(x, y, w, h)`
/// (margin-trimmed region of its raster, in raster pixels), and its display height
/// in cells when the content-box width is scaled to fill the viewport width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StackPage {
    pub section: usize,
    pub content: (u32, u32, u32, u32),
    pub rows: u16,
}

/// A page's display height in cells at fit-width: scale the content box so its
/// width fills `body_w` cells, then measure its height in cells. `cell` is the
/// pixel size `(w, h)` of one terminal cell. Always ≥ 1.
pub(super) fn page_rows(content: (u32, u32, u32, u32), body_w: u16, cell: (u16, u16)) -> u16 {
    let (_, _, cw, ch) = content;
    let cw = cw.max(1) as u64;
    let ch = ch.max(1) as u64;
    let disp_w_px = body_w.max(1) as u64 * cell.0.max(1) as u64;
    // displayed height in px = ch * (disp_w_px / cw); rows = that / cell height.
    let disp_h_px = ch * disp_w_px / cw;
    let rows = disp_h_px.div_ceil(cell.1.max(1) as u64);
    rows.clamp(1, u16::MAX as u64) as u16
}

/// Lay out the visible page bands into [`PageTarget`]s. `pages[0]` is the anchor
/// (topmost page); `scroll` is how many rows of the anchor have scrolled above the
/// viewport top. Each page occupies `rows` cells followed by [`STACK_GAP`] blank
/// rows; the walk emits a target for every page band that intersects the viewport,
/// clipping the top page (by `scroll`) and the bottom page (by the viewport edge)
/// and translating each visible slice into a source-pixel crop of its content box.
/// Pure — the caller filters to pages whose pixels are actually ready.
pub(super) fn stack_targets(body: Rect, scroll: usize, pages: &[StackPage]) -> Vec<PageTarget> {
    let vh = body.height as i64;
    let mut targets = Vec::new();
    // Top of the current band relative to the viewport top, in cells. The anchor
    // starts `scroll` rows above the top.
    let mut cursor: i64 = -(scroll as i64);
    for pg in pages {
        if cursor >= vh {
            break;
        }
        let ph = pg.rows.max(1) as i64;
        let top = cursor;
        let bot = cursor + ph;
        if bot > 0 && top < vh {
            // The slice of this page inside the viewport.
            let vis_top = top.max(0);
            let vis_bot = bot.min(vh);
            let dest_rows = (vis_bot - vis_top) as u16;
            let skip = (vis_top - top) as u64; // rows clipped off this page's top
            if dest_rows > 0 {
                let (cx, cy, cw, ch) = pg.content;
                let ch64 = ch.max(1) as u64;
                let ph64 = ph as u64;
                let src_y = cy + (skip * ch64 / ph64) as u32;
                let src_h = ((dest_rows as u64 * ch64 / ph64) as u32).max(1);
                // Keep the crop within the content box.
                let src_h = src_h.min((cy + ch).saturating_sub(src_y)).max(1);
                targets.push(PageTarget {
                    section: pg.section,
                    rect: Rect::new(body.x, body.y + vis_top as u16, body.width, dest_rows),
                    crop: Some((cx, src_y, cw, src_h)),
                });
            }
        }
        cursor = bot + STACK_GAP as i64;
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
    }

    fn pages() -> Vec<StackPage> {
        // Two identical 40-row pages (content 1000×800 at 100 cols → 800/20 = 40).
        vec![
            StackPage {
                section: 3,
                content: (0, 0, 1000, 800),
                rows: 40,
            },
            StackPage {
                section: 4,
                content: (0, 0, 1000, 800),
                rows: 40,
            },
        ]
    }

    fn body() -> Rect {
        Rect::new(0, 2, 100, 50)
    }

    /// At scroll 0 the anchor sits flush at the top with no clip; the next page
    /// follows after the gap, clipped by the viewport bottom.
    #[test]
    fn stack_at_top_places_anchor_then_next() {
        let t = stack_targets(body(), 0, &pages());
        assert_eq!(t.len(), 2);
        // Anchor: whole page, at the body top.
        assert_eq!(t[0].section, 3);
        assert_eq!(t[0].rect.y, 2);
        assert_eq!(t[0].rect.height, 40);
        assert_eq!(t[0].crop, Some((0, 0, 1000, 800)), "anchor uncropped slice");
        // Next page starts after the anchor + the gap.
        assert_eq!(t[1].section, 4);
        assert_eq!(t[1].rect.y, 2 + 40 + STACK_GAP);
        // The viewport (50 rows) cuts the second page short: 50 - 40 - gap = 9.
        assert_eq!(t[1].rect.height, 50 - 40 - STACK_GAP);
        // Its crop starts at the page top and shows only the visible rows.
        let (_, sy, _, sh) = t[1].crop.unwrap();
        assert_eq!(sy, 0, "second page shows from its top");
        assert!(sh < 800, "second page is clipped by the viewport bottom");
    }

    /// Scrolling into the anchor clips its top: the anchor's crop moves down its
    /// content and the destination shifts to the viewport top.
    #[test]
    fn scrolling_clips_the_anchor_top() {
        let t = stack_targets(body(), 10, &pages());
        assert_eq!(t[0].section, 3);
        assert_eq!(t[0].rect.y, 2, "clipped anchor still starts at the top");
        assert_eq!(t[0].rect.height, 30, "10 of 40 rows scrolled off");
        let (_, sy, _, sh) = t[0].crop.unwrap();
        assert_eq!(sy, 10 * 800 / 40, "crop skips the scrolled-off pixels");
        assert_eq!(sh, 30 * 800 / 40);
    }

    /// A page taller than the viewport shows only a viewport-high slice; the next
    /// page isn't reached.
    #[test]
    fn tall_page_fills_the_viewport_alone() {
        let tall = vec![StackPage {
            section: 0,
            content: (0, 0, 1000, 4000),
            rows: 200,
        }];
        let t = stack_targets(body(), 0, &tall);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].rect.height, 50, "fills the whole viewport");
    }

    /// The crop x/width always span the whole content box width (fit-width fills
    /// the viewport horizontally); only the vertical slice changes.
    #[test]
    fn crop_spans_full_content_width() {
        let content = (150u32, 100u32, 700u32, 1200u32);
        let pg = vec![StackPage {
            section: 1,
            content,
            rows: 60,
        }];
        let t = stack_targets(body(), 20, &pg);
        let (x, _, w, _) = t[0].crop.unwrap();
        assert_eq!((x, w), (150, 700), "full content width, trimmed margins");
    }
}
