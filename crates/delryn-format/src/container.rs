//! Format-neutral container helpers: the low-level DOM- and resource-walking
//! primitives shared by every text backend (EPUB today, MOBI/AZW3 later). These
//! own *no* format-specific or parsing-policy logic — just the mechanical bits
//! that were otherwise reimplemented per extractor. The HTML layer's
//! parsing-aware helpers stay in `html/dom.rs`; this sits below them.

use std::ffi::OsStr;
use std::path::Path;

use ego_tree::NodeRef;
use scraper::{Html, Node};

/// Concatenate the text of every descendant of `node`, in document order.
///
/// - `br_as_newline` turns each `<br>` element into a `'\n'` so code / line-split
///   callers keep their line structure; plain-text callers pass `false`.
/// - `byte_cap` stops once that many bytes have been gathered — an optimization
///   for callers that only need a short leading prefix; `None` gathers all text.
///
/// Callers that want trimming/whitespace-collapsing do it on the result; that
/// part differs per call site and isn't this helper's concern.
pub(crate) fn descendant_text(
    node: NodeRef<Node>,
    br_as_newline: bool,
    byte_cap: Option<usize>,
) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        match d.value() {
            Node::Text(t) => {
                s.push_str(&t.text);
                if let Some(cap) = byte_cap
                    && s.len() > cap
                {
                    break;
                }
            }
            Node::Element(e) if br_as_newline && e.name() == "br" => s.push('\n'),
            _ => {}
        }
    }
    s
}

/// The document's `<body>` element, or the tree root when there is none — the
/// node every block/nav walk should start from.
pub(crate) fn body_or_root(doc: &Html) -> NodeRef<'_, Node> {
    doc.tree
        .root()
        .descendants()
        .find(|n| matches!(n.value(), Node::Element(e) if e.name() == "body"))
        .unwrap_or_else(|| doc.tree.root())
}

/// Whether `value` — a space-separated token list such as `class` / `epub:type` /
/// `role` — carries `want` as one of its tokens (case-insensitive). The shared
/// matcher behind both the `Element`-level and string-level token checks.
pub(crate) fn has_token(value: &str, want: &str) -> bool {
    value
        .split_whitespace()
        .any(|t| t.eq_ignore_ascii_case(want))
}

/// Whether `path`'s final component equals `target` — the file-name fallback used
/// when a resource's full path doesn't line up with the one referenced from
/// content. Each caller keeps its own iteration order (resource map vs. spine), so
/// only this comparison is shared.
pub(crate) fn filename_eq(path: &Path, target: &OsStr) -> bool {
    path.file_name() == Some(target)
}
