//! Shared low-level DOM helpers: token matching over `class` / `epub:type` /
//! `role`, used by the toolchain detector and the semantic classifier.

use scraper::node::Element;

/// Whether `attr` carries `want` as one of its space-separated tokens
/// (case-insensitive). `epub:type`, `role`, and `class` are all token lists, so
/// always match tokens — never whole-string compare.
pub(super) fn attr_has_token(e: &Element, attr: &str, want: &str) -> bool {
    e.attr(attr)
        .is_some_and(|v| v.split_whitespace().any(|t| t.eq_ignore_ascii_case(want)))
}

/// Whether the element's `class` carries `want` as a token (case-insensitive).
pub(super) fn class_has_token(e: &Element, want: &str) -> bool {
    attr_has_token(e, "class", want)
}

/// Whether an element is a *marker* the reader regenerates itself — an explicit
/// list item number (`class="ItemNumber"`) or a footnote number/backref
/// (`class="FootnoteNumber"`). Like code line numbers, these are chrome: dropped
/// so we don't double them with our own list markers / `[n]` footnote labels.
pub(super) fn is_marker_chrome(e: &Element) -> bool {
    e.attr("class").is_some_and(|c| {
        c.split([' ', '-', '_']).any(|t| {
            matches!(
                t.to_ascii_lowercase().as_str(),
                "itemnumber" | "footnotenumber" | "footnotemark"
            )
        })
    })
}
