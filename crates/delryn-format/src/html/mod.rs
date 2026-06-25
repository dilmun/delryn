//! XHTML → structured block model. Used by EPUB (and future HTML-based
//! formats) to drive the rich typography engine. Produces headings, styled
//! paragraphs, lists, blockquotes, and code blocks rather than flat text.

use ego_tree::NodeRef;
use scraper::{Html, Node};

use super::{Anchor, Block, CalloutKind, ImageWidth, Inline, Span, TableCell};

mod callout;
mod code;
mod dom;
mod inline;
mod math;
mod normalize;
mod semantics;
mod table;
mod toolchain;

use callout::*;
use code::*;
use dom::*;
use inline::*;
use math::*;
use normalize::*;
use semantics::*;
use table::*;
use toolchain::*;

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

/// Every element `id` in a section → a short text locator (its leading visible
/// text), in document order, first id winning on duplicates. The reader builds a
/// book-wide index from these so a cross-reference / citation can be followed to
/// the element it targets (resolved to a display line by the locator text).
pub fn collect_targets(xhtml: &str) -> Vec<(String, String)> {
    let xhtml = expand_self_closing(xhtml);
    let doc = Html::parse_document(&xhtml);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for node in doc.tree.root().descendants() {
        if let Node::Element(e) = node.value()
            && let Some(id) = e.attr("id")
            && !id.is_empty()
            && seen.insert(id.to_string())
        {
            out.push((id.to_string(), leading_text(node, 60)));
        }
    }
    out
}

