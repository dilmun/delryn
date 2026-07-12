//! Theme-aware colour engine: recolour, flatten, and invert graphics to a theme.
//
// Many EPUBs ship math equations (and other line drawings) as PNGs of black ink
// on a transparent or white background. Painted straight onto a dark reader they
// render black-on-black — invisible. A good reader instead recolours such
// graphics to the page's text colour, the way macOS Books does. This module does
// that systematically: classify a graphic as *line-art* vs *photograph* by its
// colourfulness + sparsity (no per-publisher rules), then repaint line-art as an
// ink-coverage matte in the theme's colours. Photographs/colour charts are left
// untouched (but flattened onto the page colour so transparency never hides
// them). See `DESIGN.md` §0.

use delryn_infra::config::ImageMode;
use image::{DynamicImage, Rgba, RgbaImage};

use crate::decode::{decode, encode_png};

/// Foreground/background colours (sRGB 0–255) for recolouring an ink graphic:
/// `ink` = the reader's text colour, `paper` = its page/background colour. Part
/// of [`crate::ImgKey`] so a built image is re-tinted when the theme changes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ink {
    pub ink: [u8; 3],
    pub paper: [u8; 3],
}

/// How to render a graphic for the current frame: the theme `tint` plus the
/// adaptation `mode`. Part of [`crate::ImgKey`], so changing either re-renders.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RenderPolicy {
    pub tint: Ink,
    pub mode: ImageMode,
}

/// The kind of background a graphic sits on, decided once per image.
pub(crate) enum Background {
    /// Has meaningful transparency — its alpha channel *is* the ink matte.
    Alpha,
    /// Opaque, on a near-uniform background of this (normalised) colour.
    Solid([f32; 3]),
}

/// Fraction of transparent pixels above which we treat alpha as the ink matte.
const TRANSPARENT_FRAC: f32 = 0.02;
/// Per-pixel alpha below this (0–255) counts as "transparent" for the scan.
const ALPHA_CUTOFF: u8 = 250;

/// Inspect a graphic's border + alpha to decide its background.
pub(crate) fn analyze_background(img: &RgbaImage) -> Background {
    let (w, h) = img.dimensions();
    let total = (w * h).max(1) as f32;
    let transparent = img.pixels().filter(|p| p[3] < ALPHA_CUTOFF).count() as f32;
    if transparent / total > TRANSPARENT_FRAC {
        return Background::Alpha;
    }
    // Opaque: the background is whatever dominates the border (margins).
    let mut sum = [0f32; 3];
    let mut n = 0f32;
    let mut accumulate = |p: &Rgba<u8>| {
        for i in 0..3 {
            sum[i] += p[i] as f32;
        }
        n += 1.0;
    };
    for x in 0..w {
        accumulate(img.get_pixel(x, 0));
        accumulate(img.get_pixel(x, h - 1));
    }
    for y in 0..h {
        accumulate(img.get_pixel(0, y));
        accumulate(img.get_pixel(w - 1, y));
    }
    let n = n.max(1.0);
    Background::Solid([sum[0] / n / 255.0, sum[1] / n / 255.0, sum[2] / n / 255.0])
}

/// How much a pixel is "ink" (1.0) vs "background" (0.0): alpha for transparent
/// graphics, else its colour distance from the detected background.
pub(crate) fn ink_coverage(p: &Rgba<u8>, bg: &Background) -> f32 {
    match bg {
        Background::Alpha => p[3] as f32 / 255.0,
        Background::Solid(b) => {
            let d = (0..3)
                .map(|i| (p[i] as f32 / 255.0 - b[i]).abs())
                .fold(0f32, f32::max);
            d.clamp(0.0, 1.0)
        }
    }
}

/// A pixel's chroma (max−min channel, normalised) — 0 for greys, high for
/// saturated colours. Photographs and colour charts have high mean chroma.
fn chroma(p: &Rgba<u8>) -> f32 {
    let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
    (r.max(g).max(b) - r.min(g).min(b)) / 255.0
}

/// Mean chroma below which ink counts as greyscale (0–1).
pub(crate) const INK_CHROMA_MAX: f32 = 0.12;

