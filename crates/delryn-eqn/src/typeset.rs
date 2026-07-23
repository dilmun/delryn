//! Typeset converters: turn a recovered [`MarkupSource`] into the layout engine's math
//! tree (`Vec<ParseNode>`), the crisp-path input for RaTeX. Fresh code — it shares nothing
//! with the old math paths.
//!
//! - **LaTeX** parses directly (authored source is the engine's native input).
//! - **Presentation MathML** maps *structurally* to `ParseNode`s (real fractions, scripts,
//!   roots, accents, braces, fences) — no lossy round-trip through a LaTeX string — while
//!   leaf symbols resolve through the engine's own table by parsing their command form, so
//!   every glyph is the one it draws. Any element not mapped yet returns `None`, and the
//!   render ladder falls to the publisher picture, then text.

use ego_tree::NodeRef;
use ratex_parser::parse_node::{AtomFamily, Mode, ParseNode};
use ratex_parser::parser::parse;
use scraper::{Html, Node};

use delryn_model::MarkupSource;

/// Convert a recovered markup source into the engine's math tree, or `None` when it can't
/// be built (→ the render ladder falls to the picture, then the Unicode floor).
pub fn to_nodes(src: &MarkupSource) -> Option<Vec<ParseNode>> {
    match src {
        MarkupSource::Latex(latex) => parse(latex).ok().filter(|v| !v.is_empty()),
        MarkupSource::PresentationMathml(mml) => mathml_to_nodes(mml),
        // Content MathML → presentation is a later step; for now the ladder falls back.
        MarkupSource::ContentMathml(_) => None,
    }
}

/// Map a Presentation-MathML string to a `ParseNode` tree.
fn mathml_to_nodes(src: &str) -> Option<Vec<ParseNode>> {
    let frag = Html::parse_fragment(src);
    let mut out = Vec::new();
    for child in frag.root_element().children() {
        match child.value() {
            Node::Text(t) if !t.text.trim().is_empty() => {
                out.push(leaf(&symbol_latex(t.text.trim()))?)
            }
            Node::Element(e) if !is_annotation(local(e.name())) => out.push(map_node(child, 1)?),
            _ => {}
        }
    }
    (!out.is_empty()).then_some(out)
}

const MAX_DEPTH: u16 = 128;

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn is_annotation(name: &str) -> bool {
    matches!(name, "annotation" | "annotation-xml" | "semantics")
}

fn elem_children(node: NodeRef<Node>) -> Vec<NodeRef<Node>> {
    node.children()
        .filter(|c| {
            c.value()
                .as_element()
                .is_some_and(|e| !is_annotation(local(e.name())))
        })
        .collect()
}

fn token_text(node: NodeRef<Node>) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        if let Node::Text(t) = d.value() {
            s.push_str(&t.text);
        }
    }
    s.trim().to_string()
}

/// An `OrdGroup` wrapping `body` — `<mrow>` and every implicit grouping.
fn ord(body: Vec<ParseNode>) -> ParseNode {
    ParseNode::OrdGroup {
        mode: Mode::Math,
        body,
        semisimple: None,
        loc: None,
    }
}

/// A base with optional super/subscripts. When the base is a big operator (an `Op` with
/// limits), the engine draws the scripts as limits above/below.
fn sup_sub(base: ParseNode, sup: Option<ParseNode>, sub: Option<ParseNode>) -> ParseNode {
    ParseNode::SupSub {
        mode: Mode::Math,
        base: Some(Box::new(base)),
        sup: sup.map(Box::new),
        sub: sub.map(Box::new),
        loc: None,
    }
}

/// A single leaf symbol, resolved through the engine's own table by parsing its command
/// form (`∑` → `\sum` → an `Op` with limits, `α` → `\alpha`). Multi-token results wrap in
/// one group; `None` if unparseable (→ the equation falls back).
fn leaf(latex: &str) -> Option<ParseNode> {
    let mut nodes = parse(latex).ok()?;
    match nodes.len() {
        0 => None,
        1 => nodes.pop(),
        _ => Some(ord(nodes)),
    }
}

