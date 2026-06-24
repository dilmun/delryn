//! XHTML → structured block model. Used by EPUB (and future HTML-based
//! formats) to drive the rich typography engine. Produces headings, styled
//! paragraphs, lists, blockquotes, and code blocks rather than flat text.

use std::sync::OnceLock;

use ego_tree::NodeRef;
use regex::Regex;
use scraper::{Html, Node};

use super::{Anchor, Block, CalloutKind, Inline, Span, TableCell};

/// Parse a section's XHTML into a list of reflowable blocks.
pub fn parse_blocks(xhtml: &str) -> Vec<Block> {
    let xhtml = expand_self_closing(xhtml);
    let doc = Html::parse_document(&xhtml);
    let body = doc
        .tree
        .root()
        .descendants()
        .find(|n| matches!(n.value(), Node::Element(e) if e.name() == "body"));

    let mut out = Vec::new();
    let root = body.unwrap_or_else(|| doc.tree.root());
    walk_children(root, &Ctx::default(), &mut out);
    out
}

/// EPUB content is XHTML, where `<span id="x"/>` is self-closing. The HTML5
/// parser instead reads it as an *unclosed* `<span>` that swallows every
/// following sibling — collapsing whole sections (headings, paragraphs, code)
/// into one inline blob. Rewrite self-closing tags of non-void elements to
/// explicit empty pairs so the document structure survives. Void elements
/// (`<br/>`, `<img/>`, …) are valid self-closing in HTML and left as-is.
fn expand_self_closing(xhtml: &str) -> std::borrow::Cow<'_, str> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // <name attrs… /> — attrs may hold quoted `>`/`/`, so consume quotes whole.
        Regex::new(r#"<([A-Za-z][\w:-]*)((?:"[^"]*"|'[^']*'|[^>"'])*?)\s*/>"#).unwrap()
    });
    re.replace_all(xhtml, |c: &regex::Captures| {
        let name = &c[1];
        if is_void_element(name) {
            c[0].to_string()
        } else {
            format!("<{name}{}></{name}>", &c[2])
        }
    })
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

#[derive(Default, Clone)]
struct Ctx {
    indent: u8,
    quote: bool,
}

fn is_block(node: NodeRef<Node>) -> bool {
    match node.value() {
        // Real figure/cover images render block-level; math/icon images stay
        // inline (handled in collect_inline).
        Node::Element(e) if e.name() == "img" => is_real_image(e),
        Node::Element(e) => matches!(
            e.name(),
            "p" | "div"
                | "section"
                | "article"
                | "aside"
                | "header"
                | "footer"
                | "main"
                | "figure"
                | "figcaption"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "ul"
                | "ol"
                | "li"
                | "blockquote"
                | "pre"
                | "hr"
                | "table"
                | "thead"
                | "tbody"
                | "tr"
                | "td"
                | "th"
        ),
        _ => false,
    }
}

/// A "real" image is a figure/cover — not a math equation or a UI icon.
fn is_real_image(e: &scraper::node::Element) -> bool {
    let alt = e.attr("alt").unwrap_or("");
    let src = e.attr("src").unwrap_or("");
    !delryn_model::math::is_math(alt) && !is_icon_src(src)
}

fn is_icon_src(src: &str) -> bool {
    let s = src.to_lowercase();
    [
        "warning", "info", "tip", "note", "pencil", "key", "question", "icon", "leanpub_",
    ]
    .iter()
    .any(|k| s.contains(k))
}

/// Map a small inline UI icon (by its `alt`/`src`) to a themed, single-width
/// Unicode glyph — so list checks and admonition markers (Tip / Warning /
/// Remember / Technical Stuff …) render as a symbol rather than `[tip]` text.
/// Text-presentation code points only (no colour emoji). `None` for non-icons.
fn icon_glyph(alt: &str, src: &str) -> Option<char> {
    let key = format!("{} {}", alt, src.rsplit('/').next().unwrap_or(src)).to_ascii_lowercase();
    let has = |w: &str| key.contains(w);
    Some(if has("check") || has("tick") {
        '✓'
    } else if has("warning") || has("caution") || has("danger") {
        '△'
    } else if has("tip") || has("hint") {
        '✲'
    } else if has("remember") {
        '⚑'
    } else if has("technical") || has("geek") || has("nerd") {
        '※'
    } else if has("note") || has("info") {
        'ⓘ'
    } else {
        return None;
    })
}

/// Iterate children, grouping loose inline content into implicit paragraphs and
/// recursing into block-level elements.
fn walk_children(parent: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let mut inline: Vec<Span> = Vec::new();
    for child in parent.children() {
        if is_block(child) {
            flush(&mut inline, ctx, out);
            block_element(child, ctx, out);
        } else {
            collect_inline(child, Inline::default(), &mut inline);
        }
    }
    flush(&mut inline, ctx, out);
}

fn flush(inline: &mut Vec<Span>, ctx: &Ctx, out: &mut Vec<Block>) {
    if inline.iter().any(|s| !s.text.trim().is_empty()) {
        superscript_math_exponents(inline);
        out.push(Block::Para {
            spans: std::mem::take(inline),
            indent: ctx.indent,
            quote: ctx.quote,
            marker: None,
        });
    } else {
        inline.clear();
    }
}

