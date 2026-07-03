//! The `Center` view mode: a single reading column, padded by `side_padding`
//! percent on each side and centred in the body.

use super::{
    GUTTER_COLS, LayoutCtx, LayoutPlan, LayoutStrategy, Placement, TextColumn, centered_column,
    measure_for,
};

pub(super) struct CenterStrategy;

impl LayoutStrategy for CenterStrategy {
    fn plan(&self, ctx: &LayoutCtx) -> LayoutPlan {
        // Reflowed text keeps its full reading margin. A page image (which carries
        // its own, trimmed, margins) uses half that margin on each edge — big, but
        // not edge-to-edge.
        let measure = if ctx.paged {
            let full_pad = ((ctx.body.width as u32 * ctx.config.side_padding as u32 / 100) as u16)
                .max(GUTTER_COLS);
            ctx.body.width.saturating_sub(full_pad).max(1)
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