/// A `<mi>` identifier's command form: standard function names render upright (`\sin`),
/// everything else goes through the symbol map.
fn mi_latex(t: &str) -> String {
    match t {
        "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "sinh" | "cosh" | "tanh" | "coth"
        | "log" | "ln" | "lg" | "exp" | "lim" | "limsup" | "liminf" | "max" | "min" | "sup"
        | "inf" | "arg" | "det" | "dim" | "ker" | "deg" | "gcd" | "hom" | "arcsin" | "arccos"
        | "arctan" => format!("\\{t} "),
        _ => symbol_latex(t),
    }
}

/// Map one MathML node to a single `ParseNode`, or `None` for anything not mapped yet.
fn map_node(node: NodeRef<Node>, depth: u16) -> Option<ParseNode> {
    if depth >= MAX_DEPTH {
        return None;
    }
    let e = match node.value() {
        Node::Element(e) => e,
        Node::Text(t) if !t.text.trim().is_empty() => return leaf(&symbol_latex(t.text.trim())),
        _ => return None,
    };
    let kids = elem_children(node);
    match local(e.name()) {
        "mi" => {
            let t = token_text(node);
            (!t.is_empty()).then(|| leaf(&mi_latex(&t))).flatten()
        }
        "mn" => {
            let t = token_text(node);
            (!t.is_empty()).then(|| leaf(&t)).flatten()
        }
        "mo" => {
            let t = token_text(node);
            if t.is_empty() {
                Some(ord(Vec::new()))
            } else {
                leaf(&symbol_latex(&t))
            }
        }
        "mtext" | "ms" => {
            let t = token_text(node);
            (!t.is_empty())
                .then(|| leaf(&format!("\\text{{{}}}", escape_text(&t))))
                .flatten()
        }
        "mfrac" if kids.len() >= 2 => Some(ParseNode::GenFrac {
            mode: Mode::Math,
            continued: false,
            numer: Box::new(map_node(kids[0], depth + 1)?),
            denom: Box::new(map_node(kids[1], depth + 1)?),
            has_bar_line: !matches!(
                e.attr("linethickness").map(str::trim),
                Some("0" | "0pt" | "0px" | "0em" | "none")
            ),
            left_delim: None,
            right_delim: None,
            bar_size: None,
            loc: None,
        }),
        "msup" if kids.len() >= 2 => Some(sup_sub(
            map_node(kids[0], depth + 1)?,
            Some(map_node(kids[1], depth + 1)?),
            None,
        )),
        "msub" if kids.len() >= 2 => Some(sup_sub(
            map_node(kids[0], depth + 1)?,
            None,
            Some(map_node(kids[1], depth + 1)?),
        )),
        // MathML orders <msubsup> as base, subscript, superscript.
        "msubsup" if kids.len() >= 3 => Some(sup_sub(
            map_node(kids[0], depth + 1)?,
            Some(map_node(kids[2], depth + 1)?),
            Some(map_node(kids[1], depth + 1)?),
        )),
        "munderover" if kids.len() >= 3 => under_over(kids[0], Some(kids[1]), Some(kids[2]), depth),
        "munder" if kids.len() >= 2 => under_over(kids[0], Some(kids[1]), None, depth),
        "mover" if kids.len() >= 2 => under_over(kids[0], None, Some(kids[1]), depth),
        "msqrt" => Some(ParseNode::Sqrt {
            mode: Mode::Math,
            body: Box::new(ord(map_children(node, depth + 1)?)),
            index: None,
            loc: None,
        }),
        "mroot" if kids.len() >= 2 => Some(ParseNode::Sqrt {
            mode: Mode::Math,
            body: Box::new(map_node(kids[0], depth + 1)?),
            index: Some(Box::new(map_node(kids[1], depth + 1)?)),
            loc: None,
        }),
        "mfenced" => fenced(node, e, depth),
        // A spacing element → a small gap (so a `, y > 0` annotation doesn't jam). Never
        // fails: an unsupported `<mspace>` must not sink the whole equation to the raster.
        "mspace" => Some(leaf("\\;").unwrap_or_else(|| ord(Vec::new()))),
        "mrow" | "math" | "mstyle" | "mpadded" | "mphantom" => {
            let body = map_children(node, depth + 1)?;
            // An empty group maps to an empty ord, not `None` — publishers use an empty
            // `<mrow/>` inside a fence for a lone stretchy bar (InDesign renders `|dx/dy|` as
            // `[|][frac][|]`), and that must not fail the equation.
            Some(if body.is_empty() {
                ord(body)
            } else {
                normalize(ord(body))
            })
        }
        // <mtable> and anything else unmapped → fall back to the picture/text.
        _ => None,
    }
}

