//! Reader layout composition engine (Phase 7.1).
//!
//! A [`LayoutStrategy`] maps `(viewport cells, reading position, content kind)`
//! to a list of [`Placement`]s — a cell `Rect` plus what to draw there: a paged
//! image ([`Placement::Page`]) or a reflowed text-column slice
//! ([`Placement::Text`]). `view/reader.rs` is then a dumb renderer that just
//! draws the placements it's handed; it never needs to know which mode is active.
//!
//! This is the seam that lets later reading modes (spreads, N-up, sliding
//! windows, manga RTL, cross-section scroll — Phase 7.3+) each be a strategy plus
//! parameters instead of another `if` in the renderer. Adding a mode touches
//! [`plan`] (one match arm) and a new strategy file, never the renderer.
//!
//! Placement *planning* is pure geometry (no `Frame`, no drawing), so it's
//! unit-testable — see the tests below.

use ratatui::layout::{Constraint, Layout, Rect};

use crate::config::{Config, ViewMode};

mod center;
mod spread;

/// Cells reserved in the left margin for the bookmark gutter: the icon plus a
/// one-cell gap so it never butts against the text. A text column can only draw
/// its ribbon when it has at least this much margin to its left.
pub(crate) const GUTTER_COLS: u16 = 2;

/// One region of the reader body and what to draw in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// A reflowed text column: draw the wrapped lines from `scroll`, plus its
    /// inline figures and (when `gutter`) the bookmark ribbon in the left margin.
    Text(TextColumn),
    /// A paged image (a PDF page): the `PageDeck` places `section`'s page image
    /// centred + aspect-fit within `area`.
    Page { section: usize, area: Rect },
}

/// A reflowed text column slice: the wrapped-line flow starting at `scroll`,
/// drawn into `area`. `gutter` is true when there's margin to the left of `area`
/// to paint the bookmark ribbon (the outer margin, or the inter-column gap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextColumn {
    pub area: Rect,
    pub scroll: usize,
    pub gutter: bool,
}

/// The per-frame layout result: the placements to draw plus the scroll scalars
/// the reader writes back (so nav/scroll/page-mode math sees this frame's
/// geometry). `measure` is the reflow column width in cells (drives wrapping);
/// `page_lines` is one column's height in lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LayoutPlan {
    pub measure: u16,
    pub page_lines: usize,
    pub placements: Vec<Placement>,
}

/// Everything a strategy needs to plan a frame: the body rect, the config knobs,
/// the content kind, and the reading position. Pure inputs — no `Reader`, no
/// `Frame` — so planning stays testable.
pub(crate) struct LayoutCtx<'a> {
    /// The content area below the chapter header (excludes sidebar + status).
    pub body: Rect,
    pub config: &'a Config,
    /// The document renders as whole page images (PDF), not reflowed text.
    pub paged: bool,
    /// Reflow scroll offset — the first text column's flow starts here.
    pub scroll: usize,
    /// Current section — the single paged page shown in a one-page layout.
    pub section: usize,
    /// The paged spread pairing (cover-offset aware), from `Reader::spread_pages`.
    /// Used by multi-page paged layouts; empty for reflowable content.
    pub spread: &'a [usize],
}

/// A reader layout: turns a [`LayoutCtx`] into a [`LayoutPlan`]. One
/// implementation per reading mode; [`plan`] dispatches on the active [`ViewMode`].
pub(crate) trait LayoutStrategy {
    fn plan(&self, ctx: &LayoutCtx) -> LayoutPlan;
}

/// Plan the frame for the active view mode. The renderer calls only this; adding
/// a mode adds a match arm here, never a renderer edit.
pub(crate) fn plan(view_mode: ViewMode, ctx: &LayoutCtx) -> LayoutPlan {
    match view_mode {
        ViewMode::Center => center::CenterStrategy.plan(ctx),
        ViewMode::TwoPage => spread::SpreadStrategy.plan(ctx),
    }
}

