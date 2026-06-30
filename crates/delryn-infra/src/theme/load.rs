//! Loading user-authored themes from `<config>/themes/*.toml` and mapping their
//! palette onto the flat [`Theme`]. Best-effort: a malformed or unreadable theme
//! file is skipped, never fatal — a bad theme can't crash delryn.

use ratatui::style::Color;
use serde::Deserialize;

use super::Theme;
use super::palette::{Palette, parse_hex};

/// A parsed theme file: a display `name`, an optional `syntect` code theme, and
/// the colour `[palette]`.
#[derive(Debug, Deserialize)]
struct ThemeFile {
    name: String,
    #[serde(default = "default_syntect")]
    syntect: String,
    palette: Palette,
}

fn default_syntect() -> String {
    "base16-ocean.dark".to_string()
}

/// Scan [`crate::paths::themes_dir`] and load every readable `*.toml` theme,
/// sorted by filename for a stable cycle order. Empty when the directory is
/// absent or unreadable.
pub fn load_user_themes() -> Vec<Theme> {
    let dir = crate::paths::themes_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|text| toml::from_str::<ThemeFile>(&text).ok())
        .filter_map(theme_from)
        .collect()
}

/// Map a parsed theme file onto the flat [`Theme`], deriving any omitted roles
/// from `bg`/`text`/`accent`. `bg`/`text` are required; a file missing them (or
/// with unparsable colours) is dropped. Names + syntect are leaked to
/// `&'static str` — themes load once at startup, so the bounded leak lets `Theme`
/// stay `Copy` (and keeps every existing `theme.field` call site unchanged).
fn theme_from(tf: ThemeFile) -> Option<Theme> {
    let p = &tf.palette;
    let bg = parse_hex(&p.bg)?;
    let fg = parse_hex(&p.text)?;
    let hex = |o: &Option<String>| o.as_deref().and_then(parse_hex);
    let accent = hex(&p.accent).unwrap_or(fg);
    let muted = hex(&p.muted).unwrap_or_else(|| mix(fg, bg, 0.5));
    Some(Theme {
        name: leak(tf.name),
        bg: Some(bg),
        fg,
        heading: hex(&p.heading).unwrap_or(fg),
        quote: hex(&p.quote).unwrap_or(muted),
        link: hex(&p.link).unwrap_or(accent),
        muted,
        marker: hex(&p.marker).unwrap_or(accent),
        code_fg: hex(&p.code).unwrap_or(fg),
        status_fg: hex(&p.status_fg).unwrap_or(bg),
        status_bg: hex(&p.status_bg).unwrap_or(accent),
        accent,
        danger: hex(&p.danger).unwrap_or(Color::Rgb(0xe0, 0x5a, 0x5a)),
        syntect: leak(tf.syntect),
    })
}

/// Leak a `String` to `&'static str`. Bounded — themes load once at startup.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Blend `a` toward `b` by `t` (0..=1). Concrete colours only; a terminal-relative
/// colour returns `a` unchanged.
fn mix(a: Color, b: Color, t: f32) -> Color {
    match (super::rgb_of(a), super::rgb_of(b)) {
        (Some(x), Some(y)) => {
            let m = |i: usize| (x[i] as f32 * (1.0 - t) + y[i] as f32 * t).round() as u8;
            Color::Rgb(m(0), m(1), m(2))
        }
        _ => a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_a_minimal_palette_with_derivations() {
        let toml = r##"
            name = "mine"
            [palette]
            bg = "#101010"
            text = "#e0e0e0"
            accent = "#88aaff"
        "##;
        let tf: ThemeFile = toml::from_str(toml).unwrap();
        let t = theme_from(tf).expect("valid theme");
        assert_eq!(t.name, "mine");
        assert_eq!(t.bg, Some(Color::Rgb(0x10, 0x10, 0x10)));
        assert_eq!(t.fg, Color::Rgb(0xe0, 0xe0, 0xe0));
        // Omitted roles derive: link/marker from accent, heading from text.
        assert_eq!(t.link, Color::Rgb(0x88, 0xaa, 0xff));
        assert_eq!(t.heading, t.fg);
        // Default syntect when unset.
        assert_eq!(t.syntect, "base16-ocean.dark");
    }

    #[test]
    fn rejects_a_palette_missing_required_colours() {
        // No `text`, and a bad `bg` — both make the file unusable.
        let bad_bg = r##"name = "x"
            [palette]
            bg = "nope"
            text = "#fff""##;
        let tf: ThemeFile = toml::from_str(bad_bg).unwrap();
        assert!(theme_from(tf).is_none());
    }
}
