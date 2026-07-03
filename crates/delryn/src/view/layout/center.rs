//! The `Center` view mode: a single reading column, padded by `side_padding`
//! percent on each side and centred in the body.

use super::{
    GUTTER_COLS, LayoutCtx, LayoutPlan, LayoutStrategy, Placement, TextColumn, centered_column,
    measure_for,
};

pub(super) struct CenterStrategy;

impl LayoutStrategy for CenterStrategy {
    fn plan(&self, ctx: &LayoutCtx) -> LayoutPlan {
        // Reflowed text keeps its reading margin. A page image fills the pane
        // edge-to-edge — it carries its own margin (halved by the trim, toggled
        // with `x`), so no extra reading margin is added.
        let measure = if ctx.paged {
            ctx.body.width.max(1)
        } else {
            measure_for(ctx.body.width, ctx.config.side_padding)
        };
        let left_pad = ctx.body.width.saturating_sub(measure) / 2;
        let text_area = centered_column(ctx.body, left_pad, measure);
        let page_lines = text_area.height as usize;

        let placements = if ctx.paged {
            // One whole page image for the current section.
            vec![Placement::Page {
                section: ctx.section,
                area: text_area,
            }]
        } else {
            vec![Placement::Text(TextColumn {
                area: text_area,
                scroll: ctx.scroll,
                // The ribbon needs the left margin to exist.
                gutter: left_pad >= GUTTER_COLS,
            })]
        };
        LayoutPlan {
            measure,
            page_lines,
            placements,
        }
    }
}
