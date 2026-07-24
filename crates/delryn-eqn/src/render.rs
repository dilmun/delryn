//! The render ladder: turn a recovered [`MathItem`] into something drawable, trying the
//! sources in order — **typeset** (crisp, our engine) → **picture** (the publisher's own
//! image) → **text** (Unicode). A failure at any rung *descends* to the next, so an equation
//! can never render nothing (the never-blank guarantee). See `docs/MATH-RENDERING.md`.
//!
//! This stage produces engine output + a size hint; sizing-to-the-page and theming happen
//! downstream. The typeset raster is black-on-transparent (recoloured to the theme later)
//! and carries its em-relative baseline split so inline math can sit on the text baseline.

use ratex_layout::{LayoutOptions, layout, to_display_list};
use ratex_parser::parse_node::{AtomFamily, ParseNode};
use ratex_render::{RenderOptions, render_to_png};
use ratex_types::color::Color;
use ratex_types::math_style::MathStyle;

use crate::typeset::to_nodes;
use delryn_model::{MarkupSource, MathItem, PictureSize};

/// The chosen render for one equation. Each variant is a rung of the ladder; `Text` is the
/// floor that always succeeds.
#[derive(Debug, Clone, PartialEq)]
pub enum Rendered {
    /// Crisp, re-typeset from recovered markup: a black-on-transparent raster + em metrics.
    Typeset(Raster),
    /// The publisher's own picture bytes plus its text-relative size hint (decoded, cropped,
    /// sized, and recoloured by the sizing/delivery stages).
    Picture { png: Vec<u8>, size: PictureSize },
    /// The Unicode approximation — the floor the ladder can never fall past.
    Text(String),
}

