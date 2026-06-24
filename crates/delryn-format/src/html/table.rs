//! `<table>` → `Block::Table` parsing.

use super::*;

/// Parse a `<table>` into a [`Block::Table`]. The first row is the header when
/// it sits in a `<thead>` or is made entirely of `<th>` cells. Cell content is
/// flattened to inline spans. (Nested tables are uncommon in books; their rows
/// are folded into the outer table.)
pub(super) fn parse_table(node: NodeRef<Node>) -> Option<Block> {
    let is_named =
        |n: &NodeRef<Node>, name: &str| matches!(n.value(), Node::Element(e) if e.name() == name);
    let cells_of = |tr: NodeRef<Node>| -> Vec<TableCell> {
        tr.children()
            .filter(|c| is_named(c, "td") || is_named(c, "th"))
            .map(|cell| {
                let mut spans = Vec::new();
                for c in cell.children() {
                    collect_inline(c, Inline::default(), &mut spans);
                }
                spans
            })
            .collect()
    };

    let mut header: Option<Vec<TableCell>> = None;
    let mut rows: Vec<Vec<TableCell>> = Vec::new();
    for tr in node.descendants().filter(|n| is_named(n, "tr")) {
        let cells = cells_of(tr);
        if cells.is_empty() {
            continue;
        }
        let all_th = tr
            .children()
            .filter(|c| is_named(c, "td") || is_named(c, "th"))
            .all(|c| is_named(&c, "th"));
        let in_thead = tr.ancestors().any(|a| is_named(&a, "thead"));
        if header.is_none() && rows.is_empty() && (all_th || in_thead) {
            header = Some(cells);
        } else {
            rows.push(cells);
        }
    }

    (header.is_some() || !rows.is_empty()).then_some(Block::Table { header, rows })
}
