//! Inline run collection: text styles, links/anchors, inline code,
//! icon glyphs, inline-image fallbacks.

use super::*;

/// A "real" image is a figure/cover — not a math equation or a UI icon.
pub(super) fn is_real_image(e: &scraper::node::Element) -> bool {
    let alt = e.attr("alt").unwrap_or("");
    let src = img_src(e).unwrap_or_default();
    !delryn_model::math::is_math(alt) && !is_icon_src(&src)
}

/// An image element's source: an `<img>`'s `src`, or an SVG `<image>`'s
/// `xlink:href` / `href`. The latter is namespaced, so scraper's `attr("href")`
/// misses it — iterate the attributes and match the local name instead. EPUB
/// covers are very often an `<svg><image xlink:href="cover.jpg"/></svg>`.
pub(super) fn img_src(e: &scraper::node::Element) -> Option<String> {
    if let Some(s) = e.attr("src") {
        return Some(s.to_string());
    }
    // `href` when scraper namespaces `xlink:href` (local name `href`), or the
    // literal `xlink:href` when it doesn't — match either.
    e.attrs()
        .find(|(k, _)| *k == "href" || k.ends_with(":href"))
        .map(|(_, v)| v.to_string())
}

/// Maximum inline-nesting depth before the walk stops recursing and flattens the
/// rest to text — guards against a stack overflow on pathological markup (e.g.
/// thousands of nested `<span>`s). Real inline nesting is a few levels deep.
const MAX_INLINE_DEPTH: u16 = 256;

pub(super) fn collect_inline(node: NodeRef<Node>, style: Inline, out: &mut Vec<Span>) {
    collect_inline_at(node, style, 0, out);
}

/// Emit all descendant text of `node` as flat spans without recursing — the
/// escape hatch when inline nesting exceeds [`MAX_INLINE_DEPTH`]. `descendants()`
/// walks iteratively, so this cannot overflow the stack.
fn flatten_inline_text(node: NodeRef<Node>, style: Inline, out: &mut Vec<Span>) {
    for d in node.descendants() {
        if let Node::Text(t) = d.value() {
            out.push(Span {
                text: t.text.to_string(),
                style,
                anchor: None,
                math: None,
            });
        }
    }
}

