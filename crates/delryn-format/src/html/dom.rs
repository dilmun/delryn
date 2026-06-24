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
