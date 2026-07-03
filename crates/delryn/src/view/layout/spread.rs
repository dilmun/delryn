//! The `TwoPage` view mode: two side-by-side columns forming a spread. For
//! reflowable content the right column continues from the left (scroll flows
//! left-to-right); for paged content it's a facing-page spread.

use ratatui::layout::{Constraint, Layout};

use super::{GUTTER_COLS, LayoutCtx, LayoutPlan, LayoutStrategy, Placement, TextColumn};

pub(super) struct SpreadStrategy;

impl LayoutStrategy for SpreadStrategy {
    fn plan(&self, ctx: &LayoutCtx) -> LayoutPlan {
        let body = ctx.body;
        let config = ctx.config;
        // Both keep the configurable inter-page gap (like the EPUB spread) so the
        // two pages don't touch. Reflowed columns keep the per-side reading margin;
        // paged (PDF) pages fill the outer edges — they carry their own margin
        // (halved by the trim, toggled with `x`), so no reading margin is added.
        let (pad, gap) = if ctx.paged {
            (0, config.page_gap)
        } else {
            (
                ((body.width as u32 * config.side_padding as u32 / 100) as u16).max(GUTTER_COLS),
                config.page_gap,
            )
        };
        let usable = body.width.saturating_sub(pad * 2 + gap).max(2);
        let col_w = (usable / 2).max(1);
        // Re-center any rounding remainder into the outer margins.
        let side_pad = body.width.saturating_sub(col_w * 2 + gap) / 2;
        let cols = Layout::horizontal([
            Constraint::Length(side_pad),
            Constraint::Length(col_w),
            Constraint::Length(gap),
            Constraint::Length(col_w),
            Constraint::Min(0),
        ])
        .split(body);
        let left_area = cols[1];
        let right_area = cols[3];
        let h = left_area.height as usize;

        let placements = if ctx.paged {
            // A facing-page spread; the reader decides the pairing (cover-offset
            // aware). A lone page (the cover, or a trailing odd page) centres
            // across the whole area rather than sitting in one column.
            match ctx.spread {
                [only] => vec![Placement::Page {
                    section: *only,
                    area: body,
                }],
                [l, r, ..] => vec![
                    Placement::Page {
                        section: *l,
                        area: left_area,
                    },
                    Placement::Page {
                        section: *r,
                        area: right_area,
                    },
                ],
                [] => Vec::new(),
            }
        } else {
            vec![
                Placement::Text(TextColumn {
                    area: left_area,
                    scroll: ctx.scroll,
                    // The left column's ribbon uses the outer margin (when present).
                    gutter: side_pad >= GUTTER_COLS,
                }),
                Placement::Text(TextColumn {
                    area: right_area,
                    scroll: ctx.scroll + h,
                    // The right column's ribbon uses the inter-column gap (always
                    // wide enough).
                    gutter: true,
                }),
            ]
        };
        LayoutPlan {
            measure: col_w,
            page_lines: h,
            placements,
        }
    }
}