/// The leading visible text of `node`, whitespace-collapsed and capped to `max`
/// chars — enough for `find_line` to locate the element without walking a whole
/// subtree.
fn leading_text(node: NodeRef<Node>, max: usize) -> String {
    let mut buf = String::new();
    for d in node.descendants() {
        if let Node::Text(t) = d.value() {
            buf.push_str(&t.text);
            if buf.len() > max * 2 {
                break; // enough raw text to fill `max` after collapsing
            }
        }
    }
    buf.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

#[derive(Default, Clone)]
struct Ctx {
    indent: u8,
    quote: bool,
    /// Pack paragraphs tight (no blank line between them) — for list-like
    /// regions: tables of contents and definition lists.
    tight: bool,
}

impl Ctx {
    /// A copy at the same indent with `quote` set (for blockquotes/footnotes).
    fn with_quote(&self, quote: bool) -> Ctx {
        Ctx { quote, ..*self }
    }

    /// A copy that packs paragraphs tight (ToC / definition-list entries).
    fn tightened(&self) -> Ctx {
        Ctx {
            tight: true,
            ..*self
        }
    }

    /// The marker that makes a paragraph count as a tight list item (empty text,
    /// no visible glyph) when `tight`, else none.
    fn item_marker(&self) -> Option<String> {
        self.tight.then(String::new)
    }
}

fn is_block(node: NodeRef<Node>) -> bool {
    match node.value() {
        // Real figure/cover images render block-level; math/icon images stay
        // inline (handled in collect_inline).
        Node::Element(e) if e.name() == "img" => is_real_image(e),
        // Display (block) MathML is a block; inline math stays inline.
        Node::Element(e) if is_math_element(e) => is_display_math(e),
        Node::Element(e) => matches!(
            e.name(),
            "p" | "div"
                | "section"
                | "article"
                | "aside"
                | "header"
                | "footer"
                | "main"
                | "nav"
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
                | "dl"
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

/// Iterate children, grouping loose inline content into implicit paragraphs and
/// recursing into block-level elements.
fn walk_children(parent: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let mut inline: Vec<Span> = Vec::new();
    for child in parent.children() {
        // Drop regenerated markers (list item numbers, footnote backrefs).
        if matches!(child.value(), Node::Element(e) if is_marker_chrome(e)) {
            continue;
        }
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
            marker: ctx.item_marker(),
        });
    } else {
        inline.clear();
    }
}

/// Turn a classified block-level element into `Block`s. Pure dispatch — every
/// "what is this?" decision lives in [`semantics::classify`]; this maps each
/// role to its extractor.
fn block_element(node: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let Node::Element(e) = node.value() else {
        return;
    };
    match classify(e, node) {
        ElementRole::Callout(kind) => emit_callout(node, kind, ctx, out),
        ElementRole::Footnote { id, label } => {
            let mut blocks = Vec::new();
            walk_children(node, &ctx.with_quote(false), &mut blocks);
            if !blocks.is_empty() {
                out.push(Block::Footnote { id, label, blocks });
            }
        }
        ElementRole::DisplayMathImage(src, alt) => out.push(Block::Image {
            src,
            alt,
            data: Vec::new(),
            caption: Vec::new(),
            math: true,
            // Equation images render at native size, so the authored width is moot.
            width: ImageWidth::Auto,
        }),
        ElementRole::CodeBlock => {
            let lines = strip_line_numbers(trim_blank_edges(code_lines(node).into_iter()));
            if !lines.is_empty() {
                out.push(Block::Code {
                    lang: detect_lang(node),
                    lines,
                });
            }
        }
        ElementRole::DisplayMath => {
            let tex = native_math_unicode(node);
            if !tex.trim().is_empty() {
                out.push(Block::Math { tex });
            }
        }
        ElementRole::Heading(level) => {
            let spans = inline_spans(node);
            if spans.iter().any(|s| !s.text.trim().is_empty()) {
                // A heading-based ToC (Packt et al.) encodes depth in the heading
                // level; render those entries indented instead of as flat headings.
                if level >= 2 && in_toc(node) {
                    out.push(Block::Para {
                        spans,
                        indent: level - 2,
                        quote: ctx.quote,
                        // Empty marker → counts as a list item, so consecutive ToC
                        // entries pack tight (no blank line between rows).
                        marker: Some(String::new()),
                    });
                } else {
                    out.push(Block::Heading { level, spans });
                }
            }
        }
        ElementRole::Paragraph | ElementRole::Cell => emit_paragraph(node, ctx, out),
        ElementRole::List { ordered, start } => {
            let mut n = start;
            for c in node.children() {
                if matches!(c.value(), Node::Element(le) if le.name() == "li") {
                    let (marker, ordinal) = if ordered {
                        (format!("{n}. "), Some(n))
                    } else {
                        ("• ".to_string(), None)
                    };
                    n += 1;
                    list_item(c, ctx, marker, ordinal, out);
                }
            }
        }
        ElementRole::Quote => walk_children(node, &ctx.with_quote(true), out),
        ElementRole::DefList => emit_deflist(node, &ctx.tightened(), out),
        ElementRole::Rule => out.push(Block::Rule),
        ElementRole::Image => out.push(Block::Image {
            src: e.attr("src").unwrap_or("").to_string(),
            alt: e.attr("alt").unwrap_or("").to_string(),
            data: Vec::new(),
            caption: Vec::new(),
            math: false,
            width: parse_img_width(e.attr("width"), e.attr("style")),
        }),
        ElementRole::AsideIconTable(kind) => {
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
        ElementRole::Table => {
            if let Some(table) = parse_table(node) {
                out.push(table);
            }
        }
        ElementRole::Container => {
            // A ToC region (or a Springer ToC-level div) indents to its level and
            // packs entries tight; the flag flows to all descendants.
            let toc = is_toc_root(e) || toc_level(e).is_some();
            let c = Ctx {
                indent: toc_level(e).unwrap_or(ctx.indent),
                quote: ctx.quote,
                tight: ctx.tight || toc,
            };
            walk_children(node, &c, out);
        }
    }
}

/// The authored display width of an `<img>`, from its inline CSS `width` (which
/// wins) or its presentational `width` attribute. This is the publisher's
/// *intended* size; the renderer prefers it over the file's pixel resolution.
fn parse_img_width(width_attr: Option<&str>, style_attr: Option<&str>) -> ImageWidth {
    if let Some(style) = style_attr
        && let Some(w) = css_width(style)
    {
        return w;
    }
    width_attr.map(parse_len).unwrap_or(ImageWidth::Auto)
}

/// Pull the `width` declaration out of an inline `style`, ignoring `max-width` /
/// `min-width` (those bound but don't set the size).
fn css_width(style: &str) -> Option<ImageWidth> {
    for decl in style.to_ascii_lowercase().split(';') {
        if let Some(val) = decl.trim().strip_prefix("width")
            && let Some(val) = val.trim_start().strip_prefix(':')
        {
            return Some(parse_len(val));
        }
    }
    None
}

/// Parse a CSS/HTML length: `80%` → a column fraction, `600`/`600px` → pixels.
/// Other units (em/pt/…) are too context-dependent to map reliably, so they fall
/// back to [`ImageWidth::Auto`] (normalized like an unsized image).
fn parse_len(s: &str) -> ImageWidth {
    let s = s.trim();
    if let Some(pct) = s
        .strip_suffix('%')
        .and_then(|n| n.trim().parse::<f32>().ok())
    {
        return ImageWidth::Pct((pct / 100.0).clamp(0.0, 1.0));
    }
    let n = s.strip_suffix("px").unwrap_or(s).trim();
    match n.parse::<f32>() {
        Ok(px) if px > 0.0 => ImageWidth::Px(px.round() as u32),
        _ => ImageWidth::Auto,
    }
}

/// Collect an element's children as inline spans.
fn inline_spans(node: NodeRef<Node>) -> Vec<Span> {
    let mut spans = Vec::new();
    for c in node.children() {
        collect_inline(c, Inline::default(), &mut spans);
    }
    spans
}

/// Emit a paragraph from an element's inline content (with math exponent fix-up),
/// dropping it if there's no visible text. List markers are attached separately
/// by [`list_item`].
fn emit_paragraph(node: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let mut spans = inline_spans(node);
    if spans.iter().any(|s| !s.text.trim().is_empty()) {
        superscript_math_exponents(&mut spans);
        out.push(Block::Para {
            spans,
            indent: ctx.indent,
            quote: ctx.quote,
            marker: ctx.item_marker(),
        });
    }
}

fn list_item(
    node: NodeRef<Node>,
    ctx: &Ctx,
    marker: String,
    ordinal: Option<usize>,
    out: &mut Vec<Block>,
) {
    // Collect the item's content (handles `<li>text`, `<li><p>…`, `<li><div>…`,
    // and nested lists) then attach the marker to its first paragraph.
    let mut item: Vec<Block> = Vec::new();
    let inner = Ctx {
        indent: ctx.indent + 1,
        ..*ctx
    };
    walk_children(node, &inner, &mut item);

    // Some publishers hard-code the item's own number inside the `<li>` (e.g.
    // Springer's `<div class="CitationNumber">4.</div>`), which our generated
    // ordered marker would then double ("4. 4."). Drop that redundant ordinal.
    if let Some(n) = ordinal {
        strip_leading_ordinal(&mut item, n);
    }

    if let Some(Block::Para {
        marker: m, indent, ..
    }) = item.iter_mut().find(|b| matches!(b, Block::Para { .. }))
    {
        *m = Some(marker);
        *indent = ctx.indent;
    }
    out.append(&mut item);
}

/// In an ordered-list item, drop a leading ordinal that merely restates the
/// item's own number `n` — whether it sits in its own block ("4.") or as an
/// inline prefix ("4. Text…"). Matches only the exact number followed by a
/// `.`/`)`/`]`/`:` delimiter, so genuine numeric prose ("4 reasons …") is kept.
fn strip_leading_ordinal(blocks: &mut Vec<Block>, n: usize) {
    let Some(idx) = blocks.iter().position(
        |b| matches!(b, Block::Para { spans, .. } if spans.iter().any(|s| !s.text.trim().is_empty())),
    ) else {
        return;
    };
    let Block::Para { spans, .. } = &mut blocks[idx] else {
        return;
    };
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();

    let mut chars = text.chars().peekable();
    let mut consumed = 0usize;
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
        consumed += 1;
    }
    let mut digits = String::new();
    while let Some(&c) = chars.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        digits.push(c);
        chars.next();
        consumed += 1;
    }
    if digits.parse::<usize>().ok() != Some(n) {
        return;
    }
    if !chars
        .peek()
        .is_some_and(|c| matches!(c, '.' | ')' | ']' | ':'))
    {
        return;
    }
    chars.next();
    consumed += 1;
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
        consumed += 1;
    }

    if chars.peek().is_none() {
        // The whole block was just the ordinal — drop it entirely.
        blocks.remove(idx);
    } else {
        strip_leading_chars(spans, consumed);
    }
}

