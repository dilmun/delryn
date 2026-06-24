//! The semantic classifier: given a block-level element, decide what it *is* —
//! in one priority-ordered place. This is the single decision the orchestrator
//! dispatches on, so detection logic never scatters across the extractors.
//!
//! Priority (most-standard first): admonition/footnote/display-math containers
//! (by `epub:type`/`role`/class) → code → heading → the structural HTML element.

use super::*;

/// What a block-level element resolves to. Variants carry the data the
/// classifier already computed, so extractors don't re-detect.
pub(super) enum ElementRole {
    Callout(CalloutKind),
    /// Footnote/endnote definition: its raw anchor `id` (match key) and the
    /// `label` shown at the foot of the section.
    Footnote {
        id: String,
        label: String,
    },
    /// Display math backed by an image: `(src, unicode-alt)`.
    DisplayMathImage(String, String),
    CodeBlock,
    /// A native `<math display="block">` display equation.
    DisplayMath,
    Heading(u8),
    Paragraph,
    List {
        ordered: bool,
        start: usize,
    },
    Quote,
    Rule,
    Image,
    /// An aside/callout laid out as an icon-cell + content-cell table.
    AsideIconTable(CalloutKind),
    Table,
    /// A stray `<td>`/`<th>` outside a recognised table → degrade to a paragraph.
    Cell,
    /// Anything else: a generic container to recurse into.
    Container,
}

/// Classify a block-level element. The order mirrors the detection priority:
/// semantic containers before the plain structural element, so a
/// `<blockquote class="note">` becomes a callout rather than a quote, and a
/// `<p class="Code"><code>…<br>…` becomes code rather than a paragraph.
pub(super) fn classify(e: &scraper::node::Element, node: NodeRef<Node>) -> ElementRole {
    let name = e.name();

    // 1. Admonition / callout containers (class or epub:type note/tip/warning/…).
    if matches!(name, "div" | "section" | "aside" | "blockquote")
        && let Some(kind) = callout_kind(e)
    {
        return ElementRole::Callout(kind);
    }
    // 2. Footnote / endnote definitions.
    if matches!(name, "div" | "section" | "aside" | "p" | "li")
        && let Some((id, label)) = footnote_def(e)
    {
        return ElementRole::Footnote { id, label };
    }
    // 3. Display (block) math backed by an image — render the image, alt as fallback.
    if matches!(name, "p" | "div")
        && let Some((src, alt)) = display_math_image(node)
    {
        return ElementRole::DisplayMathImage(src, alt);
    }
    // 4. Code listings (<pre>, styled container, or multi-line <code>).
    if is_code_block(e, node) {
        return ElementRole::CodeBlock;
    }
    // 5. Native display math, headings, then the structural element.
    match name {
        "math" => ElementRole::DisplayMath,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => ElementRole::Heading(name.as_bytes()[1] - b'0'),
        "p" => ElementRole::Paragraph,
        "ul" | "ol" => ElementRole::List {
            ordered: name == "ol",
            start: e
                .attr("start")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(1),
        },
        "blockquote" => ElementRole::Quote,
        "hr" => ElementRole::Rule,
        "img" => ElementRole::Image,
        "table" if aside_icon_src(node).is_some() => ElementRole::AsideIconTable(
            aside_kind_from_icon(&aside_icon_src(node).unwrap_or_default()),
        ),
        "table" => ElementRole::Table,
        "td" | "th" => ElementRole::Cell,
        _ => ElementRole::Container,
    }
}

/// If a container is a footnote/endnote definition, its `(id, label)`:
/// - `id` is the raw anchor id (the key a reference resolves against);
/// - `label` is the number shown — the digits of the `id`, else the `id`, else `note`.
///
/// Detected by the standardised semantics: EPUB `epub:type`
/// (`footnote`/`endnote`/`rearnote`), the equivalent DPUB-ARIA `role`
/// (`doc-footnote`/`doc-endnote`), or a conventional `class`.
pub(super) fn footnote_def(e: &scraper::node::Element) -> Option<(String, String)> {
    // Exact-token match (not substring): a `footnotes` *section* must not be read
    // as a `footnote`, and a wrapper isn't a definition just for containing one.
    let etype = e.attr("epub:type").unwrap_or("").to_ascii_lowercase();
    let by_type = etype
        .split_whitespace()
        .any(|t| matches!(t, "footnote" | "endnote" | "rearnote"));
    let role = e.attr("role").unwrap_or("").to_ascii_lowercase();
    let by_role = role
        .split_whitespace()
        .any(|t| matches!(t, "doc-footnote" | "doc-endnote" | "doc-rearnote"));
    let by_class = e.attr("class").is_some_and(|c| {
        c.split([' ', '-', '_']).any(|t| {
            matches!(
                t.to_ascii_lowercase().as_str(),
                "footnote" | "endnote" | "rearnote" | "fn"
            )
        })
    });
    let id = e.attr("id").unwrap_or("");
    // Precise semantics (epub:type / DPUB role) mark a definition on their own; a
    // class-only match must carry an `id` (a real, referenceable definition), so
    // section/wrapper containers (`class="FootnoteSection"`, `<div class="Footnote">`)
    // around the actual definition aren't themselves mistaken for one.
    if !(by_type || by_role || (by_class && !id.is_empty())) {
        return None;
    }
    let digits: String = id.chars().filter(char::is_ascii_digit).collect();
    let label = if !digits.is_empty() {
        digits
    } else if !id.is_empty() {
        id.to_string()
    } else {
        "note".to_string()
    };
    Some((id.to_string(), label))
}
