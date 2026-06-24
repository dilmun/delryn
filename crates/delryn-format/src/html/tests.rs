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
        Block::Math { tex } => Some(tex.as_str()),
        _ => None,
    })
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