/// Remove the first `count` characters across a run of inline spans, dropping any
/// span fully consumed.
fn strip_leading_chars(spans: &mut Vec<Span>, mut count: usize) {
    while count > 0 && !spans.is_empty() {
        let len = spans[0].text.chars().count();
        if len <= count {
            count -= len;
            spans.remove(0);
        } else {
            spans[0].text = spans[0].text.chars().skip(count).collect();
            count = 0;
        }
    }
}

/// Render a definition list (`<dl>`): pair each `<dt>` term with the following
/// `<dd>` description(s), as "**term**  description" on one entry (the term in
/// bold, then the description), so terms and definitions don't run together.
fn emit_deflist(node: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let mut term: Vec<Span> = Vec::new();
    for child in node.children() {
        let Node::Element(e) = child.value() else {
            continue;
        };
        match e.name() {
            "dt" => term = inline_spans(child),
            "dd" => {
                emit_def_entry(std::mem::take(&mut term), child, ctx, out);
            }
            _ => {}
        }
    }
    // A trailing `<dt>` with no `<dd>` (rare) still shows.
    if term.iter().any(|s| !s.text.trim().is_empty()) {
        out.push(Block::Para {
            spans: bold_spans(term),
            indent: ctx.indent,
            quote: ctx.quote,
            marker: ctx.item_marker(),
        });
    }
}

/// Emit one `<dt>`/`<dd>` entry: the bold term prefixed onto the description's
/// first paragraph (a hanging indent), with any further description blocks kept.
fn emit_def_entry(term: Vec<Span>, dd: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let mut blocks = Vec::new();
    walk_children(dd, ctx, &mut blocks);
    let mut prefix = bold_spans(term);
    if let Some(Block::Para { spans, .. }) =
        blocks.iter_mut().find(|b| matches!(b, Block::Para { .. }))
    {
        if !prefix.is_empty() {
            prefix.push(Span::plain("  "));
            spans.splice(0..0, prefix);
        }
    } else {
        // No description paragraph: the term on its own line.
        blocks.insert(
            0,
            Block::Para {
                spans: prefix,
                indent: ctx.indent,
                quote: ctx.quote,
                marker: ctx.item_marker(),
            },
        );
    }
    out.append(&mut blocks);
}

/// Re-style spans as bold (for definition-list terms).
fn bold_spans(spans: Vec<Span>) -> Vec<Span> {
    spans
        .into_iter()
        .map(|s| Span {
            text: s.text,
            style: Inline {
                bold: true,
                ..s.style
            },
            anchor: s.anchor,
        })
        .collect()
}

#[cfg(test)]
mod tests;
