//! Best-effort MathML → LaTeX-ish transcoding, so [`delryn_model::math::
//! latex_to_unicode`] can finish the job (scripts, fractions, symbols).
//!
//! EPUBs — especially those converted from OOXML/DOCX — ship math as MathML
//! serialised into an `<img alt="…">`. Rather than render it as raw tags, we walk
//! the presentation elements and emit the same `^`/`_`/`\frac` syntax the LaTeX
//! path already understands. This won't typeset matrices, but it makes sums,
//! products, fractions, roots, and scripted symbols readable in a terminal.

use ego_tree::NodeRef;
use scraper::{Html, Node};

/// Transcode a MathML string to LaTeX-ish text (feed the result through
/// `latex_to_unicode` to get Unicode).
pub fn to_latex(src: &str) -> String {
    let frag = Html::parse_fragment(src);
    let mut out = String::new();
    // `parse_fragment` wraps the content under an <html> root; walking its
    // children descends through any <body>/<math> wrappers into the content.
    for child in frag.root_element().children() {
        emit(child, &mut out);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Element name without any namespace prefix (`mml:msup` → `msup`).
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Element children only (skips text/whitespace between MathML elements).
fn elem_children(node: NodeRef<Node>) -> Vec<NodeRef<Node>> {
    node.children()
        .filter(|c| matches!(c.value(), Node::Element(_)))
        .collect()
}

/// LaTeX-ish for a single node (used for script/fraction arguments).
fn part(node: NodeRef<Node>) -> String {
    let mut s = String::new();
    emit(node, &mut s);
    s.trim().to_string()
}

/// Concatenated text of a token element (`mi`/`mn`/`mo`/`mtext`).
fn token_text(node: NodeRef<Node>) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        if let Node::Text(t) = d.value() {
            s.push_str(&t.text);
        }
    }
    s.trim().to_string()
}

fn emit(node: NodeRef<Node>, out: &mut String) {
    match node.value() {
        Node::Text(t) => {
            let t = t.text.trim();
            if !t.is_empty() {
                out.push_str(t);
                out.push(' ');
            }
        }
        Node::Element(e) => {
            let kids = elem_children(node);
            match local(e.name()) {
                "mi" | "mn" | "mo" | "mtext" | "ms" => {
                    let t = token_text(node);
                    if !t.is_empty() {
                        out.push_str(&t);
                        out.push(' ');
                    }
                }
                "msubsup" | "munderover" if kids.len() >= 3 => {
                    out.push_str(&part(kids[0]));
                    out.push_str(&format!("_{{{}}}^{{{}}} ", part(kids[1]), part(kids[2])));
                }
                "msub" | "munder" if kids.len() >= 2 => {
                    out.push_str(&part(kids[0]));
                    out.push_str(&format!("_{{{}}} ", part(kids[1])));
                }
                "msup" | "mover" if kids.len() >= 2 => {
                    out.push_str(&part(kids[0]));
                    out.push_str(&format!("^{{{}}} ", part(kids[1])));
                }
                "mfrac" if kids.len() >= 2 => {
                    out.push_str(&format!(
                        "\\frac{{{}}}{{{}}} ",
                        part(kids[0]),
                        part(kids[1])
                    ));
                }
                "msqrt" => {
                    let mut inner = String::new();
                    for c in node.children() {
                        emit(c, &mut inner);
                    }
                    out.push_str(&format!("√({}) ", inner.trim()));
                }
                "mroot" if kids.len() >= 2 => {
                    out.push_str(&format!("√({}) ", part(kids[0])));
                }
                "mfenced" => {
                    let open = e.attr("open").unwrap_or("(");
                    let close = e.attr("close").unwrap_or(")");
                    let sep = e.attr("separators").unwrap_or(",");
                    let inner: Vec<String> = kids.iter().map(|&c| part(c)).collect();
                    let sep = sep.chars().next().map(String::from).unwrap_or_default();
                    out.push_str(open);
                    out.push_str(&inner.join(&format!("{sep} ")));
                    out.push_str(close);
                    out.push(' ');
                }
                "mspace" => out.push(' '),
                // Use the presentation child; skip the (LaTeX/MathML) annotation.
                "semantics" => {
                    if let Some(&first) = kids.first() {
                        emit(first, out);
                    }
                }
                "annotation" => {}
                // mrow / math / mstyle / mpadded / menclose / unknown → recurse.
                _ => {
                    for c in node.children() {
                        emit(c, out);
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use delryn_model::math::latex_to_unicode;

    /// End-to-end: MathML → LaTeX-ish → Unicode.
    fn render(src: &str) -> String {
        latex_to_unicode(&to_latex(src))
    }

    #[test]
    fn sum_with_limits_and_script() {
        // ∑_{i=1}^{n} i²  (munderover on ∑, msup on i).
        let src = r#"<math xmlns="http://www.w3.org/1998/Math/MathML"><munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>n</mi></munderover><msup><mi>i</mi><mn>2</mn></msup></math>"#;
        let out = render(src);
        assert!(out.contains('∑'), "operator kept: {out:?}");
        // The upper limit `n` becomes a superscript.
        assert!(
            out.contains('ⁿ') || out.contains('n'),
            "upper limit: {out:?}"
        );
        assert!(out.contains('²'), "superscript 2 → ²: {out:?}");
        assert!(!out.contains("<m"), "no raw tags: {out:?}");
    }

    #[test]
    fn prefixed_mml_namespace_is_handled() {
        let src = r#"<mml:math xmlns:mml="http://www.w3.org/1998/Math/MathML"><mml:mi mathvariant="normal">Σ</mml:mi></mml:math>"#;
        let out = render(src);
        assert_eq!(out.trim(), "Σ");
    }

    #[test]
    fn fraction_renders() {
        let src = r#"<math><mfrac><mn>1</mn><mn>2</mn></mfrac></math>"#;
        let out = render(src);
        // latex_to_unicode turns \frac{1}{2} into a slash form (or ½); either way
        // the digits survive and there are no tags.
        assert!(out.contains('1') && out.contains('2'), "{out:?}");
        assert!(!out.contains("frac{"), "frac consumed: {out:?}");
    }
}
