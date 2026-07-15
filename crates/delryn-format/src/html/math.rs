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

/// The MathML/LaTeX source in an `<img>`'s trailing HTML comment. Wiley / For-Dummies
/// ship math as `<img alt="math" src="eqN.png"><!--<m:math>…</m:math>-->`: the PNG is
/// the `src`, the `alt` is a bare marker, and the real source hides in the comment.
/// Returns it when the next non-whitespace sibling is a comment that looks like math.
pub(super) fn img_math_comment(node: NodeRef<Node>) -> Option<String> {
    let mut sib = node.next_sibling();
    while let Some(n) = sib {
        match n.value() {
            Node::Comment(c) => {
                let s = c.trim();
                let looks_math = delryn_model::math::is_math(s)
                    || delryn_model::math::is_mathml(s)
                    || s.contains(":math");
                return looks_math.then(|| s.to_string());
            }
            Node::Text(t) if t.text.trim().is_empty() => sib = n.next_sibling(),
            _ => return None, // real content before any comment — not this img's
        }
    }
    None
}

/// Whether an `<img>`/SVG `<image>` carries math (and isn't a UI icon): LaTeX/MathML
/// in its `alt`, or — Wiley-style — a MathML/LaTeX source in a trailing comment.
pub(super) fn is_math_img(node: NodeRef<Node>) -> bool {
    let Some(e) = node.value().as_element() else {
        return false;
    };
    matches!(e.name(), "img" | "image")
        && !is_icon_src(&img_src(e).unwrap_or_default())
        && (e.attr("alt").is_some_and(delryn_model::math::is_math)
            || img_math_comment(node).is_some())
}

/// Whether a node is a math source we render — a native `<math>` or a math image.
pub(super) fn is_math_node(node: NodeRef<Node>) -> bool {
    node.value().as_element().is_some_and(is_math_element) || is_math_img(node)
}

/// The recovered form of a math node: its Unicode approximation, the LaTeX source
/// (delimiter-stripped) when one exists, and — for a rasterised equation — the
/// publisher image `src` as a last-resort visual.
pub(super) struct MathBlock {
    pub unicode: String,
    pub latex: Option<String>,
    pub img_src: Option<String>,
    /// The publisher raster's authored width (its `<img>` CSS width) — an `em` value
    /// is the reliable text-relative size the reader uses to scale the equation to the
    /// prose. `Auto` for native `<math>` / LaTeX (delryn renders those at its own em).
    pub width: delryn_model::ImageWidth,
}

/// Recover everything renderable from a math node (native `<math>` or math image),
/// or the first math descendant of a container. Prefers a LaTeX source (for a crisp
/// RaTeX render); keeps the image `src` only as a fallback when there's no LaTeX.
pub(super) fn recover_math_block(node: NodeRef<Node>) -> MathBlock {
    if let Some(e) = node.value().as_element() {
        if is_math_element(e) {
            let (unicode, latex) = native_math(node);
            return MathBlock {
                unicode,
                latex,
                img_src: None,
                width: delryn_model::ImageWidth::Auto,
            };
        }
        if matches!(e.name(), "img" | "image") {
            // Source: LaTeX/MathML in the `alt`, else a trailing comment (Wiley).
            let raw = e
                .attr("alt")
                .filter(|a| delryn_model::math::is_math(a))
                .map(str::to_string)
                .or_else(|| img_math_comment(node));
            let latex = raw
                .as_deref()
                .filter(|s| !delryn_model::math::is_mathml(s))
                .map(delryn_model::math::strip_delimiters)
                .filter(|l| !l.trim().is_empty());
            return MathBlock {
                unicode: raw.as_deref().map(math_unicode).unwrap_or_default(),
                latex,
                img_src: img_src(e).filter(|s| !s.is_empty()),
                width: super::parse_img_width(e.attr("width"), e.attr("style")),
            };
        }
    }
    // A wrapper element: recover from the first math node inside it.
    for d in node.descendants() {
        if is_math_node(d) {
            return recover_math_block(d);
        }
    }
    MathBlock {
        unicode: String::new(),
        latex: None,
        img_src: None,
        width: delryn_model::ImageWidth::Auto,
    }
}

