//! XHTML → structured block model. Used by EPUB (and future HTML-based
//! formats) to drive the rich typography engine. Produces headings, styled
//! paragraphs, lists, blockquotes, and code blocks rather than flat text.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::LazyLock;

use ego_tree::NodeRef;
use regex::Regex;
use scraper::{Html, Node};

use super::{Anchor, Block, CalloutKind, ImageWidth, Inline, Span, TableCell};
use crate::container::{body_or_root, descendant_text};

thread_local! {
    /// Class names the section's CSS gives `display: block` (or list-item/table/…),
    /// so an inline element (a `<span>`/`<a>`) carrying one is laid out as its own
    /// block — otherwise Springer/Apress citation lines (author · title · DOI link),
    /// each a `display:block` `<span>` with no whitespace between them, run together.
    /// Set for the duration of one [`parse_blocks_with_css`] call, then cleared.
    static BLOCK_CLASSES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Whether the current parse's CSS makes an element with `class` block-level.
fn class_is_block_display(e: &scraper::node::Element) -> bool {
    let Some(class) = e.attr("class") else {
        return false;
    };
    BLOCK_CLASSES
        .with_borrow(|set| !set.is_empty() && class.split_whitespace().any(|t| set.contains(t)))
}

/// The class names any `display: block | list-item | table | flex | grid` rule
/// targets — the *last* class of each comma-separated selector (its subject), so a
/// descendant rule (`.Wrapper .BookTitle{display:block}`) marks `BookTitle`, not the
/// wrapper. Comments are stripped; `@media` bodies are scanned too (their inner
/// rules match the same way). A best-effort text scan, not a full CSS engine.
fn block_display_classes(css: &str) -> HashSet<String> {
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/\*[\s\S]*?\*/").unwrap());
    // Innermost `selector { body }` (no braces inside either), so a rule nested in
    // an `@media { … }` block still matches on its own.
    static RULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([^{}]*)\{([^{}]*)\}").unwrap());
    static BLOCKISH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"display\s*:\s*(?:block|list-item|table|flex|grid)\b").unwrap()
    });
    static CLASS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\.(-?[A-Za-z_][A-Za-z0-9_-]*)").unwrap());
    let css = COMMENT.replace_all(css, " ");
    let mut out = HashSet::new();
    for rule in RULE.captures_iter(&css) {
        if !BLOCKISH.is_match(&rule[2]) {
            continue;
        }
        for sel in rule[1].split(',') {
            if let Some(subject) = CLASS.captures_iter(sel).last() {
                out.insert(subject[1].to_string());
            }
        }
    }
    out
}

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

/// Parse a section's XHTML into a list of reflowable blocks. Any `<style>` in the
/// document is honoured for block-`display`; a linked stylesheet (EPUB) is passed
/// via [`parse_blocks_with_css`].
pub fn parse_blocks(xhtml: &str) -> Vec<Block> {
    parse_blocks_with_css(xhtml, "")
}

/// As [`parse_blocks`], plus `extra_css` (the section's linked stylesheets, which
/// EPUB resolves from the archive) so `display: block` on inline citation `<span>`s
/// is honoured — otherwise their lines run together with no whitespace.
pub fn parse_blocks_with_css(xhtml: &str, extra_css: &str) -> Vec<Block> {
    let xhtml = expand_self_closing(xhtml);
    let doc = Html::parse_document(&xhtml);
    // Collect block-display classes from the linked CSS and any inline `<style>`.
    let mut css = extra_css.to_string();
    for node in doc.tree.root().descendants() {
        if matches!(node.value(), Node::Element(e) if e.name() == "style") {
            css.push('\n');
            css.push_str(&descendant_text(node, false, None));
        }
    }
    let classes = block_display_classes(&css);
    BLOCK_CLASSES.with_borrow_mut(|set| *set = classes);

    let mut out = Vec::new();
    walk_children(body_or_root(&doc), &Ctx::default(), &mut out);
    attach_trailing_captions(&mut out);
    join_stranded_labels(&mut out);

    BLOCK_CLASSES.with_borrow_mut(HashSet::clear); // don't leak across sections
    out
}

