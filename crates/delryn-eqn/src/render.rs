//! The render ladder: turn a recovered [`MathItem`] into something drawable, trying the
//! sources in order — **typeset** (crisp, our engine) → **picture** (the publisher's own
//! image) → **text** (Unicode). A failure at any rung *descends* to the next, so an equation
//! can never render nothing (the never-blank guarantee). See `docs/MATH-RENDERING.md`.
//!
//! This stage produces engine output + a size hint; sizing-to-the-page and theming happen
//! downstream. The typeset raster is black-on-transparent (recoloured to the theme later)
//! and carries its em-relative baseline split so inline math can sit on the text baseline.

use ratex_layout::{LayoutOptions, layout, to_display_list};
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
    // A) Typeset — the crisp path.
    if let Some(src) = &item.typeset
        && let Some(raster) = render_typeset(src, item.display, em_px)
    {
        return Rendered::Typeset(raster);
    }
    // B) Picture — the publisher's own visual, kept as the fallback.
    if let Some(pic) = &item.picture
        && let Some(png) = resolve_picture(&pic.src)
        && !png.is_empty()
    {
        return Rendered::Picture {
            png,
            size: pic.size,
        };
    }
    // C) Text — the Unicode floor, always available.
    Rendered::Text(item.text.clone())
}

/// Rasterise a typeset source, or `None` on any failure (unmapped markup, a layout panic,
/// an empty raster) so the caller descends the ladder. Panic-guarded: the engine is young,
/// so a pathological equation degrades to the picture/text rather than taking the app down.
fn render_typeset(src: &MarkupSource, display: bool, em_px: u32) -> Option<Raster> {
    let em_px = em_px.clamp(8, 400);
    let nodes = to_nodes(src)?;
    std::panic::catch_unwind(move || build(&nodes, display, em_px))
        .ok()
        .flatten()
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
        // <mtable> isn't mapped → typeset fails → the ladder shows the publisher picture.
        let it = item(
            Some(MarkupSource::PresentationMathml(
                "<math><mtable><mtr><mtd><mn>1</mn></mtd></mtr></mtable></math>".into(),
            )),
            Some(PictureRef {
                src: "eq.png".into(),
                size: PictureSize::Em(3.0),
            }),
            "table",
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
            }),
            "x + 1",
        );
        assert_eq!(render(&it2, 40, |_| None), Rendered::Text("x + 1".into()));
    }
}
