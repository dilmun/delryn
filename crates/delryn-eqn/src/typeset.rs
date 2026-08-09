//! Typeset converters: turn a recovered [`MarkupSource`] into the layout engine's math
//! tree (`Vec<ParseNode>`), the crisp-path input for RaTeX. Fresh code — it shares nothing
//! with the old math paths.
//!
//! - **LaTeX** parses directly (authored source is the engine's native input).
//! - **Presentation MathML** maps *structurally* to `ParseNode`s (real fractions, scripts,
//!   roots, accents, braces, fences) — no lossy round-trip through a LaTeX string — while
//!   leaf symbols resolve through the engine's own table by parsing their command form, so
//!   every glyph is the one it draws.
//!
//! **Graceful degradation.** Mapping never fails the *whole* equation on one bad node: an
//! element we don't map renders its children, an unparseable leaf renders its raw text, and a
//! degenerate/empty node renders nothing. `to_nodes` returns `None` only when the equation
//! yields *no* visible content at all (or the source is Content MathML) — then the render
//! ladder falls to the publisher picture, then the Unicode floor.

use ego_tree::NodeRef;
use ratex_parser::parse_node::{AlignSpec, AlignType, AtomFamily, Mode, ParseNode};
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

/// Map a Presentation-MathML string to a `ParseNode` tree. `None` only when nothing visible
/// maps (an entirely empty/degenerate equation) — the ladder then falls to the picture.
fn mathml_to_nodes(src: &str) -> Option<Vec<ParseNode>> {
    let frag = Html::parse_fragment(src);
    let out = map_children_degraded(*frag.root_element(), 1);
    (!out.is_empty()).then_some(out)
}

