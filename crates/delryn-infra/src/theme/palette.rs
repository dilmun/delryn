//! A user theme file's colour palette — the named swatches an author fills in,
//! plus hex parsing. The mapping from a palette to the flat [`super::Theme`]
//! (with derivations for omitted swatches) lives in [`super::load`].

use ratatui::style::Color;
use serde::Deserialize;

/// The swatches a `<config>/themes/*.toml` file may set. Only `bg` and `text` are
/// required; every other role falls back to a derivation of those plus `accent`
/// (see [`super::load`]).
#[derive(Debug, Clone, Deserialize)]
pub struct Palette {
    pub bg: String,
    pub text: String,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub muted: Option<String>,
    #[serde(default)]
    pub marker: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub danger: Option<String>,
}

/// Parse a `#rrggbb` or `#rgb` hex string into an RGB colour.
pub fn parse_hex(s: &str) -> Option<Color> {
    let t = s.trim();
    let h = t.strip_prefix('#').unwrap_or(t);
    match h.len() {
        6 => {
            let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
            Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
        }
        3 => {
            // Shorthand: each nibble doubled (`f` → `ff`).
            let nib = |c: u8| (c as char).to_digit(16).map(|v| (v * 17) as u8);
            let b = h.as_bytes();
            Some(Color::Rgb(nib(b[0])?, nib(b[1])?, nib(b[2])?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_forms() {
        assert_eq!(parse_hex("#1e1e2e"), Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(parse_hex("1e1e2e"), Some(Color::Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(parse_hex("#fff"), Some(Color::Rgb(255, 255, 255)));
        assert_eq!(parse_hex("#abc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex("nope"), None);
        assert_eq!(parse_hex("#12"), None);
    }
}