/// Whether `text` opens with a figure-label caption (`Figure 5.`, `Table 1.2`,
/// `Fig. 3`, `Listing 2-1`, …). The trailing digit is required so ordinary prose
/// ("Figure out how…", "Table of contents") is never mistaken for a caption.
fn is_figure_caption(text: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^\s*(figure|fig\.?|table|chart|photo|plate|diagram|exhibit|illustration|scheme|listing)\s*\.?\s*\d",
        )
        .unwrap()
    });
    RE.is_match(text)
}

/// Attach a figure-label paragraph that immediately follows a bare image as that
/// image's caption, so it renders centred beneath the picture even when the source
/// didn't use `<figure>`/`<figcaption>` — the common shape in MOBI and many
/// EPUBs (`<img>` then a separate `<p>Figure 5. …</p>`). Only fills an *empty*
/// caption on a non-math image, and only when the paragraph starts with a figure
/// label, so real captions and ordinary body text are left untouched.
fn attach_trailing_captions(blocks: &mut Vec<Block>) {
    let mut i = 0;
    while i + 1 < blocks.len() {
        let image_wants_caption = matches!(
            &blocks[i],
            Block::Image { caption, math: false, .. } if caption.is_empty()
        );
        let next_is_caption = matches!(
            &blocks[i + 1],
            Block::Para { spans, marker: None, .. }
                if is_figure_caption(&spans.iter().map(|s| s.text.as_str()).collect::<String>())
        );
        if image_wants_caption && next_is_caption {
            let Block::Para { spans, .. } = blocks.remove(i + 1) else {
                unreachable!()
            };
            if let Block::Image { caption, .. } = &mut blocks[i] {
                *caption = spans;
            }
        }
        i += 1;
    }
}

/// Whether a paragraph's whole text is a bare reference marker — `(1)`, `2.`,
/// `[a]`, `†` — rather than prose. Publishers emit these as a label beside the
/// thing they mark; on their own they are meaningless.
fn is_stranded_label(text: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[\(\[]?\s*(?:\d{1,3}|[a-zA-Z]|[ivxIVX]{1,4}|[*†‡§¶])\s*[\)\]]?\.?$").unwrap()
    });
    let t = text.trim();
    // Bounded so a genuinely short sentence can never qualify.
    !t.is_empty() && t.chars().count() <= 6 && RE.is_match(t)
}