const MAX_DEPTH: u16 = 128;

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Annotation siblings inside `<semantics>` (`<annotation>`, `<annotation-xml>`) carry the
/// non-presentation encodings (MathType-MTEF binary, Content MathML, speech) — skip them.
/// `<semantics>` itself is NOT annotation: it's a transparent wrapper around the presentation.
fn is_annotation(name: &str) -> bool {
    matches!(name, "annotation" | "annotation-xml")
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

/// Map a node, degrading rather than failing: a node we can't map becomes visible content
/// (its children, or its raw text) so one bad node never sinks the whole equation.
fn map_or_degrade(node: NodeRef<Node>, depth: u16) -> ParseNode {
    map_node(node, depth).unwrap_or_else(|| degrade(node, depth))
}

/// The fallback for a node `map_node` returned `None` for: an unknown/malformed element
/// renders whatever children it has (the semantically-closest thing); a stray text leaf
/// renders literally; nothing renders an empty group.
fn degrade(node: NodeRef<Node>, depth: u16) -> ParseNode {
    if depth >= MAX_DEPTH {
        return ord(Vec::new());
    }
    match node.value() {
        Node::Element(_) => normalize(ord(map_children_degraded(node, depth + 1))),
        Node::Text(t) => text_leaf(t.text.trim()),
        _ => ord(Vec::new()),
    }
}

/// A leaf that renders `raw` literally — the last-resort glyph fallback (the engine passes
/// unknown non-ASCII through as text; `\text{…}` covers whatever it can't).
fn text_leaf(raw: &str) -> ParseNode {
    if raw.is_empty() {
        return ord(Vec::new());
    }
    leaf(raw)
        .or_else(|| leaf(&format!("\\text{{{}}}", escape_text(raw))))
        .unwrap_or_else(|| ord(Vec::new()))
}

/// A text node's content → a node: a known operator becomes its command, else it renders
/// literally. Invisible operators (function-application, invisible-times, …) drop to nothing.
fn text_or_symbol(raw: &str) -> ParseNode {
    if is_invisible_op(raw) {
        return ord(Vec::new());
    }
    leaf(&symbol_latex(raw)).unwrap_or_else(|| text_leaf(raw))
}

/// The Unicode "invisible operators" MathType inserts explicitly — function application,
/// invisible times/comma/plus. They carry no ink; emitting them yields tofu, so drop them.
fn is_invisible_op(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().all(|c| ('\u{2061}'..='\u{2064}').contains(&c))
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
        "mi" => Some(token(e, &token_text(node), mi_latex)),
        "mn" => Some(token(e, &token_text(node), |s| s.to_string())),
        "mo" => {
            let t = token_text(node);
            if is_invisible_op(&t) {
                Some(ord(Vec::new()))
            } else {
                Some(token(e, &t, symbol_latex))
            }
        }
        "mtext" | "ms" => {
            let t = token_text(node);
            if t.is_empty() {
                // An empty or whitespace-only `<mtext>` is a spacing gap — publishers use one
                // between words (`for` ⋯ `y`). A small space, never the whole equation lost.
                Some(leaf("\\;").unwrap_or_else(|| ord(Vec::new())))
            } else {
                Some(
                    leaf(&format!("\\text{{{}}}", escape_text(&t)))
                        .unwrap_or_else(|| text_leaf(&t)),
                )
            }
        }
        "mfrac" if kids.len() >= 2 => Some(ParseNode::GenFrac {
            mode: Mode::Math,
            continued: false,
            numer: Box::new(map_or_degrade(kids[0], depth + 1)),
            denom: Box::new(map_or_degrade(kids[1], depth + 1)),
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
            map_or_degrade(kids[0], depth + 1),
            Some(map_or_degrade(kids[1], depth + 1)),
            None,
        )),
        "msub" if kids.len() >= 2 => Some(sup_sub(
            map_or_degrade(kids[0], depth + 1),
            None,
            Some(map_or_degrade(kids[1], depth + 1)),
        )),
        // MathML orders <msubsup> as base, subscript, superscript.
        "msubsup" if kids.len() >= 3 => Some(sup_sub(
            map_or_degrade(kids[0], depth + 1),
            Some(map_or_degrade(kids[2], depth + 1)),
            Some(map_or_degrade(kids[1], depth + 1)),
        )),
        "munderover" if kids.len() >= 3 => {
            Some(under_over(kids[0], Some(kids[1]), Some(kids[2]), depth))
        }
        "munder" if kids.len() >= 2 => Some(under_over(kids[0], Some(kids[1]), None, depth)),
        "mover" if kids.len() >= 2 => Some(under_over(kids[0], None, Some(kids[1]), depth)),
        "msqrt" => Some(ParseNode::Sqrt {
            mode: Mode::Math,
            body: Box::new(ord(map_children_degraded(node, depth + 1))),
            index: None,
            loc: None,
        }),
        "mroot" if kids.len() >= 2 => Some(ParseNode::Sqrt {
            mode: Mode::Math,
            body: Box::new(map_or_degrade(kids[0], depth + 1)),
            index: Some(Box::new(map_or_degrade(kids[1], depth + 1))),
            loc: None,
        }),
        "mfenced" => Some(fenced(node, e, depth)),
        // A grid: matrices, and the outer right|left layout table InDesign wraps every
        // display equation in. Delimiters, when present, come from a surrounding `<mfenced>`.
        "mtable" => mtable(node, e, depth),
        // A spacing element → a small gap (so a `, y > 0` annotation doesn't jam).
        "mspace" => Some(leaf("\\;").unwrap_or_else(|| ord(Vec::new()))),
        // Invisible: reserves space, renders nothing (never leak its content as visible math).
        "mphantom" => Some(ParseNode::Phantom {
            mode: Mode::Math,
            body: map_children_degraded(node, depth + 1),
            loc: None,
        }),
        // `<semantics>` = a transparent wrapper: prefer an embedded LaTeX annotation, else the
        // presentation child (its `<annotation>`/`<annotation-xml>` siblings are filtered out).
        "semantics" => Some(semantics(node, depth)),
        // Transparent grouping. A `mathvariant` on `<mstyle>` pushes its font onto the group.
        "mrow" | "math" | "mstyle" | "mpadded" => {
            // A `|…|` / `‖…‖` / `(matrix)` run around tall content → stretchy `\left…\right`.
            if let Some(p) = promote_delims(node, depth) {
                return Some(variant(e, p));
            }
            let body = map_children_degraded(node, depth + 1);
            let group = if body.is_empty() {
                ord(body)
            } else {
                normalize(ord(body))
            };
            Some(variant(e, group))
        }
        // Anything else → let the caller degrade it (render its children).
        _ => None,
    }
}

