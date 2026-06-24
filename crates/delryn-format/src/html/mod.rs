//! XHTML → structured block model. Used by EPUB (and future HTML-based
//! formats) to drive the rich typography engine. Produces headings, styled
//! paragraphs, lists, blockquotes, and code blocks rather than flat text.

use ego_tree::NodeRef;
use scraper::{Html, Node};

use super::{Anchor, Block, CalloutKind, Inline, Span, TableCell};

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
}

impl Ctx {
    /// A copy at the same indent with `quote` set (for blockquotes/footnotes).
    fn with_quote(&self, quote: bool) -> Ctx {
        Ctx {
            indent: self.indent,
            quote,
        }
    }

    /// A copy at a specific indent level (for printed-ToC depth).
    fn with_indent(&self, indent: u8) -> Ctx {
        Ctx {
            indent,
            quote: self.quote,
        }
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
            marker: None,
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
                out.push(Block::Heading { level, spans });
            }
        }
        ElementRole::Paragraph | ElementRole::Cell => emit_paragraph(node, ctx, out),
        ElementRole::List { ordered, start } => {
            let mut n = start;
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
        ElementRole::Quote => walk_children(node, &ctx.with_quote(true), out),
        ElementRole::DefList => emit_deflist(node, ctx, out),
        ElementRole::Rule => out.push(Block::Rule),
        ElementRole::Image => out.push(Block::Image {
            src: e.attr("src").unwrap_or("").to_string(),
            alt: e.attr("alt").unwrap_or("").to_string(),
            data: Vec::new(),
            caption: Vec::new(),
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
        ElementRole::Container => match toc_level(e) {
            // A printed ToC entry: indent it to its level so the page reads as a
            // hierarchy rather than a flat list.
            Some(lvl) => walk_children(node, &ctx.with_indent(lvl), out),
            None => walk_children(node, ctx, out),
        },
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
            marker: None,
        });
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
            marker: None,
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
                marker: None,
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
