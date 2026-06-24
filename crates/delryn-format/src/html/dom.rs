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

/// Whether an element is print/marker chrome the reflowable reader doesn't want:
/// a list item number (`ItemNumber`) or footnote backref (`FootnoteNumber`) we
/// regenerate, or a print page number (`TocPageNumber`/`PageNumber`) that means
/// nothing without fixed pages. Dropped like code line numbers.
pub(super) fn is_marker_chrome(e: &Element) -> bool {
    e.attr("class").is_some_and(|c| {
        let lc = c.to_ascii_lowercase();
        lc.contains("pagenumber")
            || c.split([' ', '-', '_']).any(|t| {
                matches!(
                    t.to_ascii_lowercase().as_str(),
                    "itemnumber" | "footnotenumber" | "footnotemark"
                )
            })
    })
}

/// For a printed table-of-contents entry, its nesting depth from the level class
/// (`TocChapter`/`TocPart` = 0, `TocSection1` = 1, `TocSection2` = 2, …) so the
/// otherwise-flat ToC page indents like the hierarchy it represents. `None` for
/// non-ToC elements.
pub(super) fn toc_level(e: &Element) -> Option<u8> {
    e.attr("class")?.split_whitespace().find_map(|c| {
        let c = c.to_ascii_lowercase();
        if matches!(c.as_str(), "tocchapter" | "tocpart" | "tocfrontmatter") {
            Some(0)
        } else {
            c.strip_prefix("tocsection")
                .and_then(|n| n.parse::<u8>().ok())
        }
    })
}
