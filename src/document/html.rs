//! XHTML → structured block model. Used by EPUB (and future HTML-based
//! formats) to drive the rich typography engine. Produces headings, styled
//! paragraphs, lists, blockquotes, and code blocks rather than flat text.

use ego_tree::NodeRef;
use scraper::{Html, Node};

use super::{Block, Inline, Span};

/// Parse a section's XHTML into a list of reflowable blocks.
pub fn parse_blocks(xhtml: &str) -> Vec<Block> {
    let doc = Html::parse_document(xhtml);
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

#[derive(Default, Clone)]
struct Ctx {
    indent: u8,
    quote: bool,
}

fn is_block(node: NodeRef<Node>) -> bool {
    matches!(
        node.value(),
        Node::Element(e) if matches!(
            e.name(),
            "p" | "div" | "section" | "article" | "header" | "footer" | "main"
                | "figure" | "figcaption" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                | "ul" | "ol" | "li" | "blockquote" | "pre" | "hr"
                | "table" | "thead" | "tbody" | "tr" | "td" | "th"
        )
    )
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
        "pre" => {
            let text = raw_text(node);
            let lines = trim_blank_edges(text.split('\n').map(|l| l.trim_end().to_string()));
            let lines = strip_line_numbers(lines);
            if !lines.is_empty() {
                out.push(Block::Code {
                    lang: detect_lang(node),
                    lines,
                });
            }
        }
        "hr" => out.push(Block::Rule),
        // Tables: degrade to one paragraph per cell for now (real column layout
        // is a later refinement).
        "td" | "th" => {
            let mut spans = Vec::new();
            for c in node.children() {
                collect_inline(c, Inline::default(), &mut spans);
            }
            if spans.iter().any(|s| !s.text.trim().is_empty()) {
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

    if let Some(Block::Para { marker: m, indent, .. }) =
        item.iter_mut().find(|b| matches!(b, Block::Para { .. }))
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
        }),
        Node::Element(e) => {
            let style = match e.name() {
                "em" | "i" | "cite" | "var" | "dfn" => Inline {
                    italic: true,
                    ..style
                },
                "strong" | "b" => Inline { bold: true, ..style },
                "code" | "kbd" | "samp" | "tt" => Inline { code: true, ..style },
                "a" => Inline { link: true, ..style },
                "br" => {
                    out.push(Span::plain(" "));
                    return;
                }
                // Images can't render in the terminal yet (graphics protocol is
                // a later task); show the alt text or a kind-of-image symbol so
                // icons/figures aren't silently dropped.
                "img" => {
                    out.push(Span {
                        text: img_label(e),
                        style: Inline {
                            italic: true,
                            ..style
                        },
                    });
                    return;
                }
                _ => style,
            };
            for c in node.children() {
                collect_inline(c, style, out);
            }
        }
        _ => {}
    }
}

/// Concatenate all descendant text verbatim (for `<pre>`).
fn raw_text(node: NodeRef<Node>) -> String {
    let mut s = String::new();
    for d in node.descendants() {
        if let Node::Text(t) = d.value() {
            s.push_str(&t.text);
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
        if let Some(rest) = t.strip_prefix(&(i + 1).to_string()) {
            if rest.is_empty() || rest.starts_with([' ', '\t']) {
                numbered += 1;
            }
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

/// A stand-in for an image: its alt text, or a symbol guessed from the source
/// filename (warning/tip/note icons are common in technical books).
fn img_label(e: &scraper::node::Element) -> String {
    if let Some(alt) = e.attr("alt") {
        let alt = alt.trim();
        if !alt.is_empty() {
            return format!("🖼 {alt}");
        }
    }
    let src = e.attr("src").unwrap_or("").to_lowercase();
    let symbol = if src.contains("warning") || src.contains("caution") {
        "⚠"
    } else if src.contains("tip") {
        "💡"
    } else if src.contains("note") || src.contains("info") {
        "ℹ"
    } else if src.contains("error") || src.contains("danger") {
        "✖"
    } else {
        "🖼"
    };
    symbol.to_string()
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
        if let Node::Element(e) = n.value() {
            if let Some(class) = e.attr("class") {
                if let Some(lang) = from_class(class) {
                    return Some(lang);
                }
            }
        }
    }
    None
}
