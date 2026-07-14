//! Integration tests for the XHTML → block parser (exercise `parse_blocks`).

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
fn deeply_nested_markup_does_not_overflow_the_stack() {
    // A malicious or broken book with thousands of nested elements must not abort
    // the process through unbounded recursion. The DOM walk caps its depth and
    // flattens the remainder to text (MAX_BLOCK_DEPTH / MAX_INLINE_DEPTH), so the
    // call returns and the innermost content still survives. Without the guard
    // this input overflows the stack and aborts the test binary.
    let n = 5000;
    let mut html = String::from("<html><body>");
    html.push_str(&"<div>".repeat(n));
    html.push_str("deep block text");
    html.push_str(&"</div>".repeat(n));
    html.push_str(&"<span>".repeat(n));
    html.push_str("deep inline text");
    html.push_str(&"</span>".repeat(n));
    html.push_str("</body></html>");

    let text = block_text(&parse_blocks(&html));
    assert!(
        text.contains("deep block text"),
        "deep block text preserved"
    );
    assert!(
        text.contains("deep inline text"),
        "deep inline text preserved"
    );
}

#[test]
fn trailing_figure_paragraph_becomes_the_image_caption() {
    // A bare <img> followed by a "Figure N." paragraph (MOBI / plain-EPUB shape,
    // no <figure>/<figcaption>): the paragraph is attached as the image caption
    // (so it renders centred beneath) and no longer a left-aligned paragraph.
    let blocks = parse_blocks(
        r#"<html><body><img src="chart.png" alt="chart"/><p>Figure 5. Example showing buy trades</p><p>Body text after.</p></body></html>"#,
    );
    let cap = blocks.iter().find_map(|b| match b {
        Block::Image { caption, .. } => {
            Some(caption.iter().map(|s| s.text.as_str()).collect::<String>())
        }
        _ => None,
    });
    assert_eq!(cap.as_deref(), Some("Figure 5. Example showing buy trades"));
    let text = block_text(&blocks);
    assert!(
        text.contains("Body text after"),
        "real body paragraph stays"
    );
    assert!(
        !text.contains("Figure 5"),
        "caption is not also left as a paragraph"
    );
}

#[test]
fn prose_mentioning_a_figure_is_not_swallowed_as_a_caption() {
    // "Figure out …" has no figure number, so it stays a normal paragraph and the
    // image keeps an empty caption — ordinary prose is never mistaken for one.
    let blocks = parse_blocks(
        r#"<html><body><img src="x.png" alt="chart"/><p>Figure out how the method works.</p></body></html>"#,
    );
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, Block::Image { caption, .. } if caption.is_empty())),
        "image keeps an empty caption"
    );
    assert!(
        block_text(&blocks).contains("Figure out how"),
        "prose stays a paragraph"
    );
}

/// EPUB cover pages commonly wrap the cover in an SVG `<image xlink:href>`
/// rather than a plain `<img>`; the parser must still emit a `Block::Image`
/// with the referenced source (scraper exposes `xlink:href` as local `href`).
#[test]
fn svg_image_becomes_block_image() {
    let blocks = parse_blocks(
        r#"<html><body><svg version="1.1"><image width="827" height="1246" xlink:href="css/cover.jpeg"/></svg></body></html>"#,
    );
    let src = blocks.iter().find_map(|b| match b {
        Block::Image { src, .. } => Some(src.as_str()),
        _ => None,
    });
    assert_eq!(
        src,
        Some("css/cover.jpeg"),
        "SVG <image> href → Block::Image"
    );
}