fn block_element(node: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let Node::Element(e) = node.value() else {
        return;
    };
    match e.name() {
        // Admonition / callout containers (class or epub:type note/tip/warning/…),
        // checked first so a `<blockquote class="note">` becomes a callout rather
        // than a plain quote.
        "div" | "section" | "aside" | "blockquote" if callout_kind(e).is_some() => {
            emit_callout(node, callout_kind(e).unwrap(), ctx, out);
        }
        // Footnote / endnote definitions (epub:type or class), kept as a distinct
        // block so the renderer can set them apart and (later) jump to them.
        "div" | "section" | "aside" | "p" | "li" if footnote_label(e).is_some() => {
            let label = footnote_label(e).unwrap();
            let mut blocks = Vec::new();
            walk_children(
                node,
                &Ctx {
                    indent: ctx.indent,
                    quote: false,
                },
                &mut blocks,
            );
            if !blocks.is_empty() {
                out.push(Block::Footnote { label, blocks });
            }
        }
        // Display (block) math backed by an image renders as the *image* (high
        // fidelity), with the math Unicode as the alt for the no-graphics
        // fallback. Inline math stays inline (converted to Unicode).
        "p" | "div" if display_math_image(node).is_some() => {
            let (src, alt) = display_math_image(node).unwrap();
            out.push(Block::Image {
                src,
                alt,
                data: Vec::new(),
                caption: Vec::new(),
            });
        }
        // Code listings — `<pre>`, a styled code container (lstlisting /
        // ProgramCode / …), or a block holding a multi-line `<code>` whose lines
        // are split by `<br/>` (e.g. `<p class="Code"><code>…<br/>…</code></p>`).
        // Recognised before the paragraph/heading handling so the line structure
        // survives instead of collapsing into flowing prose.
        _ if is_code_block(e, node) => {
            let lines = strip_line_numbers(trim_blank_edges(code_lines(node).into_iter()));
            if !lines.is_empty() {
                out.push(Block::Code {
                    lang: detect_lang(node),
                    lines,
                });
            }
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = e.name().as_bytes()[1] - b'0';
            let mut spans = Vec::new();
            for c in node.children() {
                collect_inline(c, Inline::default(), &mut spans);
            }
            if spans.iter().any(|s| !s.text.trim().is_empty()) {
                out.push(Block::Heading { level, spans });
            }
        }
        "p" => {
            let mut spans = Vec::new();
            for c in node.children() {
                collect_inline(c, Inline::default(), &mut spans);
            }
            if spans.iter().any(|s| !s.text.trim().is_empty()) {
                superscript_math_exponents(&mut spans);
                out.push(Block::Para {
                    spans,
                    indent: ctx.indent,
                    quote: ctx.quote,
                    marker: None,
                });
            }
        }
        "ul" | "ol" => {
            let ordered = e.name() == "ol";
            let mut n: usize = e
                .attr("start")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(1);
            for c in node.children() {
                if matches!(c.value(), Node::Element(le) if le.name() == "li") {
                    let marker = if ordered {
                        format!("{n}. ")
                    } else {
                        "• ".to_string()
                    };
                    n += 1;
                    list_item(c, ctx, marker, out);
                }
            }
        }
        "blockquote" => {
            let inner = Ctx {
                indent: ctx.indent,
                quote: true,
            };
            walk_children(node, &inner, out);
        }
        "hr" => out.push(Block::Rule),
        "img" => out.push(Block::Image {
            src: e.attr("src").unwrap_or("").to_string(),
            alt: e.attr("alt").unwrap_or("").to_string(),
            data: Vec::new(),
            caption: Vec::new(),
        }),
        // Aside/callout tables (icon cell + content cell): the icon image's src
        // classifies the admonition; the content cell becomes the callout body.
        "table" if aside_icon_src(node).is_some() => {
            let kind = aside_kind_from_icon(&aside_icon_src(node).unwrap_or_default());
            let mut content = Vec::new();
            for cell in content_cells(node) {
                walk_children(cell, ctx, &mut content);
            }
            if !content.is_empty() {
                out.push(Block::Callout {
                    kind,
                    title: None,
                    blocks: content,
                });
            }
        }
        // Real data tables → a structured Block::Table (aligned column layout).
        "table" => {
            if let Some(table) = parse_table(node) {
                out.push(table);
            }
        }
        // A stray cell outside a recognised table: degrade to a paragraph.
        "td" | "th" => {
            let mut spans = Vec::new();
            for c in node.children() {
                collect_inline(c, Inline::default(), &mut spans);
            }
            if spans.iter().any(|s| !s.text.trim().is_empty()) {
                superscript_math_exponents(&mut spans);
                out.push(Block::Para {
                    spans,
                    indent: ctx.indent,
                    quote: ctx.quote,
                    marker: None,
                });
            }
        }
        // Generic containers: recurse.
        _ => walk_children(node, ctx, out),
    }
}

fn list_item(node: NodeRef<Node>, ctx: &Ctx, marker: String, out: &mut Vec<Block>) {
    // Collect the item's content (handles `<li>text`, `<li><p>…`, `<li><div>…`,
    // and nested lists) then attach the marker to its first paragraph.
    let mut item: Vec<Block> = Vec::new();
    let inner = Ctx {
        indent: ctx.indent + 1,
        quote: ctx.quote,
    };
    walk_children(node, &inner, &mut item);

    if let Some(Block::Para {
        marker: m, indent, ..
    }) = item.iter_mut().find(|b| matches!(b, Block::Para { .. }))
    {
        *m = Some(marker);
        *indent = ctx.indent;
    }
    out.append(&mut item);
}