/// A publisher equation shipped as a *plain* `<img>` — no math `alt`/comment, so
/// [`is_math_img`] misses it — but wrapped in a **display-equation container**
/// (`class="Equation"`/`EquationContent`/`disp-formula`…). The container class is the
/// math signal, so the raster is recovered as the equation (carrying its `em` width).
///
/// Fires only when `node` **itself** is a display-equation container ([`math_class_signal`]
/// `== Some(true)`) holding exactly one non-icon image. Both guards matter: the class
/// gate keeps prose containers (`class="chapter"`, a bare `<p>`) — which `classify`
/// also visits on the way down — from being mistaken for an equation and swallowing
/// their text; the single-image count keeps a broad `class="equations"` section (many
/// equations/paragraphs) from collapsing into one, so it recurses to the individual
/// equation containers instead. `None` otherwise.
pub(super) fn equation_image_node(node: NodeRef<Node>) -> Option<NodeRef<Node>> {
    if math_class_signal(node) != Some(true) {
        return None;
    }
    let mut imgs = node.descendants().filter(|d| {
        d.value().as_element().is_some_and(|e| {
            matches!(e.name(), "img" | "image") && !is_icon_src(&img_src(e).unwrap_or_default())
        })
    });
    let first = imgs.next()?;
    imgs.next().is_none().then_some(first) // exactly one image
}

/// The raw math source (delimiters intact) for the display/inline signal — the
/// `alttext`/annotation LaTeX of a `<math>`, or an image's `alt`.
fn math_source_raw(node: NodeRef<Node>) -> String {
    let Some(e) = node.value().as_element() else {
        return String::new();
    };
    if is_math_element(e) {
        if let Some(alt) = e.attr("alttext").filter(|a| !a.trim().is_empty()) {
            return alt.to_string();
        }
        return annotation_tex(node).unwrap_or_default();
    }
    // An image: its `alt` if that's the source, else a trailing comment (Wiley).
    e.attr("alt")
        .filter(|a| delryn_model::math::is_math(a))
        .map(str::to_string)
        .or_else(|| img_math_comment(node))
        .unwrap_or_default()
}

/// `\begin{…}` environments that always typeset as *display* math.
const DISPLAY_ENVS: &[&str] = &[
    "equation",
    "align",
    "aligned",
    "gather",
    "gathered",
    "multline",
    "eqnarray",
    "split",
    "alignat",
    "flalign",
    "displaymath",
    "cases",
    "array",
    "matrix",
    "pmatrix",
    "bmatrix",
    "vmatrix",
    "Vmatrix",
    "smallmatrix",
];

/// Whether a math node is **display** (block) vs inline, from the strongest
/// available signal — publisher-agnostic, covering the surveyed encodings:
///
/// 1. Explicit `display` attribute — MathML `block`/`inline`, MathJax
///    `mjx-container` `true`/`false`.
/// 2. MathML `displaystyle="true"` (Springer display equations carry no `display`).
/// 3. The nearest math container **class** (self or a tight wrapper): `*inline*`
///    (Springer `InlineEquation`, Pandoc `math inline`, JATS `inline-formula`) →
///    inline; an `equation`/`formula`/`disp`/`display` container → display.
/// 4. The recovered source's delimiter/environment: `\[`, `\displaystyle`,
///    `\begin{<display-env>}` → display; `\(` or a lone `$…$` → inline.
/// 5. Structural: a math node that is the sole content of a block element stands
///    alone → display.
///
/// Defaults to inline (bare math mid-text) when nothing else decides.
pub(super) fn math_is_display(node: NodeRef<Node>) -> bool {
    if let Some(e) = node.value().as_element() {
        if let Some(d) = e.attr("display") {
            if d.eq_ignore_ascii_case("block") || d.eq_ignore_ascii_case("true") {
                return true;
            }
            if d.eq_ignore_ascii_case("inline") || d.eq_ignore_ascii_case("false") {
                return false;
            }
        }
        if e.attr("displaystyle")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        {
            return true;
        }
    }
    if let Some(sig) = math_class_signal(node) {
        return sig;
    }
    if let Some(sig) = source_display_delim(&math_source_raw(node)) {
        return sig;
    }
    math_structural_display(node)
}