/// A typeset raster and the metrics needed to place it. Metrics are in **em units**
/// (resolution-independent): `height` = baseline→top (ascent), `depth` = baseline→bottom
/// (descent); total ink height = `height + depth`; `width` = advance width.
#[derive(Debug, Clone, PartialEq)]
pub struct Raster {
    /// Black-on-transparent PNG (recoloured to the theme downstream).
    pub png: Vec<u8>,
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

/// Render `item` down the ladder at `em_px` pixels per em. `resolve_picture` turns the
/// picture's `src` into bytes (resolved against the book's resources); return `None` if it
/// can't, and the ladder falls through to the text. Never returns "nothing".
pub fn render(
    item: &MathItem,
    em_px: u32,
    resolve_picture: impl Fn(&str) -> Option<Vec<u8>>,
) -> Rendered {
    render_wrapped(item, em_px, None, resolve_picture)
}

/// Like [`render`], but a **display** equation whose natural width exceeds `max_em` (the text
/// column width, in em) is broken into stacked lines at its top-level relations/operators —
/// each continuation line indented to the relation column. `None` never breaks (the caller's
/// layout scales a wide raster instead). Inline math and the picture/text rungs are unaffected.
pub fn render_wrapped(
    item: &MathItem,
    em_px: u32,
    max_em: Option<f32>,
    resolve_picture: impl Fn(&str) -> Option<Vec<u8>>,
) -> Rendered {
    // A) Typeset — the crisp path.
    if let Some(src) = &item.typeset
        && let Some(raster) = render_typeset(src, item.display, em_px, max_em)
    {
        return Rendered::Typeset(raster);
    }
    // B) Picture — the publisher's own visual, kept as the fallback. Prefer bytes already
    // resolved onto the ref (the loader fills them for typeset-able equations); otherwise
    // resolve by src.
    if let Some(pic) = &item.picture {
        let png = if pic.data.is_empty() {
            resolve_picture(&pic.src).unwrap_or_default()
        } else {
            pic.data.clone()
        };
        if !png.is_empty() {
            return Rendered::Picture {
                png,
                size: pic.size,
            };
        }
    }
    // C) Text — the Unicode floor, always available.
    Rendered::Text(item.text.clone())
}

/// Rasterise a typeset source, or `None` on any failure (unmapped markup, a layout panic,
/// an empty raster) so the caller descends the ladder. Panic-guarded: the engine is young,
/// so a pathological equation degrades to the picture/text rather than taking the app down.
fn render_typeset(
    src: &MarkupSource,
    display: bool,
    em_px: u32,
    max_em: Option<f32>,
) -> Option<Raster> {
    let em_px = em_px.clamp(8, 400);
    let nodes = to_nodes(src)?;
    std::panic::catch_unwind(move || match (display, max_em) {
        (true, Some(m)) if m > 0.0 => build_wrapped(&nodes, em_px, m),
        _ => build(&nodes, display, em_px),
    })
    .ok()
    .flatten()
}

/// Display options — the layout regime a broken line is measured and rendered in.
fn display_opts() -> LayoutOptions {
    LayoutOptions::default()
        .with_style(MathStyle::Display)
        .with_color(Color::BLACK)
}

/// The advance width (in em) of a node slice, measured via layout — no PNG render.
fn measure(nodes: &[ParseNode], opts: &LayoutOptions) -> f32 {
    to_display_list(&layout(nodes, opts)).width as f32
}

/// The break-able top-level list: `to_nodes` wraps a multi-token equation in one `OrdGroup`,
/// so unwrap it to reach the relations/operators we break at.
fn flat_body(nodes: &[ParseNode]) -> Vec<ParseNode> {
    match nodes {
        [ParseNode::OrdGroup { body, .. }] => body.clone(),
        _ => nodes.to_vec(),
    }
}

/// A break class for a top-level node: `Some(0)` = relation (=, ≤, →), the preferred break;
/// `Some(1)` = binary operator (+, −), the secondary break; `None` = not a break point.
fn break_rank(n: &ParseNode) -> Option<u8> {
    match n {
        ParseNode::Atom {
            family: AtomFamily::Rel,
            ..
        } => Some(0),
        ParseNode::Atom {
            family: AtomFamily::Bin,
            ..
        } => Some(1),
        _ => None,
    }
}

/// One broken line: the nodes on it and its left indent (em).
struct Line {
    nodes: Vec<ParseNode>,
    indent: f32,
}

/// Build a display equation, breaking it into indented stacked lines when it exceeds `max_em`.
/// Falls back to the single (unbroken) raster when it fits or can't be broken — the caller's
/// layout then scales that raster to the column, so nothing ever overflows.
fn build_wrapped(nodes: &[ParseNode], em_px: u32, max_em: f32) -> Option<Raster> {
    let whole = build(nodes, true, em_px)?;
    if whole.width <= max_em {
        return Some(whole);
    }
    let opts = display_opts();
    let flat = flat_body(nodes);
    let lines = break_into_lines(&flat, &opts, max_em);
    if lines.len() < 2 {
        return Some(whole); // no usable break point → one line (layout scales it)
    }
    composite_lines(&lines, em_px).or(Some(whole))
}

/// Greedily fill lines: break only before a top-level relation/operator, taking the farthest
/// break within the width budget (relation column indent applied to continuation lines).
fn break_into_lines(flat: &[ParseNode], opts: &LayoutOptions, max_em: f32) -> Vec<Line> {
    // Stops we may end a line at: before each relation/operator, plus the very end.
    let mut stops: Vec<usize> = (1..flat.len())
        .filter(|&i| break_rank(&flat[i]).is_some())
        .collect();
    if stops.is_empty() {
        return vec![Line {
            nodes: flat.to_vec(),
            indent: 0.0,
        }];
    }
    stops.push(flat.len());

    // Continuation lines indent to the relation column — the width of the head before the
    // first relation (capped at half the column so a long head can't swallow it).
    let indent = flat
        .iter()
        .position(|n| matches!(break_rank(n), Some(0)))
        .map(|i| measure(&flat[..i], opts))
        .unwrap_or(0.0)
        .clamp(0.0, max_em * 0.5);

    let mut lines = Vec::new();
    let mut start = 0usize;
    while start < flat.len() {
        let budget = if lines.is_empty() {
            max_em
        } else {
            max_em - indent
        };
        // The farthest stop whose line width fits the budget; widths grow left-to-right, so
        // the first overflow ends the search. If even the first stop overflows, take it anyway
        // (an unbreakable over-long run — the layout scales that one line down).
        let mut end = None;
        for &s in stops.iter().filter(|&&s| s > start) {
            if measure(&flat[start..s], opts) <= budget {
                end = Some(s);
            } else {
                if end.is_none() {
                    end = Some(s);
                }
                break;
            }
        }
        let end = end.unwrap_or(flat.len());
        lines.push(Line {
            nodes: flat[start..end].to_vec(),
            indent: if lines.is_empty() { 0.0 } else { indent },
        });
        start = end;
    }
    lines
}

/// Render each line and stack them into one tall raster, each pasted at its left indent.
fn composite_lines(lines: &[Line], em_px: u32) -> Option<Raster> {
    use image::RgbaImage;
    let em = em_px as f32;
    let gap_px = (0.3 * em).round().max(1.0) as u32;

    let mut imgs: Vec<(u32, RgbaImage)> = Vec::with_capacity(lines.len());
    for ln in lines {
        let r = build(&ln.nodes, true, em_px)?;
        let img = image::load_from_memory(&r.png).ok()?.to_rgba8();
        let indent_px = (ln.indent * em).round().max(0.0) as u32;
        imgs.push((indent_px, img));
    }
    let max_w = imgs
        .iter()
        .map(|(x, img)| x + img.width())
        .max()
        .unwrap_or(1)
        .max(1);
    let total_h = imgs.iter().map(|(_, img)| img.height()).sum::<u32>()
        + gap_px * (imgs.len().saturating_sub(1)) as u32;
    let mut canvas = RgbaImage::new(max_w, total_h.max(1));
    let mut y = 0i64;
    for (indent_px, img) in &imgs {
        image::imageops::overlay(&mut canvas, img, *indent_px as i64, y);
        y += img.height() as i64 + gap_px as i64;
    }
    let mut png = Vec::new();
    canvas
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(Raster {
        png,
        width: max_w as f32 / em,
        height: total_h as f32 / em, // a stacked block: whole height above the baseline
        depth: 0.0,
    })
}

/// Lay out a math tree and rasterise it to a black-on-transparent PNG plus its em metrics.
fn build(
    nodes: &[ratex_parser::parse_node::ParseNode],
    display: bool,
    em_px: u32,
) -> Option<Raster> {
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };
    let opts = LayoutOptions::default()
        .with_style(style)
        .with_color(Color::BLACK);
    let dl = to_display_list(&layout(nodes, &opts));
    // The baseline split, carried out with the PNG (in em units, resolution-independent).
    let (width, height, depth) = (dl.width as f32, dl.height as f32, dl.depth as f32);
    let render_opts = RenderOptions {
        font_size: em_px as f32,
        padding: 2.0,
        // Transparent — recoloured to the theme at display time.
        background_color: Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        },
        device_pixel_ratio: 1.0,
        ..Default::default()
    };
    let png = render_to_png(&dl, &render_opts)
        .ok()
        .filter(|p| !p.is_empty())?;
    Some(Raster {
        png,
        width,
        height,
        depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use delryn_model::PictureRef;

    fn item(typeset: Option<MarkupSource>, picture: Option<PictureRef>, text: &str) -> MathItem {
        MathItem {
            display: false,
            typeset,
            picture,
            text: text.to_string(),
        }
    }

    #[test]
    fn typeset_rung_rasterises_with_metrics() {
        let it = item(
            Some(MarkupSource::PresentationMathml(
                "<math><mfrac><mn>1</mn><mn>2</mn></mfrac></math>".into(),
            )),
            None,
            "1/2",
        );
        match render(&it, 40, |_| None) {
            Rendered::Typeset(r) => {
                assert_eq!(&r.png[..4], &[0x89, b'P', b'N', b'G'], "PNG magic");
                assert!(
                    r.height > 0.0 && r.depth > 0.0,
                    "a fraction has ascent + descent"
                );
            }
            other => panic!("expected a typeset raster, got {other:?}"),
        }
    }

    #[test]
    fn unmapped_markup_falls_to_the_picture() {
        // Content MathML isn't mapped yet → typeset fails → the ladder shows the publisher picture.
        let it = item(
            Some(MarkupSource::ContentMathml(
                "<math><apply><ci>x</ci></apply></math>".into(),
            )),
            Some(PictureRef {
                src: "eq.png".into(),
                size: PictureSize::Em(3.0),
                data: Vec::new(),
            }),
            "x",
        );
        match render(&it, 40, |src| (src == "eq.png").then(|| vec![1, 2, 3, 4])) {
            Rendered::Picture { png, size } => {
                assert_eq!(png, vec![1, 2, 3, 4]);
                assert_eq!(size, PictureSize::Em(3.0));
            }
            other => panic!("expected the picture fallback, got {other:?}"),
        }
    }

    #[test]
    fn no_graphics_falls_to_text_never_blank() {
        // No typeset, no resolvable picture → the Unicode floor. Never nothing.
        let it = item(None, None, "x + 1");
        assert_eq!(render(&it, 40, |_| None), Rendered::Text("x + 1".into()));

        // Even with a picture ref, if the bytes can't be resolved it still falls to text.
        let it2 = item(
            None,
            Some(PictureRef {
                src: "missing.png".into(),
                size: PictureSize::MeasureInk,
                data: Vec::new(),
            }),
            "x + 1",
        );
        assert_eq!(render(&it2, 40, |_| None), Rendered::Text("x + 1".into()));
    }

    #[test]
    fn picture_bytes_on_the_ref_are_used_without_the_resolver() {
        // The loader resolves a typeset-able equation's picture onto the ref; an unmapped
        // markup then falls back to those bytes even with a no-op resolver.
        let it = item(
            Some(MarkupSource::ContentMathml(
                "<math><apply><ci>x</ci></apply></math>".into(),
            )),
            Some(PictureRef {
                src: "eq.png".into(),
                size: PictureSize::Em(2.0),
                data: vec![9, 8, 7],
            }),
            "x",
        );
        match render(&it, 40, |_| None) {
            Rendered::Picture { png, size } => {
                assert_eq!(png, vec![9, 8, 7]);
                assert_eq!(size, PictureSize::Em(2.0));
            }
            other => panic!("expected the ref's picture bytes, got {other:?}"),
        }
    }

    fn typeset(it: &MathItem, max_em: Option<f32>) -> Raster {
        match render_wrapped(it, 40, max_em, |_| None) {
            Rendered::Typeset(r) => r,
            other => panic!("expected a typeset raster, got {other:?}"),
        }
    }

    fn disp(latex: &str) -> MathItem {
        MathItem {
            display: true,
            typeset: Some(MarkupSource::Latex(latex.to_string())),
            picture: None,
            text: String::new(),
        }
    }

    #[test]
    fn wide_display_equation_breaks_into_stacked_lines() {
        let it = disp("a = b + c + d + e + f + g + h + i + j");
        let whole = typeset(&it, None);
        // Break to ~40% of the natural width → several stacked lines: narrower and taller.
        let broken = typeset(&it, Some(whole.width * 0.4));
        assert!(
            broken.width < whole.width,
            "broken is narrower: {} vs {}",
            broken.width,
            whole.width
        );
        assert!(
            broken.height > whole.height,
            "broken is taller (stacked): {} vs {}",
            broken.height,
            whole.height
        );
        assert_eq!(
            broken.depth, 0.0,
            "a composited block carries no baseline depth"
        );
    }

    #[test]
    fn display_equation_that_fits_is_not_broken() {
        // Plenty of width → the single build, which keeps a real baseline depth (a fraction
        // descends below the baseline); a break would have zeroed it.
        let r = typeset(&disp("\\frac{a}{b} = c"), Some(1000.0));
        assert!(
            r.depth > 0.0,
            "single build keeps baseline depth: {}",
            r.depth
        );
    }

    #[test]
    fn inline_math_is_never_broken() {
        // max_em only affects display math; an inline item ignores it.
        let it = item(Some(MarkupSource::Latex("a+b+c+d+e+f".into())), None, "");
        // display=false via the item helper default — assert it still renders as one raster.
        assert!(matches!(
            render_wrapped(&it, 40, Some(0.5), |_| None),
            Rendered::Typeset(_)
        ));
    }
}
