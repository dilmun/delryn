//! Graphical math: render LaTeX math to a themed-ready PNG, on-disk cached.
//!
//! Wraps [RaTeX](https://crates.io/crates/ratex-render) (pure-Rust LaTeX → raster,
//! KaTeX coverage, embedded KaTeX fonts) behind a tiny API so the rest of delryn
//! never depends on it directly. Equations render **black on transparent** so
//! delryn's existing equation-image recolour paints them in the theme ink (exactly
//! like publisher equation PNGs); the PNG is therefore theme-independent and cached
//! to `<config>/math/<hash>.png`, so re-opens and re-wraps are instant.
//!
//! Every failure path — a parse error, a render error, or a panic inside the young
//! renderer — returns `None`, so the caller keeps the Unicode approximation. Nothing
//! here can take the app down or block the main thread (callers render off-thread).

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use ratex_layout::{LayoutOptions, layout, to_display_list};
use ratex_parser::parser::parse;
use ratex_render::{RenderOptions, render_to_png};
use ratex_types::color::Color;
use ratex_types::math_style::MathStyle;

/// Bumped when the render parameters below change, so stale cached PNGs are ignored
/// (the hash includes it) without needing to clear the cache dir.
const RENDER_VERSION: u32 = 2;

/// Whether a display (block) or inline equation is being rendered — display uses
/// full-size operators and limits above/below; inline (text style) is compact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Display,
    Inline,
}

/// Render `latex` to a black-on-transparent PNG at `em_px` pixels per em (its
/// natural resolution — the caller sizes it to the terminal so equations show at a
/// consistent text-relative size and crisply, since delryn-media shows math at native
/// px). Disk-cached. Returns `None` when RaTeX can't handle the input — the caller
/// then keeps the Unicode fallback.
pub fn render(latex: &str, style: Style, em_px: u32) -> Option<Vec<u8>> {
    let em_px = em_px.clamp(8, 400);
    let path = cache_path(latex, style, em_px);
    if let Ok(bytes) = std::fs::read(&path)
        && !bytes.is_empty()
    {
        return Some(bytes);
    }
    let png = render_uncached(latex, style, em_px)?;
    // Best-effort cache write; a failure just means we re-render next time.
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, &png);
    }
    Some(png)
}

/// The RaTeX parse → layout → raster pipeline, guarded against panics (it's a young
/// library, so a pathological equation degrades to the Unicode fallback rather than
/// bringing the app down).
fn render_uncached(latex: &str, style: Style, em_px: u32) -> Option<Vec<u8>> {
    let latex = latex.to_string();
    std::panic::catch_unwind(move || {
        let ast = parse(&latex).ok()?;
        let math_style = match style {
            Style::Display => MathStyle::Display,
            Style::Inline => MathStyle::Text,
        };
        let opts = LayoutOptions::default()
            .with_style(math_style)
            .with_color(Color::BLACK);
        let dl = to_display_list(&layout(&ast, &opts));
        let opts = RenderOptions {
            font_size: em_px as f32,
            padding: 2.0,
            // Transparent — the ink is recoloured to the theme at display time.
            background_color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            // 1× — the raster is already produced at the target resolution, and
            // delryn-media shows math at native px, so it's crisp with no downscale.
            device_pixel_ratio: 1.0,
            ..Default::default()
        };
        render_to_png(&dl, &opts).ok().filter(|p| !p.is_empty())
    })
    .ok()
    .flatten()
}

/// `<config>/math/<16-hex>.png`, keyed by the render version + style + em size +
/// source. The theme is deliberately *not* in the key: the PNG is black ink on
/// transparent and recoloured at display time, so one cache entry serves every theme.
fn cache_path(latex: &str, style: Style, em_px: u32) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    RENDER_VERSION.hash(&mut h);
    matches!(style, Style::Display).hash(&mut h);
    em_px.hash(&mut h);
    latex.hash(&mut h);
    delryn_infra::paths::config_dir()
        .join("math")
        .join(format!("{:016x}.png", h.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real equation renders to a cached PNG; the cache returns identical bytes;
    /// and pathological input degrades gracefully (no panic, no crash).
    #[test]
    fn renders_caches_and_degrades() {
        let tmp = std::env::temp_dir().join(format!("delryn_math_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // SAFETY: single test, no other thread touches the env; scopes the cache dir.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };

        let png =
            render("x^2 + \\frac{1}{2}", Style::Display, 40).expect("renders a real equation");
        assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G'], "PNG magic");
        assert!(png.len() > 100, "non-trivial PNG");
        // The cache file now exists and returns the same bytes.
        assert!(cache_path("x^2 + \\frac{1}{2}", Style::Display, 40).exists());
        let again = render("x^2 + \\frac{1}{2}", Style::Display, 40).expect("served from cache");
        assert_eq!(again, png);
        // Garbage never panics; it returns None or a PNG, but does not crash.
        let _ = render("\\notacommand{{{", Style::Inline, 40);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