/// Join a paragraph that is nothing but a reference marker onto the paragraph
/// that follows it.
///
/// Springer-style markup pairs a label with its value as an inline span beside a
/// block sibling — `<span class="AffiliationNumber">(1)</span><div
/// class="AffiliationText">Novosibirsk State University…</div>`. The block child
/// ends the inline run, so the label is emitted as its own paragraph and lands
/// stranded on a line of its own, a blank line away from the text it labels.
///
/// Only a paragraph whose *entire* text is a marker is joined, and only onto a
/// following plain paragraph — never a heading, a list item, or an image — so
/// ordinary prose and real list numbering are untouched.
fn join_stranded_labels(blocks: &mut Vec<Block>) {
    let mut i = 0;
    while i + 1 < blocks.len() {
        let is_label = matches!(
            &blocks[i],
            Block::Para { spans, marker: None, quote: false, .. }
                if is_stranded_label(&spans.iter().map(|s| s.text.as_str()).collect::<String>())
        );
        let next_takes_it = matches!(
            &blocks[i + 1],
            Block::Para { spans, marker: None, .. } if !spans.is_empty()
        );
        if is_label && next_takes_it {
            let Block::Para { spans: label, .. } = blocks.remove(i) else {
                unreachable!("matched Para above")
            };
            let Block::Para { spans, .. } = &mut blocks[i] else {
                unreachable!("matched Para above")
            };
            // The label keeps its own styling; one space separates it from the value.
            let mut joined = label;
            joined.push(Span::plain(" "));
            joined.append(spans);
            *spans = joined;
            continue; // `i` now indexes the merged paragraph — don't skip past it
        }
        i += 1;
    }
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
    // Cap the gather at ~`max` chars' worth of bytes — enough to fill `max` after
    // whitespace-collapsing, without walking a whole subtree.
    descendant_text(node, false, Some(max * 2))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// Maximum block-nesting depth delryn's own DOM walk recurses before it stops and
/// emits the remaining subtree as flat text. Real books nest a handful of levels
/// deep; this only trips on pathological / malicious markup (thousands of nested
/// `<div>`s), where unbounded recursion would overflow the stack and abort the
/// process. No real content is lost below this bound. Kept well under the point
/// where the recursive walk's (debug-build, un-coalesced) frames would exhaust a
/// default thread stack — the block and inline walks can each reach this depth.
const MAX_BLOCK_DEPTH: u16 = 128;

#[derive(Default, Clone)]
struct Ctx {
    indent: u8,
    quote: bool,
    /// Pack paragraphs tight (no blank line between them) — for list-like
    /// regions: tables of contents and definition lists.
    tight: bool,
    /// Block-nesting depth, incremented at each descent so a pathologically deep
    /// document can't overflow the stack (see [`MAX_BLOCK_DEPTH`]).
    depth: u16,
}

impl Ctx {
    /// A copy at the same indent with `quote` set (for blockquotes/footnotes).
    fn with_quote(&self, quote: bool) -> Ctx {
        Ctx { quote, ..*self }
    }

    /// A copy one block-nesting level deeper (the recursion guard).
    fn deeper(&self) -> Ctx {
        Ctx {
            depth: self.depth.saturating_add(1),
            ..*self
        }
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
        // An inline element the CSS gives `display: block` (Springer/Apress citation
        // `<span>`s: author · title · DOI link) — lay it out on its own line so the
        // parts don't concatenate. Its content recurses as a `Container`.
        Node::Element(e) if class_is_block_display(e) => true,
        // Math — a native `<math>` or a rasterised equation image: block iff the
        // display classifier says so (display attr / displaystyle / container class
        // / delimiter / standalone); inline math is handled in collect_inline.
        Node::Element(_) if is_math_node(node) => math_is_display(node),
        // Real figure/cover images render block-level; icon images stay inline.
        Node::Element(e) if matches!(e.name(), "img" | "image") => is_real_image(e),
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
                // SVG wrapper (e.g. an EPUB cover page) — a block container so
                // the walk recurses to its <image> child.
                | "svg"
        ),
        _ => false,
    }
}

