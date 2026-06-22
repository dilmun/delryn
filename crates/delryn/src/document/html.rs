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
    match node.value() {
        // Real figure/cover images render block-level; math/icon images stay
        // inline (handled in collect_inline).
        Node::Element(e) if e.name() == "img" => is_real_image(e),
        Node::Element(e) => matches!(
            e.name(),
            "p" | "div" | "section" | "article" | "header" | "footer" | "main"
                | "figure" | "figcaption" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                | "ul" | "ol" | "li" | "blockquote" | "pre" | "hr"
                | "table" | "thead" | "tbody" | "tr" | "td" | "th"
        ),
        _ => false,
    }
}

/// A "real" image is a figure/cover — not a math equation or a UI icon.
fn is_real_image(e: &scraper::node::Element) -> bool {
    let alt = e.attr("alt").unwrap_or("");
    let src = e.attr("src").unwrap_or("");
    !crate::math::is_math(alt) && !is_icon_src(src)
}

fn is_icon_src(src: &str) -> bool {
    let s = src.to_lowercase();
    ["warning", "info", "tip", "note", "pencil", "key", "question", "icon", "leanpub_"]
        .iter()
        .any(|k| s.contains(k))
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
        "img" => out.push(Block::Image {
            src: e.attr("src").unwrap_or("").to_string(),
            alt: e.attr("alt").unwrap_or("").to_string(),
            data: Vec::new(),
        }),
        // Aside/callout tables (icon cell + content cell): render the content
        // inline, prefixed with a symbol standing in for the icon.
        "table" if aside_icon(node).is_some() => {
            let symbol = aside_icon(node).unwrap_or_default();
            let mut content = Vec::new();
            for cell in content_cells(node) {
                walk_children(cell, ctx, &mut content);
            }
            prefix_symbol(&mut content, &symbol);
            out.append(&mut content);
        }
        // Other tables: degrade to one paragraph per cell for now (real column
        // layout is a later refinement).
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
        // Code blocks marked up as styled containers rather than <pre>/<code>
        // (e.g. Springer/Apress `<div class="ProgramCode">` with per-line
        // `<div class="FixedLine">`). Render them as real code.
        _ if is_code_container(e) => {
            let lines = trim_blank_edges(code_lines(node).into_iter());
            let lines = strip_line_numbers(lines);
            if !lines.is_empty() {
                out.push(Block::Code {
                    lang: detect_lang(node),
                    lines,
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
                    let alt = e.attr("alt").unwrap_or("");
                    let text = if crate::math::is_math(alt) {
                        crate::math::latex_to_unicode(alt)
                    } else {
                        img_label(e)
                    };
                    out.push(Span {
                        text,
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

/// Does the element carry `want` as one of its space-separated class tokens
/// (case-insensitive)?
fn class_has_token(e: &scraper::node::Element, want: &str) -> bool {
    e.attr("class")
        .is_some_and(|c| c.split_whitespace().any(|t| t.eq_ignore_ascii_case(want)))
}

/// A non-`<pre>` block that is really a code listing, by its class. Covers the
/// common publisher conventions (notably Springer/Apress `ProgramCode`).
fn is_code_container(e: &scraper::node::Element) -> bool {
    matches!(e.name(), "div" | "section")
        && ["ProgramCode", "SourceCode", "CodeBlock", "code", "sourceCode"]
            .iter()
            .any(|t| class_has_token(e, t))
}

/// Code lines from a styled code container. When the source wraps each line in a
/// `<div class="FixedLine">` (Springer/Apress), one line per such div; otherwise
/// fall back to splitting the concatenated text on newlines.
fn code_lines(node: NodeRef<Node>) -> Vec<String> {
    let fixed: Vec<String> = node
        .descendants()
        .filter(|n| matches!(n.value(), Node::Element(e) if class_has_token(e, "FixedLine")))
        .map(|n| raw_text(n).trim_end().to_string())
        .collect();
    if !fixed.is_empty() {
        return fixed;
    }
    raw_text(node).split('\n').map(|l| l.trim_end().to_string()).collect()
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

/// A stand-in for an inline image: its alt text, else a plain "[image]" label.
/// (Real image rendering is the graphics-protocol task.)
fn img_label(e: &scraper::node::Element) -> String {
    match e.attr("alt").map(str::trim).filter(|a| !a.is_empty()) {
        Some(alt) => format!("[{alt}]"),
        None => "[image]".to_string(),
    }
}

/// If a table is an aside/callout, the BMP symbol for its icon. We deliberately
/// use Dingbat/Geometric/Enclosed glyphs (same blocks as `▸ • ─ │`), not emoji,
/// which many terminal fonts render as a tofu box.
fn aside_icon(node: NodeRef<Node>) -> Option<String> {
    let is_aside =
        matches!(node.value(), Node::Element(e) if e.attr("class").is_some_and(|c| c.contains("aside")));
    if !is_aside {
        return None;
    }
    node.descendants().find_map(|n| match n.value() {
        Node::Element(e) if e.name() == "img" => Some(icon_symbol(e)),
        _ => None,
    })
}

fn icon_symbol(e: &scraper::node::Element) -> String {
    let src = e.attr("src").unwrap_or("").to_lowercase();
    if src.contains("pencil") {
        "✎" // exercise
    } else if src.contains("warning") || src.contains("caution") {
        "▲"
    } else if src.contains("info") {
        "ⓘ"
    } else if src.contains("key") {
        "✦" // important
    } else if src.contains("question") {
        "ⓠ"
    } else if src.contains("tip") {
        "✦"
    } else {
        "■"
    }
    .to_string()
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

/// Prepend the icon symbol to the first heading/paragraph of a callout.
fn prefix_symbol(blocks: &mut [Block], symbol: &str) {
    for b in blocks.iter_mut() {
        let spans = match b {
            Block::Heading { spans, .. } | Block::Para { spans, .. } => spans,
            _ => continue,
        };
        spans.insert(0, Span::plain(format!("{symbol}  ")));
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn code_lines_of(blocks: &[Block]) -> Option<&Vec<String>> {
        blocks.iter().find_map(|b| match b {
            Block::Code { lines, .. } => Some(lines),
            _ => None,
        })
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
