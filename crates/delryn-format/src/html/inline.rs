//! Inline run collection: text styles, links/anchors, inline code,
//! icon glyphs, inline-image fallbacks.

use super::*;

/// A "real" image is a figure/cover — not a math equation or a UI icon.
pub(super) fn is_real_image(e: &scraper::node::Element) -> bool {
    let alt = e.attr("alt").unwrap_or("");
    let src = e.attr("src").unwrap_or("");
    !delryn_model::math::is_math(alt) && !is_icon_src(src)
}

fn is_icon_src(src: &str) -> bool {
    let s = src.to_lowercase();
    profile().icon_src_keywords.iter().any(|k| s.contains(k))
}

/// Map a small inline UI icon (by its `alt`/`src`) to a themed, single-width
/// Unicode glyph — so list checks and admonition markers (Tip / Warning /
/// Remember / Technical Stuff …) render as a symbol rather than `[tip]` text.
/// Text-presentation code points only (no colour emoji). `None` for non-icons.
pub(super) fn icon_glyph(alt: &str, src: &str) -> Option<char> {
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

pub(super) fn collect_inline(node: NodeRef<Node>, style: Inline, out: &mut Vec<Span>) {
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
                // Native inline MathML → Unicode (block math is handled at the
                // block level). Don't recurse, or the raw token text leaks out.
                "math" => {
                    out.push(Span {
                        text: native_math_unicode(node),
                        style: Inline {
                            math: true,
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

/// A stand-in for an inline image: its alt text, else a plain "[image]" label.
/// (Real image rendering is the graphics-protocol task.)
pub(super) fn img_label(e: &scraper::node::Element) -> String {
    match e.attr("alt").map(str::trim).filter(|a| !a.is_empty()) {
        Some(alt) => format!("[{alt}]"),
        None => "[image]".to_string(),
    }
}

/// Build the navigation [`Anchor`] for an `<a>`: a footnote reference (epub:type
/// noteref, or an internal `#id` that looks like a note), an internal
/// cross-reference, or an external link.
pub(super) fn link_anchor(e: &scraper::node::Element) -> Option<Anchor> {
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