fn collect_inline(node: NodeRef<Node>, style: Inline, out: &mut Vec<Span>) {
    match node.value() {
        Node::Text(t) => out.push(Span {
            text: t.text.to_string(),
            style,
            anchor: None,
        }),
        Node::Element(e) => {
            // Links carry a navigation anchor (footnote ref / cross-ref / URL);
            // collect the inner runs, then stamp the anchor on each.
            if e.name() == "a" {
                let astyle = Inline {
                    link: true,
                    ..style
                };
                let mut inner = Vec::new();
                for c in node.children() {
                    collect_inline(c, astyle, &mut inner);
                }
                if let Some(anchor) = link_anchor(e) {
                    for s in inner.iter_mut().filter(|s| s.anchor.is_none()) {
                        s.anchor = Some(anchor.clone());
                    }
                }
                out.append(&mut inner);
                return;
            }
            let style = match e.name() {
                "em" | "i" | "cite" | "var" | "dfn" => Inline {
                    italic: true,
                    ..style
                },
                "strong" | "b" => Inline {
                    bold: true,
                    ..style
                },
                "code" | "kbd" | "samp" | "tt" => Inline {
                    code: true,
                    ..style
                },
                "br" => {
                    out.push(Span::plain(" "));
                    return;
                }
                // Images can't render in the terminal yet (graphics protocol is
                // a later task); show the alt text or a kind-of-image symbol so
                // icons/figures aren't silently dropped.
                "img" => {
                    let alt = e.attr("alt").unwrap_or("");
                    let src = e.attr("src").unwrap_or("");
                    let text = if let Some(g) = icon_glyph(alt, src) {
                        // A small UI icon (check / tip / warning / remember / …)
                        // shown as a themed glyph instead of "[tip]" text.
                        g.to_string()
                    } else if delryn_model::math::is_math(alt) {
                        math_unicode(alt)
                    } else if is_placeholder_alt(alt) {
                        // Inline image with no useful alt (often math the converter
                        // rasterised) — a quiet marker beats dumping "[images]".
                        "▢".to_string()
                    } else {
                        img_label(e)
                    };
                    out.push(Span {
                        text,
                        style: Inline {
                            italic: true,
                            ..style
                        },
                        anchor: None,
                    });
                    return;
                }
                _ => style,
            };
            // Math runs (tagged by a math class — e.g. InDesign MathTools, or any
            // `*math*` class) carry a flag so math-only fixups stay out of prose.
            let style = if is_math_class(e) {
                Inline {
                    math: true,
                    ..style
                }
            } else {
                style
            };
            for c in node.children() {
                collect_inline(c, style, out);
            }
        }
        _ => {}
    }
}

/// Does the element carry `want` as one of its space-separated class tokens
/// (case-insensitive)?
fn class_has_token(e: &scraper::node::Element, want: &str) -> bool {
    e.attr("class")
        .is_some_and(|c| c.split_whitespace().any(|t| t.eq_ignore_ascii_case(want)))
}

/// Whether an element's class marks it as math content. Covers the common
/// conventions (InDesign `…MathTools…Math_…`, MathJax/MathML wrappers, generic
/// `math`/`equation` classes) by matching the substring — publisher-agnostic.
fn is_math_class(e: &scraper::node::Element) -> bool {
    e.attr("class").is_some_and(|c| {
        let c = c.to_ascii_lowercase();
        c.contains("math") || c.contains("equation")
    })
}