/// Map every renderable child of `node` into a list; `None` if any child fails.
fn map_children(node: NodeRef<Node>, depth: u16) -> Option<Vec<ParseNode>> {
    if depth >= MAX_DEPTH {
        return None;
    }
    let mut out = Vec::new();
    for c in node.children() {
        match c.value() {
            Node::Text(t) if !t.text.trim().is_empty() => {
                out.push(leaf(&symbol_latex(t.text.trim()))?)
            }
            Node::Element(e) if !is_annotation(local(e.name())) => {
                out.push(map_node(c, depth + 1)?)
            }
            _ => {}
        }
    }
    Some(out)
}

/// Unwrap a single-element group so `<mrow><mi>x</mi></mrow>` is just `x`.
fn normalize(node: ParseNode) -> ParseNode {
    if let ParseNode::OrdGroup { body, .. } = &node
        && body.len() == 1
    {
        return body[0].clone();
    }
    node
}

/// `<munder>`/`<mover>`/`<munderover>`: an accent over a base becomes a real accent; a
/// horizontal brace a `HorizBrace`; otherwise scripts (drawn as limits over a big operator).
fn under_over(
    base: NodeRef<Node>,
    under: Option<NodeRef<Node>>,
    over: Option<NodeRef<Node>>,
    depth: u16,
) -> Option<ParseNode> {
    let base = map_node(base, depth + 1)?;
    if under.is_none()
        && let Some(o) = over
    {
        let t = token_text(o);
        if let Some(label) = accent_label(&t) {
            return accent_node(label, base);
        }
        if is_top_brace(&t) {
            return brace_node(base, true);
        }
    }
    if over.is_none()
        && let Some(u) = under
        && is_bottom_brace(&token_text(u))
    {
        return brace_node(base, false);
    }
    let sup = match over {
        Some(o) => Some(map_node(o, depth + 1)?),
        None => None,
    };
    let sub = match under {
        Some(u) => Some(map_node(u, depth + 1)?),
        None => None,
    };
    Some(sup_sub(base, sup, sub))
}

fn is_top_brace(s: &str) -> bool {
    s.trim() == "\u{23DE}" // ⏞
}
fn is_bottom_brace(s: &str) -> bool {
    s.trim() == "\u{23DF}" // ⏟
}

/// The engine's accent command for an over-glyph, or `None` if it isn't a recognised accent.
fn accent_label(over: &str) -> Option<&'static str> {
    match over.trim() {
        "^" | "ˆ" | "\u{0302}" => Some("\\hat"),
        "~" | "˜" | "\u{0303}" => Some("\\tilde"),
        "¯" | "‾" | "\u{0304}" => Some("\\bar"),
        "→" | "\u{20D7}" => Some("\\vec"),
        "˙" | "\u{0307}" => Some("\\dot"),
        "¨" | "\u{0308}" => Some("\\ddot"),
        "ˇ" | "\u{030C}" => Some("\\check"),
        "˘" | "\u{0306}" => Some("\\breve"),
        "´" | "\u{0301}" => Some("\\acute"),
        "`" | "\u{0300}" => Some("\\grave"),
        _ => None,
    }
}