/// A token element (`<mi>`/`<mn>`/`<mo>`) → its leaf, honoring `mathvariant`. Empty → an empty
/// group; an unparseable command → the raw text (never sinks the equation).
fn token(e: &scraper::node::Element, t: &str, to_latex: impl Fn(&str) -> String) -> ParseNode {
    if t.is_empty() {
        return ord(Vec::new());
    }
    variant(e, leaf(&to_latex(t)).unwrap_or_else(|| text_leaf(t)))
}

/// Map every renderable child of `node`, degrading any child that can't map. Never fails —
/// `<annotation>`/`<annotation-xml>` siblings are skipped so `<semantics>` renders clean.
fn map_children_degraded(node: NodeRef<Node>, depth: u16) -> Vec<ParseNode> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }
    let mut out = Vec::new();
    for c in node.children() {
        match c.value() {
            Node::Text(t) if !t.text.trim().is_empty() => out.push(text_or_symbol(t.text.trim())),
            Node::Element(e) if !is_annotation(local(e.name())) => {
                out.push(map_or_degrade(c, depth + 1))
            }
            _ => {}
        }
    }
    out
}

/// `<semantics>`: prefer an embedded `<annotation encoding="application/x-tex">` (lossless
/// LaTeX), else render the presentation child.
fn semantics(node: NodeRef<Node>, depth: u16) -> ParseNode {
    if let Some(tex) = tex_annotation(node)
        && let Ok(nodes) = parse(&tex)
        && !nodes.is_empty()
    {
        return normalize(ord(nodes));
    }
    normalize(ord(map_children_degraded(node, depth + 1)))
}

/// The text of a child `<annotation encoding="…x-tex|TeX|LaTeX">`, if present.
fn tex_annotation(node: NodeRef<Node>) -> Option<String> {
    node.children().find_map(|c| {
        let e = c.value().as_element()?;
        (local(e.name()) == "annotation"
            && matches!(
                e.attr("encoding").map(str::trim),
                Some("application/x-tex" | "application/x-latex" | "TeX" | "LaTeX" | "text/x-tex")
            ))
        .then(|| token_text(c))
        .filter(|s| !s.is_empty())
    })
}

/// Apply a token/`mstyle` `mathvariant` by wrapping in the matching font command. `italic`
/// and the absent/default case are no-ops (math letters already render italic).
fn variant(e: &scraper::node::Element, node: ParseNode) -> ParseNode {
    match e.attr("mathvariant").and_then(|v| variant_cmd(v.trim())) {
        Some(cmd) => font_wrap(cmd, node),
        None => node,
    }
}

/// The engine font command for a `mathvariant` value (blackboard, bold, script, …), or `None`
/// for `italic`/unknown (the default is already italic, so no wrap).
fn variant_cmd(v: &str) -> Option<&'static str> {
    Some(match v {
        "normal" => "\\mathrm",
        "bold" => "\\mathbf",
        "bold-italic" => "\\boldsymbol",
        "double-struck" => "\\mathbb",
        "script" | "bold-script" | "calligraphic" | "bold-calligraphic" => "\\mathscr",
        "fraktur" | "bold-fraktur" => "\\mathfrak",
        "sans-serif" | "bold-sans-serif" | "sans-serif-italic" | "sans-serif-bold-italic" => {
            "\\mathsf"
        }
        "monospace" => "\\mathtt",
        _ => return None,
    })
}

