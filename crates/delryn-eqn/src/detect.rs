//! Occurrence detection + source recovery: find each math site in parsed content and
//! harvest **every** source it carries — authored LaTeX, Presentation/Content MathML
//! (native, or hidden in a `<div hidden>` / `<switch>` / trailing comment / MathJax
//! `<mjx-assistive-mml>`), a publisher picture, and a Unicode floor — into one
//! [`MathItem`]. Encoding-agnostic and one-pass; the render ladder consumes the result.

use ego_tree::NodeRef;
use scraper::{ElementRef, Node};

use crate::ir::{MarkupSource, MathItem, PictureRef, PictureSize};

/// Detect and recover a math occurrence rooted at `node`, or `None` if it isn't math.
/// Harvests the best of every encoding present and assembles a [`MathItem`] whose render
/// sources degrade gracefully (typeset → picture → text), so the equation can never be lost.
pub fn detect(node: NodeRef<Node>) -> Option<MathItem> {
    let el = node.value().as_element()?;
    let name = local(el.name());
    let is_math = match name {
        "math" => true,
        "img" | "image" => is_math_img(node, el),
        _ => is_mjx_container(el) || has_role_math(el),
    };
    if !is_math {
        return None;
    }

    let display = detect_display(el);
    let authored_latex = harvest_authored_latex(node, el);
    let mathml = harvest_mathml(node);
    let (presentation, content) = match mathml {
        Some(s) if is_content_mathml(&s) => (None, Some(s)),
        Some(s) => (Some(s), None),
        None => (None, None),
    };
    let picture = harvest_picture(node, el);
    let text = harvest_unicode(node, el);

    // Best typeset source by fidelity: authored LaTeX > Presentation MathML > Content MathML.
    let typeset = authored_latex
        .map(MarkupSource::Latex)
        .or_else(|| presentation.map(MarkupSource::PresentationMathml))
        .or_else(|| content.map(MarkupSource::ContentMathml));

    // Nothing renderable at all → not worth an item (lets a bare non-math image slip past).
    if typeset.is_none() && picture.is_none() && text.trim().is_empty() {
        return None;
    }
    Some(MathItem {
        display,
        typeset,
        picture,
        text,
    })
}

/// Element name without any namespace prefix (`m:math` / `mml:math` → `math`).
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Whether an `<img>`/`<image>` carries math (vs being a figure): a math-shaped `alt`, a
/// math class on it or an ancestor, or a trailing MathML/LaTeX comment.
fn is_math_img(node: NodeRef<Node>, el: &scraper::node::Element) -> bool {
    el.attr("alt").is_some_and(looks_like_math)
        || has_math_class(node)
        || trailing_math_comment(node).is_some()
}

/// A MathJax container: the `<mjx-container>` element or anything classed `mjx-*`.
fn is_mjx_container(el: &scraper::node::Element) -> bool {
    local(el.name()) == "mjx-container"
        || el
            .attr("class")
            .is_some_and(|c| c.split_whitespace().any(|t| t.starts_with("mjx-")))
}

fn has_role_math(el: &scraper::node::Element) -> bool {
    el.attr("role") == Some("math")
}

/// Whether a string looks like a machine-readable math source (LaTeX delimiters/commands
/// or embedded MathML) rather than a prose description.
fn looks_like_math(s: &str) -> bool {
    let t = s.trim();
    t.contains("<math")
        || t.contains(":math")
        || t.starts_with('$')
        || t.contains("\\(")
        || t.contains("\\[")
        || t.contains("\\begin")
        || (t.starts_with('\\') && t.len() > 1)
}

/// Whether `node` or an ancestor is classed as math (`math`, `equation`, `mjx-*`, …).
fn has_math_class(node: NodeRef<Node>) -> bool {
    node.ancestors()
        .chain(std::iter::once(node))
        .filter_map(|n| n.value().as_element())
        .any(|e| {
            e.attr("class").is_some_and(|c| {
                let c = c.to_ascii_lowercase();
                c.contains("math") || c.contains("equation") || c.contains("mjx-")
            })
        })
}

/// Display (block) vs inline, from the explicit signals only (structural inference is a
/// higher-level concern): a MathML/MathJax `display` attribute, or a `displaystyle`.
fn detect_display(el: &scraper::node::Element) -> bool {
    match el.attr("display") {
        Some("block") | Some("true") => return true,
        Some("inline") | Some("false") => return false,
        _ => {}
    }
    el.attr("displaystyle") == Some("true")
}

