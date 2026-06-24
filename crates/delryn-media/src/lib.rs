//! Terminal image support: protocol detection and decoding. Wraps
//! `ratatui-image` so the rest of the app doesn't depend on it directly.
//! See `DESIGN.md` §0 (graphics protocols).

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

/// Whether an opaque graphic sits on a light background (a white-page chart,
/// diagram, or screenshot) — the kind that `InvertBackgrounds` flips to dark.
fn is_light_background(img: &RgbaImage) -> bool {
    match analyze_background(img) {
        // bg is normalised 0–1; weight by luminance.
        Background::Solid(bg) => 0.299 * bg[0] + 0.587 * bg[1] + 0.114 * bg[2] > 0.6,
        Background::Alpha => false,
    }
}

/// Theme-aware lightness inversion for a light-background figure: white↔black is
/// swapped, but mapped *into the theme*, so the result sits on the reader's page
/// instead of a hardcoded black box.
///
/// - **Neutral** pixels (the white background, black axes/text) → the exact theme
///   `paper`↔`ink` by inverted lightness, so the background matches the page on
///   any theme (not just black ones).
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
        let px = if sat < 0.15 {
            // Neutral → exact theme colours (background becomes the page colour).
            lerp3(colors.paper, colors.ink, inv)
        } else {
            // Colour → keep hue/sat, map inverted lightness into the theme band.
            hsl_to_rgb(hue, sat, paper_l + (ink_l - paper_l) * inv)
        };
        out.put_pixel(x, y, Rgba([px[0], px[1], px[2], p[3]]));
    }
    DynamicImage::ImageRgba8(out)
}

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
/// by inline reader images and the full-screen viewer. The `mode` selects how:
/// - **Auto**: recolour transparent monochrome ink (equations/line drawings) to
///   the theme; flatten transparent colour graphics onto the page; opaque
///   graphics (figures, photos, white-bg diagrams) carry their own background and
///   are left untouched.
/// - **InvertBackgrounds**: as Auto, but opaque *light-background* figures are
///   lightness-inverted so they're dark-friendly with detail intact.
/// - **Faithful**: never recolour or invert — only flatten transparency onto the
///   page so nothing is invisible; original colours preserved.
pub fn render_for_theme(img: &DynamicImage, tint: Ink, mode: ImageMode) -> DynamicImage {
    let rgba = img.to_rgba8();
    if mode == ImageMode::Faithful {
        return flatten_onto(img, tint.paper);
    }
    if transparent_frac(&rgba) > TRANSPARENT_FRAC {
        // Transparent ink graphic.
        return if opaque_chroma(&rgba) < INK_CHROMA_MAX {
            recolor_ink(img, tint) // equation/line-art → theme matte
        } else {
            flatten_onto(img, tint.paper) // transparent colour → composite
        };
    }
    // Opaque graphic: carries its own background.
    if mode == ImageMode::InvertBackgrounds && is_light_background(&rgba) {
        theme_invert(img, tint) // light-bg figure → theme-matched dark, detail kept
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

/// Cell size (cols, rows) for a `w`×`h` px image: fit its aspect within
/// `avail_cols`×`max_rows` cells **without enlarging past native size**, then cap
/// the displayed longest side to `max_px` pixels so the data transmitted to the
/// terminal stays bounded. `fw`×`fh` is the terminal cell size in px. Used by
/// both the up-front row estimate and the background build, so the two always
/// agree (no gap).
pub fn target_cells(
    w: u32,
    h: u32,
    fw: u16,
    fh: u16,
    avail_cols: u16,
    max_rows: u16,
    max_px: u16,
) -> (u16, u16) {
    if w == 0 || h == 0 || fw == 0 || fh == 0 {
        return (1, 1);
    }
    let (wf, hf, fwf, fhf) = (w as f64, h as f64, fw as f64, fh as f64);
    // Fit within the box, but never upscale: a small equation rendered at native
    // size stays proportional to the text, instead of being blown up to fill the
    // column (which made some equations huge while wider ones looked right).
    let mut scale = (avail_cols as f64 * fwf / wf)
        .min(max_rows as f64 * fhf / hf)
        .min(1.0);
    let longest = (wf * scale).max(hf * scale);
    if max_px > 0 && longest > max_px as f64 {
        scale *= max_px as f64 / longest;
    }
    let cols = ((wf * scale / fwf).ceil() as u16).clamp(1, avail_cols.max(1));
    let rows = ((hf * scale / fhf).ceil() as u16).clamp(1, max_rows.max(1));
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
}

/// Kitty escape sequence to delete an image (and free its data) by id.
pub fn delete_image_seq(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id}\x1b\\")
}

/// Decode, upscale-to-fill, and encode one image into a sliced protocol. This
/// is the expensive step (RGBA encode), so it runs on the [`ImageBuilder`]
/// worker.
fn build_plan(
    picker: &Picker,
    bytes: &[u8],
    avail_cols: u16,
    max_rows: u16,
    max_px: u16,
    policy: RenderPolicy,
) -> Option<ImagePlan> {
    let img = decode(bytes)?;
    let (w, h) = img.dimensions();
    let fs = picker.font_size();
    let (cols, rows) = target_cells(w, h, fs.width, fs.height, avail_cols, max_rows, max_px);

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
                let plan = build_plan(&picker, &req.bytes, k.avail, k.max_rows, k.max_px, k.policy);
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

    pub fn request(&self, key: ImgKey, bytes: Vec<u8>) {
        let _ = self.req_tx.send(BuildReq { key, bytes });
    }

    pub fn poll(&self) -> impl Iterator<Item = BuiltImage> + '_ {
        self.res_rx.try_iter()
    }
}

