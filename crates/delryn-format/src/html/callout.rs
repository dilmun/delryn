//! Admonition / callout detection and extraction (note/tip/warning/…),
//! including aside-icon tables.

use super::*;

/// Classify a container as an admonition by its `class` / `epub:type` tokens.
/// Splits on spaces, hyphens and underscores and matches each segment exactly,
/// so `admonition-warning` is a Warning while `footnote` is *not* a Note.
pub(super) fn callout_kind(e: &scraper::node::Element) -> Option<CalloutKind> {
    [e.attr("class"), e.attr("epub:type"), e.attr("type")]
        .into_iter()
        .flatten()
        .flat_map(|a| a.split([' ', '-', '_', '\t']))
        .find_map(CalloutKind::from_word)
}

/// Build a [`Block::Callout`] from a container's children (quote context reset —
/// the callout's own border replaces any blockquote styling).
pub(super) fn emit_callout(
    node: NodeRef<Node>,
    kind: CalloutKind,
    ctx: &Ctx,
    out: &mut Vec<Block>,
) {
    let inner_ctx = Ctx {
        quote: false,
        ..*ctx
    };
    let mut blocks = Vec::new();
    walk_children(node, &inner_ctx, &mut blocks);
    if !blocks.is_empty() {
        out.push(Block::Callout {
            kind,
            title: None,
            blocks,
        });
    }
}

/// The icon image `src` of an aside/callout table, if it is one. The src word
/// (warning/info/tip/…) classifies the admonition in [`aside_kind_from_icon`].
pub(super) fn aside_icon_src(node: NodeRef<Node>) -> Option<String> {
    let is_aside = matches!(node.value(), Node::Element(e) if e.attr("class").is_some_and(|c| c.contains("aside")));
    if !is_aside {
        return None;
    }
    node.descendants().find_map(|n| match n.value() {
        Node::Element(e) if e.name() == "img" => Some(e.attr("src").unwrap_or("").to_string()),
        _ => None,
    })
}

/// Map an aside icon's `src` filename to a callout kind (info/pencil/question and
/// anything unrecognised fall back to Note).
pub(super) fn aside_kind_from_icon(src: &str) -> CalloutKind {
    let s = src.to_lowercase();
    if s.contains("warning") || s.contains("caution") || s.contains("danger") {
        CalloutKind::Warning
    } else if s.contains("key") || s.contains("important") {
        CalloutKind::Important
    } else if s.contains("tip") || s.contains("hint") {
        CalloutKind::Tip
    } else {
        CalloutKind::Note
    }
}

/// Table cells that carry text (i.e. the content cell, not the icon-only cell).
pub(super) fn content_cells(node: NodeRef<Node>) -> Vec<NodeRef<Node>> {
    node.descendants()
        .filter(|n| matches!(n.value(), Node::Element(e) if matches!(e.name(), "td" | "th")))
        .filter(|cell| {
            cell.descendants()
                .any(|d| matches!(d.value(), Node::Text(t) if !t.text.trim().is_empty()))
        })
        .collect()
}
