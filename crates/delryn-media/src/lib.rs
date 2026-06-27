//! Terminal image support: protocol detection and decoding. Wraps
//! `ratatui-image` so the rest of the app doesn't depend on it directly.
//! See `DESIGN.md` §0 (graphics protocols).

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

pub use delryn_infra::config::ImageMode;
use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::sliced::SlicedProtocol;

/// Detect the terminal's image protocol + cell size by querying stdio. Returns
/// `None` if there's no tty or detection fails (then images are unavailable).
/// Call before entering the alternate screen / raw mode.
pub fn detect_picker() -> Option<Picker> {
    // Enable the OSC 11 background-colour query so the `terminal` theme can match
    // its real backdrop (read back via [`terminal_background`]). The query ends in
    // a Device Status Report every terminal answers, so it never hangs.
    let opts = ratatui_image::picker::cap_parser::QueryStdioOptions {
        terminal_background_color_osc: true,
        ..Default::default()
    };
    Picker::from_query_stdio_with_options(opts).ok()
}

/// The terminal's background colour, if it answered the OSC 11 query during
/// [`detect_picker`]. Lets the `terminal` theme — which has no colours of its own
/// — recolour/invert images against the real backdrop instead of white paper.
pub fn terminal_background(picker: &Picker) -> Option<[u8; 3]> {
    picker.capabilities().iter().find_map(|c| match c {
        ratatui_image::picker::Capability::Background(r, g, b) => Some([*r, *g, *b]),
        _ => None,
    })
}

pub fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

// ── Theme-aware ink rendering ────────────────────────────────────────────────
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

/// Foreground/background colours (sRGB 0–255) for recolouring an ink graphic:
/// `ink` = the reader's text colour, `paper` = its page/background colour. Part
/// of [`ImgKey`] so a built image is re-tinted when the theme changes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ink {
    pub ink: [u8; 3],
    pub paper: [u8; 3],
}

/// How to render a graphic for the current frame: the theme `tint` plus the
/// adaptation `mode`. Part of [`ImgKey`], so changing either re-renders.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RenderPolicy {
    pub tint: Ink,
    pub mode: ImageMode,
}

