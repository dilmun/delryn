//! Best-effort MathML → Unicode: the render ladder's **text floor**. When neither a crisp
//! typeset nor a publisher picture is available (or both fail), an equation must still read
//! as *something* — so we walk the presentation MathML and transcribe it to Unicode
//! (super/subscripts, fractions, roots, fenced groups): `∑_{i=1}^{N} i²` → `∑ᵢ₌₁ᴺ i²`.
//!
//! This is the never-blank guarantee's last rung, so it errs toward readable-approximation
//! over exactness (a matrix flattens; a fraction becomes `num/den`). Authored LaTeX is
//! transcribed by [`delryn_model::math::latex_to_unicode`] instead; this handles the
//! MathML-only occurrences (native `<math>`, harvested assistive/hidden MathML).

use delryn_model::math::{subscript_str, superscript_str};
use ego_tree::NodeRef;
use scraper::{Html, Node};

/// Maximum MathML nesting depth walked before the transcoder stops recursing and emits the
/// remaining subtree as flat text — guards against a stack overflow on pathologically nested
/// `<mrow>` / script markup. Real math nests shallowly.
const MAX_MATH_DEPTH: u16 = 128;

/// Transcribe a MathML string to a Unicode approximation.
pub fn to_unicode(src: &str) -> String {
    let frag = Html::parse_fragment(src);
    let mut out = String::new();
    // `parse_fragment` wraps the content under an <html> root; walking its children descends
    // through any <body>/<math> wrappers into the content.
    for child in frag.root_element().children() {
        emit(child, 0, &mut out);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Element name without any namespace prefix (`mml:msup` → `msup`).
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn elem_children(node: NodeRef<Node>) -> Vec<NodeRef<Node>> {
    node.children()
        .filter(|c| matches!(c.value(), Node::Element(_)))
        .collect()
}

/// Rendered (whitespace-collapsed) Unicode for one node — used for script/fraction arguments.
fn part(node: NodeRef<Node>, depth: u16) -> String {
    let mut s = String::new();
    emit(node, depth, &mut s);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn part_str(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The trimmed text content of a token element (`<mi>`/`<mn>`/`<mo>`/…).
fn token_text(node: NodeRef<Node>) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        if let Node::Text(t) = d.value() {
            s.push_str(&t.text);
        }
    }
    s.trim().to_string()
}

/// Render `arg` as a subscript — Unicode where every char maps, else a clean parenthesised
/// fallback (`_(i+1)`), never raw braces.
fn as_sub(arg: &str) -> String {
    fallback_script(arg, true)
}
fn as_sup(arg: &str) -> String {
    fallback_script(arg, false)
}
fn fallback_script(arg: &str, sub: bool) -> String {
    let tight: String = arg.split_whitespace().collect();
    let mapped = if sub {
        subscript_str(&tight)
    } else {
        superscript_str(&tight)
    };
    match mapped {
        Some(u) => u,
        None if tight.chars().count() <= 1 => format!("{}{tight}", if sub { '_' } else { '^' }),
        None => format!("{}({tight})", if sub { '_' } else { '^' }),
    }
}

/// Operators that read better with surrounding spaces.
fn is_spaced_op(op: &str) -> bool {
    matches!(
        op,
        "=" | "≠"
            | "≈"
            | "≡"
            | "≅"
            | "<"
            | ">"
            | "≤"
            | "≥"
            | "+"
            | "-"
            | "−"
            | "±"
            | "∓"
            | "×"
            | "⋅"
            | "·"
            | "∗"
            | "*"
            | "/"
            | "→"
            | "↦"
            | "⇒"
            | "⇔"
            | "∈"
            | "∉"
            | "⊂"
            | "⊆"
            | "⊃"
            | "⊇"
            | "∪"
            | "∩"
            | "∧"
            | "∨"
            | "∝"
            | "∼"
    )
}

/// A fraction: `num/den`, parenthesising a side that is more than one token.
fn frac(num: &str, den: &str) -> String {
    let wrap = |s: &str| {
        if s.chars().count() > 1 && s.chars().any(|c| !c.is_alphanumeric()) {
            format!("({s})")
        } else {
            s.to_string()
        }
    };
    format!("{}/{}", wrap(num), wrap(den))
}

fn emit(node: NodeRef<Node>, depth: u16, out: &mut String) {
    // Pathological nesting: stop recursing and append the subtree's flat text so the stack
    // can't overflow.
    if depth >= MAX_MATH_DEPTH {
        let t = token_text(node);
        if !t.is_empty() {
            out.push_str(&t);
            out.push(' ');
        }
        return;
    }
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
                "mi" | "mn" | "mtext" | "ms" => {
                    out.push_str(&token_text(node));
                }
                "mo" => {
                    let op = token_text(node);
                    if op.is_empty() {
                    } else if is_spaced_op(&op) {
                        out.push(' ');
                        out.push_str(&op);
                        out.push(' ');
                    } else if op == "," || op == ";" {
                        out.push_str(&op);
                        out.push(' ');
                    } else {
                        out.push_str(&op);
                    }
                }
                "msubsup" | "munderover" if kids.len() >= 3 => {
                    out.push_str(&part(kids[0], depth + 1));
                    out.push_str(&as_sub(&part(kids[1], depth + 1)));
                    out.push_str(&as_sup(&part(kids[2], depth + 1)));
                    out.push(' ');
                }
                "msub" | "munder" if kids.len() >= 2 => {
                    out.push_str(&part(kids[0], depth + 1));
                    out.push_str(&as_sub(&part(kids[1], depth + 1)));
                    out.push(' ');
                }
                "msup" | "mover" if kids.len() >= 2 => {
                    out.push_str(&part(kids[0], depth + 1));
                    out.push_str(&as_sup(&part(kids[1], depth + 1)));
                    out.push(' ');
                }
                "mfrac" if kids.len() >= 2 => {
                    out.push_str(&frac(&part(kids[0], depth + 1), &part(kids[1], depth + 1)));
                    out.push(' ');
                }
                "msqrt" => {
                    let mut inner = String::new();
                    for c in node.children() {
                        emit(c, depth + 1, &mut inner);
                    }
                    out.push_str(&format!("√({}) ", part_str(&inner)));
                }
                "mroot" if kids.len() >= 2 => {
                    out.push_str(&format!("√({}) ", part(kids[0], depth + 1)));
                }
                "mfenced" => {
                    let open = e.attr("open").unwrap_or("(");
                    let close = e.attr("close").unwrap_or(")");
                    let sep = e
                        .attr("separators")
                        .and_then(|s| s.chars().next())
                        .unwrap_or(',');
                    let inner: Vec<String> = kids.iter().map(|&c| part(c, depth + 1)).collect();
                    out.push_str(open);
                    out.push_str(&inner.join(&format!("{sep} ")));
                    out.push_str(close);
                    out.push(' ');
                }
                "mspace" => out.push(' '),
                "semantics" => {
                    if let Some(&first) = kids.first() {
                        emit(first, depth + 1, out);
                    }
                }
                "annotation" => {}
                // mrow / math / mstyle / mpadded / menclose / unknown → recurse.
                _ => {
                    for c in node.children() {
                        emit(c, depth + 1, out);
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

    #[test]
    fn sum_with_limits_and_scripts() {
        // ∑_{i=1}^{N} i²
        let src = r#"<math xmlns="http://www.w3.org/1998/Math/MathML"><munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>N</mi></munderover><msup><mi>i</mi><mn>2</mn></msup></math>"#;
        let out = to_unicode(src);
        assert_eq!(out, "∑ᵢ₌₁ᴺ i²", "got: {out:?}");
    }

    #[test]
    fn prefixed_namespace_and_single_symbol() {
        let src = r#"<mml:math xmlns:mml="http://www.w3.org/1998/Math/MathML"><mml:mi mathvariant="normal">Σ</mml:mi></mml:math>"#;
        assert_eq!(to_unicode(src), "Σ");
    }

    #[test]
    fn subscript_with_relation_and_no_internal_spaces() {
        // x_{100} — no spaces leak inside scripts.
        let src = r#"<math><msub><mi>x</mi><mn>100</mn></msub></math>"#;
        assert_eq!(to_unicode(src), "x₁₀₀");
    }

    #[test]
    fn unmappable_script_uses_paren_fallback_not_braces() {
        let src =
            r#"<math><msup><mi>e</mi><mrow><mi>x</mi><mo>+</mo><mn>1</mn></mrow></msup></math>"#;
        let out = to_unicode(src);
        assert_eq!(out, "eˣ⁺¹", "got: {out:?}");
        let src2 =
            r#"<math><msup><mi>a</mi><mrow><mi>b</mi><mo>,</mo><mi>c</mi></mrow></msup></math>"#;
        let out2 = to_unicode(src2);
        assert!(
            out2.starts_with("a^(") || out2.starts_with("aᵇ"),
            "got: {out2:?}"
        );
        assert!(!out2.contains('{'), "no raw braces: {out2:?}");
    }

    #[test]
    fn fraction_and_relation_spacing() {
        let src = r#"<math><mfrac><mn>1</mn><mn>2</mn></mfrac><mo>=</mo><mn>0.5</mn></math>"#;
        assert_eq!(to_unicode(src), "1/2 = 0.5");
    }
}