/// A real `Accent` node: graft `base` into a parsed accent template so the fiddly
/// stretchy/shifty flags match exactly what the engine uses for that command.
fn accent_node(label: &str, base: ParseNode) -> Option<ParseNode> {
    match parse(&format!("{label}{{x}}")).ok()?.into_iter().next()? {
        ParseNode::Accent {
            label,
            is_stretchy,
            is_shifty,
            ..
        } => Some(ParseNode::Accent {
            mode: Mode::Math,
            label,
            is_stretchy,
            is_shifty,
            base: Box::new(base),
            loc: None,
        }),
        _ => None,
    }
}

/// A horizontal brace over/under `base`, grafted from an `\overbrace`/`\underbrace` template.
fn brace_node(base: ParseNode, is_over: bool) -> Option<ParseNode> {
    let cmd = if is_over {
        "\\overbrace"
    } else {
        "\\underbrace"
    };
    match parse(&format!("{cmd}{{x}}")).ok()?.into_iter().next()? {
        ParseNode::HorizBrace { label, is_over, .. } => Some(ParseNode::HorizBrace {
            mode: Mode::Math,
            label,
            is_over,
            base: Box::new(base),
            loc: None,
        }),
        _ => None,
    }
}

/// `<mfenced>`: a `\left…\right` delimited group, children separated by the `separators` glyph.
fn fenced(node: NodeRef<Node>, e: &scraper::node::Element, depth: u16) -> Option<ParseNode> {
    let left = fence_delim(e.attr("open").unwrap_or("("))?;
    let right = fence_delim(e.attr("close").unwrap_or(")"))?;
    let sep = e
        .attr("separators")
        .and_then(|s| s.chars().find(|c| !c.is_whitespace()))
        .unwrap_or(',');
    let mut body = Vec::new();
    for (i, &c) in elem_children(node).iter().enumerate() {
        if i > 0 {
            body.push(ParseNode::Atom {
                mode: Mode::Math,
                family: AtomFamily::Punct,
                text: sep.to_string(),
                loc: None,
            });
        }
        body.push(map_node(c, depth + 1)?);
    }
    Some(ParseNode::LeftRight {
        mode: Mode::Math,
        body,
        left,
        right,
        right_color: None,
        loc: None,
    })
}