/// Iterate children, grouping loose inline content into implicit paragraphs and
/// recursing into block-level elements.
fn walk_children(parent: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    // One level deeper for any block child — the single point that advances the
    // recursion-depth guard (see `block_element`).
    let deeper = ctx.deeper();
    let mut inline: Vec<Span> = Vec::new();
    for child in parent.children() {
        // Drop regenerated markers (list item numbers, footnote backrefs).
        if matches!(child.value(), Node::Element(e) if is_marker_chrome(e)) {
            continue;
        }
        if is_block(child) {
            flush(&mut inline, ctx, out);
            block_element(child, &deeper, out);
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

/// Emit a caption wrapper (Springer's `<div class="Caption">` = a `CaptionNumber`
/// span plus a `SimplePara` title) as **one** caption line — "Table 3. …title…" —
/// instead of letting the number span and the title paragraph stack as two spaced
/// blocks with the number stranded on its own wasted row. Only a *textual* caption
/// is merged; one carrying an image or table falls back to the normal walk so
/// nothing is dropped.
fn emit_caption(node: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let mut inner = Vec::new();
    walk_children(node, ctx, &mut inner);
    if !inner
        .iter()
        .all(|b| matches!(b, Block::Para { .. } | Block::Heading { .. }))
    {
        out.extend(inner);
        return;
    }
    let spans = coalesce_caption_spans(inner);
    if spans.iter().any(|s| !s.text.trim().is_empty()) {
        out.push(Block::Para {
            spans,
            indent: ctx.indent,
            quote: ctx.quote,
            marker: None,
        });
    }
}

/// Flatten a caption's child blocks into one run of spans, bridging each element
/// boundary the source left unspaced (`</span><p>`) with a single space so the
/// number and title read as one line rather than jammed ("Table 3.Classification").
fn coalesce_caption_spans(blocks: Vec<Block>) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for b in blocks {
        let runs = match b {
            Block::Para { spans, .. } | Block::Heading { spans, .. } => spans,
            _ => continue,
        };
        if runs.iter().all(|s| s.text.trim().is_empty()) {
            continue;
        }
        let need_gap = spans
            .last()
            .is_some_and(|l| !l.text.ends_with(char::is_whitespace))
            && !runs
                .first()
                .is_some_and(|f| f.text.starts_with(char::is_whitespace));
        if need_gap {
            spans.push(Span {
                text: " ".to_string(),
                style: Inline::default(),
                anchor: None,
                math: None,
            });
        }
        spans.extend(runs);
    }
    spans
}

/// Turn a classified block-level element into `Block`s. Pure dispatch — every
/// "what is this?" decision lives in [`semantics::classify`]; this maps each
/// role to its extractor.
fn block_element(node: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let Node::Element(e) = node.value() else {
        return;
    };
    // Pathologically deep nesting: stop recursing and emit the subtree as flat
    // text (collected iteratively) so the stack can't overflow. `collect_inline`
    // has its own depth guard for the inline case.
    if ctx.depth >= MAX_BLOCK_DEPTH {
        let mut spans = Vec::new();
        collect_inline(node, Inline::default(), &mut spans);
        flush(&mut spans, ctx, out);
        return;
    }
    match classify(e, node) {
        ElementRole::Callout(kind) => emit_callout(node, kind, ctx, out),
        ElementRole::Footnote { id, label } => {
            let mut blocks = Vec::new();
            walk_children(node, &ctx.with_quote(false), &mut blocks);
            if !blocks.is_empty() {
                out.push(Block::Footnote { id, label, blocks });
            }
        }
        ElementRole::CodeBlock => {
            let lines = strip_line_numbers(trim_blank_edges(code_lines(node).into_iter()));
            if !lines.is_empty() {
                out.push(Block::Code {
                    lang: detect_lang(node),
                    lines,
                });
            }
        }
        ElementRole::DisplayMath(item) => {
            // Choose the most efficient renderable form up front: crisp vector type when the
            // engine can map the markup, else the publisher's raster (bytes resolved by the
            // format layer), else the Unicode floor. Nothing renderable → drop it.
            if let Some(block) = display_math_block(item) {
                out.push(block);
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
            src: img_src(e).unwrap_or_default(),
            alt: e.attr("alt").unwrap_or("").to_string(),
            data: Vec::new(),
            caption: Vec::new(),
            math: false,
            width: parse_img_width(e.attr("width"), e.attr("style")),
            ink: None,
        }),
        ElementRole::Figure => emit_figure(node, ctx, out),
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
        ElementRole::Caption => emit_caption(node, ctx, out),
        ElementRole::Container => {
            // A ToC region (or a Springer ToC-level div) indents to its level and
            // packs entries tight; the flag flows to all descendants.
            let toc = is_toc_root(e) || toc_level(e).is_some();
            let c = Ctx {
                indent: toc_level(e).unwrap_or(ctx.indent),
                quote: ctx.quote,
                tight: ctx.tight || toc,
                depth: ctx.depth,
            };
            walk_children(node, &c, out);
        }
    }
}

/// Emit a `<figure>` as one captioned image: the picture plus its caption text,
/// so the caption renders centred beneath the image (see `layout::emit_image`)
/// instead of leaking out as a stray left-aligned heading/paragraph next to it.
///
/// Only the common single-picture figure is merged. A figure with no *real*
/// image (a text-only figure, or one wrapping a math/icon image) or with several
/// falls back to the generic container walk, so nothing is dropped or misread.
fn emit_figure(node: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    let mut images = node.descendants().filter_map(|d| match d.value() {
        Node::Element(e) if matches!(e.name(), "img" | "image") && is_real_image(e) => Some(e),
        _ => None,
    });
    let (Some(img), None) = (images.next(), images.next()) else {
        walk_children(node, ctx, out);
        return;
    };
    out.push(Block::Image {
        src: img_src(img).unwrap_or_default(),
        alt: img.attr("alt").unwrap_or("").to_string(),
        data: Vec::new(),
        caption: figure_caption_spans(node, ctx),
        math: false,
        width: parse_img_width(img.attr("width"), img.attr("style")),
        ink: None,
    });
}

/// The caption text of a figure wrapper, in priority order: a `<figcaption>`, a
/// caption-classed element (`<p class="…Caption">`), else the first non-empty
/// heading/paragraph beside the image (publishers mark captions up as any of
/// these rather than always a `<figcaption>`). Empty when there is no caption.
fn figure_caption_spans(figure: NodeRef<Node>, ctx: &Ctx) -> Vec<Span> {
    let is_el = |d: &NodeRef<Node>, pred: &dyn Fn(&scraper::node::Element) -> bool| matches!(d.value(), Node::Element(e) if pred(e));
    let caption_node = figure
        .descendants()
        .find(|d| is_el(d, &|e| e.name() == "figcaption"))
        .or_else(|| {
            figure.descendants().find(|d| {
                is_el(d, &|e| {
                    e.attr("class")
                        .is_some_and(|c| c.to_ascii_lowercase().contains("caption"))
                })
            })
        })
        .or_else(|| {
            figure.descendants().find(|d| {
                is_el(d, &|e| {
                    matches!(e.name(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p")
                }) && inline_spans(*d).iter().any(|s| !s.text.trim().is_empty())
            })
        });
    caption_node
        .map(|n| caption_line_spans(n, ctx))
        .unwrap_or_default()
}

/// One line of caption spans from a caption element, bridging the block boundaries the
/// source left unspaced.
///
/// Publishers ship the label and the title as *siblings* with no whitespace between
/// them — `<span class="CaptionNumber">Fig. 8.2</span><p class="SimplePara">Decision
/// rules…</p>`. A browser breaks the line at the block `<p>`, so the space is never
/// needed there; delryn renders a caption as one line, so flattening the subtree inline
/// glued them into "Fig. 8.2Decision rules…". Routing through the same walk +
/// [`coalesce_caption_spans`] that [`emit_caption`] uses for standalone caption wrappers
/// puts the boundary back as a single space.
///
/// Falls back to the flat inline walk when the block walk yields no text — a caption
/// built from something [`coalesce_caption_spans`] drops (it keeps only paragraphs and
/// headings) must not come back empty.
fn caption_line_spans(node: NodeRef<Node>, ctx: &Ctx) -> Vec<Span> {
    let mut inner = Vec::new();
    walk_children(node, ctx, &mut inner);
    let spans = coalesce_caption_spans(inner);
    if spans.iter().any(|s| !s.text.trim().is_empty()) {
        spans
    } else {
        inline_spans(node)
    }
}

/// The authored display width of an `<img>`, from its inline CSS `width` (which
/// wins) or its presentational `width` attribute. This is the publisher's
/// *intended* size; the renderer prefers it over the file's pixel resolution.
pub(super) fn parse_img_width(width_attr: Option<&str>, style_attr: Option<&str>) -> ImageWidth {
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

/// Parse a CSS/HTML length: `80%` → a column fraction, `7.88em`/`rem` → a font-
/// relative (text) width, `600`/`600px` → pixels. Other units (pt/…) are too
/// context-dependent to map reliably, so they fall back to [`ImageWidth::Auto`].
fn parse_len(s: &str) -> ImageWidth {
    let s = s.trim();
    if let Some(pct) = s
        .strip_suffix('%')
        .and_then(|n| n.trim().parse::<f32>().ok())
    {
        return ImageWidth::Pct((pct / 100.0).clamp(0.0, 1.0));
    }
    // Font-relative width: the publisher's text-relative size — the reliable, DPI-
    // independent hint for sizing an equation raster to the surrounding text.
    if let Some(em) = s
        .strip_suffix("rem")
        .or_else(|| s.strip_suffix("em"))
        .and_then(|n| n.trim().parse::<f32>().ok())
        && em > 0.0
    {
        return ImageWidth::Em(em);
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

/// The single real (figure) image when a block's *only* meaningful content is that
/// image — an "image paragraph" (`<p class="image"><img/></p>`, the common
/// InDesign/Calibre shape for a block figure not wrapped in `<figure>`). Such a
/// figure must render block-level (sized to the column), not as an inline picture
/// drawn mid-line at text height — the bug that shrank captioned figures to a
/// thumbnail. `None` when the block has visible text, math, or more than one image,
/// so an image *amid* prose (a genuine inline symbol) stays inline. Anchors
/// (`<a id="fig…">`) and whitespace are ignored.
fn sole_figure_image<'a>(node: NodeRef<'a, Node>) -> Option<&'a scraper::node::Element> {
    let mut img = None;
    for d in node.descendants() {
        match d.value() {
            Node::Text(t) if !t.text.trim().is_empty() => return None,
            Node::Element(e) if matches!(e.name(), "img" | "image") => {
                if img.is_some() || !is_real_image(e) {
                    return None; // a second image, or a math/icon image → not a lone figure
                }
                img = Some(e);
            }
            // An image sitting beside math (a labelled equation) is not a plain figure.
            Node::Element(e) if e.name() == "math" => return None,
            _ => {}
        }
    }
    img
}

/// Emit a paragraph from an element's inline content (with math exponent fix-up),
/// dropping it if there's no visible text. List markers are attached separately
/// by [`list_item`].
fn emit_paragraph(node: NodeRef<Node>, ctx: &Ctx, out: &mut Vec<Block>) {
    // A paragraph that is *only* a figure image (`<p class="image"><img/></p>`)
    // renders as a block figure, not an inline picture: promote it to a `Block::Image`
    // so the block sizer treats it as a column-width figure (and `attach_trailing_captions`
    // can pick up a following "Figure N …" caption paragraph).
    if let Some(img) = sole_figure_image(node) {
        out.push(Block::Image {
            src: img_src(img).unwrap_or_default(),
            alt: img.attr("alt").unwrap_or("").to_string(),
            data: Vec::new(),
            caption: Vec::new(),
            math: false,
            width: parse_img_width(img.attr("width"), img.attr("style")),
            ink: None,
        });
        return;
    }
    let mut spans = inline_spans(node);
    if spans.iter().any(|s| !s.text.trim().is_empty()) {
        superscript_math_exponents(&mut spans);
        // A paragraph that opens with an admonition keyword *immediately* followed by a
        // colon ("Note:", "Warning:", "Hint:") is an inline callout — the reliable content
        // signal publishers use for boxed asides (`<p class="box">`) and inline notes alike.
        // Promote it to a styled Callout (note/tip/warning border + label) instead of leaving
        // it as plain bold text. Multi-word or non-keyword leads ("Definition:", "f: D → ℝ",
        // a URL) never match, so ordinary prose is untouched.
        if !ctx.quote
            && ctx.item_marker().is_none()
            && let Some((kind, prefix)) = leading_callout(&spans)
        {
            // Strip the "Keyword:" prefix from the body — the callout's own label already
            // announces the kind, so a leading "Note:" inside the box reads as a double note.
            strip_leading_chars(&mut spans, prefix);
            out.push(Block::Callout {
                kind,
                title: None,
                blocks: vec![Block::Para {
                    spans,
                    indent: 0,
                    quote: false,
                    marker: None,
                }],
            });
            return;
        }
        out.push(Block::Para {
            spans,
            indent: ctx.indent,
            quote: ctx.quote,
            marker: ctx.item_marker(),
        });
    }
}

/// The admonition kind and the length (in chars) of the leading `Keyword:` prefix (plus any
/// following space) when a paragraph opens with a keyword immediately followed by a colon
/// (`Note:`, `Warning:`, `Hint:`); `None` for ordinary prose. Requires a single leading word
/// before the first colon, so "Definition:", "f: D → ℝ", "http://…", and "Note that …" (no
/// colon after the word) are all left as normal paragraphs. The keyword/colon/space are ASCII,
/// so the byte length is also the char length of the prefix.
fn leading_callout(spans: &[Span]) -> Option<(CalloutKind, usize)> {
    let mut lead = String::new();
    for s in spans {
        lead.push_str(&s.text);
        if lead.len() >= 32 {
            break;
        }
    }
    let ws = lead.len() - lead.trim_start().len(); // leading whitespace bytes
    let colon = lead[ws..].find(':')?;
    let word = lead[ws..ws + colon].trim();
    if word.split_whitespace().count() != 1 {
        return None;
    }
    let kind = CalloutKind::from_word(word)?;
    let after = ws + colon + 1; // just past ':'
    let trailing_ws = lead[after..].len() - lead[after..].trim_start().len();
    Some((kind, after + trailing_ws))
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
            math: s.math,
        })
        .collect()
}

#[cfg(test)]
mod tests;
