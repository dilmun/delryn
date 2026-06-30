//! Metadata editor: record mouse hit-rects for the editor overlay.

use super::*;

/// Capture the editor's clickable regions for mouse hit-testing, mirroring the
/// tab strip / body layouts (kept beside the render geometry it shadows).
pub(crate) fn record_hits(app: &mut App, tab_strip: Rect, body: Rect) {
    let (tab, results_len) = match &app.overlay {
        Overlay::MetaEdit(e) => (e.tab, e.search().results.len()),
        _ => return,
    };

    // Tab strip: " N label " cells separated by a single space (see render_tabs).
    let mut tabs = Vec::new();
    let mut tx = tab_strip.x;
    for (i, t) in EditTab::ALL.iter().enumerate() {
        if i > 0 {
            tx += 1;
        }
        let w = t.label().chars().count() as u16 + 4; // " N label "
        tabs.push((
            *t,
            Rect {
                x: tx,
                y: tab_strip.y,
                width: w,
                height: 1,
            },
        ));
        tx += w;
    }
    app.mouse.edit_tabs = tabs;

    let row = |i: u16| Rect {
        x: body.x,
        y: body.y + i,
        width: body.width,
        height: 1,
    };
    let in_body = |y: u16| y < body.y + body.height;
    let value_start = body.x + 3 + LABEL_W as u16; // marker (3) + label column
    let mut fields = Vec::new();
    let mut results = Vec::new();
    let mut search = None;
    match tab {
        EditTab::Details => {
            // Mirror render_details: a section header (+ a gap before later
            // groups) precedes each group's fields, shifting their rows down.
            let mut line = 0u16;
            for (gi, (_, group)) in DETAILS_GROUPS.iter().enumerate() {
                if gi > 0 {
                    line += 1; // blank between groups
                }
                line += 1; // section header
                for &fi in *group {
                    if in_body(body.y + line) {
                        fields.push((fi, value_start, row(line)));
                    }
                    line += 1;
                }
            }
        }
        EditTab::Online => {
            // Mirror render_online: query (row 0), gap (1), three seed fields
            // (rows 2..5), rule (5), results from row 6.
            for i in 0..LOOKUP_FIELDS as u16 {
                let y = body.y + 2 + i;
                if in_body(y) {
                    fields.push((i as usize, value_start, row(2 + i)));
                }
            }
            // query (1) + gap (1) + fields (LOOKUP_FIELDS) + rule (1).
            let first = 3 + LOOKUP_FIELDS as u16;
            for i in 0..results_len as u16 {
                let y = body.y + first + i;
                if !in_body(y) {
                    break;
                }
                results.push((
                    i as usize,
                    Rect {
                        x: body.x,
                        y,
                        width: body.width,
                        height: 1,
                    },
                ));
            }
        }
        EditTab::Cover => {
            search = Some(row(0)); // search bar occupies the first body row
            let rw = body.width.saturating_sub(38); // results sit left of the preview
            for i in 0..results_len as u16 {
                let y = body.y + 2 + i; // one search row + one blank
                if !in_body(y) {
                    break;
                }
                results.push((
                    i as usize,
                    Rect {
                        x: body.x,
                        y,
                        width: rw,
                        height: 1,
                    },
                ));
            }
        }
    }
    app.mouse.edit_fields = fields;
    app.mouse.edit_results = results;
    app.mouse.edit_search = search;
}
