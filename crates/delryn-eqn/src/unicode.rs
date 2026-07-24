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
    tidy(&out)
}

/// Collapse whitespace to single spaces and drop the stray space a token push leaves *before*
/// a closing delimiter or punctuation (`E(X₁ )` → `E(X₁)`, `2ρxy) ,` → `2ρxy),`).
fn tidy(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(collapsed.len());
    for c in collapsed.chars() {
        if matches!(c, ')' | ']' | '}' | ',' | ';' | '.') && out.ends_with(' ') {
            out.pop();
        }
        out.push(c);
    }
    out
}

/// Whether `c` is a Unicode super/subscript glyph (a digit, sign, or letter that renders
/// *attached* to its base) — so a fraction side like `x²` or `∂²` counts as one term, not two.
fn is_script_char(c: char) -> bool {
    matches!(c as u32,
        0x00B2 | 0x00B3 | 0x00B9      // ² ³ ¹
        | 0x2070..=0x209F             // super/subscript digits, signs, aeoxₐₑₒₓ …
        | 0x1D43..=0x1D6B             // superscript latin letters ᵃᵇ… and ᵢᵣᵤᵥ
        | 0x02B0..=0x02E4             // modifier letters ʰʲʳˡˢˣʷʸ
        | 0x2C7C,                     // ⱼ
    )
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
    tidy(&s)
}