/// Wrap `body` in a font command by grafting the command's `Font` node (so the exact font
/// string the engine uses is preserved). A command that isn't a plain font (e.g. `\boldsymbol`)
/// falls back to the unwrapped body rather than failing.
fn font_wrap(cmd: &str, body: ParseNode) -> ParseNode {
    match parse(&format!("{cmd}{{x}}")).ok().and_then(|mut v| v.pop()) {
        Some(ParseNode::Font { mode, font, .. }) => ParseNode::Font {
            mode,
            font,
            body: Box::new(body),
            loc: None,
        },
        _ => body,
    }
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
) -> ParseNode {
    let base = map_or_degrade(base, depth + 1);
    if under.is_none()
        && let Some(o) = over
    {
        let t = token_text(o);
        if let Some(label) = accent_label(&t)
            && let Some(a) = accent_node(label, base.clone())
        {
            return a;
        }
        if is_top_brace(&t)
            && let Some(b) = brace_node(base.clone(), true)
        {
            return b;
        }
    }
    if over.is_none()
        && let Some(u) = under
    {
        let t = token_text(u);
        if is_bottom_brace(&t)
            && let Some(b) = brace_node(base.clone(), false)
        {
            return b;
        }
        // A rule under the base, not a script. Underlining is how a great many maths
        // texts write vectors and matrices, and `<munder>` carries it as a plain `_`
        // — which, left to `sup_sub` below, renders as a *subscript*: every `a` in the
        // book came out as `a_`.
        if is_underline(&t)
            && let Some(l) = line_node("\\underline", base.clone())
        {
            return l;
        }
    }
    let sup = over.map(|o| map_or_degrade(o, depth + 1));
    let sub = under.map(|u| map_or_degrade(u, depth + 1));
    sup_sub(base, sup, sub)
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
/// Whether an under-glyph is a **rule** rather than a script — the underline (single or
/// doubled) that marks a vector or matrix in most maths typography.
///
/// The over-side equivalent lives in [`accent_label`], which maps `‾`/`¯` to `\bar`; there
/// is no matching accent for the under-side, so it needs naming here. Kept narrow on
/// purpose: anything that could be a genuine limit or index must fall through to the script
/// path.
fn is_underline(under: &str) -> bool {
    let t = under.trim();
    !t.is_empty()
        && t.chars().all(|c| {
            matches!(
                c,
                '_'                 // LOW LINE, what MathML exporters emit
                | '\u{0332}'        // combining low line
                | '\u{0333}'        // combining double low line (matrix notation)
                | '\u{2017}'        // double low line
                | '\u{FF3F}' // fullwidth low line
            )
        })
}

/// A `\underline` / `\overline` rule wrapped around `base`, built the same way as
/// [`accent_node`]: parse the command once and swap in the real body.
fn line_node(label: &str, base: ParseNode) -> Option<ParseNode> {
    match parse(&format!("{label}{{x}}")).ok()?.into_iter().next()? {
        ParseNode::Underline { .. } => Some(ParseNode::Underline {
            mode: Mode::Math,
            body: Box::new(base),
            loc: None,
        }),
        ParseNode::Overline { .. } => Some(ParseNode::Overline {
            mode: Mode::Math,
            body: Box::new(base),
            loc: None,
        }),
        _ => None,
    }
}

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
/// An unrecognised delimiter degrades to `.` (an invisible fence) rather than sinking the
/// equation; an empty `separators=""` means no separators.
fn fenced(node: NodeRef<Node>, e: &scraper::node::Element, depth: u16) -> ParseNode {
    let left = fence_delim(e.attr("open").unwrap_or("(")).unwrap_or_else(|| ".".into());
    let right = fence_delim(e.attr("close").unwrap_or(")")).unwrap_or_else(|| ".".into());
    // Present-but-empty `separators` → no separator; absent → the default comma.
    let sep = match e.attr("separators") {
        Some(s) => s.chars().find(|c| !c.is_whitespace()),
        None => Some(','),
    };
    let mut body = Vec::new();
    for (i, &c) in elem_children(node).iter().enumerate() {
        if i > 0
            && let Some(sep) = sep
        {
            body.push(ParseNode::Atom {
                mode: Mode::Math,
                family: AtomFamily::Punct,
                text: sep.to_string(),
                loc: None,
            });
        }
        body.push(map_or_degrade(c, depth + 1));
    }
    ParseNode::LeftRight {
        mode: Mode::Math,
        body,
        left,
        right,
        right_color: None,
        loc: None,
    }
}

/// `<mtable>` → an `Array` node — a matrix, or the outer right|left layout table InDesign
/// wraps around a display equation. Each `<mtd>` becomes a cell (an empty cell → an empty
/// group); per-column alignment comes from `columnalign` (a cell's own overrides the table's,
/// with the last table value repeating per the MathML rule). Delimiters, when present, come
/// from the surrounding `<mfenced>`. `None` only when there are no rows/cells at all — the
/// caller then degrades (renders whatever children exist).
fn mtable(node: NodeRef<Node>, e: &scraper::node::Element, depth: u16) -> Option<ParseNode> {
    if depth >= MAX_DEPTH {
        return None;
    }
    let rows: Vec<NodeRef<Node>> = elem_children(node)
        .into_iter()
        .filter(|r| {
            r.value()
                .as_element()
                .is_some_and(|el| local(el.name()) == "mtr")
        })
        .collect();
    if rows.is_empty() {
        return None;
    }

    let mut body: Vec<Vec<ParseNode>> = Vec::with_capacity(rows.len());
    let mut ncols = 0usize;
    // The first row's per-cell `columnalign` seeds each column's alignment.
    let mut cell_aligns: Vec<Option<String>> = Vec::new();
    for (ri, &row) in rows.iter().enumerate() {
        let mut out_row = Vec::new();
        for &cell in elem_children(row).iter() {
            if cell.value().as_element().map(|c| local(c.name())) != Some("mtd") {
                continue;
            }
            out_row.push(ord(map_children_degraded(cell, depth + 1)));
            if ri == 0 {
                cell_aligns.push(
                    cell.value()
                        .as_element()
                        .and_then(|c| c.attr("columnalign"))
                        .map(str::to_string),
                );
            }
        }
        ncols = ncols.max(out_row.len());
        body.push(out_row);
    }
    if ncols == 0 {
        return None;
    }

    let table_aligns: Vec<&str> = e
        .attr("columnalign")
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    let cols = (0..ncols)
        .map(|c| {
            let a = cell_aligns
                .get(c)
                .and_then(Option::as_deref)
                .or_else(|| table_aligns.get(c).or_else(|| table_aligns.last()).copied())
                .unwrap_or("center");
            AlignSpec {
                align_type: AlignType::Align,
                align: Some(align_letter(a).to_string()),
                pregap: None,
                postgap: None,
            }
        })
        .collect();

    Some(ParseNode::Array {
        mode: Mode::Math,
        body,
        row_gaps: Vec::new(),
        hlines_before_row: Vec::new(),
        cols: Some(cols),
        col_separation_type: None,
        hskip_before_and_after: None,
        add_jot: None,
        arraystretch: 1.0,
        tags: None,
        leqno: None,
        is_cd: None,
        loc: None,
    })
}

/// The engine's one-letter column alignment for a MathML `columnalign` token.
fn align_letter(a: &str) -> &'static str {
    match a.trim() {
        "left" => "l",
        "right" => "r",
        _ => "c",
    }
}