/// Authored LaTeX for this occurrence: a non-empty `alttext`, a TeX `<annotation>`, or a
/// math image's LaTeX `alt`. Delimiters are stripped so the renderer gets bare source.
fn harvest_authored_latex(node: NodeRef<Node>, el: &scraper::node::Element) -> Option<String> {
    if let Some(alt) = el.attr("alttext").map(str::trim).filter(|a| !a.is_empty()) {
        return Some(strip_delimiters(alt));
    }
    if let Some(tex) = annotation_tex(node) {
        return Some(strip_delimiters(&tex));
    }
    // A math image whose `alt` is LaTeX (not MathML).
    if matches!(local(el.name()), "img" | "image")
        && let Some(alt) = el.attr("alt").map(str::trim).filter(|a| looks_like_math(a))
        && !alt.contains("<math")
        && !alt.contains(":math")
    {
        return Some(strip_delimiters(alt));
    }
    None
}

/// The text of a TeX `<annotation encoding="application/x-tex|application/x-latex">`.
fn annotation_tex(node: NodeRef<Node>) -> Option<String> {
    for d in node.descendants() {
        if let Some(e) = d.value().as_element()
            && local(e.name()) == "annotation"
            && e.attr("encoding")
                .is_some_and(|enc| enc.contains("x-tex") || enc.contains("x-latex"))
        {
            let t = descendant_text(d);
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
    }
    None
}

/// Serialized Presentation/Content MathML for this occurrence: the node itself if it is a
/// `<math>`, else a `<math>` harvested from its subtree (a hidden div, a `<switch>` case,
/// or a MathJax `<mjx-assistive-mml>`), else a trailing MathML comment. This is the
/// harvesting that turns "just spans" / "just an image" books into crisp type.
fn harvest_mathml(node: NodeRef<Node>) -> Option<String> {
    if let Some(e) = node.value().as_element()
        && local(e.name()) == "math"
    {
        return serialize(node);
    }
    // A <math> anywhere in the subtree (assistive-mml, hidden div, switch branch).
    for d in node.descendants() {
        if d.value()
            .as_element()
            .is_some_and(|e| local(e.name()) == "math")
        {
            return serialize(d);
        }
    }
    // A <math> in a following sibling's subtree (image + adjacent hidden MathML).
    for sib in node.next_siblings().take(3) {
        if let Some(m) = sib.descendants().find(|d| {
            d.value()
                .as_element()
                .is_some_and(|e| local(e.name()) == "math")
        }) {
            return serialize(m);
        }
    }
    // A trailing HTML comment carrying MathML (Wiley / For-Dummies).
    trailing_math_comment(node)
}

/// The MathML/LaTeX source in the nearest trailing HTML comment (skipping whitespace),
/// or `None` if the next real sibling isn't such a comment.
fn trailing_math_comment(node: NodeRef<Node>) -> Option<String> {
    let mut sib = node.next_sibling();
    while let Some(n) = sib {
        match n.value() {
            Node::Comment(c) => {
                let s = c.trim();
                return (s.contains("<math") || s.contains(":math")).then(|| s.to_string());
            }
            Node::Text(t) if t.text.trim().is_empty() => sib = n.next_sibling(),
            _ => return None,
        }
    }
    None
}

/// The publisher's picture for this occurrence, plus a text-relative size hint: a math
/// image's `src`, or a `<math altimg>`, or an image inside a `<switch>` fallback.
fn harvest_picture(node: NodeRef<Node>, el: &scraper::node::Element) -> Option<PictureRef> {
    // `<math altimg="…">` — the MathML fallback image.
    if local(el.name()) == "math" {
        return el
            .attr("altimg")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|src| PictureRef {
                src: src.to_string(),
                size: css_math_size(el),
            });
    }
    // A math `<img>`/`<image>`.
    if matches!(local(el.name()), "img" | "image") {
        let src = el
            .attr("src")
            .or_else(|| el.attr("xlink:href"))
            .or_else(|| el.attr("href"))
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        return Some(PictureRef {
            src: src.to_string(),
            size: css_math_size(el),
        });
    }
    // A container (mjx / switch): the first image in its subtree.
    for d in node.descendants() {
        if let Some(e) = d.value().as_element()
            && matches!(local(e.name()), "img" | "image")
            && let Some(src) = e
                .attr("src")
                .or_else(|| e.attr("xlink:href"))
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            return Some(PictureRef {
                src: src.to_string(),
                size: css_math_size(e),
            });
        }
    }
    None
}