fn collect_inline_at(node: NodeRef<Node>, style: Inline, depth: u16, out: &mut Vec<Span>) {
    if depth >= MAX_INLINE_DEPTH {
        flatten_inline_text(node, style, out);
        return;
    }
    match node.value() {
        Node::Text(t) => out.push(Span {
            text: t.text.to_string(),
            style,
            anchor: None,
            math: None,
        }),
        Node::Element(e) => {
            // Regenerated markers (footnote backref numbers, list item numbers)
            // are chrome — drop so they don't double our labels/markers.
            if is_marker_chrome(e) {
                return;
            }
            // Links carry a navigation anchor (footnote ref / cross-ref / URL);
            // collect the inner runs, then stamp the anchor on each.
            if e.name() == "a" {
                let astyle = Inline {
                    link: true,
                    ..style
                };
                let mut inner = Vec::new();
                for c in node.children() {
                    collect_inline_at(c, astyle, depth + 1, &mut inner);
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
                    // A small UI icon (check / tip / warning / remember / …) → a
                    // themed glyph instead of "[tip]" text.
                    if let Some(g) = icon_glyph(alt, src) {
                        out.push(Span {
                            text: g.to_string(),
                            style: Inline {
                                italic: true,
                                ..style
                            },
                            anchor: None,
                            math: None,
                        });
                        return;
                    }
                    // Inline math source: LaTeX/MathML in the `alt` (MathJax-style),
                    // or a trailing MathML/LaTeX comment (Wiley/For-Dummies ship it
                    // there with a bare `alt="math"`, which otherwise printed as
                    // "[math]"). Keep the recovered LaTeX (delimiter-stripped) so the
                    // reader can render it graphically; MathML-only stays Unicode.
                    let math_raw = e
                        .attr("alt")
                        .filter(|a| delryn_model::math::is_math(a))
                        .map(str::to_string)
                        .or_else(|| img_math_comment(node));
                    if let Some(raw) = math_raw {
                        let latex = (!delryn_model::math::is_mathml(&raw)).then(|| {
                            delryn_model::SpanMath::Latex(delryn_model::math::strip_delimiters(
                                &raw,
                            ))
                        });
                        out.push(Span {
                            text: math_unicode(&raw),
                            style: Inline {
                                italic: true,
                                math: true,
                                ..style
                            },
                            anchor: None,
                            math: latex,
                        });
                        return;
                    }
                    // Any other inline image: a quiet marker for a useless alt (often
                    // math the converter rasterised), else the alt label.
                    let text = if is_placeholder_alt(alt) {
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
                        math: None,
                    });
                    return;
                }
                // Native inline math → Unicode approximation, plus the recovered
                // LaTeX source (when the parser could get it) so the reader can
                // rasterise it graphically. Don't recurse, or the raw token text
                // leaks out. Block math is handled at the block level.
                "math" => {
                    let (text, latex) = native_math(node);
                    let mstyle = Inline {
                        math: true,
                        ..style
                    };
                    out.push(match latex {
                        Some(latex) => Span {
                            text,
                            style: mstyle,
                            anchor: None,
                            math: Some(delryn_model::SpanMath::Latex(latex)),
                        },
                        None => Span {
                            text,
                            style: mstyle,
                            anchor: None,
                            math: None,
                        },
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
                collect_inline_at(c, style, depth + 1, out);
            }
        }
        _ => {}
    }
}

/// A stand-in for an inline image: its alt text, else a plain "[image]" label.
/// (Real image rendering is the graphics-protocol task.)
pub(super) fn img_label(e: &scraper::node::Element) -> String {
    match e.attr("alt").map(str::trim).filter(|a| !a.is_empty()) {
        Some(alt) => format!("[{alt}]"),
        None => "[image]".to_string(),
    }
}

/// Build the navigation [`Anchor`] for an `<a>`: a footnote reference (`epub:type`
/// noteref or DPUB-ARIA `role="doc-noteref"`, or an internal `#id` that looks
/// like a note), an internal cross-reference, or an external link.
pub(super) fn link_anchor(e: &scraper::node::Element) -> Option<Anchor> {
    let etype = e.attr("epub:type").unwrap_or("").to_ascii_lowercase();
    let role = e.attr("role").unwrap_or("").to_ascii_lowercase();
    let href = e.attr("href")?;
    // A note reference by standardised semantics: EPUB `epub:type="noteref"` or
    // the DPUB-ARIA `role="doc-noteref"`.
    if etype.contains("noteref") || role.contains("doc-noteref") {
        return Some(Anchor::Footnote(href.trim_start_matches('#').to_string()));
    }
    // A bibliography citation: `epub:type="biblioref"` / `role="doc-biblioref"`.
    // Keep the raw href — it may carry a file (`references.xhtml#ref12`).
    if etype.contains("biblioref") || role.contains("doc-biblioref") {
        return Some(Anchor::Citation(href.to_string()));
    }
    // An external link (has a URL scheme) is copied, not navigated.
    if href.contains("://") || href.starts_with("mailto:") {
        return Some(Anchor::Link(href.to_string()));
    }
    // A bare same-file fragment that looks like a note → footnote.
    if let Some(frag) = href.strip_prefix('#') {
        let low = frag.to_ascii_lowercase();
        if low.contains("fn") || low.contains("note") {
            return Some(Anchor::Footnote(frag.to_string()));
        }
    }
    // Any other internal reference: a bare `#frag`, a `file#frag`, or a `file`.
    // The raw href is kept so the reader can resolve its file/fragment.
    Some(Anchor::CrossRef(href.to_string()))
}