/// The reading-column width for a pane width, per-side padding percent, and
/// measure cap. With padding on, each side keeps at least the gutter width so a
/// bookmark ribbon always has room; a `side_padding` of 0 % is edge-to-edge.
///
/// `max_measure` (0 = uncapped) then holds the column at a readable line however
/// wide the window gets — percentage padding alone keeps widening it. Callers
/// derive the margins *from* the returned width, so a capped column re-centres
/// and the surplus falls to both sides.
pub(crate) fn measure_for(pane_width: u16, side_padding: u16, max_measure: u16) -> u16 {
    let padded = if side_padding == 0 {
        pane_width.max(1)
    } else {
        let pad = ((pane_width as u32 * side_padding as u32 / 100) as u16).max(GUTTER_COLS);
        pane_width
            .saturating_sub(pad.saturating_mul(2))
            .max(crate::config::MIN_TEXT_COLS.min(pane_width).max(1))
    };
    match max_measure {
        0 => padded,
        cap => padded.min(cap),
    }
}

/// Split `body` into a centred column of width `measure` with `left_pad` cells of
/// margin on its left (rounding remainder falls to the right). Uses the same
/// `Layout` path as the renderer so the rect is identical to the hand-rolled one.
fn centered_column(body: Rect, left_pad: u16, measure: u16) -> Rect {
    Layout::horizontal([
        Constraint::Length(left_pad),
        Constraint::Length(measure),
        Constraint::Min(0),
    ])
    .split(body)[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn body() -> Rect {
        Rect::new(0, 2, 100, 40)
    }

    /// Percentage padding alone widens the column with the window, so a maximised
    /// terminal ends up with a line the eye can't track back from. The cap holds
    /// the measure and hands the surplus to the margins.
    #[test]
    fn the_measure_cap_stops_the_column_filling_a_wide_window() {
        // Uncapped, the column grows without limit.
        assert_eq!(measure_for(400, 10, 0), 320);
        // Capped, it stops — and the margins absorb the rest.
        assert_eq!(measure_for(400, 10, 72), 72);
        // A window too narrow to reach the cap is left entirely alone: an 80-cell
        // pane at 10 % leaves a 64-character column, short of the 72 cap.
        assert_eq!(measure_for(80, 10, 0), 64);
        assert_eq!(measure_for(80, 10, 72), 64);
        // The cap never widens a column past what the padding left it.
        assert!(measure_for(100, 25, 120) <= measure_for(100, 25, 0));
    }

    /// A capped column stays centred: the strategy derives the margins from the
    /// width it is handed, so the surplus splits evenly instead of pooling on one
    /// side.
    #[test]
    fn a_capped_column_stays_centred() {
        let cfg = Config {
            max_measure: 40,
            ..Config::default()
        };
        let wide = Rect::new(0, 2, 200, 40);
        let ctx = LayoutCtx {
            body: wide,
            config: &cfg,
            paged: false,
            scroll: 0,
            section: 0,
            spread: &[],
        };
        let p = plan(ViewMode::Center, &ctx);
        assert_eq!(p.measure, 40, "held at the cap");
        let Placement::Text(col) = p.placements[0] else {
            panic!("center reflow is a text column")
        };
        let left = col.area.x - wide.x;
        let right = (wide.x + wide.width) - (col.area.x + col.area.width);
        assert!(
            left.abs_diff(right) <= 1,
            "centred within a cell (left {left}, right {right})"
        );
    }

    #[test]
    fn center_reflow_is_one_gutter_column() {
        let cfg = Config::default();
        let ctx = LayoutCtx {
            body: body(),
            config: &cfg,
            paged: false,
            scroll: 7,
            section: 0,
            spread: &[],
        };
        let p = plan(ViewMode::Center, &ctx);
        assert_eq!(p.placements.len(), 1);
        match p.placements[0] {
            Placement::Text(c) => {
                assert_eq!(c.scroll, 7);
                assert_eq!(c.area.width, p.measure);
                assert_eq!(p.page_lines, c.area.height as usize);
            }
            _ => panic!("center reflow should be a single text column"),
        }
    }

    #[test]
    fn center_paged_is_one_page_at_current_section() {
        let cfg = Config::default();
        let ctx = LayoutCtx {
            body: body(),
            config: &cfg,
            paged: true,
            scroll: 0,
            section: 3,
            spread: &[3],
        };
        let p = plan(ViewMode::Center, &ctx);
        assert_eq!(p.placements.len(), 1);
        assert!(matches!(
            p.placements[0],
            Placement::Page { section: 3, .. }
        ));
    }

    #[test]
    fn two_page_reflow_flows_left_then_right() {
        let cfg = Config::default();
        let ctx = LayoutCtx {
            body: body(),
            config: &cfg,
            paged: false,
            scroll: 10,
            section: 0,
            spread: &[],
        };
        let p = plan(ViewMode::TwoPage, &ctx);
        let cols: Vec<TextColumn> = p
            .placements
            .iter()
            .filter_map(|pl| match pl {
                Placement::Text(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(cols.len(), 2, "a spread is two columns");
        // The right column continues from the left: scroll + one column height.
        assert_eq!(cols[0].scroll, 10);
        assert_eq!(cols[1].scroll, 10 + cols[0].area.height as usize);
        // The right column always has the inter-column gap for its ribbon.
        assert!(cols[1].gutter);
    }

    #[test]
    fn two_page_paged_pairs_the_spread() {
        let cfg = Config::default();
        let spread = [4usize, 5usize];
        let ctx = LayoutCtx {
            body: body(),
            config: &cfg,
            paged: true,
            scroll: 0,
            section: 4,
            spread: &spread,
        };
        let p = plan(ViewMode::TwoPage, &ctx);
        let pages: Vec<usize> = p
            .placements
            .iter()
            .filter_map(|pl| match pl {
                Placement::Page { section, .. } => Some(*section),
                _ => None,
            })
            .collect();
        assert_eq!(pages, vec![4, 5]);
    }

    #[test]
    fn two_page_paged_rtl_swaps_facing_pages() {
        // Manga: the facing pages swap sides so the spread reads right-to-left —
        // the later page sits on the left, the earlier on the right.
        let cfg = Config {
            reading_direction: crate::config::ReadingDirection::Rtl,
            ..Config::default()
        };
        let spread = [4usize, 5usize];
        let ctx = LayoutCtx {
            body: body(),
            config: &cfg,
            paged: true,
            scroll: 0,
            section: 4,
            spread: &spread,
        };
        let p = plan(ViewMode::TwoPage, &ctx);
        let mut pages: Vec<(usize, u16)> = p
            .placements
            .iter()
            .filter_map(|pl| match pl {
                Placement::Page { section, area } => Some((*section, area.x)),
                _ => None,
            })
            .collect();
        assert_eq!(pages.len(), 2, "a spread is two pages");
        pages.sort_by_key(|(_, x)| *x);
        assert_eq!(pages[0].0, 5, "RTL: the later page is on the left");
        assert_eq!(pages[1].0, 4, "RTL: the earlier page is on the right");
    }

    #[test]
    fn two_page_paged_lone_page_spans_the_body() {
        let cfg = Config::default();
        let spread = [0usize];
        let ctx = LayoutCtx {
            body: body(),
            config: &cfg,
            paged: true,
            scroll: 0,
            section: 0,
            spread: &spread,
        };
        let p = plan(ViewMode::TwoPage, &ctx);
        assert_eq!(p.placements.len(), 1);
        match p.placements[0] {
            // A lone page (cover / trailing odd page) centres across the whole body.
            Placement::Page { section: 0, area } => assert_eq!(area, ctx.body),
            _ => panic!("a lone paged page should span the body"),
        }
    }
}
