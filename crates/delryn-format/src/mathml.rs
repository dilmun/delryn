//! Best-effort MathML → Unicode. EPUBs — especially those converted from
//! OOXML/DOCX — ship math as MathML serialised into an `<img alt="…">`; rather
//! than show raw tags we walk the presentation elements and render them to
//! Unicode (super/subscripts, fractions, roots, fenced groups). This won't
//! typeset matrices, but sums, products, fractions, and scripted symbols come
//! out readable in a terminal: `∑_{i=1}^{N} i²` → `∑ᵢ₌₁ᴺ i²`.

use delryn_model::math::{subscript_str, superscript_str};
use ego_tree::NodeRef;
use scraper::{Html, Node};

/// Maximum MathML nesting depth walked before the transcoder stops recursing and
/// emits the remaining subtree as flat text — guards against a stack overflow on
/// pathologically nested `<mrow>` / script markup. Real math nests shallowly.
const MAX_MATH_DEPTH: u16 = 128;

/// Render a MathML string to Unicode.
pub fn to_unicode(src: &str) -> String {
    let frag = Html::parse_fragment(src);
    let mut out = String::new();
    // `parse_fragment` wraps the content under an <html> root; walking its
    // children descends through any <body>/<math> wrappers into the content.
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

/// Rendered (trimmed) Unicode for one node — used for script/fraction arguments.
fn part(node: NodeRef<Node>, depth: u16) -> String {
    let mut s = String::new();
    emit(node, depth, &mut s);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn token_text(node: NodeRef<Node>) -> String {
    crate::container::descendant_text(node, false, None)
        .trim()
        .to_string()
}

/// Render `arg` as a subscript — Unicode where every char maps, else a clean
/// parenthesised fallback (`_(i+1)`), never raw braces.
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

fn emit(node: NodeRef<Node>, depth: u16, out: &mut String) {
    // Pathological nesting: stop recursing and append the subtree's flat text
    // (collected iteratively) so the stack can't overflow.
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

fn part_str(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── MathML → LaTeX ───────────────────────────────────────────────────────────
//
// A MathML-only equation (no `alttext` / TeX `<annotation>`) can still be rendered
// *graphically* if we synthesise LaTeX from the presentation tree — far better than
// the lossy Unicode approximation (which drops fraction bars, exponents, under/over-
// braces). Best-effort: unknown constructs degrade, and a downstream RaTeX render
// failure falls back to the Unicode string anyway, so this never regresses.

/// Render a MathML string to a LaTeX source, or `None` when nothing renderable comes
/// out (→ the caller keeps the Unicode fallback).
pub fn to_latex(src: &str) -> Option<String> {
    let frag = Html::parse_fragment(src);
    let mut out = String::new();
    for child in frag.root_element().children() {
        latex_emit(child, 0, &mut out);
    }
    let s = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (!s.trim().is_empty()).then_some(s)
}

/// One node's LaTeX, wrapped in `{…}` so it is a single group for a script / fraction
/// argument (`x^{2}`, `\frac{a}{b}`).
fn latex_braced(node: NodeRef<Node>, depth: u16) -> String {
    let mut s = String::new();
    latex_emit(node, depth, &mut s);
    format!("{{{}}}", s.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Whether a token is a bottom (`⏟`) or top (`⏞`) horizontal brace — the marker an
/// under/over-brace annotation carries as its script.
fn is_bottom_brace(s: &str) -> bool {
    s.trim() == "\u{23DF}" // ⏟
}
fn is_top_brace(s: &str) -> bool {
    s.trim() == "\u{23DE}" // ⏞
}

fn latex_emit(node: NodeRef<Node>, depth: u16, out: &mut String) {
    if depth >= MAX_MATH_DEPTH {
        let t = token_text(node);
        if !t.is_empty() {
            out.push_str(&latex_symbol(&t));
            out.push(' ');
        }
        return;
    }
    match node.value() {
        Node::Text(t) => {
            let t = t.text.trim();
            if !t.is_empty() {
                out.push_str(&latex_symbol(t));
                out.push(' ');
            }
        }
        Node::Element(e) => {
            let kids = elem_children(node);
            match local(e.name()) {
                "mi" | "mn" => {
                    out.push_str(&latex_symbol(&token_text(node)));
                    out.push(' ');
                }
                "mtext" | "ms" => {
                    let t = token_text(node);
                    if !t.is_empty() {
                        out.push_str(&format!("\\text{{{}}} ", escape_text(&t)));
                    }
                }
                "mo" => {
                    let op = token_text(node);
                    if !op.is_empty() {
                        out.push_str(&latex_symbol(&op));
                        out.push(' ');
                    }
                }
                "msubsup" | "munderover" if kids.len() >= 3 => {
                    out.push_str(&latex_braced(kids[0], depth + 1));
                    out.push_str(&format!(
                        "_{}^{} ",
                        latex_braced(kids[1], depth + 1),
                        latex_braced(kids[2], depth + 1)
                    ));
                }
                "msub" if kids.len() >= 2 => {
                    out.push_str(&latex_braced(kids[0], depth + 1));
                    out.push_str(&format!("_{} ", latex_braced(kids[1], depth + 1)));
                }
                "msup" if kids.len() >= 2 => {
                    out.push_str(&latex_braced(kids[0], depth + 1));
                    out.push_str(&format!("^{} ", latex_braced(kids[1], depth + 1)));
                }
                // Under/over: a horizontal brace becomes `\underbrace`/`\overbrace`;
                // anything else stacks with `\underset`/`\overset` (KaTeX-supported),
                // which also handles a limit (`lim`) or an under/over-brace *label*.
                "munder" if kids.len() >= 2 => {
                    let under = part(kids[1], depth + 1);
                    if is_bottom_brace(&under) {
                        out.push_str(&format!(
                            "\\underbrace{} ",
                            latex_braced(kids[0], depth + 1)
                        ));
                    } else {
                        out.push_str(&format!(
                            "\\underset{}{} ",
                            latex_braced(kids[1], depth + 1),
                            latex_braced(kids[0], depth + 1)
                        ));
                    }
                }
                "mover" if kids.len() >= 2 => {
                    let over = part(kids[1], depth + 1);
                    if is_top_brace(&over) {
                        out.push_str(&format!("\\overbrace{} ", latex_braced(kids[0], depth + 1)));
                    } else {
                        out.push_str(&format!(
                            "\\overset{}{} ",
                            latex_braced(kids[1], depth + 1),
                            latex_braced(kids[0], depth + 1)
                        ));
                    }
                }
                "mfrac" if kids.len() >= 2 => {
                    out.push_str(&format!(
                        "\\frac{}{} ",
                        latex_braced(kids[0], depth + 1),
                        latex_braced(kids[1], depth + 1)
                    ));
                }
                "msqrt" => {
                    let mut inner = String::new();
                    for c in node.children() {
                        latex_emit(c, depth + 1, &mut inner);
                    }
                    out.push_str(&format!("\\sqrt{{{}}} ", part_str(&inner)));
                }
                "mroot" if kids.len() >= 2 => {
                    out.push_str(&format!(
                        "\\sqrt[{}]{} ",
                        part(kids[1], depth + 1),
                        latex_braced(kids[0], depth + 1)
                    ));
                }
                "mfenced" => {
                    let open = e.attr("open").unwrap_or("(");
                    let close = e.attr("close").unwrap_or(")");
                    let inner: Vec<String> = kids
                        .iter()
                        .map(|&c| {
                            let mut s = String::new();
                            latex_emit(c, depth + 1, &mut s);
                            part_str(&s)
                        })
                        .collect();
                    out.push_str(&format!(
                        "\\left{} {} \\right{} ",
                        latex_symbol(open),
                        inner.join(", "),
                        latex_symbol(close)
                    ));
                }
                // A table (aligned equations / a left-aligned single equation): rows on
                // `\\`, cells on `&`, inside `aligned`.
                "mtable" => {
                    let rows: Vec<String> = kids
                        .iter()
                        .filter(|c| local(c.value().as_element().unwrap().name()) == "mtr")
                        .map(|&mtr| {
                            elem_children(mtr)
                                .iter()
                                .map(|&mtd| {
                                    let mut s = String::new();
                                    latex_emit(mtd, depth + 1, &mut s);
                                    part_str(&s)
                                })
                                .collect::<Vec<_>>()
                                .join(" & ")
                        })
                        .collect();
                    out.push_str(&format!(
                        "\\begin{{aligned}} {} \\end{{aligned}} ",
                        rows.join(" \\\\ ")
                    ));
                }
                "mspace" => out.push(' '),
                "semantics" => {
                    if let Some(&first) = kids.first() {
                        latex_emit(first, depth + 1, out);
                    }
                }
                "annotation" => {}
                // mrow / math / mstyle / mpadded / menclose / mtd / unknown → recurse.
                _ => {
                    for c in node.children() {
                        latex_emit(c, depth + 1, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Escape the LaTeX specials that appear in `\text{…}` content.
fn escape_text(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\textbackslash "),
            '&' | '%' | '$' | '#' | '_' | '{' | '}' => {
                o.push('\\');
                o.push(c);
            }
            '^' => o.push_str("\\textasciicircum "),
            '~' => o.push_str("\\textasciitilde "),
            _ => o.push(c),
        }
    }
    o
}

/// Map a MathML token (a `<mi>`/`<mn>`/`<mo>` value or loose text) to LaTeX. Common
/// symbols become their commands (so RaTeX gets standard input and big operators keep
/// their limits); a multi-letter identifier and anything unmapped pass through as-is
/// (KaTeX renders most Unicode math characters).
fn latex_symbol(s: &str) -> String {
    let t = s.trim();
    let mapped = match t {
        "−" => "-",
        "·" | "⋅" => "\\cdot ",
        "×" => "\\times ",
        "÷" => "\\div ",
        "∗" => "\\ast ",
        "⋯" => "\\cdots ",
        "…" => "\\ldots ",
        "⋮" => "\\vdots ",
        "⋱" => "\\ddots ",
        "≠" => "\\ne ",
        "≤" | "⩽" => "\\le ",
        "≥" | "⩾" => "\\ge ",
        "≈" => "\\approx ",
        "≡" => "\\equiv ",
        "≅" => "\\cong ",
        "∼" => "\\sim ",
        "∝" => "\\propto ",
        "±" => "\\pm ",
        "∓" => "\\mp ",
        "∑" => "\\sum ",
        "∏" => "\\prod ",
        "∫" => "\\int ",
        "∮" => "\\oint ",
        "⋃" => "\\bigcup ",
        "⋂" => "\\bigcap ",
        "∞" => "\\infty ",
        "∂" => "\\partial ",
        "∇" => "\\nabla ",
        "∅" => "\\emptyset ",
        "→" => "\\to ",
        "↦" => "\\mapsto ",
        "⇒" => "\\Rightarrow ",
        "⇔" => "\\Leftrightarrow ",
        "∈" => "\\in ",
        "∉" => "\\notin ",
        "∋" => "\\ni ",
        "⊂" => "\\subset ",
        "⊆" => "\\subseteq ",
        "⊃" => "\\supset ",
        "⊇" => "\\supseteq ",
        "∪" => "\\cup ",
        "∩" => "\\cap ",
        "∖" => "\\setminus ",
        "∀" => "\\forall ",
        "∃" => "\\exists ",
        "¬" => "\\neg ",
        "∧" => "\\wedge ",
        "∨" => "\\vee ",
        "⊕" => "\\oplus ",
        "⊗" => "\\otimes ",
        "⌊" => "\\lfloor ",
        "⌋" => "\\rfloor ",
        "⌈" => "\\lceil ",
        "⌉" => "\\rceil ",
        "⟨" => "\\langle ",
        "⟩" => "\\rangle ",
        "ℝ" => "\\mathbb{R}",
        "ℕ" => "\\mathbb{N}",
        "ℤ" => "\\mathbb{Z}",
        "ℚ" => "\\mathbb{Q}",
        "ℂ" => "\\mathbb{C}",
        "𝔼" => "\\mathbb{E}",
        "ℙ" => "\\mathbb{P}",
        // Greek — map so RaTeX gets a known command (KaTeX also accepts the literal,
        // but the command is the safe form).
        "α" => "\\alpha ",
        "β" => "\\beta ",
        "γ" => "\\gamma ",
        "δ" => "\\delta ",
        "ε" | "ϵ" => "\\epsilon ",
        "ζ" => "\\zeta ",
        "η" => "\\eta ",
        "θ" => "\\theta ",
        "ι" => "\\iota ",
        "κ" => "\\kappa ",
        "λ" => "\\lambda ",
        "μ" => "\\mu ",
        "ν" => "\\nu ",
        "ξ" => "\\xi ",
        "π" => "\\pi ",
        "ρ" => "\\rho ",
        "σ" => "\\sigma ",
        "τ" => "\\tau ",
        "υ" => "\\upsilon ",
        "φ" | "ϕ" => "\\phi ",
        "χ" => "\\chi ",
        "ψ" => "\\psi ",
        "ω" => "\\omega ",
        "Γ" => "\\Gamma ",
        "Δ" => "\\Delta ",
        "Θ" => "\\Theta ",
        "Λ" => "\\Lambda ",
        "Ξ" => "\\Xi ",
        "Π" => "\\Pi ",
        "Σ" => "\\Sigma ",
        "Φ" => "\\Phi ",
        "Ψ" => "\\Psi ",
        "Ω" => "\\Omega ",
        other => return other.to_string(),
    };
    mapped.to_string()
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
        // x_{100} and a relational subscript — no spaces leak inside scripts.
        let src = r#"<math><msub><mi>x</mi><mn>100</mn></msub></math>"#;
        assert_eq!(to_unicode(src), "x₁₀₀");
    }

    #[test]
    fn unmappable_script_uses_paren_fallback_not_braces() {
        // A fraction superscript can't be Unicode super → readable fallback.
        let src =
            r#"<math><msup><mi>e</mi><mrow><mi>x</mi><mo>+</mo><mn>1</mn></mrow></msup></math>"#;
        let out = to_unicode(src);
        assert_eq!(out, "eˣ⁺¹", "got: {out:?}");
        // And one that truly can't map (contains a comma) parenthesises.
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
    fn to_latex_recovers_a_fraction() {
        // MathML-only `n!/(n-r)!` — the Unicode transcode drops the bar; LaTeX keeps it.
        let src = r#"<math><mfrac><mrow><mi>n</mi><mo>!</mo></mrow><mrow><mo>(</mo><mi>n</mi><mo>−</mo><mi>r</mi><mo>)</mo><mo>!</mo></mrow></mfrac></math>"#;
        let latex = to_latex(src).expect("some latex");
        assert!(latex.contains("\\frac{"), "keeps a real fraction: {latex}");
        assert!(latex.contains("n - r") || latex.contains("n-r"), "{latex}");
    }

    #[test]
    fn to_latex_sum_with_limits_and_superscript() {
        let src = r#"<math><munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>N</mi></munderover><msup><mi>i</mi><mn>2</mn></msup></math>"#;
        let latex = to_latex(src).expect("some latex");
        assert!(latex.contains("\\sum"), "big operator command: {latex}");
        assert!(
            latex.contains("_{") && latex.contains("^{"),
            "limits: {latex}"
        );
        assert!(
            latex.contains("i}^{2}") || latex.contains("^{2}"),
            "{latex}"
        );
    }

    #[test]
    fn to_latex_underbrace_and_table() {
        // An underbrace with a text label inside a one-row aligned table.
        let src = r#"<math display="block"><mtable><mtr><mtd><munder><munder><mrow><mn>16</mn></mrow><mo>⏟</mo></munder><mtext>roll</mtext></munder></mtd></mtr></mtable></math>"#;
        let latex = to_latex(src).expect("some latex");
        assert!(
            latex.contains("\\begin{aligned}"),
            "table → aligned: {latex}"
        );
        assert!(
            latex.contains("\\underbrace{"),
            "brace → underbrace: {latex}"
        );
        assert!(latex.contains("\\text{roll}"), "label as text: {latex}");
    }

    #[test]
    fn to_latex_none_for_empty() {
        assert!(to_latex("<math></math>").is_none(), "empty math → no latex");
    }
}