/// Whether `img` is a transparent monochrome ink graphic — a math equation or
/// line drawing whose *transparent* background exposes the dark page, so its
/// black ink renders invisible. These are recoloured to the theme.
///
/// The decisive signal is transparency, not sparsity: equations are shipped on a
/// transparent background (confirmed across publishers), while figures, photos,
/// and white-background diagrams are *opaque* — they carry their own background
/// and stay legible on any theme, so they're left untouched (we never risk
/// monochroming a real figure). Transparent *colour* graphics aren't line-art
/// either — we keep their colours.
pub fn is_line_art(img: &DynamicImage) -> bool {
    let rgba = img.to_rgba8();
    transparent_frac(&rgba) > TRANSPARENT_FRAC && opaque_chroma(&rgba) < INK_CHROMA_MAX
}

/// Fraction of pixels (0–1) that are at least partly transparent.
fn transparent_frac(img: &RgbaImage) -> f32 {
    let total = (img.width() * img.height()).max(1) as f32;
    img.pixels().filter(|p| p[3] < ALPHA_CUTOFF).count() as f32 / total
}

/// Mean chroma over the opaque (ink) pixels — i.e. what colour the strokes are.
pub(crate) fn opaque_chroma(img: &RgbaImage) -> f32 {
    let mut sum = 0f32;
    let mut n = 0f32;
    for p in img.pixels() {
        if p[3] > 128 {
            sum += chroma(p);
            n += 1.0;
        }
    }
    if n == 0.0 { 0.0 } else { sum / n }
}

