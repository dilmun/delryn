//! Math in the DOM: detect math classes, standalone math images, alt-text
//! conversion, and the conservative exponent fix-up. Conversion of MathML text
//! to Unicode lives in `crate::mathml`.

use super::*;

/// Whether `class` contains any of `keywords` as a substring (case-insensitive).
/// The shared check behind both math-class predicates; the keyword vocabulary
/// itself lives in the toolchain registry.
fn class_contains_any(class: &str, keywords: &[&str]) -> bool {
    let c = class.to_ascii_lowercase();
    keywords.iter().any(|k| c.contains(k))
}

/// Whether an element's class marks it as math content. Covers the common
/// conventions (InDesign `…MathTools…Math_…`, MathJax/MathML wrappers, generic
/// `math`/`equation` classes) by matching the substring — publisher-agnostic.
pub(super) fn is_math_class(e: &scraper::node::Element) -> bool {
    e.attr("class")
        .is_some_and(|c| class_contains_any(c, MATH_CLASS_KEYWORDS))
}

/// Conservative, math-scoped exponent fix: a run of digits immediately following
/// a closing `)`/`]` inside math is a power, so super-script it
/// (`(x−μ)2` → `(x−μ)²`). Deliberately narrow — ambiguous cases like `σ2`/`μ3`
/// (exponent vs subscript index) are left flat rather than guessed wrong, and
/// non-math text is never touched. Publishers that flatten scripts to plain
/// glyphs (no sub/sup tag) lose the rest irrecoverably.
pub(super) fn superscript_math_exponents(spans: &mut [Span]) {
    // Whether the previous math glyph was a `)`/`]` (or a digit we just lifted),
    // tracked across spans since math is emitted one glyph per span.
    let mut after_close = false;
    for span in spans.iter_mut() {
        if !span.style.math {
            after_close = false;
            continue;
        }
        let chars: Vec<char> = span.text.chars().collect();
        let mut out = String::with_capacity(span.text.len());
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if after_close && c.is_ascii_digit() {
                let mut j = i;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                let run: String = chars[i..j].iter().collect();
                match delryn_model::math::superscript_str(&run) {
                    Some(s) => out.push_str(&s),
                    None => out.push_str(&run),
                }
                i = j; // a digit run keeps `after_close` true (multi-digit power)
                continue;
            }
            out.push(c);
            after_close = matches!(c, ')' | ']');
            i += 1;
        }
        span.text = out;
    }
}

/// If a block is standalone display math backed by an image, its `(src,
/// Unicode-alt)`. True when it holds a math image and either no other text or a
/// math/equation/display class — so an equation on its own line renders as the
/// image (Unicode alt as fallback), while math mid-sentence stays inline.
pub(super) fn display_math_image(node: NodeRef<Node>) -> Option<(String, String)> {
    let class_math = matches!(node.value(), Node::Element(e) if e.attr("class").is_some_and(|c| {
        class_contains_any(c, MATH_CLASS_KEYWORDS) || class_contains_any(c, &[DISPLAY_MATH_CLASS_KEYWORD])
    }));
    let mut found = None;
    let mut other_text = false;
    for d in node.descendants() {
        match d.value() {
            Node::Element(e) if e.name() == "img" => {
                let alt = e.attr("alt").unwrap_or("");
                if delryn_model::math::is_math(alt) {
                    found = Some((e.attr("src").unwrap_or("").to_string(), math_unicode(alt)));
                }
            }
            Node::Text(t) if !t.text.trim().is_empty() => other_text = true,
            _ => {}
        }
    }
    match found {
        Some(x) if !other_text || class_math => Some(x),
        _ => None,
    }
}

/// Math source (LaTeX or MathML) → final Unicode for rendering.
pub(super) fn math_unicode(alt: &str) -> String {
    if delryn_model::math::is_mathml(alt) {
        crate::mathml::to_unicode(alt)
    } else {
        delryn_model::math::latex_to_unicode(alt)
    }
}

/// Whether an element is a native MathML `<math>`.
pub(super) fn is_math_element(e: &scraper::node::Element) -> bool {
    e.name() == "math"
}

/// Whether a `<math>` is a *display* (block) equation (`display="block"`), as
/// opposed to inline math.
pub(super) fn is_display_math(e: &scraper::node::Element) -> bool {
    e.attr("display")
        .is_some_and(|d| d.eq_ignore_ascii_case("block"))
}

/// Convert a native `<math>` element to Unicode. Prefers the authored text
/// equivalents (`alttext` / `<annotation encoding="…tex">`, which carry LaTeX and
/// aren't otherwise rendered), then walks the presentation MathML.
pub(super) fn native_math_unicode(node: NodeRef<Node>) -> String {
    let Some(e) = node.value().as_element() else {
        return String::new();
    };
    // 1. alttext attribute (usually LaTeX, e.g. from LaTeXML).
    if let Some(alt) = e.attr("alttext")
        && !alt.trim().is_empty()
    {
        return delryn_model::math::latex_to_unicode(alt);
    }
    // 2. <annotation encoding="application/x-tex"> embedded LaTeX.
    if let Some(tex) = annotation_tex(node) {
        return delryn_model::math::latex_to_unicode(&tex);
    }
    // 3. Walk the presentation MathML (serialise the subtree, then transcode).
    match scraper::ElementRef::wrap(node) {
        Some(el) => crate::mathml::to_unicode(&el.html()),
        None => String::new(),
    }
}

/// The text of a `<math>`'s TeX `<annotation>`, if present.
fn annotation_tex(node: NodeRef<Node>) -> Option<String> {
    node.descendants()
        .filter_map(|n| n.value().as_element().map(|e| (n, e)))
        .filter(|(_, e)| e.name() == "annotation")
        .find(|(_, e)| {
            e.attr("encoding")
                .is_some_and(|enc| enc.to_ascii_lowercase().contains("tex"))
        })
        .map(|(n, _)| {
            n.descendants()
                .filter_map(|d| match d.value() {
                    Node::Text(t) => Some(t.text.as_ref()),
                    _ => None,
                })
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

/// An image `alt` with no useful content (empty or a generic placeholder).
pub(super) fn is_placeholder_alt(alt: &str) -> bool {
    let a = alt.trim();
    a.is_empty() || a.eq_ignore_ascii_case("image") || a.eq_ignore_ascii_case("images")
}