/// The engine delimiter form of a fence character (`{` → `\{`, `⟨` → `\langle`); `None` for
/// an unrecognised one so the equation falls back rather than mis-render.
fn fence_delim(s: &str) -> Option<String> {
    Some(
        match s.trim() {
            "" | "." => ".",
            "(" | ")" | "[" | "]" | "|" | "/" => s.trim(),
            "{" => "\\{",
            "}" => "\\}",
            "‖" | "∥" => "\\|",
            "⟨" | "〈" => "\\langle",
            "⟩" | "〉" => "\\rangle",
            "⌊" => "\\lfloor",
            "⌋" => "\\rfloor",
            "⌈" => "\\lceil",
            "⌉" => "\\rceil",
            _ => return None,
        }
        .to_string(),
    )
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

/// Map a MathML token (a `<mi>`/`<mn>`/`<mo>` value) to a LaTeX command the engine knows —
/// common operators become their commands (so big operators keep limits); a multi-letter
/// identifier and anything unmapped pass through (the engine renders most Unicode math).
fn symbol_latex(s: &str) -> String {
    let t = s.trim();
    let mapped = match t {
        "−" => "-",
        "·" | "⋅" => "\\cdot ",
        "×" => "\\times ",
        "÷" => "\\div ",
        "∗" => "\\ast ",
        "⋯" => "\\cdots ",
        "…" => "\\ldots ",
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
        "ℝ" => "\\mathbb{R}",
        "ℕ" => "\\mathbb{N}",
        "ℤ" => "\\mathbb{Z}",
        "ℚ" => "\\mathbb{Q}",
        "ℂ" => "\\mathbb{C}",
        "𝔼" => "\\mathbb{E}",
        "ℙ" => "\\mathbb{P}",
        "α" => "\\alpha ",
        "β" => "\\beta ",
        "γ" => "\\gamma ",
        "δ" => "\\delta ",
        "ε" | "ϵ" => "\\epsilon ",
        "ζ" => "\\zeta ",
        "η" => "\\eta ",
        "θ" => "\\theta ",
        "κ" => "\\kappa ",
        "λ" => "\\lambda ",
        "μ" => "\\mu ",
        "ν" => "\\nu ",
        "ξ" => "\\xi ",
        "π" => "\\pi ",
        "ρ" => "\\rho ",
        "σ" => "\\sigma ",
        "τ" => "\\tau ",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes_of(src: MarkupSource) -> String {
        format!("{:?}", to_nodes(&src).expect("maps"))
    }

    #[test]
    fn mspace_and_empty_fence_do_not_fail_the_equation() {
        // InDesign/MathType ships a stretchy `|` as an *empty* `<mfenced>`, and uses
        // `<mspace>` for annotation gaps (`, y > 0`). Neither may sink the whole equation to
        // the publisher raster — both must map so the equation typesets (crisp, prose-sized).
        let d = nodes_of(MarkupSource::PresentationMathml(
            r#"<math><mstyle mathsize="2em"><mfenced open="|" close="" separators=""><mrow/></mfenced></mstyle><mfrac><mrow><mi>d</mi><mi>x</mi></mrow><mrow><mi>d</mi><mi>y</mi></mrow></mfrac><mspace width="1em"/><mi>y</mi></math>"#.into(),
        ));
        assert!(
            d.contains("LeftRight"),
            "the empty stretchy `|` fence maps to a delimiter group: {d}"
        );
        assert!(d.contains("GenFrac"), "the fraction still maps: {d}");
    }

    #[test]
    fn latex_source_parses() {
        let d = nodes_of(MarkupSource::Latex("\\frac{1}{2}".into()));
        assert!(d.contains("GenFrac"), "latex fraction → GenFrac: {d}");
    }

    #[test]
    fn mathml_fraction_builds_a_real_genfrac() {
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><mfrac><mn>1</mn><mn>2</mn></mfrac></math>".into(),
        ));
        assert!(
            d.contains("GenFrac") && d.contains("has_bar_line: true"),
            "{d}"
        );
    }

    #[test]
    fn mathml_barless_fraction() {
        let d = nodes_of(MarkupSource::PresentationMathml(
            r#"<math><mfrac linethickness="0"><mn>1</mn><mn>2</mn></mfrac></math>"#.into(),
        ));
        assert!(
            d.contains("has_bar_line: false"),
            "linethickness=0 → no bar: {d}"
        );
    }

    #[test]
    fn mathml_sum_with_limits_is_supsub_over_op() {
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>N</mi></munderover></math>".into(),
        ));
        assert!(
            d.contains("SupSub") && d.contains("Op {"),
            "limits over a big operator: {d}"
        );
    }

    #[test]
    fn mathml_over_hat_is_a_real_accent() {
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><mover><mi>x</mi><mo>^</mo></mover></math>".into(),
        ));
        assert!(d.contains("Accent"), "over-hat → real accent: {d}");
    }

    #[test]
    fn mathml_sqrt_and_root() {
        assert!(
            nodes_of(MarkupSource::PresentationMathml(
                "<math><msqrt><mi>x</mi></msqrt></math>".into()
            ))
            .contains("Sqrt")
        );
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><mroot><mi>x</mi><mn>3</mn></mroot></math>".into(),
        ));
        assert!(
            d.contains("Sqrt") && d.contains("index: Some"),
            "mroot carries its index: {d}"
        );
    }

    #[test]
    fn unmapped_and_content_fall_back_to_none() {
        assert!(
            to_nodes(&MarkupSource::PresentationMathml(
                "<math><mtable><mtr><mtd><mn>1</mn></mtd></mtr></mtable></math>".into()
            ))
            .is_none(),
            "mtable is deferred → None (render falls to the picture)"
        );
        assert!(
            to_nodes(&MarkupSource::ContentMathml("<math><apply/></math>".into())).is_none(),
            "content MathML not yet mapped → None"
        );
    }
}