fn lerp3(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

/// Repaint a line-art graphic in theme colours: each pixel's ink coverage blends
/// from `paper` (background) to `ink` (stroke), yielding an opaque image that
/// sits seamlessly on the reader's page — the equation tracks the theme instead
/// of rendering black-on-black.
pub fn recolor_ink(img: &DynamicImage, colors: Ink) -> DynamicImage {
    let src = img.to_rgba8();
    let bg = analyze_background(&src);
    let (w, h) = src.dimensions();
    let mut out = RgbaImage::new(w, h);
    for (x, y, p) in src.enumerate_pixels() {
        let c = ink_coverage(p, &bg);
        let px = lerp3(colors.paper, colors.ink, c);
        out.put_pixel(x, y, Rgba([px[0], px[1], px[2], 255]));
    }
    DynamicImage::ImageRgba8(out)
}

/// Composite a (possibly transparent) graphic onto an opaque `paper` background,
/// so transparent photos/figures are never invisible on a dark page. A no-op in
/// effect for fully opaque images (their pixels pass straight through).
pub fn flatten_onto(img: &DynamicImage, paper: [u8; 3]) -> DynamicImage {
    let src = img.to_rgba8();
    let (w, h) = src.dimensions();
    let mut out = RgbaImage::new(w, h);
    for (x, y, p) in src.enumerate_pixels() {
        let a = p[3] as f32 / 255.0;
        let over = |bg: u8, fg: u8| (bg as f32 * (1.0 - a) + fg as f32 * a).round() as u8;
        out.put_pixel(
            x,
            y,
            Rgba([
                over(paper[0], p[0]),
                over(paper[1], p[1]),
                over(paper[2], p[2]),
                255,
            ]),
        );
    }
    DynamicImage::ImageRgba8(out)
}

/// Whether an opaque graphic is predominantly light (a white-page chart, a light
/// dialog or screenshot) — the kind `InvertBackgrounds` flips to dark. Uses the
/// *whole-image* mean luminance, not just the border, so screenshots with a dark
/// window frame or title bar at the edges still count as light.
fn is_predominantly_light(img: &RgbaImage) -> bool {
    let mut sum = 0f32;
    let mut n = 0f32;
    for p in img.pixels() {
        let a = p[3] as f32 / 255.0;
        sum += a * delryn_infra::color::luma([p[0], p[1], p[2]]);
        n += a.max(0.001);
    }
    n > 0.0 && sum / n > 128.0
}

/// Theme-aware lightness inversion for a light-background figure: white↔black is
/// swapped, but mapped *into the theme*, so the result sits on the reader's page
/// instead of a hardcoded black box.
///
/// - **Near-white** pixels (the background and its JPEG ringing/halo) → snapped to
///   the exact page colour, so lossy-compression noise around edges collapses into
///   the background instead of becoming coloured specks on the dark page.
/// - **Neutral** pixels (black axes/text) → the exact theme `paper`↔`ink` by
///   inverted lightness, so they match the page/text colour on any theme.
/// - **Coloured** pixels (a red curve, blue lines) → keep their hue + saturation,
///   with lightness inverted and remapped into the theme's `paper…ink` band so
///   they stay vivid and visible on the dark page.
///
/// Unlike a naive `255−RGB` negate, this never turns a photo into a colour
/// negative and tracks the active theme. On a pure black/white theme the band is
/// the full `[0,1]`, so it reduces to a plain lightness invert.
pub fn theme_invert(img: &DynamicImage, colors: Ink) -> DynamicImage {
    let (_, _, paper_l) = rgb_to_hsl(colors.paper[0], colors.paper[1], colors.paper[2]);
    let (_, _, ink_l) = rgb_to_hsl(colors.ink[0], colors.ink[1], colors.ink[2]);
    let src = img.to_rgba8();
    let (w, h) = src.dimensions();
    let mut out = RgbaImage::new(w, h);
    for (x, y, p) in src.enumerate_pixels() {
        let (hue, sat, lit) = rgb_to_hsl(p[0], p[1], p[2]);
        let inv = 1.0 - lit;
        let px = if lit >= NEAR_WHITE_L {
            // Background + compression halo → clean page colour (kills the speckle).
            colors.paper
        } else if chroma(p) < INVERT_CHROMA_MIN {
            // Neutral → exact theme colours by inverted lightness. Tested by
            // *chroma*, not HSL saturation: saturation is `d/(1-|2l-1|)`, which
            // explodes near white/black, so a faint-grey anti-aliased pixel would
            // otherwise read as fully "saturated" and get painted a vivid hue
            // (the rainbow on a black-and-white chart). Chroma stays honest.
            lerp3(colors.paper, colors.ink, inv)
        } else {
            // Colour → keep hue/sat, map inverted lightness into the theme band.
            hsl_to_rgb(hue, sat, paper_l + (ink_l - paper_l) * inv)
        };
        out.put_pixel(x, y, Rgba([px[0], px[1], px[2], p[3]]));
    }
    DynamicImage::ImageRgba8(out)
}

/// Lightness at/above which a pixel counts as the (light) figure background — incl.
/// the faint JPEG ringing around edges that would otherwise invert into specks.
const NEAR_WHITE_L: f32 = 0.86;
/// Chroma at/above which an inverted pixel keeps its hue (a real coloured line);
/// below it the pixel is treated as neutral ink. Chroma, not HSL saturation,
/// because saturation is unstable near white/black (see `theme_invert`).
const INVERT_CHROMA_MIN: f32 = 0.18;

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l); // greyscale: hue/sat undefined
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = 60.0
        * if (max - r).abs() < 1e-6 {
            ((g - b) / d).rem_euclid(6.0)
        } else if (max - g).abs() < 1e-6 {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    if s < 1e-6 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    [to(r1), to(g1), to(b1)]
}

/// Apply theme-aware rendering to one decoded graphic — the single policy shared
/// by inline reader images and the full-screen viewer.
///
/// Transparent monochrome line-art (equations, line drawings) is *text rendered
/// as an image*: it has no background of its own, so on a dark page its black ink
/// is invisible. It is therefore **always** recoloured to the theme ink, in every
/// mode — an invisible equation is never what the reader wants. The `mode` only
/// governs real *pictures* (figures, photos, screenshots that carry their own
/// background):
/// - **Auto** / **Faithful**: keep pictures as authored (just composite any
///   transparency onto the page so nothing is hidden).
/// - **InvertBackgrounds**: additionally lightness-invert opaque *light-background*
///   pictures so they're dark-friendly with detail intact.
pub fn render_for_theme(img: &DynamicImage, tint: Ink, mode: ImageMode) -> DynamicImage {
    let rgba = img.to_rgba8();
    if transparent_frac(&rgba) > TRANSPARENT_FRAC {
        // Transparent graphic. Monochrome ink is line-art/equations (text) →
        // recolour to the theme so it's legible on any background, in all modes.
        if opaque_chroma(&rgba) < INK_CHROMA_MAX {
            return recolor_ink(img, tint);
        }
        // Transparent *colour* graphic (a chart/diagram: coloured fills plus dark
        // neutral axes, ticks, and labels on a transparent background). Composite
        // onto the page so nothing is hidden. In Invert mode also lightness-invert
        // it, so its dark ink — invisible black-on-black on a dark page otherwise —
        // becomes legible while the coloured strokes keep their hue; Auto/Faithful
        // keep the colours exactly as authored.
        return if mode == ImageMode::InvertBackgrounds {
            flatten_onto(&theme_invert(img, tint), tint.paper)
        } else {
            flatten_onto(img, tint.paper)
        };
    }
    // Opaque picture: carries its own background. Invert mode flips light ones to
    // match a dark page; Auto/Faithful leave them as authored.
    if mode == ImageMode::InvertBackgrounds && is_predominantly_light(&rgba) {
        theme_invert(img, tint)
    } else {
        img.clone()
    }
}