#[test]
fn div_class_becomes_callout() {
    let blocks =
        parse_blocks(r#"<html><body><div class="note"><p>remember this</p></div></body></html>"#);
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

    let tip = parse_blocks(r#"<html><body><div epub:type="tip"><p>handy</p></div></body></html>"#);
    assert_eq!(first_callout(&tip).unwrap().0, CalloutKind::Tip);
}

#[test]
fn blockquote_with_callout_class_is_a_callout_not_a_quote() {
    let blocks = parse_blocks(
        r#"<html><body><blockquote class="important"><p>key point</p></blockquote></body></html>"#,
    );
    assert_eq!(first_callout(&blocks).unwrap().0, CalloutKind::Important);
    // A plain blockquote stays a quote, not a callout.
    let plain =
        parse_blocks(r#"<html><body><blockquote><p>just a quote</p></blockquote></body></html>"#);
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
    let blocks =
        parse_blocks(r#"<html><body><div class="footnote"><p>1. a source</p></div></body></html>"#);
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

#[test]
fn footnotes_section_and_wrapper_are_not_definitions() {
    // Only the inner `epub:type="footnote"` with an id is the definition — the
    // plural `footnotes` section and the class-only `<div class="Footnote">`
    // wrapper (no id) are containers, not nested "[note]" definitions.
    let blocks = parse_blocks(
        r##"<html><body>
            <aside epub:type="footnotes"><div class="Heading">Footnotes</div>
                <div class="Footnote"><span class="FootnoteNumber"><a href="#s1">1</a></span>
                    <div class="FootnoteContent" epub:type="footnote" id="Fn1"><p>the note</p></div>
                </div>
            </aside></body></html>"##,
    );
    let labels: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Footnote { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        ["1"],
        "exactly one definition, labelled by its id: {labels:?}"
    );
}

#[test]
fn regenerated_markers_are_stripped() {
    // Explicit list item numbers and footnote backref numbers are chrome we
    // regenerate — they must not double our own marker / `[n]` label.
    let list = parse_blocks(
        r#"<html><body><ol><li class="ListItem">
            <div class="ItemNumber">(1)</div>
            <div class="ItemContent"><p>first item</p></div>
        </li></ol></body></html>"#,
    );
    let t = block_text(&list);
    assert!(t.contains("first item"), "content kept: {t:?}");
    assert!(!t.contains("(1)"), "explicit item number dropped: {t:?}");

    let foot = parse_blocks(
        r##"<html><body><div class="Footnote">
            <span class="FootnoteNumber"><a href="#Fn1_source">1</a></span>
            <div class="FootnoteContent" epub:type="footnote" id="Fn1"><p>note body</p></div>
        </div></body></html>"##,
    );
    // Exactly one footnote (labelled "1"), and no stray standalone backref line.
    assert_eq!(first_footnote(&foot), Some("1"));
    assert!(
        !foot
            .iter()
            .any(|b| matches!(b, Block::Para { spans, .. } if spans.iter().map(|s| s.text.trim()).collect::<String>() == "1")),
        "no standalone backref number paragraph"
    );
}

#[test]
fn ordered_list_does_not_double_hardcoded_numbers() {
    // Springer-style bibliography: an <ol> whose items hard-code their own number
    // in a separate block. Our ordered marker must not double it ("4. 4.").
    let blocks = parse_blocks(
        r#"<html><body><ol class="BibliographyWrapper">
            <li class="Citation"><div class="CitationNumber">1.</div><div class="CitationContent">First ref</div></li>
            <li class="Citation"><div class="CitationNumber">2.</div><div class="CitationContent">Second ref</div></li>
        </ol></body></html>"#,
    );
    let paras: Vec<(Option<String>, String)> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Para { marker, spans, .. } => Some((
                marker.clone(),
                spans.iter().map(|s| s.text.as_str()).collect(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(paras.len(), 2, "no stray number paragraphs: {paras:?}");
    assert_eq!(paras[0].0.as_deref(), Some("1. "));
    assert_eq!(paras[0].1, "First ref");
    assert_eq!(paras[1].0.as_deref(), Some("2. "));
    assert_eq!(paras[1].1, "Second ref");

    // The inline form ("<li>3. Third ref</li>") is de-duplicated too.
    let inline =
        parse_blocks(r#"<html><body><ol start="3"><li>3. Third ref</li></ol></body></html>"#);
    let p = inline
        .iter()
        .find_map(|b| match b {
            Block::Para { marker, spans, .. } => Some((
                marker.clone(),
                spans.iter().map(|s| s.text.as_str()).collect::<String>(),
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(p.0.as_deref(), Some("3. "));
    assert_eq!(p.1, "Third ref");

    // A genuine numbered lead that is NOT the ordinal is left intact.
    let keep = parse_blocks(r#"<html><body><ol><li>4 reasons to refactor</li></ol></body></html>"#);
    let kept: String = keep
        .iter()
        .find_map(|b| match b {
            Block::Para { spans, .. } => Some(spans.iter().map(|s| s.text.as_str()).collect()),
            _ => None,
        })
        .unwrap();
    assert_eq!(kept, "4 reasons to refactor");
}

#[test]
fn printed_toc_indents_by_level_and_drops_page_numbers() {
    let blocks = parse_blocks(
        r##"<html><body>
            <div class="TocChapter">
                <div class="TocEntry"><a href="c.xhtml">11 Other</a><span class="TocPageNumber">359</span></div>
                <div class="TocSection1">
                    <div class="TocEntry"><a href="c.xhtml#s1">11.1 Ensemble</a><span class="TocPageNumber">359</span></div>
                </div>
            </div></body></html>"##,
    );
    let t = block_text(&blocks);
    assert!(!t.contains("359"), "print page numbers dropped: {t:?}");
    let indents: Vec<u8> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Para { indent, spans, .. } => {
                let txt: String = spans.iter().map(|s| s.text.as_str()).collect();
                (txt.contains("Other") || txt.contains("Ensemble")).then_some(*indent)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        indents,
        vec![0, 1],
        "chapter at depth 0, section at depth 1: {indents:?}"
    );
}

#[test]
fn heading_based_toc_indents_by_level() {
    // A heading-based ToC (e.g. Packt) encodes depth in the heading level.
    let blocks = parse_blocks(
        r##"<html><body><div epub:type="toc">
            <h1>Table of Contents</h1>
            <h2><a href="c.xhtml">Chapter 1</a></h2>
            <h3><a href="c.xhtml#s1">Section 1.1</a></h3>
        </div></body></html>"##,
    );
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, Block::Heading { level: 1, .. })),
        "the ToC title stays a heading"
    );
    let indents: Vec<u8> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Para { indent, spans, .. } => {
                let t: String = spans.iter().map(|s| s.text.as_str()).collect();
                (t.contains("Chapter 1") || t.contains("Section 1.1")).then_some(*indent)
            }
            _ => None,
        })
        .collect();
    assert_eq!(indents, vec![0, 1], "h2→0, h3→1: {indents:?}");

    // Outside a ToC, headings stay headings (not indented paragraphs).
    let normal = parse_blocks(r#"<html><body><h2>Real Heading</h2></body></html>"#);
    assert!(
        normal
            .iter()
            .any(|b| matches!(b, Block::Heading { level: 2, .. })),
        "a normal heading is unaffected"
    );
}

#[test]
fn definition_list_pairs_terms_with_descriptions() {
    let blocks = parse_blocks(
        r#"<html><body><dl>
            <dt class="Term">A3C</dt><dd><p>Asynchronous Advantage Actor-Critic</p></dd>
            <dt class="Term">ABC</dt><dd><p>Artificial Bee Colony</p></dd>
        </dl></body></html>"#,
    );
    let paras: Vec<String> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Para { spans, .. } => Some(spans.iter().map(|s| s.text.as_str()).collect()),
            _ => None,
        })
        .collect();
    assert!(
        paras.iter().any(
            |t: &String| t.contains("A3C") && t.contains("Asynchronous Advantage Actor-Critic")
        ),
        "term + description on one entry: {paras:?}"
    );
    // Entries are separate — not run together into one blob.
    assert!(
        !paras
            .iter()
            .any(|t: &String| t.contains("A3C") && t.contains("ABC")),
        "entries kept separate: {paras:?}"
    );
}

#[test]
fn biblioref_link_becomes_citation_anchor() {
    let blocks = parse_blocks(
        r##"<html><body><p>per<a epub:type="biblioref" href="#ref12">[12]</a></p></body></html>"##,
    );
    assert_eq!(
        first_anchor(&blocks),
        Some(&Anchor::Citation("#ref12".into())),
        "raw href kept (may carry a file)"
    );
}

#[test]
fn collect_targets_maps_ids_to_leading_text() {
    let targets = collect_targets(
        r#"<html><body>
            <h2 id="sec2">Chapter Two</h2>
            <figure id="fig1"><figcaption>Figure 1: a system diagram</figcaption></figure>
            <p>no id here</p>
        </body></html>"#,
    );
    assert!(
        targets
            .iter()
            .any(|(id, loc)| id == "sec2" && loc == "Chapter Two"),
        "heading id → its text: {targets:?}"
    );
    assert!(
        targets
            .iter()
            .any(|(id, loc)| id == "fig1" && loc.contains("Figure 1")),
        "figure id → caption: {targets:?}"
    );
    assert_eq!(targets.len(), 2, "only elements with ids: {targets:?}");
}

#[test]
fn footnote_definition_keeps_raw_id_for_resolution() {
    // The definition retains its raw `id` so a reference can resolve to it —
    // the digit `label` is for display only.
    let blocks = parse_blocks(
        r#"<html><body><aside epub:type="footnote" id="fn7"><p>the source</p></aside></body></html>"#,
    );
    let def = delryn_model::find_footnote(&blocks, "#fn7").expect("resolves by id");
    assert!(matches!(def, Block::Footnote { id, label, .. } if id == "fn7" && label == "7"));
}

#[test]
fn dpub_aria_roles_classify_ref_and_def() {
    // DPUB-ARIA semantics (no epub:type): role="doc-noteref" / role="doc-footnote".
    let refs = parse_blocks(
        r##"<html><body><p>see<a role="doc-noteref" href="#n3">3</a></p></body></html>"##,
    );
    assert_eq!(first_anchor(&refs), Some(&Anchor::Footnote("n3".into())));

    let defs = parse_blocks(
        r#"<html><body><aside role="doc-footnote" id="n3"><p>a source</p></aside></body></html>"#,
    );
    assert_eq!(first_footnote(&defs), Some("3"));
    assert!(
        delryn_model::find_footnote(&defs, "n3").is_some(),
        "doc-footnote def is resolvable"
    );
}

/// The `(src, alt)` of a display-math image block, if any.
fn display_math_img(blocks: &[Block]) -> Option<(&str, &str)> {
    blocks.iter().find_map(|b| match b {
        Block::Image { src, alt, .. } => Some((src.as_str(), alt.as_str())),
        _ => None,
    })
}

/// A publisher display equation shipped as a plain `<img>` whose `alt` is just the
/// file path (no math) — Springer's `_HTML.png` fallback — but wrapped in an
/// `EquationContent` container. The container class is the math signal: it must become
/// a `math`-flagged image carrying its `em` width (so the reader sizes it text-
/// relatively / DPI-independently), NOT a plain figure normalised to the column.
#[test]
fn path_alt_equation_image_in_container_is_math_with_em_width() {
    let html = r#"<html><body>
        <div class="Equation NumberedEquation" id="Equ3">
          <div class="EquationWrapper">
            <div class="EquationContent">
              <div class="MediaObject">
                <img alt="../images/ch/Equ3_HTML.png"
                     src="../images/ch/Equ3_HTML.png" style="width:27.08em"/>
              </div>
            </div>
            <div class="EquationNumber">(2.2)</div>
          </div>
        </div>
      </body></html>"#;
    let blocks = super::parse_blocks(html);
    let img = blocks.iter().find_map(|b| match b {
        Block::Image {
            src, math, width, ..
        } => Some((src.as_str(), *math, *width)),
        _ => None,
    });
    match img {
        Some((src, math, width)) => {
            assert!(
                src.ends_with("Equ3_HTML.png"),
                "the publisher raster: {src}"
            );
            assert!(
                math,
                "flagged as math (equation raster), not a plain figure"
            );
            assert_eq!(
                width,
                ImageWidth::Em(27.08),
                "carries the authored text-relative em width"
            );
        }
        None => panic!("expected a math image block, got {blocks:?}"),
    }
}

/// The path-alt equation detection must NOT fire on a broad container that merely
/// *contains* an equation among prose (a chapter/section div, visited on the way
/// down) — that would swallow the surrounding text into one image. The prose must
/// survive as paragraphs, with the equation as its own block.
#[test]
fn broad_container_with_prose_is_not_collapsed_to_one_equation() {
    let html = r#"<html><body>
        <div class="ChapterBody">
          <p>Before the equation we say something.</p>
          <div class="Equation" id="Equ9">
            <div class="EquationContent"><div class="MediaObject">
              <img alt="../images/ch/Equ9_HTML.png"
                   src="../images/ch/Equ9_HTML.png" style="width:12.8em"/>
            </div></div>
          </div>
          <p>After the equation we continue.</p>
        </div>
      </body></html>"#;
    let blocks = super::parse_blocks(html);
    let text: String = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Para { spans, .. } => {
                Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        text.contains("Before the equation") && text.contains("After the equation"),
        "prose must survive around the equation, got blocks: {blocks:?}"
    );
    let math_imgs = blocks
        .iter()
        .filter(|b| matches!(b, Block::Image { math: true, .. }))
        .count();
    assert_eq!(math_imgs, 1, "exactly the one equation is a math image");
}

/// First `Block::Code`'s lines, if any.
fn first_code(blocks: &[Block]) -> Option<&[String]> {
    blocks.iter().find_map(|b| match b {
        Block::Code { lines, .. } => Some(lines.as_slice()),
        _ => None,
    })
}

#[test]
fn parses_authored_image_width() {
    // Inline CSS `width` wins over the presentational attribute; `max-width` /
    // `min-width` are ignored (they bound but don't set the size).
    match parse_img_width(Some("999"), Some("max-width:100%; width:60%")) {
        ImageWidth::Pct(p) => assert!((p - 0.6).abs() < 1e-6, "got {p}"),
        other => panic!("expected Pct(0.6), got {other:?}"),
    }
    // The pixel `width` attribute when there's no CSS (bare number or `px`).
    assert_eq!(parse_img_width(Some("480"), None), ImageWidth::Px(480));
    assert_eq!(parse_img_width(Some("480px"), None), ImageWidth::Px(480));
    // A font-relative `em`/`rem` width is kept — the publisher's text-relative size,
    // the reliable DPI-independent hint for sizing equation rasters to the prose.
    assert_eq!(parse_img_width(Some("7.88em"), None), ImageWidth::Em(7.88));
    assert_eq!(
        parse_img_width(None, Some("width:1.5rem")),
        ImageWidth::Em(1.5)
    );
    // Other context-dependent units and absent sizes fall back to Auto (normalized).
    assert_eq!(parse_img_width(Some("12pt"), None), ImageWidth::Auto);
    assert_eq!(parse_img_width(None, None), ImageWidth::Auto);
}

/// The first `Block::Image` with its caption text, if any.
fn first_captioned_image(blocks: &[Block]) -> Option<(&str, String)> {
    blocks.iter().find_map(|b| match b {
        Block::Image { src, caption, .. } => Some((
            src.as_str(),
            caption.iter().map(|s| s.text.as_str()).collect::<String>(),
        )),
        _ => None,
    })
}

#[test]
fn figure_caption_attaches_to_the_image() {
    // Publishers commonly mark a figure caption up as an <h6>/<p> beside the
    // <img> (not a <figcaption>). The parser merges them so the caption becomes
    // the image's caption (rendered centred beneath it), not a stray heading.
    let blocks = parse_blocks(
        r#"<html><body><figure><div class="figure">
            <img alt="a chart" src="assets/fig.png" width="500"/>
            <h6><span class="label">Figure 4-5. </span>Compare distributions.</h6>
        </div></figure></body></html>"#,
    );
    let (src, caption) = first_captioned_image(&blocks).expect("a captioned image");
    assert_eq!(src, "assets/fig.png");
    assert_eq!(caption, "Figure 4-5. Compare distributions.");
    // The caption is not ALSO emitted as a separate heading/paragraph block.
    assert!(
        !blocks.iter().any(|b| matches!(b, Block::Heading { .. })),
        "caption consumed into the image, not left as a heading: {blocks:?}"
    );
    // A <figcaption> works the same way.
    let blocks = parse_blocks(
        r#"<html><body><figure>
            <img alt="d" src="d.png"/><figcaption>Fig 2: the pipeline</figcaption>
        </figure></body></html>"#,
    );
    let (_, caption) = first_captioned_image(&blocks).expect("captioned image");
    assert_eq!(caption, "Fig 2: the pipeline");
}

#[test]
fn imagewrap_div_with_caption_class_merges() {
    // No <figure>/<figcaption>: a `<div class="imagewrap">` holding one image and
    // a caption-classed `<p>` (Pearson-style). The caption-class marker makes it a
    // figure so the caption attaches to the image and centres beneath it.
    let blocks = parse_blocks(
        r#"<html><body><div id="fig1-2" class="imagewrap">
            <img class="image" src="../images/img_p26.png" alt="images" width="255"/>
            <p class="EB28FSCaption"><strong>Figure 1.2:</strong> Fundamental relationship.</p>
        </div></body></html>"#,
    );
    let (src, caption) = first_captioned_image(&blocks).expect("captioned image");
    assert_eq!(src, "../images/img_p26.png");
    assert_eq!(caption, "Figure 1.2: Fundamental relationship.");
    assert!(
        !blocks.iter().any(|b| matches!(b, Block::Para { .. })),
        "caption consumed into the image, not left as a paragraph: {blocks:?}"
    );

    // A plain content div (no caption marker) is NOT merged — its prose survives.
    let blocks = parse_blocks(
        r#"<html><body><div>
            <img src="x.png" alt="x"/><p>This is ordinary body prose, not a caption.</p>
        </div></body></html>"#,
    );
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, Block::Para { spans, .. } if spans.iter().any(|s| s.text.contains("ordinary body prose")))),
        "non-figure div keeps its prose paragraph: {blocks:?}"
    );
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
fn standalone_latex_math_image_renders_via_latex() {
    // EPUB display math: a math image with LaTeX (`\[ … \]`) in its alt on its own
    // line becomes a Block::Math carrying the recovered (delimiter-stripped) LaTeX,
    // so the reader renders it crisply with RaTeX rather than the publisher's PNG.
    let blocks = parse_blocks(
        r#"<html><body><p><img alt="\[\int_0^1 x\,dx\]" src="eq.png"/></p></body></html>"#,
    );
    assert_eq!(
        first_math_latex(&blocks).as_deref(),
        Some(r"\int_0^1 x\,dx"),
        "display math image → Block::Math with the recovered LaTeX"
    );
    let uni = first_math(&blocks).expect("a math block");
    assert!(
        uni.contains('∫') && !uni.contains("\\int"),
        "unicode alt: {uni:?}"
    );
}

#[test]
fn standalone_mathml_image_falls_back_to_the_publisher_image() {
    // A math image whose alt is MathML (no LaTeX to re-render) keeps the publisher's
    // equation image, with the Unicode transcription as the fallback alt.
    let alt =
        r#"<mml:math xmlns:mml="http://www.w3.org/1998/Math/MathML"><mml:mi>Σ</mml:mi></mml:math>"#
            .replace('"', "&quot;");
    let blocks = parse_blocks(&format!(
        r#"<html><body><p><img alt="{alt}" src="e.png"/></p></body></html>"#
    ));
    let (src, alt_text) = display_math_img(&blocks).expect("a display-math image");
    assert_eq!(src, "e.png", "no LaTeX → keeps the publisher image");
    assert!(
        alt_text.contains('Σ') && !alt_text.contains("<m"),
        "unicode alt: {alt_text:?}"
    );
}

#[test]
fn mathml_in_image_alt_renders_unicode_not_tags() {
    // EPUBs converted from OOXML ship math as MathML inside an <img alt="…">,
    // with the inner quotes escaped (scraper decodes them back).
    let raw =
        r#"<mml:math xmlns:mml="http://www.w3.org/1998/Math/MathML"><mml:mi>Σ</mml:mi></mml:math>"#;
    let alt = raw.replace('"', "&quot;");
    // Standalone → display-math image; its alt is Unicode, never raw tags.
    let block = parse_blocks(&format!(
        r#"<html><body><p><img alt="{alt}" src="e.png"/></p></body></html>"#
    ));
    let (_src, alt_text) = display_math_img(&block).expect("display math image from MathML alt");
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
    // Cross-refs keep the raw href (bare fragment or file#fragment) so the
    // reader can resolve the file and id.
    let cross =
        parse_blocks(r##"<html><body><p><a href="#sec2">see section 2</a></p></body></html>"##);
    assert_eq!(
        first_anchor(&cross),
        Some(&Anchor::CrossRef("#sec2".into()))
    );

    let xfile =
        parse_blocks(r##"<html><body><p><a href="ch2.xhtml#fig1">Figure 1</a></p></body></html>"##);
    assert_eq!(
        first_anchor(&xfile),
        Some(&Anchor::CrossRef("ch2.xhtml#fig1".into())),
        "cross-file ref keeps its file"
    );

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

/// First `Block::Math`'s rendered text, if any.
fn first_math(blocks: &[Block]) -> Option<&str> {
    blocks.iter().find_map(|b| match b {
        Block::Math { unicode, .. } => Some(unicode.as_str()),
        _ => None,
    })
}

/// The first `Block::Math`'s recovered LaTeX source (`None` when MathML-only).
fn first_math_latex(blocks: &[Block]) -> Option<String> {
    blocks
        .iter()
        .find_map(|b| match b {
            Block::Math { latex, .. } => Some(latex.clone()),
            _ => None,
        })
        .flatten()
}

#[test]
fn native_display_mathml_becomes_a_math_block() {
    // `<math display="block">` is a display equation → a Block::Math, never prose.
    let blocks = parse_blocks(
        r#"<html><body><math display="block"><msup><mi>x</mi><mn>2</mn></msup></math></body></html>"#,
    );
    let tex = first_math(&blocks).expect("a math block");
    assert!(
        tex.contains("x²") || tex.contains('x'),
        "transcoded: {tex:?}"
    );
    // No raw MathML token names leak into the output.
    assert!(
        !tex.contains("msup") && !tex.contains("<m"),
        "no tags: {tex:?}"
    );
}

#[test]
fn native_math_prefers_authored_tex() {
    // `alttext` (LaTeX) is authored and exact — use it over walking presentation.
    let blocks = parse_blocks(
        r#"<html><body><math display="block" alttext="\int_0^1 x\,dx"><mrow><mi>noise</mi></mrow></math></body></html>"#,
    );
    let tex = first_math(&blocks).expect("a math block");
    assert!(tex.contains('∫'), "alttext LaTeX → unicode: {tex:?}");
    assert!(!tex.contains("\\int"), "no raw LaTeX: {tex:?}");

    // `<annotation encoding="application/x-tex">` is the embedded-LaTeX form.
    let annotated = parse_blocks(
        r#"<html><body><math display="block"><mrow><mi>x</mi></mrow><annotation encoding="application/x-tex">\alpha</annotation></math></body></html>"#,
    );
    assert_eq!(
        first_math(&annotated),
        Some("α"),
        "annotation TeX → unicode"
    );
}

#[test]
fn native_math_retains_latex_source() {
    // `alttext` LaTeX is retained (alongside the Unicode) for the graphical renderer.
    let alt = parse_blocks(
        r#"<html><body><math display="block" alttext="\int_0^1 x\,dx"><mi>noise</mi></math></body></html>"#,
    );
    assert_eq!(first_math_latex(&alt).as_deref(), Some(r"\int_0^1 x\,dx"));

    // `<annotation …tex>` LaTeX is retained too.
    let annotated = parse_blocks(
        r#"<html><body><math display="block"><mi>x</mi><annotation encoding="application/x-tex">\alpha</annotation></math></body></html>"#,
    );
    assert_eq!(first_math_latex(&annotated).as_deref(), Some(r"\alpha"));

    // Presentation-MathML-only math (no alttext/annotation) is transpiled to LaTeX so
    // it can still render graphically instead of as lossy Unicode.
    let mathml = parse_blocks(
        r#"<html><body><math display="block"><msup><mi>x</mi><mn>2</mn></msup></math></body></html>"#,
    );
    let latex = first_math_latex(&mathml).expect("MathML transpiled to LaTeX");
    assert!(
        latex.contains("x") && latex.contains("^{2}"),
        "presentation MathML → LaTeX superscript: {latex}"
    );
}

#[test]
fn native_inline_mathml_stays_in_the_paragraph() {
    // Inline `<math>` (no display="block") renders as Unicode within its prose,
    // not as a standalone Block::Math.
    let blocks = parse_blocks(
        r#"<html><body><p>let <math alttext="\alpha"><mi>α</mi></math> be small</p></body></html>"#,
    );
    assert!(first_math(&blocks).is_none(), "inline math is not a block");
    let text = block_text(&blocks);
    assert!(
        text.contains("let") && text.contains('α') && text.contains("small"),
        "got: {text:?}"
    );
    assert!(
        !text.contains("<m") && !text.contains("alttext"),
        "no tags: {text:?}"
    );
}

// ── Display-vs-inline math classification across publisher encodings ─────────
// One test per real-world encoding, so the classifier stays publisher-agnostic
// (regression guard for the survey in `docs/parsing.md`).

/// Springer display equations carry **no** `display` attr — they use
/// `displaystyle="true"` and sit in an `EquationContent` container. Must become a
/// display Block::Math (RaTeX-rendered), not garbled inline Unicode.
#[test]
fn springer_displaystyle_math_is_a_display_block() {
    let blocks = parse_blocks(
        r#"<html><body><div class="EquationContent"><math displaystyle="true" alttext="$$P(A|B)={P(A\cap B) \over P(B)}$$"><mi>noise</mi></math></div><div class="EquationNumber">(2)</div></body></html>"#,
    );
    assert_eq!(
        first_math_latex(&blocks).as_deref(),
        Some(r"P(A|B)={P(A\cap B) \over P(B)}"),
        "displaystyle → display Block::Math with stripped LaTeX"
    );
}

/// Springer inline equations carry `display="inline"` and an `InlineEquation`
/// container class — they must stay in the paragraph, never a display block.
#[test]
fn springer_inline_equation_stays_inline() {
    let blocks = parse_blocks(
        r#"<html><body><p>since <span class="InlineEquation"><math display="inline" alttext="$$P(B)>0$$"><mi>noise</mi></math></span> holds</p></body></html>"#,
    );
    assert!(
        first_math(&blocks).is_none(),
        "inline math is not a display block"
    );
    let t = block_text(&blocks);
    assert!(
        t.contains("since") && t.contains("holds"),
        "stays in prose: {t:?}"
    );
}

/// The `EquationContent`/`Equation` container class alone marks display, even with
/// no `display`/`displaystyle` attr (older Springer / generic equation wrappers).
#[test]
fn equation_container_class_marks_display() {
    let blocks = parse_blocks(
        r#"<html><body><div class="Equation"><math alttext="x = 1"><mi>x</mi></math></div></body></html>"#,
    );
    assert_eq!(
        first_math(&blocks),
        Some("x = 1"),
        "equation class → display block"
    );
}

/// A MathJax-style equation image (`<img alt="$$…$$">`) inside an equation
/// container renders via its recovered LaTeX (RaTeX), not the raster.
#[test]
fn mathjax_image_equation_renders_via_latex() {
    let blocks = parse_blocks(
        r#"<html><body><div class="EquationContent"><div class="MediaObject"><img alt="$$ \frac{a}{b} $$" src="Equ2.png"/></div></div></body></html>"#,
    );
    assert_eq!(
        first_math_latex(&blocks).as_deref(),
        Some(r"\frac{a}{b}"),
        "equation-container image → Block::Math from the alt LaTeX"
    );
}

/// JATS-derived markup: a `disp-formula` class is display, `inline-formula` inline.
#[test]
fn jats_formula_classes_classify() {
    let disp = parse_blocks(
        r#"<html><body><div class="disp-formula"><math alttext="a+b"><mi>a</mi></math></div></body></html>"#,
    );
    assert_eq!(first_math(&disp), Some("a+b"), "disp-formula → display");

    let inline = parse_blocks(
        r#"<html><body><p>see <span class="inline-formula"><math alttext="c"><mi>c</mi></math></span> here</p></body></html>"#,
    );
    assert!(first_math(&inline).is_none(), "inline-formula → inline");
}

/// A `\[…\]` (display) vs `\(…\)` (inline) delimiter decides when there's no class
/// or attribute signal — the plain TeX/MathJax convention.
#[test]
fn tex_delimiter_decides_without_a_class() {
    let disp =
        parse_blocks(r#"<html><body><div><img alt="\[ x^2 \]" src="d.png"/></div></body></html>"#);
    assert_eq!(
        first_math_latex(&disp).as_deref(),
        Some("x^2"),
        "\\[…\\] → display"
    );

    let inline =
        parse_blocks(r#"<html><body><p>a <img alt="\( y \)" src="i.png"/> b</p></body></html>"#);
    assert!(
        first_math(&inline).is_none() && display_math_img(&inline).is_none(),
        "\\(…\\) → inline"
    );
}

/// Wiley/For-Dummies encode math as `<img alt="math" src="eqN.png">` with the real
/// MathML in a trailing HTML comment. Recover from the comment (not the bare
/// `alt="math"`, which used to print as literal "[math]").
#[test]
fn wiley_comment_sourced_math_is_recovered() {
    let mml = r#"<m:math xmlns:m="http://www.w3.org/1998/Math/MathML"><m:mi>A</m:mi><m:mo>&#8745;</m:mo><m:mi>B</m:mi></m:math>"#;
    // Inline (mid-sentence) → Unicode from the comment, never "[math]".
    let inline = parse_blocks(&format!(
        r#"<html><body><p>count outcomes in <img alt="math" src="eq.png"/><!--{mml}--> twice.</p></body></html>"#
    ));
    let t = block_text(&inline);
    assert!(t.contains('∩'), "recovered from the comment: {t:?}");
    assert!(!t.contains("[math]"), "no bare marker leaks: {t:?}");

    // Standalone → the publisher equation image (MathML has no LaTeX to re-render).
    let disp = parse_blocks(&format!(
        r#"<html><body><p><img alt="math" src="eq2.png"/><!--{mml}--></p></body></html>"#
    ));
    let (src, _) = display_math_img(&disp).expect("standalone comment-math → publisher image");
    assert_eq!(src, "eq2.png");
}

/// With no class/attr/delimiter signal, a math node that is the sole content of a
/// block element stands alone → display; the same math mid-sentence stays inline.
#[test]
fn standalone_math_is_display_but_mid_sentence_is_inline() {
    let standalone = parse_blocks(
        r#"<html><body><p><math alttext="E=mc^2"><mi>E</mi></math></p></body></html>"#,
    );
    assert_eq!(
        first_math(&standalone),
        Some("E=mc²"),
        "sole content of <p> → display"
    );

    let inline = parse_blocks(
        r#"<html><body><p>recall <math alttext="E=mc^2"><mi>E</mi></math> from before</p></body></html>"#,
    );
    assert!(first_math(&inline).is_none(), "mid-sentence → inline");
}

/// An empty marker element beside a lone equation (InDesign `<st>` story anchors,
/// an empty `<a id>` link target) must NOT knock the equation off the display path.
/// Packt / InDesign MathTools ship display equations as `<p><img …/><st></st></p>`;
/// the empty `<st>` used to make the equation read as inline Unicode (deaf to the
/// display Math-size knob) instead of the em-sized publisher raster.
#[test]
fn empty_marker_sibling_does_not_force_a_display_equation_inline() {
    for sib in [
        r#"<st c="7051"></st>"#,
        r#"<a id="anchor"></a>"#,
        "<span></span>",
    ] {
        let html = format!(
            r#"<html><body><p><img src="image/98.png" alt="&lt;math display=&quot;block&quot;&gt;&lt;msub&gt;&lt;mi&gt;x&lt;/mi&gt;&lt;mi&gt;i&lt;/mi&gt;&lt;/msub&gt;&lt;/math&gt;" style="width:1.812em"/>{sib}</p></body></html>"#
        );
        let blocks = parse_blocks(&html);
        let img = blocks.iter().find_map(|b| match b {
            Block::Image {
                src, math, width, ..
            } => Some((src.as_str(), *math, *width)),
            _ => None,
        });
        assert_eq!(
            img,
            Some(("image/98.png", true, ImageWidth::Em(1.812))),
            "with sibling {sib:?}, the lone equation must be a math image, got {blocks:?}"
        );
    }
}

/// A container that wraps *several* display equations with no separating text must
/// emit them all — the classifier recurses instead of collapsing to the first (which
/// used to silently drop every equation after it).
#[test]
fn several_display_equations_in_one_container_all_render() {
    // Two native <math display="block"> equations, side by side in one <div>.
    let two = parse_blocks(
        r#"<html><body><div><math display="block" alttext="a=1"><mi>a</mi></math><math display="block" alttext="b=2"><mi>b</mi></math></div></body></html>"#,
    );
    let latex: Vec<_> = two
        .iter()
        .filter_map(|b| match b {
            Block::Math { latex, .. } => latex.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(
        latex,
        vec!["a=1", "b=2"],
        "both equations render, got {two:?}"
    );

    // The equation-class variant: two `EquationContent` wrappers inside one
    // `Equation` container — each is its own display block, none dropped.
    let wrapped = parse_blocks(
        r#"<html><body><div class="Equation"><div class="EquationContent"><math alttext="x=1"><mi>x</mi></math></div><div class="EquationContent"><math alttext="y=2"><mi>y</mi></math></div></div></body></html>"#,
    );
    let n = wrapped
        .iter()
        .filter(|b| matches!(b, Block::Math { .. }))
        .count();
    assert_eq!(n, 2, "both wrapped equations render, got {wrapped:?}");
}

/// The paragraph texts, in order (skips images/rules/etc.).
fn para_texts(blocks: &[Block]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Para { spans, .. } => {
                Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn block_display_classes_extracts_the_selector_subject() {
    let css = r#"
        .A, .Wrapper .B { color: red; display: block; }
        .C { display: inline; }
        /* .D { display: block; } */
        @media screen { .E { display: block; } }
    "#;
    let set = block_display_classes(css);
    assert!(set.contains("A"), "grouped selector subject");
    assert!(
        set.contains("B"),
        "descendant selector *subject* (last class)"
    );
    assert!(!set.contains("Wrapper"), "not the descendant ancestor");
    assert!(!set.contains("C"), "display:inline is not block");
    assert!(!set.contains("D"), "commented-out rule ignored");
    assert!(set.contains("E"), "rule inside @media still counts");
}

#[test]
fn css_block_display_spans_render_on_separate_lines() {
    // The Springer/Apress citation shape: author · title · DOI link, each a
    // `display:block` <span> with no whitespace between them.
    let html = r#"
        <html><head><style>
            .Author, .BookTitle, .ChapterDOI { display: block; }
        </style></head><body>
          <div class="cite"><span class="Author">Sayan Mukhopadhyay</span><span class="BookTitle">Advanced Data Analytics Using Python</span><span class="ChapterDOI"><a href="https://doi.org/x">https://doi.org/x</a></span></div>
        </body></html>
    "#;
    let texts = para_texts(&parse_blocks(html));
    assert!(
        texts
            .iter()
            .any(|t| t == "Advanced Data Analytics Using Python"),
        "title on its own line: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "https://doi.org/x"),
        "DOI link on its own line: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("Pythonhttps")),
        "title and DOI must not run together: {texts:?}"
    );
}

#[test]
fn inline_spans_stay_inline_without_a_block_rule() {
    // No display:block anywhere → adjacent spans still join (the existing behaviour,
    // so we don't split ordinary inline markup).
    let html = r#"<html><body><p><span>foo</span><span>bar</span></p></body></html>"#;
    let texts = para_texts(&parse_blocks(html));
    assert_eq!(
        texts,
        vec!["foobar".to_string()],
        "no block rule → one paragraph"
    );
}