/// Conservative, math-scoped exponent fix: a run of digits immediately following
/// a closing `)`/`]` inside math is a power, so super-script it
/// (`(x−μ)2` → `(x−μ)²`). Deliberately narrow — ambiguous cases like `σ2`/`μ3`
/// (exponent vs subscript index) are left flat rather than guessed wrong, and
/// non-math text is never touched. Publishers that flatten scripts to plain
/// glyphs (no sub/sup tag) lose the rest irrecoverably.
fn superscript_math_exponents(spans: &mut [Span]) {
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

/// A non-`<pre>` block that is really a code listing, by its class. Covers the
/// common publisher conventions (notably Springer/Apress `ProgramCode`).
fn is_code_container(e: &scraper::node::Element) -> bool {
    matches!(e.name(), "div" | "section" | "p")
        && [
            "ProgramCode",
            "SourceCode",
            "CodeBlock",
            "code",
            "sourceCode",
            // LaTeX `listings` package + DocBook program listings.
            "lstlisting",
            "listing",
            "programlisting",
        ]
        .iter()
        .any(|t| class_has_token(e, t))
}

/// Whether an element should render as a code block: a `<pre>`, a styled code
/// container (by class), or a block holding a multi-line `<code>`.
fn is_code_block(e: &scraper::node::Element, node: NodeRef<Node>) -> bool {
    e.name() == "pre" || is_code_container(e) || has_multiline_code(node)
}

/// A block whose direct `<code>` child spans several lines (its lines split by
/// `<br/>`) — a code listing written without `<pre>`, e.g.
/// `<p class="Code"><code>line1<br/>line2</code></p>`. The `<br/>` requirement
/// keeps short inline `<code>` snippets out.
fn has_multiline_code(node: NodeRef<Node>) -> bool {
    node.children().any(|c| {
        matches!(c.value(), Node::Element(e) if e.name() == "code")
            && c.descendants()
                .any(|d| matches!(d.value(), Node::Element(e) if e.name() == "br"))
    })
}

/// Code lines from a styled code container. When the source wraps each line in a
/// `<div class="FixedLine">` (Springer/Apress), one line per such div; otherwise
/// fall back to splitting the concatenated text on newlines.
fn code_lines(node: NodeRef<Node>) -> Vec<String> {
    let fixed: Vec<String> = node
        .descendants()
        .filter(|n| matches!(n.value(), Node::Element(e) if class_has_token(e, "FixedLine")))
        .map(code_text)
        .collect();
    let raw = if fixed.is_empty() {
        code_text(node)
    } else {
        fixed.join("\n")
    };
    // Normalise the spacing publishers use in code: non-breaking spaces for
    // indentation back to real spaces, and drop zero-width spaces.
    let normalized = raw.replace('\u{a0}', " ").replace('\u{200b}', "");
    // Collapse runs of blank lines (publishers often emit several `<br/>` between
    // code lines) down to a single blank.
    let mut out = Vec::new();
    let mut prev_blank = false;
    for l in normalized.split('\n') {
        let l = l.trim_end().to_string();
        let blank = l.is_empty();
        if blank && prev_blank {
            continue;
        }
        prev_blank = blank;
        out.push(l);
    }
    out
}

/// Concatenate descendant text, turning `<br/>` into a newline — so code laid out
/// with `<br/>` line breaks (instead of `<pre>`) keeps its line structure.
fn code_text(node: NodeRef<Node>) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        match d.value() {
            Node::Text(t) => s.push_str(&t.text),
            Node::Element(e) if e.name() == "br" => s.push('\n'),
            _ => {}
        }
    }
    s
}

/// Some books bake line numbers into the code text ("1 import std;"). When most
/// lines start with their own 1-based index, strip those so our gutter is the
/// single source of line numbers.
fn strip_line_numbers(lines: Vec<String>) -> Vec<String> {
    let mut nonempty = 0usize;
    let mut numbered = 0usize;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.is_empty() {
            continue;
        }
        nonempty += 1;
        if let Some(rest) = t.strip_prefix(&(i + 1).to_string())
            && (rest.is_empty() || rest.starts_with([' ', '\t']))
        {
            numbered += 1;
        }
    }
    if nonempty < 2 || numbered * 4 < nonempty * 3 {
        return lines;
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let t = l.trim_start();
            match t.strip_prefix(&(i + 1).to_string()) {
                Some(rest) => rest.strip_prefix([' ', '\t']).unwrap_or(rest).to_string(),
                None => l,
            }
        })
        .collect()
}

/// A stand-in for an inline image: its alt text, else a plain "[image]" label.
/// (Real image rendering is the graphics-protocol task.)
fn img_label(e: &scraper::node::Element) -> String {
    match e.attr("alt").map(str::trim).filter(|a| !a.is_empty()) {
        Some(alt) => format!("[{alt}]"),
        None => "[image]".to_string(),
    }
}