/// An open image viewer: a set of decoded images (as resize protocols) for the
/// current section, with a selected index.
pub struct ImageView {
    pub protocols: Vec<StatefulProtocol>,
    pub sel: usize,
}

impl ImageView {
    /// Build a viewer from raw image bytes; `None` if nothing decodes. `policy`
    /// applies the same theme-aware rendering as the inline reader.
    pub fn new(picker: &Picker, images: &[Vec<u8>], policy: RenderPolicy) -> Option<ImageView> {
        let protocols: Vec<StatefulProtocol> = images
            .iter()
            .filter_map(|b| decode(b))
            .map(|img| picker.new_resize_protocol(render_for_theme(&img, policy.tint, policy.mode)))
            .collect();
        if protocols.is_empty() {
            None
        } else {
            Some(ImageView { protocols, sel: 0 })
        }
    }

    pub fn len(&self) -> usize {
        self.protocols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.protocols.is_empty()
    }

    pub fn next(&mut self) {
        if !self.protocols.is_empty() {
            self.sel = (self.sel + 1) % self.protocols.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.protocols.is_empty() {
            self.sel = (self.sel + self.protocols.len() - 1) % self.protocols.len();
        }
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
    fn faithful_mode_never_recolours_equations() {
        // A transparent black equation: Auto recolours to theme ink; Faithful
        // only composites onto paper (keeps the original black ink).
        let eq = transparent_ink(10, 10, (4, 4, 6, 6));
        let faithful = render_for_theme(&eq, DARK, ImageMode::Faithful).to_rgba8();
        assert_eq!(faithful.get_pixel(5, 5).0, [0, 0, 0, 255], "ink kept black");
        assert_eq!(
            faithful.get_pixel(0, 0).0,
            [10, 12, 16, 255],
            "transparent → paper"
        );
        let auto = render_for_theme(&eq, DARK, ImageMode::Auto).to_rgba8();
        assert_eq!(
            auto.get_pixel(5, 5).0,
            [220, 220, 220, 255],
            "auto recolours to ink"
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