/// The display/inline signal from a container class, if it carries one: `inline`
/// wins (so `math inline` isn't read as display), then an equation/formula/display
/// container. `None` for a class with no math-layout signal.
fn class_display_signal(class: &str) -> Option<bool> {
    if class_contains_any(class, INLINE_MATH_CLASS_KEYWORDS) {
        return Some(false);
    }
    if class_contains_any(class, DISPLAY_MATH_CLASS_KEYWORDS) {
        return Some(true);
    }
    None
}

/// Walk the math node and its *tight* wrapper ancestors (each the sole element
/// child of its parent, bounded) for the first class carrying a display/inline
/// signal. The tight-wrapper rule stops a broad section class (`class="equations"`
/// around many paragraphs) from capturing genuinely inline math.
fn math_class_signal(node: NodeRef<Node>) -> Option<bool> {
    let mut cur = node;
    for _ in 0..5 {
        if let Some(e) = cur.value().as_element()
            && let Some(class) = e.attr("class")
            && let Some(sig) = class_display_signal(class)
        {
            return Some(sig);
        }
        let Some(parent) = cur.parent() else { break };
        let elem_children = parent.children().filter(|c| c.value().is_element()).count();
        if elem_children != 1 {
            break; // not a tight wrapper — a broad ancestor can't decide
        }
        cur = parent;
    }
    None
}

/// The display/inline signal from a math source's delimiters/environment. `$$…$$`
/// is deliberately *no* signal — some publishers (Springer) wrap inline math in
/// `$$` too, so it's resolved by class/attr/structure instead.
fn source_display_delim(raw: &str) -> Option<bool> {
    let s = raw.trim();
    if s.starts_with("\\[") || s.contains("\\displaystyle") {
        return Some(true);
    }
    if DISPLAY_ENVS
        .iter()
        .any(|env| s.contains(&format!("\\begin{{{env}")))
    {
        return Some(true);
    }
    if s.starts_with("\\(") || (s.starts_with('$') && !s.starts_with("$$")) {
        return Some(false);
    }
    None
}

/// If a block-level element's content is a *single* standalone display equation,
/// the math node inside it (native `<math>` or a math image). This is how most
/// publishers ship display math: the equation node sits inside a `<p>`/`<div>`
/// container, so it's found here (at the container) rather than as a direct block
/// child. Returns the math node when it classifies as display *and* either it's the
/// container's sole content or the container carries an equation class (so an
/// equation-number sibling doesn't disqualify it); `None` for prose containing
/// inline math, and `None` when the container holds *several* equations (letting
/// `classify` recurse so each equation is emitted as its own block — collapsing to
/// the first would silently drop the rest).
pub(super) fn standalone_math_node(node: NodeRef<Node>) -> Option<NodeRef<Node>> {
    let mut math: Option<NodeRef<Node>> = None;
    let mut other_text = false;
    for d in node.descendants() {
        if is_math_node(d) {
            if math.is_some() {
                // A second equation in the same container: don't collapse to the
                // first (which would drop the rest). Recurse into the container so
                // each equation is classified — and emitted — on its own.
                return None;
            }
            math = Some(d);
            continue;
        }
        // Text *inside* the math node is its content, not sibling prose. Skip the
        // ancestor walk once we've already seen prose — one non-math text node is
        // enough to set the flag, so large prose containers aren't rescanned.
        if !other_text
            && let Node::Text(t) = d.value()
            && !t.text.trim().is_empty()
            && !d.ancestors().any(is_math_node)
        {
            other_text = true;
        }
    }
    let m = math?;
    (math_is_display(m) && (!other_text || math_class_signal(m).is_some())).then_some(m)
}