/// If a block is standalone display math backed by an image, its `(src,
/// Unicode-alt)`. True when it holds a math image and either no other text or a
/// math/equation/display class — so an equation on its own line renders as the
/// image (Unicode alt as fallback), while math mid-sentence stays inline.
fn display_math_image(node: NodeRef<Node>) -> Option<(String, String)> {
    let class_math = matches!(node.value(), Node::Element(e) if e.attr("class").is_some_and(|c| {
        let l = c.to_ascii_lowercase();
        l.contains("math") || l.contains("equation") || l.contains("display")
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
fn math_unicode(alt: &str) -> String {
    if delryn_model::math::is_mathml(alt) {
        crate::mathml::to_unicode(alt)
    } else {
        delryn_model::math::latex_to_unicode(alt)
    }
}

/// An image `alt` with no useful content (empty or a generic placeholder).
fn is_placeholder_alt(alt: &str) -> bool {
    let a = alt.trim();
    a.is_empty() || a.eq_ignore_ascii_case("image") || a.eq_ignore_ascii_case("images")
}

/// Build the navigation [`Anchor`] for an `<a>`: a footnote reference (epub:type
/// noteref, or an internal `#id` that looks like a note), an internal
/// cross-reference, or an external link.
fn link_anchor(e: &scraper::node::Element) -> Option<Anchor> {
    let etype = e.attr("epub:type").unwrap_or("").to_ascii_lowercase();
    let href = e.attr("href")?;
    if etype.contains("noteref") {
        return Some(Anchor::Footnote(href.trim_start_matches('#').to_string()));
    }
    if let Some(id) = href.strip_prefix('#') {
        let low = id.to_ascii_lowercase();
        if low.contains("fn") || low.contains("note") {
            return Some(Anchor::Footnote(id.to_string()));
        }
        return Some(Anchor::CrossRef(id.to_string()));
    }
    Some(Anchor::Link(href.to_string()))
}

/// If a container is a footnote/endnote definition (by `epub:type` or `class`),
/// its label — the digits of its `id`, else the `id`, else `note`.
fn footnote_label(e: &scraper::node::Element) -> Option<String> {
    let etype = e.attr("epub:type").unwrap_or("").to_ascii_lowercase();
    let by_type = ["footnote", "endnote", "rearnote"]
        .iter()
        .any(|k| etype.contains(k));
    let by_class = e.attr("class").is_some_and(|c| {
        c.split([' ', '-', '_']).any(|t| {
            matches!(
                t.to_ascii_lowercase().as_str(),
                "footnote" | "endnote" | "rearnote" | "fn"
            )
        })
    });
    if !by_type && !by_class {
        return None;
    }
    let id = e.attr("id").unwrap_or("");
    let digits: String = id.chars().filter(char::is_ascii_digit).collect();
    Some(if !digits.is_empty() {
        digits
    } else if !id.is_empty() {
        id.to_string()
    } else {
        "note".to_string()
    })
}

/// Classify a container as an admonition by its `class` / `epub:type` tokens.
/// Splits on spaces, hyphens and underscores and matches each segment exactly,
/// so `admonition-warning` is a Warning while `footnote` is *not* a Note.
fn callout_kind(e: &scraper::node::Element) -> Option<CalloutKind> {
    [e.attr("class"), e.attr("epub:type"), e.attr("type")]
        .into_iter()
        .flatten()
        .flat_map(|a| a.split([' ', '-', '_', '\t']))
        .find_map(CalloutKind::from_word)
}

/// Build a [`Block::Callout`] from a container's children (quote context reset —
/// the callout's own border replaces any blockquote styling).
fn emit_callout(node: NodeRef<Node>, kind: CalloutKind, ctx: &Ctx, out: &mut Vec<Block>) {
    let inner_ctx = Ctx {
        indent: ctx.indent,
        quote: false,
    };
    let mut blocks = Vec::new();
    walk_children(node, &inner_ctx, &mut blocks);
    if !blocks.is_empty() {
        out.push(Block::Callout {
            kind,
            title: None,
            blocks,
        });
    }
}

/// The icon image `src` of an aside/callout table, if it is one. The src word
/// (warning/info/tip/…) classifies the admonition in [`aside_kind_from_icon`].
fn aside_icon_src(node: NodeRef<Node>) -> Option<String> {
    let is_aside = matches!(node.value(), Node::Element(e) if e.attr("class").is_some_and(|c| c.contains("aside")));
    if !is_aside {
        return None;
    }
    node.descendants().find_map(|n| match n.value() {
        Node::Element(e) if e.name() == "img" => Some(e.attr("src").unwrap_or("").to_string()),
        _ => None,
    })
}

/// Map an aside icon's `src` filename to a callout kind (info/pencil/question and
/// anything unrecognised fall back to Note).
fn aside_kind_from_icon(src: &str) -> CalloutKind {
    let s = src.to_lowercase();
    if s.contains("warning") || s.contains("caution") || s.contains("danger") {
        CalloutKind::Warning
    } else if s.contains("key") || s.contains("important") {
        CalloutKind::Important
    } else if s.contains("tip") || s.contains("hint") {
        CalloutKind::Tip
    } else {
        CalloutKind::Note
    }
}

/// Parse a `<table>` into a [`Block::Table`]. The first row is the header when
/// it sits in a `<thead>` or is made entirely of `<th>` cells. Cell content is
/// flattened to inline spans. (Nested tables are uncommon in books; their rows
/// are folded into the outer table.)
fn parse_table(node: NodeRef<Node>) -> Option<Block> {
    let is_named =
        |n: &NodeRef<Node>, name: &str| matches!(n.value(), Node::Element(e) if e.name() == name);
    let cells_of = |tr: NodeRef<Node>| -> Vec<TableCell> {
        tr.children()
            .filter(|c| is_named(c, "td") || is_named(c, "th"))
            .map(|cell| {
                let mut spans = Vec::new();
                for c in cell.children() {
                    collect_inline(c, Inline::default(), &mut spans);
                }
                spans
            })
            .collect()
    };

    let mut header: Option<Vec<TableCell>> = None;
    let mut rows: Vec<Vec<TableCell>> = Vec::new();
    for tr in node.descendants().filter(|n| is_named(n, "tr")) {
        let cells = cells_of(tr);
        if cells.is_empty() {
            continue;
        }
        let all_th = tr
            .children()
            .filter(|c| is_named(c, "td") || is_named(c, "th"))
            .all(|c| is_named(&c, "th"));
        let in_thead = tr.ancestors().any(|a| is_named(&a, "thead"));
        if header.is_none() && rows.is_empty() && (all_th || in_thead) {
            header = Some(cells);
        } else {
            rows.push(cells);
        }
    }

    (header.is_some() || !rows.is_empty()).then_some(Block::Table { header, rows })
}

/// Table cells that carry text (i.e. the content cell, not the icon-only cell).
fn content_cells(node: NodeRef<Node>) -> Vec<NodeRef<Node>> {
    node.descendants()
        .filter(|n| matches!(n.value(), Node::Element(e) if matches!(e.name(), "td" | "th")))
        .filter(|cell| {
            cell.descendants()
                .any(|d| matches!(d.value(), Node::Text(t) if !t.text.trim().is_empty()))
        })
        .collect()
}

fn trim_blank_edges(lines: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = lines.collect();
    while v.first().is_some_and(|l| l.trim().is_empty()) {
        v.remove(0);
    }
    while v.last().is_some_and(|l| l.trim().is_empty()) {
        v.pop();
    }
    v
}

/// Detect a code language from a `class="language-xxx"`/`lang-xxx` on the
/// `<pre>` or its child `<code>`.
fn detect_lang(node: NodeRef<Node>) -> Option<String> {
    fn from_class(class: &str) -> Option<String> {
        class.split_whitespace().find_map(|c| {
            c.strip_prefix("language-")
                .or_else(|| c.strip_prefix("lang-"))
                .map(|s| s.to_string())
        })
    }
    for n in node.descendants() {
        if let Node::Element(e) = n.value()
            && let Some(class) = e.attr("class")
            && let Some(lang) = from_class(class)
        {
            return Some(lang);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_lines_of(blocks: &[Block]) -> Option<&Vec<String>> {
        blocks.iter().find_map(|b| match b {
            Block::Code { lines, .. } => Some(lines),
            _ => None,
        })
    }

    fn first_callout(blocks: &[Block]) -> Option<(CalloutKind, &Vec<Block>)> {
        blocks.iter().find_map(|b| match b {
            Block::Callout { kind, blocks, .. } => Some((*kind, blocks)),
            _ => None,
        })
    }

    fn block_text(blocks: &[Block]) -> String {
        blocks
            .iter()
            .map(|b| match b {
                Block::Para { spans, .. } | Block::Heading { spans, .. } => {
                    spans.iter().map(|s| s.text.as_str()).collect::<String>()
                }
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn div_class_becomes_callout() {
        let blocks = parse_blocks(
            r#"<html><body><div class="note"><p>remember this</p></div></body></html>"#,
        );
        let (kind, inner) = first_callout(&blocks).expect("a callout block");
        assert_eq!(kind, CalloutKind::Note);
        assert!(block_text(inner).contains("remember this"));
    }

    #[test]
    fn epub_type_and_compound_class_classify() {
        let warn = parse_blocks(
            r#"<html><body><aside class="admonition-warning"><p>danger</p></aside></body></html>"#,
        );
        assert_eq!(first_callout(&warn).unwrap().0, CalloutKind::Warning);

        let tip =
            parse_blocks(r#"<html><body><div epub:type="tip"><p>handy</p></div></body></html>"#);
        assert_eq!(first_callout(&tip).unwrap().0, CalloutKind::Tip);
    }

    #[test]
    fn blockquote_with_callout_class_is_a_callout_not_a_quote() {
        let blocks = parse_blocks(
            r#"<html><body><blockquote class="important"><p>key point</p></blockquote></body></html>"#,
        );
        assert_eq!(first_callout(&blocks).unwrap().0, CalloutKind::Important);
        // A plain blockquote stays a quote, not a callout.
        let plain = parse_blocks(
            r#"<html><body><blockquote><p>just a quote</p></blockquote></body></html>"#,
        );
        assert!(first_callout(&plain).is_none());
        assert!(
            plain
                .iter()
                .any(|b| matches!(b, Block::Para { quote: true, .. }))
        );
    }

    #[test]
    fn footnote_class_is_not_a_callout() {
        // "footnote" contains "note" — must NOT be misread as a Note callout.
        let blocks = parse_blocks(
            r#"<html><body><div class="footnote"><p>1. a source</p></div></body></html>"#,
        );
        assert!(first_callout(&blocks).is_none());
    }

    fn first_table(blocks: &[Block]) -> &Block {
        blocks
            .iter()
            .find(|b| matches!(b, Block::Table { .. }))
            .expect("a table block")
    }

    fn cell_text(cell: &[Span]) -> String {
        cell.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn table_with_thead_splits_header_and_rows() {
        let blocks = parse_blocks(
            r#"<html><body><table>
                <thead><tr><th>Name</th><th>Qty</th></tr></thead>
                <tbody>
                  <tr><td>Apples</td><td>12</td></tr>
                  <tr><td>Pears</td><td>3</td></tr>
                </tbody>
            </table></body></html>"#,
        );
        let Block::Table { header, rows } = first_table(&blocks) else {
            unreachable!()
        };
        let h = header.as_ref().expect("header row");
        assert_eq!(cell_text(&h[0]), "Name");
        assert_eq!(cell_text(&h[1]), "Qty");
        assert_eq!(rows.len(), 2);
        assert_eq!(cell_text(&rows[0][0]), "Apples");
        assert_eq!(cell_text(&rows[1][1]), "3");
    }

    #[test]
    fn header_inferred_from_all_th_first_row() {
        // No <thead>, but the first row is all <th>.
        let blocks = parse_blocks(
            r#"<html><body><table>
                <tr><th>A</th><th>B</th></tr>
                <tr><td>1</td><td>2</td></tr>
            </table></body></html>"#,
        );
        let Block::Table { header, rows } = first_table(&blocks) else {
            unreachable!()
        };
        assert!(header.is_some(), "all-<th> first row is the header");
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn headerless_table_is_all_rows() {
        let blocks = parse_blocks(
            r#"<html><body><table>
                <tr><td>1</td><td>2</td></tr>
                <tr><td>3</td><td>4</td></tr>
            </table></body></html>"#,
        );
        let Block::Table { header, rows } = first_table(&blocks) else {
            unreachable!()
        };
        assert!(header.is_none());
        assert_eq!(rows.len(), 2);
    }

    fn first_footnote(blocks: &[Block]) -> Option<&str> {
        blocks.iter().find_map(|b| match b {
            Block::Footnote { label, .. } => Some(label.as_str()),
            _ => None,
        })
    }

    fn first_anchor(blocks: &[Block]) -> Option<&Anchor> {
        blocks.iter().find_map(|b| match b {
            Block::Para { spans, .. } | Block::Heading { spans, .. } => {
                spans.iter().find_map(|s| s.anchor.as_ref())
            }
            _ => None,
        })
    }

    #[test]
    fn footnote_definition_by_epub_type_takes_label_from_id() {
        let blocks = parse_blocks(
            r#"<html><body><aside epub:type="footnote" id="fn7"><p>the source</p></aside></body></html>"#,
        );
        assert_eq!(first_footnote(&blocks), Some("7"));
    }

    #[test]
    fn footnote_definition_by_class() {
        let blocks = parse_blocks(
            r#"<html><body><div class="footnote" id="note-2"><p>see also</p></div></body></html>"#,
        );
        assert_eq!(first_footnote(&blocks), Some("2"));
    }

    #[test]
    fn noteref_link_becomes_footnote_anchor() {
        let blocks = parse_blocks(
            r##"<html><body><p>text<a epub:type="noteref" href="#fn7">7</a></p></body></html>"##,
        );
        assert_eq!(first_anchor(&blocks), Some(&Anchor::Footnote("fn7".into())));
    }

    /// The `(src, alt)` of a display-math image block, if any.
    fn display_math_img(blocks: &[Block]) -> Option<(&str, &str)> {
        blocks.iter().find_map(|b| match b {
            Block::Image { src, alt, .. } => Some((src.as_str(), alt.as_str())),
            _ => None,
        })
    }

    /// First `Block::Code`'s lines, if any.
    fn first_code(blocks: &[Block]) -> Option<&[String]> {
        blocks.iter().find_map(|b| match b {
            Block::Code { lines, .. } => Some(lines.as_slice()),
            _ => None,
        })
    }

    #[test]
    fn icon_images_become_glyphs_not_labels() {
        // Dummies-style marker icons render as a symbol, not "[tip]"/"[check]".
        let blocks = parse_blocks(
            r#"<html><body><p><img alt="check" src="images/check.png"/> Item one</p>
               <p><img alt="" src="images/tip.png"/> A handy tip</p>
               <p><img alt="warning" src="x/warning.png"/> Be careful</p></body></html>"#,
        );
        let text = block_text(&blocks);
        assert!(text.contains('✓'), "check → ✓: {text:?}");
        assert!(text.contains('✲'), "tip → ✲: {text:?}");
        assert!(text.contains('△'), "warning → △: {text:?}");
        assert!(
            !text.contains("[check]") && !text.contains("[tip]"),
            "no label text"
        );
    }

    #[test]
    fn self_closing_span_does_not_swallow_following_blocks() {
        // EPUB XHTML marker spans (`<span id=…/>`) must not collapse the section.
        let blocks = parse_blocks(
            r#"<html><body><section><span id="m"/><h2>Head</h2><p>Body text.</p></section></body></html>"#,
        );
        assert!(
            matches!(blocks.first(), Some(Block::Heading { .. })),
            "heading kept separate: {blocks:?}"
        );
        assert!(
            blocks.iter().any(|b| matches!(b, Block::Para { .. })),
            "paragraph kept separate"
        );
    }

    #[test]
    fn multiline_code_in_p_becomes_a_code_block() {
        // `<p class="Code"><code>…<br/>…</code></p>` (no <pre>) → real code block.
        let blocks = parse_blocks(
            r#"<html><body><p class="Code"><code>#include &lt;iostream&gt;<br/>int main() {<br/>  return 0;<br/>}</code></p></body></html>"#,
        );
        let lines = first_code(&blocks).expect("a code block");
        assert_eq!(lines.len(), 4, "one line per <br/>: {lines:?}");
        assert_eq!(lines[0], "#include <iostream>");
        assert_eq!(lines[3], "}");
    }

    #[test]
    fn lstlisting_div_is_code_and_blank_runs_collapse() {
        // LaTeX listings: <div class="lstlisting"> with <br/> breaks + nbsp spaces.
        let blocks = parse_blocks(
            "<html><body><div class=\"lstlisting\">int\u{a0}x\u{a0}=\u{a0}1;<br/><br/><br/>return\u{a0}x;</div></body></html>",
        );
        let lines = first_code(&blocks).expect("a code block");
        assert_eq!(lines[0], "int x = 1;", "nbsp → space");
        assert_eq!(
            lines,
            ["int x = 1;", "", "return x;"],
            "blank run collapsed"
        );
    }

    #[test]
    fn math_exponent_after_paren_is_superscripted() {
        // InDesign-style per-glyph math spans (contiguous, as in real files): a
        // digit right after `)` is a power.
        let blocks = parse_blocks(
            r#"<html><body><p><span class="_-----MathTools-_Math_Base">(</span><span class="_-----MathTools-_Math_Variable">x</span><span class="_-----MathTools-_Math_Base">)</span><span class="_-----MathTools-_Math_Number">2</span></p></body></html>"#,
        );
        let t = block_text(&blocks);
        assert!(t.contains("(x)²"), "exponent superscripted: {t:?}");
        assert!(!t.contains("(x)2"), "no flat exponent: {t:?}");
    }

    #[test]
    fn prose_digits_after_paren_are_untouched() {
        // No math class → the heuristic must not fire (this is plain prose).
        let blocks = parse_blocks("<html><body><p>(see note 2)3 times</p></body></html>");
        let t = block_text(&blocks);
        assert!(t.contains(")3"), "prose left alone: {t:?}");
    }

    #[test]
    fn ambiguous_variable_digit_left_flat() {
        // `σ2` (variable then digit) is ambiguous (power vs index) → not guessed.
        let blocks = parse_blocks(
            r#"<html><body><p><span class="_-----MathTools-_Math_Variable">σ</span><span class="_-----MathTools-_Math_Number">2</span></p></body></html>"#,
        );
        assert!(block_text(&blocks).contains("σ2"), "left flat");
    }

    #[test]
    fn standalone_math_image_is_a_display_image() {
        // EPUB display math: a math image (LaTeX `\[ … \]` in alt) on its own
        // line renders as the image, with the Unicode as the (fallback) alt.
        let blocks = parse_blocks(
            r#"<html><body><p><img alt="\[\int_0^1 x\,dx\]" src="eq.png"/></p></body></html>"#,
        );
        let (src, alt) = display_math_img(&blocks).expect("a display-math image");
        assert_eq!(src, "eq.png", "renders the actual equation image");
        // The alt is Unicode: \int → ∫, no raw LaTeX.
        assert!(alt.contains('∫'), "unicode alt: {alt:?}");
        assert!(!alt.contains("\\int"), "no raw LaTeX: {alt:?}");
    }

    #[test]
    fn mathml_in_image_alt_renders_unicode_not_tags() {
        // EPUBs converted from OOXML ship math as MathML inside an <img alt="…">,
        // with the inner quotes escaped (scraper decodes them back).
        let raw = r#"<mml:math xmlns:mml="http://www.w3.org/1998/Math/MathML"><mml:mi>Σ</mml:mi></mml:math>"#;
        let alt = raw.replace('"', "&quot;");
        // Standalone → display-math image; its alt is Unicode, never raw tags.
        let block = parse_blocks(&format!(
            r#"<html><body><p><img alt="{alt}" src="e.png"/></p></body></html>"#
        ));
        let (_src, alt_text) =
            display_math_img(&block).expect("display math image from MathML alt");
        assert!(alt_text.contains('Σ'), "alt: {alt_text:?}");
        assert!(!alt_text.contains("<m"), "no raw MathML tags: {alt_text:?}");

        // Inline → unicode within the paragraph, never the raw tags.
        let inline = parse_blocks(&format!(
            r#"<html><body><p>use the <img alt="{alt}" src="e.png"/> sum</p></body></html>"#
        ));
        let text: String = inline
            .iter()
            .filter_map(|b| match b {
                Block::Para { spans, .. } => {
                    Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(text.contains('Σ'), "got: {text:?}");
        assert!(
            !text.contains("mml:") && !text.contains("<m"),
            "no tags: {text:?}"
        );
    }

    #[test]
    fn math_image_amid_text_stays_inline() {
        // Math mid-sentence must NOT become a display block.
        let blocks = parse_blocks(
            r#"<html><body><p>where <img alt="\(x^2\)" src="e.png"/> is the area.</p></body></html>"#,
        );
        assert!(
            display_math_img(&blocks).is_none(),
            "stays inline, not an image"
        );
        // It renders inline (as Unicode) inside one paragraph with its context.
        let text: String = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para { spans, .. } => {
                    Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(
            text.contains("where") && text.contains("area") && !text.contains('['),
            "got: {text:?}"
        );
    }

    #[test]
    fn internal_and_external_links_classify() {
        let cross =
            parse_blocks(r##"<html><body><p><a href="#sec2">see section 2</a></p></body></html>"##);
        assert_eq!(first_anchor(&cross), Some(&Anchor::CrossRef("sec2".into())));

        let ext = parse_blocks(r#"<html><body><p><a href="https://x.dev">x</a></p></body></html>"#);
        assert_eq!(
            first_anchor(&ext),
            Some(&Anchor::Link("https://x.dev".into()))
        );

        // An internal id that looks like a note is treated as a footnote.
        let note = parse_blocks(r##"<html><body><p><a href="#fn9">9</a></p></body></html>"##);
        assert_eq!(first_anchor(&note), Some(&Anchor::Footnote("fn9".into())));
    }

    #[test]
    fn springer_program_code_div_is_a_code_block() {
        // Springer/Apress markup: no <pre>/<code>, lines in FixedLine divs.
        let xhtml = r#"<html><body>
            <p>Example:</p>
            <div class="ProgramCode" id="PC1"><div class="LineGroup">
              <div class="FixedLine">#include &lt;vector></div>
              <div class="FixedLine">int main() {}</div>
            </div></div>
        </body></html>"#;
        let blocks = parse_blocks(xhtml);
        let lines = code_lines_of(&blocks).expect("a code block");
        assert_eq!(
            lines,
            &vec!["#include <vector>".to_string(), "int main() {}".to_string()]
        );
    }

    #[test]
    fn plain_divs_are_not_code() {
        let xhtml = r#"<html><body><div class="Para">just a paragraph</div></body></html>"#;
        assert!(code_lines_of(&parse_blocks(xhtml)).is_none());
    }
}