fn part_str(s: &str) -> String {
    tidy(s)
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

/// Whether an under/over script is a **decoration line/accent** (an underline, bar, hat,
/// tilde, dot, or vector arrow), not a real limit — so `munder`/`mover` can drop it and keep
/// the base. A terminal can't stack a rule; the matrix (double-underline) / vector (single
/// underline or arrow) notation would otherwise leak as trailing `_`/`^` noise (`M____`).
fn is_decoration(script: &str) -> bool {
    let t = script.trim();
    if t.is_empty() {
        return false;
    }
    t.chars().all(|c| {
        matches!(
            c,
            // underscore, hat, tilde; dashes/overline/macron; combining low/over/macron/
            // double-low lines, tilde, hat, dots; vector arrows (spacing + combining).
            '_' | '^'
                | '~'
                | '-'
                | '–'
                | '—'
                | '―'
                | '‾'
                | '¯'
                | '→'
                | '\u{0332}'
                | '\u{0333}'
                | '\u{0305}'
                | '\u{0304}'
                | '\u{0331}'
                | '\u{0303}'
                | '\u{0302}'
                | '\u{0307}'
                | '\u{0308}'
                | '\u{20D7}'
                | '\u{20D6}'
        )
    })
}
fn fallback_script(arg: &str, sub: bool) -> String {
    // Unicode scripts drop spaces (`x₁₀₀`); the parenthesised fallback keeps them so a compound
    // script reads (`^(E(W) + ½Var(W))`, not `^(E(W)+½Var(W))`).
    let spaced = arg.split_whitespace().collect::<Vec<_>>().join(" ");
    let tight: String = spaced.chars().filter(|c| !c.is_whitespace()).collect();
    let mapped = if sub {
        subscript_str(&tight)
    } else {
        superscript_str(&tight)
    };
    match mapped {
        Some(u) => u,
        None if tight.chars().count() <= 1 => format!("{}{tight}", if sub { '_' } else { '^' }),
        None => format!("{}({spaced})", if sub { '_' } else { '^' }),
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

/// A fraction: `num/den`, parenthesising a side that is more than one *term* so the linear
/// slash can't be misread — `1/(2πτ)` not `1/2πτ`, but `x²/2` and `∂²/∂x` stay bare. A term is
/// a base glyph plus its attached scripts, so `x²`/`∂²`/`xᵢ` count as one.
fn frac(num: &str, den: &str) -> String {
    let wrap = |s: &str| {
        if s.chars().filter(|&c| !is_script_char(c)).count() > 1 {
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
                "msubsup" if kids.len() >= 3 => {
                    out.push_str(&part(kids[0], depth + 1));
                    out.push_str(&as_sub(&part(kids[1], depth + 1)));
                    out.push_str(&as_sup(&part(kids[2], depth + 1)));
                    out.push(' ');
                }
                // Under- **and** over-script: keep real limits (a big operator's bounds),
                // but drop a decoration line above/below (an underlined/overlined matrix or
                // vector — a terminal can't stack a rule, so show just the symbol).
                "munderover" if kids.len() >= 3 => {
                    out.push_str(&part(kids[0], depth + 1));
                    let under = part(kids[1], depth + 1);
                    if !is_decoration(&under) {
                        out.push_str(&as_sub(&under));
                    }
                    let over = part(kids[2], depth + 1);
                    if !is_decoration(&over) {
                        out.push_str(&as_sup(&over));
                    }
                    out.push(' ');
                }
                "msub" if kids.len() >= 2 => {
                    out.push_str(&part(kids[0], depth + 1));
                    out.push_str(&as_sub(&part(kids[1], depth + 1)));
                    out.push(' ');
                }
                "msup" if kids.len() >= 2 => {
                    out.push_str(&part(kids[0], depth + 1));
                    out.push_str(&as_sup(&part(kids[1], depth + 1)));
                    out.push(' ');
                }
                // `munder`/`mover` are also used for a bar/underline/vector accent — the
                // matrix (double-underline) and vector (single) notation this pass was
                // turning into trailing underscores (`M____`). Drop such a decoration and
                // keep the base; treat a real script (a limit) as a sub/superscript.
                "munder" if kids.len() >= 2 => {
                    out.push_str(&part(kids[0], depth + 1));
                    let under = part(kids[1], depth + 1);
                    if !is_decoration(&under) {
                        out.push_str(&as_sub(&under));
                    }
                    out.push(' ');
                }
                "mover" if kids.len() >= 2 => {
                    out.push_str(&part(kids[0], depth + 1));
                    let over = part(kids[1], depth + 1);
                    if !is_decoration(&over) {
                        out.push_str(&as_sup(&over));
                    }
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

    /// A matrix/vector accent — an under/over decoration line — is dropped, keeping the
    /// bare symbol instead of leaking trailing underscores (`M____`); a real limit stays.
    #[test]
    fn underline_and_vector_accents_are_dropped() {
        // Double-underlined matrix M: nested munder with underline rules.
        let m = r#"<math><munder><munder><mi>M</mi><mo>_</mo></munder><mo>_</mo></munder></math>"#;
        assert_eq!(
            to_unicode(m),
            "M",
            "double underline dropped, got: {:?}",
            to_unicode(m)
        );
        // Single-underlined / arrow vector.
        let v = r#"<math><munder><mi>v</mi><mo>_</mo></munder></math>"#;
        assert_eq!(to_unicode(v), "v");
        let a = r#"<math><mover><mi>a</mi><mo>→</mo></mover></math>"#;
        assert_eq!(to_unicode(a), "a");
        // A real limit under an operator is NOT a decoration — it stays a subscript.
        let lim = r#"<math><munder><mo>lim</mo><mrow><mi>n</mi><mo>→</mo><mn>0</mn></mrow></munder></math>"#;
        assert!(
            to_unicode(lim).starts_with("lim"),
            "got: {:?}",
            to_unicode(lim)
        );
        assert!(
            to_unicode(lim).contains('n'),
            "limit kept: {:?}",
            to_unicode(lim)
        );
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

    #[test]
    fn multi_term_fraction_side_is_parenthesised() {
        // 1/(2πτ): a bare `1/2πτ` reads as (1/2)πτ; the denominator must be grouped. A single
        // term with attached scripts (x²) stays bare.
        let src =
            r#"<math><mfrac><mn>1</mn><mrow><mn>2</mn><mi>π</mi><mi>τ</mi></mrow></mfrac></math>"#;
        assert_eq!(to_unicode(src), "1/(2πτ)");
        let sq = r#"<math><mfrac><msup><mi>x</mi><mn>2</mn></msup><mn>2</mn></mfrac></math>"#;
        assert_eq!(to_unicode(sq), "x²/2", "single scripted term stays bare");
    }

    #[test]
    fn compound_script_keeps_spacing() {
        // e^(x + 1/2): a script with an unmappable char (`/`) falls back to a parenthesised
        // form that keeps its word spacing (not a jammed `^(x+1/2)`).
        let src = r#"<math><msup><mi>e</mi><mrow><mi>x</mi><mo>+</mo><mfrac><mn>1</mn><mn>2</mn></mfrac></mrow></msup></math>"#;
        let out = to_unicode(src);
        assert!(out.contains("x + 1/2"), "spacing kept in script: {out:?}");
        assert!(!out.contains('{'), "no raw braces: {out:?}");
    }

    #[test]
    fn no_space_before_closing_delimiters_or_punctuation() {
        // A subscripted token inside parens must not leave `E(X₁ )`.
        let src = r#"<math><mi>E</mi><mo>(</mo><msub><mi>X</mi><mn>1</mn></msub><mo>)</mo></math>"#;
        assert_eq!(to_unicode(src), "E(X₁)");
    }
}