/// Theme a full PDF *page* raster to the active reader theme — the whole-page
/// counterpart to [`render_for_theme`], for the direct-Kitty page path.
///
/// A PDF page is an opaque scan/render carrying its own (usually white)
/// background, so left alone it ignores the reader theme: a glaring white sheet
/// on a dark page. This adapts it the way the inline pipeline adapts figures, and
/// returns the re-encoded PNG — or `None` when the page should be shown exactly as
/// rendered, so the caller transmits the original bytes with no needless re-encode:
///
/// - **Faithful**: never themed — the original page (the print look).
/// - **Auto** (default): map a predominantly-light *neutral* page (text or a line
///   diagram) into the theme — white→paper, ink→text colour — so the reading
///   surface tracks the theme (a dark theme yields a dark page). A *colourful*
///   light page (a photo or a magazine spread) is left as rendered so it isn't
///   lightness-inverted into a negative.
/// - **InvertBackgrounds**: as Auto but also themes colourful light pages — the
///   "force everything dark" choice, accepting the photo-inversion trade.
///
/// A predominantly-dark page (e.g. a slide deck on black) is already theme-
/// friendly, so it is always left as rendered.
pub fn theme_page_png(raw: &[u8], policy: RenderPolicy) -> Option<Vec<u8>> {
    if policy.mode == ImageMode::Faithful {
        return None; // original page bytes — no decode/re-encode
    }
    let img = decode(raw)?;
    let rgba = img.to_rgba8();
    if !is_predominantly_light(&rgba) {
        return None; // a dark page needs no theming
    }
    // Auto protects colourful (photo) pages from inversion; Invert forces them.
    if policy.mode == ImageMode::Auto && opaque_chroma(&rgba) >= INK_CHROMA_MAX {
        return None;
    }
    encode_png(&theme_invert(&img, policy.tint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    const DARK: Ink = Ink {
        ink: [220, 220, 220],
        paper: [10, 12, 16],
    };

    /// An opaque image: white background with a black ink region (a figure or a
    /// white-page diagram — carries its own background).
    fn opaque_ink(w: u32, h: u32, ink_rect: (u32, u32, u32, u32)) -> DynamicImage {
        fill(
            w,
            h,
            Rgba([255, 255, 255, 255]),
            ink_rect,
            Rgba([0, 0, 0, 255]),
        )
    }

    /// A transparent image with an opaque black ink region (a math equation).
    fn transparent_ink(w: u32, h: u32, ink_rect: (u32, u32, u32, u32)) -> DynamicImage {
        fill(w, h, Rgba([0, 0, 0, 0]), ink_rect, Rgba([0, 0, 0, 255]))
    }

    fn fill(
        w: u32,
        h: u32,
        bg: Rgba<u8>,
        rect: (u32, u32, u32, u32),
        ink: Rgba<u8>,
    ) -> DynamicImage {
        let mut img = RgbaImage::from_pixel(w, h, bg);
        let (x0, y0, x1, y1) = rect;
        for y in y0..y1 {
            for x in x0..x1 {
                img.put_pixel(x, y, ink);
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn invert_does_not_rainbow_near_neutral_pixels() {
        let mut img = RgbaImage::from_pixel(4, 4, Rgba([255, 255, 255, 255]));
        // A light, faintly-tilted grey — HSL saturation read this as "fully
        // coloured" at high lightness and painted it a vivid hue (the rainbow).
        img.put_pixel(1, 1, Rgba([226, 217, 208, 255]));
        // A genuinely saturated pixel must keep its colour.
        img.put_pixel(2, 2, Rgba([200, 30, 30, 255]));
        let out = theme_invert(&DynamicImage::ImageRgba8(img), DARK).to_rgba8();
        assert!(
            chroma(out.get_pixel(1, 1)) < 0.12,
            "faint grey stays neutral: {:?}",
            out.get_pixel(1, 1).0
        );
        assert!(
            chroma(out.get_pixel(2, 2)) > 0.3,
            "a real colour is kept: {:?}",
            out.get_pixel(2, 2).0
        );
    }

    #[test]
    fn recolor_maps_ink_to_text_and_background_to_paper() {
        // White page, small black stroke in the centre.
        let img = opaque_ink(20, 20, (9, 9, 11, 11));
        let out = recolor_ink(&img, DARK).to_rgba8();
        // A corner (background) becomes the paper colour.
        assert_eq!(out.get_pixel(0, 0).0, [10, 12, 16, 255]);
        // The black stroke becomes the ink (text) colour.
        assert_eq!(out.get_pixel(10, 10).0, [220, 220, 220, 255]);
    }

    #[test]
    fn recolor_handles_transparent_ink_via_alpha() {
        // Fully transparent except one opaque black pixel (an alpha matte).
        let mut img = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0]));
        img.put_pixel(4, 4, Rgba([0, 0, 0, 255]));
        let out = recolor_ink(&DynamicImage::ImageRgba8(img), DARK).to_rgba8();
        assert_eq!(
            out.get_pixel(0, 0).0,
            [10, 12, 16, 255],
            "transparent → paper"
        );
        assert_eq!(out.get_pixel(4, 4).0, [220, 220, 220, 255], "opaque → ink");
    }

    #[test]
    fn transparent_equation_is_line_art() {
        // Transparent background + black ink (a math equation) → line-art.
        assert!(is_line_art(&transparent_ink(40, 40, (15, 15, 25, 25))));
    }

    #[test]
    fn opaque_figure_is_not_line_art() {
        // An opaque white-background diagram carries its own background → NOT
        // line-art, so we never monochrome a figure (the real-file false
        // positive that motivated keying on transparency).
        assert!(!is_line_art(&opaque_ink(40, 40, (10, 10, 30, 30))));

        // A saturated colour gradient (stand-in for a photo) → not line-art.
        let mut photo = RgbaImage::new(40, 40);
        for (x, y, p) in photo.enumerate_pixels_mut() {
            *p = Rgba([(x * 6) as u8, (y * 6) as u8, 200, 255]);
        }
        assert!(!is_line_art(&DynamicImage::ImageRgba8(photo)));
    }

    #[test]
    fn transparent_colour_graphic_keeps_its_colours() {
        // Transparent but colourful → not line-art → flattened, colours kept.
        let img = fill(
            20,
            20,
            Rgba([0, 0, 0, 0]),
            (5, 5, 15, 15),
            Rgba([220, 30, 30, 255]),
        );
        assert!(!is_line_art(&img));
        let out = render_for_theme(&img, DARK, ImageMode::Auto).to_rgba8();
        assert_eq!(
            out.get_pixel(10, 10).0,
            [220, 30, 30, 255],
            "red stroke kept"
        );
    }

    #[test]
    fn invert_transparent_colour_chart_lightens_dark_ink() {
        // A chart shipped on a *transparent* background: a saturated blue fill (a
        // data region) plus a black axis line. On a dark page the black axis is
        // invisible, so Invert must lightness-invert the graphic — the dark ink
        // becomes light — while keeping the colour and mapping the transparent
        // background to the page. Auto keeps it faithful (dark ink stays dark).
        let mut img = RgbaImage::from_pixel(20, 20, Rgba([0, 0, 0, 0]));
        for y in 4..16 {
            for x in 4..16 {
                img.put_pixel(x, y, Rgba([30, 90, 220, 255]));
            }
        }
        for x in 0..20 {
            img.put_pixel(x, 18, Rgba([0, 0, 0, 255])); // black axis
        }
        let img = DynamicImage::ImageRgba8(img);

        // Auto: faithful — the black axis stays black, transparency onto paper.
        let auto = render_for_theme(&img, DARK, ImageMode::Auto).to_rgba8();
        assert_eq!(
            auto.get_pixel(0, 18).0,
            [0, 0, 0, 255],
            "auto keeps black axis"
        );
        assert_eq!(
            auto.get_pixel(0, 0).0,
            [10, 12, 16, 255],
            "transparent → paper"
        );

        // Invert: the black axis is lightened (now visible on the dark page)…
        let inv = render_for_theme(&img, DARK, ImageMode::InvertBackgrounds).to_rgba8();
        let axis = inv.get_pixel(0, 18).0;
        assert!(
            delryn_infra::color::luma([axis[0], axis[1], axis[2]]) > 128.0,
            "invert lightens the dark axis: {axis:?}"
        );
        // …the transparent background maps to the page colour…
        assert_eq!(
            inv.get_pixel(0, 0).0,
            [10, 12, 16, 255],
            "transparent → paper"
        );
        // …and the blue fill keeps its hue (still blue-dominant).
        let blue = inv.get_pixel(10, 10).0;
        assert!(
            blue[2] > blue[0] && blue[2] > blue[1],
            "blue kept: {blue:?}"
        );
    }

    #[test]
    fn theme_invert_maps_background_to_paper_keeps_hue() {
        let img = fill(
            4,
            4,
            Rgba([255, 255, 255, 255]),
            (0, 0, 1, 1),
            Rgba([255, 0, 0, 255]),
        );
        let out = theme_invert(&img, DARK).to_rgba8();
        // White background → the theme paper (not pure black), so it matches the
        // page on any theme.
        assert_eq!(
            out.get_pixel(3, 3).0,
            [10, 12, 16, 255],
            "white → theme paper"
        );
        // The red stays reddish (hue preserved).
        let red = out.get_pixel(0, 0).0;
        assert!(red[0] > red[1] && red[0] > red[2], "stays reddish: {red:?}");
    }

    #[test]
    fn invert_mode_maps_light_bg_to_paper_but_auto_keeps_it() {
        // Opaque white-bg figure with a black mark.
        let fig = opaque_ink(10, 10, (4, 4, 6, 6));
        // Auto leaves opaque figures untouched.
        let auto = render_for_theme(&fig, DARK, ImageMode::Auto).to_rgba8();
        assert_eq!(
            auto.get_pixel(0, 0).0,
            [255, 255, 255, 255],
            "auto keeps white bg"
        );
        // Invert maps the light background to the theme's paper colour.
        let inv = render_for_theme(&fig, DARK, ImageMode::InvertBackgrounds).to_rgba8();
        assert_eq!(
            inv.get_pixel(0, 0).0,
            [10, 12, 16, 255],
            "invert → theme paper"
        );
    }

    #[test]
    fn equations_stay_legible_in_every_mode() {
        // A transparent black equation is text-as-image: it must be recoloured to
        // the theme ink in ALL modes (an invisible equation is never wanted), so
        // Faithful — which preserves *pictures* — still makes equations legible.
        let eq = transparent_ink(10, 10, (4, 4, 6, 6));
        for mode in [
            ImageMode::Auto,
            ImageMode::InvertBackgrounds,
            ImageMode::Faithful,
        ] {
            let out = render_for_theme(&eq, DARK, mode).to_rgba8();
            assert_eq!(
                out.get_pixel(5, 5).0,
                [220, 220, 220, 255],
                "{mode:?}: ink → theme ink (legible)"
            );
            assert_eq!(
                out.get_pixel(0, 0).0,
                [10, 12, 16, 255],
                "{mode:?}: transparent → theme paper"
            );
        }
    }

    #[test]
    fn faithful_keeps_opaque_pictures_as_authored() {
        // An opaque light-bg figure: Faithful preserves it; only Invert flips it.
        let fig = opaque_ink(10, 10, (4, 4, 6, 6));
        let faithful = render_for_theme(&fig, DARK, ImageMode::Faithful).to_rgba8();
        assert_eq!(
            faithful.get_pixel(0, 0).0,
            [255, 255, 255, 255],
            "faithful keeps the white background"
        );
    }

    #[test]
    fn flatten_composites_transparency_onto_paper() {
        let mut img = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 0, 0]));
        img.put_pixel(0, 0, Rgba([255, 255, 255, 255])); // opaque white passes through
        let out = flatten_onto(&DynamicImage::ImageRgba8(img), [10, 12, 16]).to_rgba8();
        assert_eq!(
            out.get_pixel(1, 1).0,
            [10, 12, 16, 255],
            "transparent → paper"
        );
        assert_eq!(out.get_pixel(0, 0).0, [255, 255, 255, 255], "opaque kept");
    }

    #[test]
    fn full_color_image_is_left_alone_by_render_for_theme() {
        // render_for_theme on a photo flattens (opaque already) but keeps colours.
        let mut photo = RgbaImage::new(20, 20);
        for (x, y, p) in photo.enumerate_pixels_mut() {
            *p = Rgba([(x * 12) as u8, (y * 12) as u8, 180, 255]);
        }
        let out = render_for_theme(
            &DynamicImage::ImageRgba8(photo.clone()),
            DARK,
            ImageMode::Auto,
        )
        .to_rgba8();
        assert_eq!(out.get_pixel(10, 10).0, photo.get_pixel(10, 10).0);
    }

    // ── Full-page (PDF) theming ──────────────────────────────────────────────

    /// A page PNG with the given background and a centred ink/colour rect, for the
    /// page-theming tests.
    fn page_png(bg: Rgba<u8>, rect: (u32, u32, u32, u32), mark: Rgba<u8>) -> Vec<u8> {
        let img = fill(40, 40, bg, rect, mark);
        encode_png(&img).expect("encode test page")
    }

    fn dark_policy(mode: ImageMode) -> RenderPolicy {
        RenderPolicy { tint: DARK, mode }
    }

    #[test]
    fn faithful_page_is_never_themed() {
        // Faithful keeps the original page bytes — signalled by `None`.
        let png = page_png(
            Rgba([255, 255, 255, 255]),
            (10, 10, 30, 30),
            Rgba([0, 0, 0, 255]),
        );
        assert!(theme_page_png(&png, dark_policy(ImageMode::Faithful)).is_none());
    }

    #[test]
    fn auto_themes_a_light_neutral_page() {
        // A white text page → mapped into the theme: white→paper, ink→text colour.
        let png = page_png(
            Rgba([255, 255, 255, 255]),
            (16, 16, 24, 24),
            Rgba([0, 0, 0, 255]),
        );
        let themed = theme_page_png(&png, dark_policy(ImageMode::Auto)).expect("themed");
        let out = decode(&themed).unwrap().to_rgba8();
        assert_eq!(
            out.get_pixel(0, 0).0[..3],
            DARK.paper,
            "white page → theme paper"
        );
        assert_eq!(
            out.get_pixel(20, 20).0[..3],
            DARK.ink,
            "black ink → theme ink"
        );
    }

    #[test]
    fn auto_keeps_a_colourful_light_page_but_invert_themes_it() {
        // A predominantly-light but colourful page (a photo / magazine spread):
        // Auto leaves it as rendered (no negative); Invert forces it into the theme.
        let png = page_png(
            Rgba([255, 255, 255, 255]),
            (5, 5, 35, 35),
            Rgba([220, 30, 30, 255]),
        );
        assert!(
            theme_page_png(&png, dark_policy(ImageMode::Auto)).is_none(),
            "Auto protects a colourful page"
        );
        assert!(
            theme_page_png(&png, dark_policy(ImageMode::InvertBackgrounds)).is_some(),
            "Invert forces a colourful page"
        );
    }

    #[test]
    fn a_dark_page_is_left_as_rendered() {
        // A dark-background page is already theme-friendly → shown as rendered.
        let png = page_png(
            Rgba([20, 20, 20, 255]),
            (16, 16, 24, 24),
            Rgba([200, 200, 200, 255]),
        );
        assert!(theme_page_png(&png, dark_policy(ImageMode::Auto)).is_none());
        assert!(theme_page_png(&png, dark_policy(ImageMode::InvertBackgrounds)).is_none());
    }
}