/// Promote a group that is exactly `OPEN … CLOSE` — a matched pair of delimiter `<mo>`s
/// (`|`, `‖`, `(`, `[`, …) around *tall* content (a fraction, matrix, radical, or stacked
/// script) — into a stretchy `\left…\right` so the delimiters grow with the content. Bars
/// around short content stay literal (avoids over-tall bars); `None` when it doesn't apply.
fn promote_delims(node: NodeRef<Node>, depth: u16) -> Option<ParseNode> {
    let kids = elem_children(node);
    if kids.len() < 3 {
        return None;
    }
    let left = mo_fence(*kids.first()?)?;
    let right = mo_fence(*kids.last()?)?;
    let middle = &kids[1..kids.len() - 1];
    if !middle.iter().any(|&m| is_tall(m)) {
        return None;
    }
    let body = middle
        .iter()
        .map(|&m| map_or_degrade(m, depth + 1))
        .collect();
    Some(ParseNode::LeftRight {
        mode: Mode::Math,
        body,
        left,
        right,
        right_color: None,
        loc: None,
    })
}

/// The engine delimiter form of an `<mo>` whose glyph is a fence character, else `None`.
fn mo_fence(node: NodeRef<Node>) -> Option<String> {
    let e = node.value().as_element()?;
    (local(e.name()) == "mo").then(|| fence_delim(&token_text(node)))?
}