/// The kind of background a graphic sits on, decided once per image.
enum Background {
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
fn analyze_background(img: &RgbaImage) -> Background {
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
fn ink_coverage(p: &Rgba<u8>, bg: &Background) -> f32 {
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
const INK_CHROMA_MAX: f32 = 0.12;

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
fn opaque_chroma(img: &RgbaImage) -> f32 {
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
        sum += a * (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32);
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
        // recolour to the theme so it's legible on any background, in all modes;
        // a transparent *colour* graphic keeps its colours, composited onto the page.
        return if opaque_chroma(&rgba) < INK_CHROMA_MAX {
            recolor_ink(img, tint)
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

/// A decoded cover plus its source pixel dimensions, so the renderer can size a
/// render rect to the cover's aspect ratio (filling it with no letterbox).
pub struct CoverImage {
    pub proto: StatefulProtocol,
    /// Source pixel dimensions (w, h).
    pub dims: (u32, u32),
}

/// Decode `bytes` and build a resize protocol for `picker`, capturing the source
/// dimensions. `None` if the bytes aren't a decodable image.
pub fn build_cover(picker: &Picker, bytes: &[u8]) -> Option<CoverImage> {
    decode(bytes).map(|img| {
        let dims = (img.width(), img.height());
        CoverImage {
            proto: picker.new_resize_protocol(img),
            dims,
        }
    })
}

/// Read just an image's pixel dimensions (header only — cheap).
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// How an image's display size was authored (mirrors `delryn_model::ImageWidth`,
/// kept here so this crate stays independent of the content model).
#[derive(Clone, Copy, PartialEq)]
pub enum SizeHint {
    /// No authored size — normalize to the target fraction of the column.
    Auto,
    /// A fraction of the column width (CSS %), 0.0–1.0.
    Pct(f32),
    /// An absolute CSS-pixel width.
    Px(u32),
    /// Fill the pane (preserving aspect), bounded only by the cols×rows box —
    /// for page-as-image content (PDF), not inline figures.
    Full,
}

/// Per-image sizing intent passed to [`target_cells`] / the build worker.
#[derive(Clone, Copy, PartialEq)]
pub struct SizeSpec {
    /// The authored display width, if any.
    pub hint: SizeHint,
    /// An equation rendered as a picture: kept at native size (proportional to
    /// the text), never normalized or enlarged.
    pub math: bool,
}

impl Default for SizeSpec {
    fn default() -> SizeSpec {
        SizeSpec {
            hint: SizeHint::Auto,
            math: false,
        }
    }
}

/// Upper bound on how far a low-resolution figure may be enlarged to reach its
/// target display width. Caps the softness from upscaling — and keeps genuinely
/// tiny images (icons, ornaments) from being blown up to fill the column.
const MAX_UPSCALE: f64 = 2.5;

/// The cell geometry and caps an image must fit into: terminal cell size
/// (`fw`×`fh` px), the available `cols`×`rows` box, the longest-side pixel cap
/// (`max_px`, 0 = none), and the default figure width (`target_pct`% of the
/// column) for images with no authored size.
#[derive(Clone, Copy)]
pub struct FitBox {
    pub fw: u16,
    pub fh: u16,
    pub cols: u16,
    pub rows: u16,
    pub max_px: u16,
    pub target_pct: u16,
}

/// Cell size (cols, rows) for a `w`×`h` px image. Figures are sized to a
/// *consistent display width* — the authored width (`spec.hint`) when known, else
/// `fit.target_pct`% of the column — enlarging low-res figures up to
/// [`MAX_UPSCALE`] so they aren't tiny, but never past the `fit.cols`×`fit.rows`
/// box. Equation images (`spec.math`) keep native size and only ever shrink to
/// fit. The longest displayed side is then capped to `fit.max_px` px to bound the
/// terminal transfer. Used by both the up-front row estimate and the background
/// build, so the two always agree (no gap).
pub fn target_cells(w: u32, h: u32, fit: FitBox, spec: SizeSpec) -> (u16, u16) {
    if w == 0 || h == 0 || fit.fw == 0 || fit.fh == 0 {
        return (1, 1);
    }
    let (wf, hf) = (w as f64, h as f64);
    let (fwf, fhf) = (f64::from(fit.fw), f64::from(fit.fh));
    // The most the aspect-preserving image can scale before it overflows the
    // column width or the viewport height.
    let cap = (f64::from(fit.cols) * fwf / wf).min(f64::from(fit.rows) * fhf / hf);

    let mut scale = if spec.math {
        // Equations read best at native size, proportional to the surrounding
        // text; only shrink to fit, never enlarge.
        cap.min(1.0)
    } else if matches!(spec.hint, SizeHint::Full) {
        // A full-bleed page (PDF): fill the pane box, preserving aspect —
        // enlarging a small page or shrinking a large one to the cols×rows box.
        cap
    } else {
        // The display width we want this figure to occupy, in pixels.
        let want_px = match spec.hint {
            SizeHint::Pct(p) => f64::from(fit.cols) * fwf * f64::from(p).clamp(0.0, 1.0),
            SizeHint::Px(px) => f64::from(px),
            SizeHint::Auto => f64::from(fit.cols) * fwf * f64::from(fit.target_pct) / 100.0,
            SizeHint::Full => unreachable!("full-bleed handled above"),
        };
        // Reach it (up- or down-scaling), but never blow up tiny art past the
        // upscale cap and never exceed the box.
        (want_px / wf).min(cap).min(MAX_UPSCALE)
    };
    if scale <= 0.0 {
        scale = cap.min(1.0);
    }

    // A full-bleed page is bounded by the pane itself; the per-figure pixel cap
    // (which bounds inline-figure transfers) would only letterbox it, so skip it.
    let longest = (wf * scale).max(hf * scale);
    if fit.max_px > 0 && longest > f64::from(fit.max_px) && !matches!(spec.hint, SizeHint::Full) {
        scale *= f64::from(fit.max_px) / longest;
    }
    let cols = ((wf * scale / fwf).ceil() as u16).clamp(1, fit.cols.max(1));
    let rows = ((hf * scale / fhf).ceil() as u16).clamp(1, fit.rows.max(1));
    (cols, rows)
}

/// Identifies one built image so it can be cached and reused across sections
/// (revisiting a section reuses the already-uploaded image — no re-transmit).
/// Equal keys ⇒ identical build, so they share one cache entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImgKey {
    pub section: usize,
    pub idx: usize,
    pub avail: u16,
    pub max_rows: u16,
    pub max_px: u16,
    /// Default figure width (% of column) — part of the key so changing the knob
    /// rebuilds at the new size instead of serving a stale one.
    pub target_pct: u16,
    /// Theme tint + adaptation mode — part of the key so re-theming or changing
    /// the mode rebuilds the image rather than serving a stale one from cache.
    pub policy: RenderPolicy,
}

/// A built, ready-to-render inline image: a sliced protocol (so partial rows
/// can be drawn as it scrolls past an edge) plus its exact cell size.
pub struct ImagePlan {
    pub proto: SlicedProtocol,
    pub cols: u16,
    pub rows: u16,
}

impl ImagePlan {
    /// The terminal image id (Kitty), if any — used to delete it on eviction.
    pub fn image_id(&self) -> Option<u32> {
        self.proto.image_id()
    }

    /// The Kitty upload sequence if this image hasn't been transmitted yet, so a
    /// look-ahead page can be uploaded to the terminal *ahead* of display (no
    /// first-render upload flash). `None` for non-Kitty protocols or once sent.
    /// The caller MUST write the returned bytes to the terminal.
    pub fn pretransmit(&self) -> Option<String> {
        self.proto.pretransmit()
    }

    /// Whether this image still needs uploading to the terminal (non-consuming),
    /// so the reader can keep the loop alive until look-ahead pages are uploaded.
    pub fn needs_pretransmit(&self) -> bool {
        self.proto.needs_pretransmit()
    }
}

/// Kitty escape sequence to delete an image (and free its data) by id.
pub fn delete_image_seq(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id}\x1b\\")
}

// ── Direct Kitty image management (for full-page PDF rendering) ───────────────
//
// The unicode-placeholder path (above) is for inline figures that flow with
// text. A full PDF page is better managed directly, the way kitty's own `icat`
// does it: transmit the page to the terminal *once* as a stored image (`a=t`),
// then *display* it with a cheap placement (`a=p`) that re-uses the stored data.
// Swapping pages then never re-transmits — and the previous page can stay on
// screen until the next placement lands, so a page turn has no black gap.

/// Kitty: transmit `png` to the terminal and store it under `id` **without
/// displaying it** (`a=t`). Chunked at the protocol's 4096-base64-char limit.
/// Show it later with [`place_image_seq`]; `id` and the data persist until
/// [`delete_image_seq`]. `q=2` suppresses the terminal's responses.
pub fn transmit_image_seq(id: u32, png: &[u8]) -> String {
    use base64::Engine;
    // 4096 base64 chars ⇒ 3072 source bytes per chunk.
    const CHUNK: usize = (4096 / 4) * 3;
    let chunks = png.chunks(CHUNK);
    let n = chunks.len().max(1);
    let mut out = String::with_capacity(png.len() * 4 / 3 + n * 24);
    for (i, chunk) in chunks.enumerate() {
        out.push_str("\x1b_Gq=2,");
        if i == 0 {
            // a=t: transmit only (store, don't display). f=100: PNG (kitty reads
            // the dimensions from the header). t=d: data is inline (direct).
            let _ = write!(out, "i={id},a=t,f=100,t=d,");
        }
        let more = u8::from(i + 1 < n);
        let _ = write!(out, "m={more};");
        base64::engine::general_purpose::STANDARD.encode_string(chunk, &mut out);
        out.push_str("\x1b\\");
    }
    out
}

/// Kitty: display the already-transmitted image `id` at terminal cell
/// (`col`,`row`) (1-based), scaled to fill `cols`×`rows` cells (`a=p`).
///
/// Deliberately **no placement id** (`p=`): placements key on the
/// (image-id, placement-id) pair, so two images sharing a placement id make the
/// second delete the first (the two-page spread's left page went blank). Without
/// `p=`, each image gets its own placement and they coexist — the approach the
/// reference kitty PDF viewer (`termpdf.py`) uses. The cursor is saved/restored
/// (`\x1b7`/`\x1b8`) so the surrounding TUI is undisturbed.
pub fn place_image_seq(id: u32, col: u16, row: u16, cols: u16, rows: u16) -> String {
    format!("\x1b7\x1b[{row};{col}H\x1b_Ga=p,i={id},c={cols},r={rows},q=2\x1b\\\x1b8")
}

/// Decode, upscale-to-fill, and encode one image into a sliced protocol. This
/// is the expensive step (RGBA encode), so it runs on the [`ImageBuilder`]
/// worker.
fn build_plan(
    picker: &Picker,
    bytes: &[u8],
    fit: FitBox,
    policy: RenderPolicy,
    spec: SizeSpec,
) -> Option<ImagePlan> {
    let img = decode(bytes)?;
    let (w, h) = img.dimensions();
    let fs = picker.font_size();
    let fit = FitBox {
        fw: fs.width,
        fh: fs.height,
        ..fit
    };
    let (cols, rows) = target_cells(w, h, fit, spec);

    // Resize to exactly the target cell box in pixels (Triangle: fast, fine for
    // figures; up- or down-scales), so the protocol fills (cols, rows) precisely.
    let img = img.resize(
        cols as u32 * fs.width.max(1) as u32,
        rows as u32 * fs.height.max(1) as u32,
        image::imageops::FilterType::Triangle,
    );
    // Adapt the graphic to the theme (recolour ink / flatten / invert) per mode.
    let img = render_for_theme(&img, policy.tint, policy.mode);
    let size = ratatui::layout::Size::new(cols, rows);
    let proto =
        SlicedProtocol::new_with_resize(picker, img, size, ratatui_image::Resize::Fit(None))
            .ok()?;
    let s = proto.size();
    Some(ImagePlan {
        proto,
        cols: s.width,
        rows: s.height,
    })
}

/// A request to build one image's protocol off the main thread.
struct BuildReq {
    key: ImgKey,
    bytes: Vec<u8>,
    spec: SizeSpec,
}

/// A completed image build. `plan` is `None` if the image failed to build (so
/// the reader stops waiting); `stale` means it was skipped because the reader
/// had scrolled far away by the time the worker reached it (re-request later).
pub struct BuiltImage {
    pub key: ImgKey,
    pub plan: Option<ImagePlan>,
    pub stale: bool,
}

/// Sections farther than this from the current one are skipped by the worker, so
/// a fast-scroll backlog of flown-past sections doesn't delay the current one.
const KEEP_RADIUS: usize = 3;

/// Builds image protocols on a background thread so decoding/encoding never
/// stalls scrolling. Send requests with [`request`], collect ready ones with
/// [`poll`]. Keep the worker informed of the viewport via [`set_current`] so it
/// can drop stale work.
pub struct ImageBuilder {
    req_tx: Sender<BuildReq>,
    res_rx: Receiver<BuiltImage>,
    current: Arc<AtomicUsize>,
}

impl ImageBuilder {
    pub fn new(picker: Picker) -> ImageBuilder {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<BuildReq>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<BuiltImage>();
        let current = Arc::new(AtomicUsize::new(0));
        let worker_current = Arc::clone(&current);
        thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                let k = req.key;
                // Skip builds for sections the reader has already scrolled away
                // from — they only delay the section now in view.
                let cur = worker_current.load(Ordering::Relaxed);
                if k.section.abs_diff(cur) > KEEP_RADIUS {
                    if res_tx
                        .send(BuiltImage {
                            key: k,
                            plan: None,
                            stale: true,
                        })
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                // `fw`/`fh` are filled in by `build_plan` from the live font size.
                let fit = FitBox {
                    fw: 0,
                    fh: 0,
                    cols: k.avail,
                    rows: k.max_rows,
                    max_px: k.max_px,
                    target_pct: k.target_pct,
                };
                let plan = build_plan(&picker, &req.bytes, fit, k.policy, req.spec);
                if res_tx
                    .send(BuiltImage {
                        key: k,
                        plan,
                        stale: false,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        ImageBuilder {
            req_tx,
            res_rx,
            current,
        }
    }

    /// Tell the worker which section is in view, so it can drop stale builds.
    pub fn set_current(&self, section: usize) {
        self.current.store(section, Ordering::Relaxed);
    }

    pub fn request(&self, key: ImgKey, bytes: Vec<u8>, spec: SizeSpec) {
        let _ = self.req_tx.send(BuildReq { key, bytes, spec });
    }

    pub fn poll(&self) -> impl Iterator<Item = BuiltImage> + '_ {
        self.res_rx.try_iter()
    }
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

    /// A cell box `cols` wide (8×16px cells, `rows` tall, no px cap, 85% target).
    fn fit(cols: u16, rows: u16) -> FitBox {
        FitBox {
            fw: 8,
            fh: 16,
            cols,
            rows,
            max_px: 0,
            target_pct: 85,
        }
    }

    #[test]
    fn image_never_wider_than_the_text_column() {
        // A very wide image must be scaled to fit — its cell width can never
        // exceed the available text width, in single-page or two-page layout.
        for avail in [20u16, 48, 96, 200] {
            let (cols, _rows) = target_cells(4000, 600, fit(avail, 40), SizeSpec::default());
            assert!(
                cols <= avail,
                "avail={avail}: cols={cols} must not exceed it"
            );
        }
    }

    #[test]
    fn low_res_figures_normalize_up_but_bounded() {
        // A small figure is enlarged toward the target width (so figures look
        // consistent), not left tiny — but bounded by the upscale cap so it is
        // never blown up absurdly (an 80px image at most MAX_UPSCALE×).
        let (cols, _) = target_cells(80, 40, fit(200, 40), SizeSpec::default());
        assert!(
            cols > 10,
            "low-res figure is upscaled past native ~10 cols: {cols}"
        );
        assert!(cols <= 25, "but bounded by the upscale cap: {cols}");
    }

    #[test]
    fn equation_images_stay_native_size() {
        // Equation images keep native size (proportional to the text), never
        // normalized up to fill the column.
        let math = SizeSpec {
            hint: SizeHint::Auto,
            math: true,
        };
        let (cols, _) = target_cells(80, 40, fit(200, 40), math);
        assert!(
            cols <= 10,
            "equation at native ~10 cols, not stretched: {cols}"
        );
    }

    #[test]
    fn authored_width_is_honored() {
        // A 50% CSS width targets half the column regardless of pixel resolution.
        let half = SizeSpec {
            hint: SizeHint::Pct(0.5),
            math: false,
        };
        let (cols, _) = target_cells(4000, 2000, fit(100, 200), half);
        assert!(
            (i32::from(cols) - 50).abs() <= 2,
            "≈50% of 100 cols: {cols}"
        );
    }

    #[test]
    fn full_bleed_page_fills_the_pane() {
        // A full-bleed page (PDF) fills the pane, unlike a figure: it is bounded
        // only by the cols×rows box, ignoring the upscale cap and the px cap.
        let page = SizeSpec {
            hint: SizeHint::Full,
            math: false,
        };
        // A portrait (A4-ish) page in a wide-enough pane fills the column width.
        let (cols, _) = target_cells(1240, 1750, fit(100, 200), page);
        assert!(
            (i32::from(cols) - 100).abs() <= 1,
            "page fills width: {cols}"
        );

        // A small page is enlarged to fill — no MAX_UPSCALE cap (a figure of the
        // same size would stay near native size).
        let (small, _) = target_cells(80, 113, fit(100, 200), page);
        assert!(small >= 90, "small page upscales to fill: {small}");

        // The per-figure pixel cap must not letterbox a page (the pane bounds it).
        let capped = FitBox {
            max_px: 100,
            ..fit(100, 200)
        };
        let (cols_capped, _) = target_cells(1240, 1750, capped, page);
        assert!(
            (i32::from(cols_capped) - 100).abs() <= 1,
            "max_px does not shrink a full-bleed page: {cols_capped}"
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
}
