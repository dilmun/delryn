//! XHTML → structured block model. Used by EPUB (and future HTML-based
//! formats) to drive the rich typography engine. Produces headings, styled
//! paragraphs, lists, blockquotes, and code blocks rather than flat text.

use ego_tree::NodeRef;
use scraper::{Html, Node};

use super::{Block, CalloutKind, Inline, Span, TableCell};

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
        // Admonition / callout containers (class or epub:type note/tip/warning/…),
        // checked first so a `<blockquote class="note">` becomes a callout rather
        // than a plain quote.
        "div" | "section" | "aside" | "blockquote" if callout_kind(e).is_some() => {
            emit_callout(node, callout_kind(e).unwrap(), ctx, out);
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
        }),
        Node::Element(e) => {
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
                "a" => Inline {
                    link: true,
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
                    let text = if delryn_model::math::is_math(alt) {
                        delryn_model::math::latex_to_unicode(alt)
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
        && [
            "ProgramCode",
            "SourceCode",
            "CodeBlock",
            "code",
            "sourceCode",
        ]
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
    raw_text(node)
        .split('\n')
        .map(|l| l.trim_end().to_string())
        .collect()
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