/// Whether an element renders taller than one line — the trigger for stretchy delimiters.
fn is_tall(node: NodeRef<Node>) -> bool {
    match node.value().as_element().map(|e| local(e.name())) {
        Some(
            "mfrac" | "mtable" | "msqrt" | "mroot" | "msubsup" | "munderover" | "mover" | "munder",
        ) => true,
        Some("mrow" | "mstyle" | "mpadded" | "math" | "mphantom" | "semantics") => {
            elem_children(node).iter().any(|&c| is_tall(c))
        }
        _ => false,
    }
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
        "⋮" => "\\vdots ",
        "⋱" => "\\ddots ",
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
        "⊙" => "\\odot ",
        "∘" => "\\circ ",
        "∙" | "•" => "\\bullet ",
        "⋆" => "\\star ",
        "†" => "\\dagger ",
        "‡" => "\\ddagger ",
        "′" => "'",
        "″" => "''",
        "‴" => "'''",
        "∠" => "\\angle ",
        "⊥" => "\\perp ",
        "∣" => "\\mid ",
        "∥" => "\\parallel ",
        "←" => "\\leftarrow ",
        "↔" => "\\leftrightarrow ",
        "↑" => "\\uparrow ",
        "↓" => "\\downarrow ",
        "⇐" => "\\Leftarrow ",
        "≪" => "\\ll ",
        "≫" => "\\gg ",
        "≃" => "\\simeq ",
        "≐" => "\\doteq ",
        "≺" => "\\prec ",
        "≻" => "\\succ ",
        "⪯" => "\\preceq ",
        "⪰" => "\\succeq ",
        "⊤" => "\\top ",
        "⊢" => "\\vdash ",
        "⊨" => "\\models ",
        "⊳" => "\\triangleright ",
        "⊲" => "\\triangleleft ",
        "ℓ" => "\\ell ",
        "ℏ" => "\\hbar ",
        "ℵ" => "\\aleph ",
        "℘" => "\\wp ",
        "ℑ" => "\\Im ",
        "ℜ" => "\\Re ",
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
    fn empty_mtext_gap_does_not_fail_the_equation() {
        // Publishers drop an empty/whitespace `<mtext>` between words (`for` ⋯ `y`) as a
        // spacing gap — it must map to a space, not sink the whole equation to the raster.
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><mtext>for</mtext><mtext></mtext><mi>y</mi></math>".into(),
        ));
        assert!(d.contains("Text"), "the `for` text still maps: {d}");
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

    /// Underlining is how most maths texts write a vector or matrix, and MathML carries
    /// it as `<munder>` with a plain `_`. Treated as a script it renders as a *subscript*
    /// — a whole book of vectors came out as `a_`, `b_`, `A_B_ = B_A_`. Both spellings
    /// here are taken verbatim from a published EPUB.
    #[test]
    fn mathml_under_line_is_a_rule_not_a_subscript() {
        for src in [
            r#"<math><munder><mi>a</mi><mo stretchy="true">_</mo></munder></math>"#,
            r#"<math><munder underaccent="false"><mrow><mi>a</mi></mrow><mo>_</mo></munder></math>"#,
        ] {
            let d = nodes_of(MarkupSource::PresentationMathml(src.into()));
            assert!(d.contains("Underline"), "under-line → rule: {d}");
            assert!(
                !d.contains("SupSub"),
                "an underline must not become a script: {d}"
            );
        }
    }

    /// The narrowness matters: a real limit under an operator is a script, not a rule.
    #[test]
    fn mathml_under_limit_is_still_a_script() {
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><munder><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow></munder></math>"
                .into(),
        ));
        assert!(!d.contains("Underline"), "a limit is not a rule: {d}");
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
    fn content_mathml_falls_back_to_none() {
        assert!(
            to_nodes(&MarkupSource::ContentMathml("<math><apply/></math>".into())).is_none(),
            "content MathML not yet mapped → None"
        );
    }

    #[test]
    fn mtable_builds_a_real_array() {
        // A 2×2 matrix inside stretchy parens — the delimiters come from the `<mfenced>`, the
        // grid from the `<mtable>`. Must typeset (an `Array` in a `LeftRight`), not raster.
        let d = nodes_of(MarkupSource::PresentationMathml(
            r#"<math><mfenced open="(" close=")" separators=""><mtable><mtr><mtd><mi>a</mi></mtd><mtd><mi>b</mi></mtd></mtr><mtr><mtd><mi>c</mi></mtd><mtd><mi>d</mi></mtd></mtr></mtable></mfenced></math>"#.into(),
        ));
        assert!(
            d.contains("Array") && d.contains("LeftRight"),
            "matrix → an Array inside a stretchy delimiter group: {d}"
        );
    }

    #[test]
    fn mtable_empty_cells_and_dots_do_not_fail() {
        // Matrices carry blank cells (`<mtd/>`) and `⋮`/`⋱` fillers — none may sink the grid.
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><mtable><mtr><mtd><mn>1</mn></mtd><mtd/></mtr><mtr><mtd><mo>⋮</mo></mtd><mtd><mo>⋱</mo></mtd></mtr></mtable></math>".into(),
        ));
        assert!(
            d.contains("Array"),
            "blank cells and dots still build an array: {d}"
        );
    }

    #[test]
    fn mtable_per_column_alignment_from_columnalign() {
        // The outer right|left layout table InDesign wraps a display equation in: the first
        // cell's `columnalign` seeds the right column, the table default seeds the rest.
        let d = nodes_of(MarkupSource::PresentationMathml(
            r#"<math><mtable columnalign="left"><mtr><mtd columnalign="right"><mi>y</mi></mtd><mtd><mo>=</mo><mi>x</mi></mtd></mtr></mtable></math>"#.into(),
        ));
        assert!(
            d.contains("Array") && d.contains("\"r\"") && d.contains("\"l\""),
            "right|left column alignment carries into the array: {d}"
        );
    }

    // ---- Layer A: robustness ----

    #[test]
    fn mathvariant_maps_to_font_commands() {
        // Blackboard/bold variants (pervasive in probability texts: ℝ, bold vectors) must
        // carry into the engine's font, not render as a plain letter.
        let bb = nodes_of(MarkupSource::PresentationMathml(
            r#"<math><mstyle mathvariant="double-struck"><mi>R</mi></mstyle></math>"#.into(),
        ));
        assert!(
            bb.contains("Font") && bb.contains("mathbb"),
            "double-struck → \\mathbb: {bb}"
        );
        let bold = nodes_of(MarkupSource::PresentationMathml(
            r#"<math><mi mathvariant="bold">v</mi></math>"#.into(),
        ));
        assert!(
            bold.contains("Font") && bold.contains("mathbf"),
            "bold → \\mathbf: {bold}"
        );
    }

    #[test]
    fn semantics_renders_presentation_and_prefers_tex_annotation() {
        // A bare <semantics> renders its presentation child (not skipped as annotation).
        let pres = nodes_of(MarkupSource::PresentationMathml(
            "<math><semantics><mrow><mi>x</mi></mrow><annotation encoding=\"MathType-MTEF\">BINARY</annotation></semantics></math>".into(),
        ));
        assert!(
            !pres.contains("BINARY"),
            "the MTEF annotation blob must not leak: {pres}"
        );
        // An x-tex annotation is preferred over the presentation tree.
        let tex = nodes_of(MarkupSource::PresentationMathml(
            r#"<math><semantics><mn>0</mn><annotation encoding="application/x-tex">\frac{1}{2}</annotation></semantics></math>"#.into(),
        ));
        assert!(
            tex.contains("GenFrac"),
            "the application/x-tex LaTeX annotation is used: {tex}"
        );
    }

    #[test]
    fn invisible_operators_are_dropped() {
        // U+2061 function-application (MathType inserts it before an argument) carries no ink.
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><mi>P</mi><mo>\u{2061}</mo><mi>A</mi></math>".into(),
        ));
        assert!(
            !d.contains('\u{2061}'),
            "the invisible operator must not reach the tree: {d}"
        );
        assert!(is_invisible_op("\u{2062}"), "invisible-times recognised");
    }

    #[test]
    fn mphantom_is_invisible() {
        let d = nodes_of(MarkupSource::PresentationMathml(
            "<math><mphantom><mi>x</mi></mphantom></math>".into(),
        ));
        assert!(
            d.contains("Phantom"),
            "mphantom → an invisible Phantom node: {d}"
        );
    }

    #[test]
    fn unknown_element_degrades_to_its_children_not_none() {
        // An element we don't map (mmultiscripts) must not sink the equation — render its
        // children so the equation still typesets instead of falling to the raster.
        let d = to_nodes(&MarkupSource::PresentationMathml(
            "<math><mmultiscripts><mi>X</mi><mn>1</mn><none/></mmultiscripts></math>".into(),
        ));
        assert!(d.is_some(), "unknown element degrades, not None");
    }

    #[test]
    fn tall_bars_promote_to_stretchy_delimiters() {
        // ‖dx/dy‖ / |frac|: bars around tall content grow via \left…\right.
        let tall = nodes_of(MarkupSource::PresentationMathml(
            "<math><mo>|</mo><mfrac><mi>a</mi><mi>b</mi></mfrac><mo>|</mo></math>".into(),
        ));
        assert!(
            tall.contains("LeftRight") && tall.contains("GenFrac"),
            "bars around a fraction stretch: {tall}"
        );
        // Short content keeps literal bars (no over-tall delimiters).
        let short = nodes_of(MarkupSource::PresentationMathml(
            "<math><mo>|</mo><mi>x</mi><mo>|</mo></math>".into(),
        ));
        assert!(
            !short.contains("LeftRight"),
            "bars around a single symbol stay literal: {short}"
        );
    }

    #[test]
    fn mfenced_empty_separators_inserts_none() {
        let d = nodes_of(MarkupSource::PresentationMathml(
            r#"<math><mfenced open="(" close=")" separators=""><mi>x</mi><mi>y</mi></mfenced></math>"#.into(),
        ));
        assert!(
            d.contains("LeftRight") && !d.contains("text: \",\""),
            "separators=\"\" → no comma between children: {d}"
        );
    }
}