/// Whether the math node is the sole non-whitespace content of a **block-level**
/// parent — a standalone equation (display). An inline wrapper (`<span>`, `<a>`)
/// never qualifies, so math mid-sentence stays inline.
fn math_structural_display(node: NodeRef<Node>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let is_block_parent = matches!(parent.value(), Node::Element(e) if matches!(
        e.name(),
        "p" | "div" | "section" | "li" | "td" | "th" | "blockquote" | "figure" | "center"
    ));
    if !is_block_parent {
        return false;
    }
    !parent.children().any(|c| match c.value() {
        Node::Text(t) => !t.text.trim().is_empty(),
        // Only a sibling that carries real content disqualifies the equation from
        // being the block's sole content. An *empty* marker element — InDesign's
        // `<st>` story-position anchors, an empty `<a id="…">` link target, a bare
        // `<span></span>` — must not, or a lone display equation shipped as
        // `<p><img …/><st></st></p>` (Packt / InDesign MathTools) reads as inline.
        Node::Element(e) => {
            c.id() != node.id() && !matches!(e.name(), "br") && sibling_has_content(c)
        }
        _ => false,
    })
}

/// Whether a sibling element carries content that should keep a math node from
/// counting as the *sole* content of its block: any non-whitespace text, or an
/// embedded image / media element. Empty position markers (`<st>`, an empty
/// `<a id>` / `<span>`) carry none, so they don't disqualify the equation.
fn sibling_has_content(node: NodeRef<Node>) -> bool {
    node.descendants().any(|d| match d.value() {
        Node::Text(t) => !t.text.trim().is_empty(),
        Node::Element(e) => matches!(e.name(), "img" | "image" | "svg"),
        _ => false,
    })
}

/// Convert a native `<math>` element to `(Unicode, Option<LaTeX source>)`. Prefers
/// the authored text equivalents (`alttext` / `<annotation encoding="…tex">`, which
/// carry LaTeX and aren't otherwise rendered) — those also hand back the raw LaTeX
/// for the graphical renderer — then falls back to walking the presentation MathML
/// (Unicode only, no LaTeX to recover).
pub(super) fn native_math(node: NodeRef<Node>) -> (String, Option<String>) {
    let Some(e) = node.value().as_element() else {
        return (String::new(), None);
    };
    // 1. alttext attribute (usually LaTeX, e.g. from LaTeXML / Springer).
    if let Some(alt) = e.attr("alttext")
        && !alt.trim().is_empty()
    {
        return (
            delryn_model::math::latex_to_unicode(alt),
            Some(delryn_model::math::strip_delimiters(alt)),
        );
    }
    // 2. <annotation encoding="application/x-tex"> embedded LaTeX.
    if let Some(tex) = annotation_tex(node) {
        let unicode = delryn_model::math::latex_to_unicode(&tex);
        return (unicode, Some(delryn_model::math::strip_delimiters(&tex)));
    }
    // 3. Presentation MathML only (no LaTeX equivalent): serialise the subtree, then
    //    both transcode to Unicode (the fallback) *and* synthesise LaTeX from the tree
    //    so the equation can still render graphically (RaTeX) instead of as lossy
    //    Unicode — a render failure downstream falls back to that Unicode.
    let Some(el) = scraper::ElementRef::wrap(node) else {
        return (String::new(), None);
    };
    let html = el.html();
    (
        crate::mathml::to_unicode(&html),
        crate::mathml::to_latex(&html),
    )
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