/// A text-relative picture size from an element's CSS `width` (a `width` attribute or a
/// `width:` in its inline `style`) in `em`/`ex`; otherwise measure the ink at render time.
fn css_math_size(el: &scraper::node::Element) -> PictureSize {
    if let Some(w) = el.attr("width").and_then(parse_em_ex) {
        return w;
    }
    if let Some(style) = el.attr("style") {
        for decl in style.split(';') {
            if let Some((prop, val)) = decl.split_once(':')
                && prop.trim().eq_ignore_ascii_case("width")
                && let Some(sz) = parse_em_ex(val)
            {
                return sz;
            }
        }
    }
    PictureSize::MeasureInk
}

/// Parse a CSS length as an `em`/`ex` picture size (`"2.4em"` → `Em(2.4)`); `None` for px/%
/// or unparseable values (px/% aren't text-relative, so we measure the ink instead).
fn parse_em_ex(v: &str) -> Option<PictureSize> {
    let v = v.trim();
    if let Some(n) = v.strip_suffix("em") {
        return n.trim().parse::<f32>().ok().map(PictureSize::Em);
    }
    if let Some(n) = v.strip_suffix("ex") {
        return n.trim().parse::<f32>().ok().map(PictureSize::Ex);
    }
    None
}

/// A best-effort Unicode floor for this occurrence: the math image's plain-text `alt`, or
/// the concatenated glyph text of the markup. Always something; refined by the render layer.
fn harvest_unicode(node: NodeRef<Node>, el: &scraper::node::Element) -> String {
    if matches!(local(el.name()), "img" | "image")
        && let Some(alt) = el.attr("alt").map(str::trim)
        && !alt.is_empty()
        && !looks_like_math(alt)
    {
        return alt.to_string();
    }
    // The concatenated token text (mi/mo/mn glyphs), collapsing whitespace.
    let t = descendant_text(node);
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether serialized MathML is Content (semantic) rather than Presentation MathML.
fn is_content_mathml(s: &str) -> bool {
    s.contains("<apply")
        || s.contains(":apply")
        || s.contains("<ci>")
        || s.contains("<cn>")
        || s.contains("<csymbol")
}

/// Serialize a node's subtree to an HTML/XML string (`<math>…</math>`).
fn serialize(node: NodeRef<Node>) -> Option<String> {
    ElementRef::wrap(node).map(|e| e.html())
}

/// All descendant text of a node, concatenated (used for annotation text + the Unicode floor).
fn descendant_text(node: NodeRef<Node>) -> String {
    let mut out = String::new();
    for d in node.descendants() {
        if let Node::Text(t) = d.value() {
            out.push_str(&t.text);
        }
    }
    out
}

/// Strip the math delimiters publishers wrap authored LaTeX in (`$…$`, `$$…$$`, `\(…\)`,
/// `\[…\]`), leaving the bare source.
fn strip_delimiters(s: &str) -> String {
    let t = s.trim();
    let pairs = [("$$", "$$"), ("\\(", "\\)"), ("\\[", "\\]"), ("$", "$")];
    for (open, close) in pairs {
        if t.len() >= open.len() + close.len() && t.starts_with(open) && t.ends_with(close) {
            return t[open.len()..t.len() - close.len()].trim().to_string();
        }
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use scraper::{Html, Selector};

    /// Run `detect` on the first element matching `sel` in `html`.
    fn detect_first(html: &str, sel: &str) -> Option<MathItem> {
        let doc = Html::parse_fragment(html);
        let selector = Selector::parse(sel).unwrap();
        let el = doc.select(&selector).next()?;
        detect(*el)
    }

    #[test]
    fn native_presentation_mathml() {
        let item = detect_first(
            r#"<math display="block"><mfrac><mn>1</mn><mn>2</mn></mfrac></math>"#,
            "math",
        )
        .expect("math");
        assert!(item.display, "display=block");
        assert!(
            matches!(item.typeset, Some(MarkupSource::PresentationMathml(ref s)) if s.contains("mfrac")),
            "presentation MathML recovered: {:?}",
            item.typeset
        );
    }

    #[test]
    fn authored_latex_beats_mathml() {
        // A <math> with both presentation markup AND an authored TeX annotation: the
        // authored LaTeX wins (highest fidelity).
        let item = detect_first(
            r#"<math><semantics><mfrac><mn>1</mn><mn>2</mn></mfrac>
               <annotation encoding="application/x-tex">\frac{1}{2}</annotation></semantics></math>"#,
            "math",
        )
        .expect("math");
        assert_eq!(
            item.typeset,
            Some(MarkupSource::Latex("\\frac{1}{2}".to_string()))
        );
    }

    #[test]
    fn alttext_latex_is_recovered_and_stripped() {
        let item = detect_first(
            r#"<math alttext="$x^2$"><mrow><mi>x</mi></mrow></math>"#,
            "math",
        )
        .expect("math");
        assert_eq!(item.typeset, Some(MarkupSource::Latex("x^2".to_string())));
    }

    #[test]
    fn mathjax_assistive_mml_is_harvested() {
        // The visible form is spans; the recoverable MathML hides in <mjx-assistive-mml>.
        let item = detect_first(
            r#"<mjx-container class="MathJax" display="true">
                 <mjx-math><mjx-mfrac></mjx-mfrac></mjx-math>
                 <mjx-assistive-mml><math><mfrac><mn>1</mn><mn>2</mn></mfrac></math></mjx-assistive-mml>
               </mjx-container>"#,
            "mjx-container",
        )
        .expect("mjx-container is math");
        assert!(item.display, "display=true");
        assert!(
            matches!(item.typeset, Some(MarkupSource::PresentationMathml(ref s)) if s.contains("mfrac")),
            "harvested the assistive MathML: {:?}",
            item.typeset
        );
    }

    #[test]
    fn math_image_with_latex_alt_and_em_size() {
        let item = detect_first(
            r#"<p class="equation"><img alt="\int_0^1 x\,dx" src="eq3.png" style="width:4.2em"/></p>"#,
            "img",
        )
        .expect("math image");
        assert_eq!(
            item.typeset,
            Some(MarkupSource::Latex("\\int_0^1 x\\,dx".to_string()))
        );
        let pic = item
            .picture
            .expect("keeps the publisher picture as fallback");
        assert_eq!(pic.src, "eq3.png");
        assert_eq!(pic.size, PictureSize::Em(4.2), "text-relative em width");
    }

    #[test]
    fn math_image_with_trailing_comment_mathml() {
        // Wiley/For-Dummies: bare alt, real MathML in a trailing comment, picture as src.
        let item = detect_first(
            r#"<p class="math"><img alt="math" src="eq7.gif"/><!--<math><msqrt><mi>x</mi></msqrt></math>--></p>"#,
            "img",
        )
        .expect("math image");
        assert!(
            matches!(item.typeset, Some(MarkupSource::PresentationMathml(ref s)) if s.contains("msqrt")),
            "recovered MathML from the comment: {:?}",
            item.typeset
        );
        assert_eq!(item.picture.map(|p| p.src), Some("eq7.gif".to_string()));
    }

    #[test]
    fn mathml_with_altimg_keeps_both() {
        let item = detect_first(
            r#"<math altimg="eq.png" style="width:3em"><msup><mi>e</mi><mi>x</mi></msup></math>"#,
            "math",
        )
        .expect("math");
        assert!(matches!(
            item.typeset,
            Some(MarkupSource::PresentationMathml(_))
        ));
        let pic = item.picture.expect("altimg picture kept as fallback");
        assert_eq!(pic.src, "eq.png");
        assert_eq!(pic.size, PictureSize::Em(3.0));
    }

    #[test]
    fn content_mathml_is_classified() {
        let item = detect_first(
            r#"<math><apply><plus/><ci>x</ci><cn>1</cn></apply></math>"#,
            "math",
        )
        .expect("math");
        assert!(
            matches!(item.typeset, Some(MarkupSource::ContentMathml(_))),
            "content MathML classified separately: {:?}",
            item.typeset
        );
    }

    #[test]
    fn picture_only_still_recovers() {
        // No markup at all — just a publisher raster with an em width. Still an item
        // (the render ladder shows the picture); never dropped.
        let item = detect_first(
            r#"<span class="equation"><img alt="figure 3" src="eq.png" width="5em"/></span>"#,
            "img",
        )
        .expect("picture-only math (has math class)");
        assert!(item.typeset.is_none(), "no markup to typeset");
        assert_eq!(item.picture.map(|p| p.size), Some(PictureSize::Em(5.0)));
    }

    #[test]
    fn non_math_image_is_ignored() {
        assert!(
            detect_first(
                r#"<figure><img alt="a photo of a cat" src="cat.jpg"/></figure>"#,
                "img"
            )
            .is_none(),
            "a plain figure is not math"
        );
    }

    #[test]
    fn unicode_floor_is_always_present() {
        let item = detect_first(r#"<math><mi>x</mi><mo>+</mo><mn>1</mn></math>"#, "math").unwrap();
        assert!(
            !item.text.trim().is_empty(),
            "a Unicode floor exists: {:?}",
            item.text
        );
    }
}
