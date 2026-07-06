//! Terminal display-width measurement.
//!
//! A layout column is a grid of fixed-width cells, so every width decision must
//! count *display columns*, not `char`s: a wide CJK glyph occupies two cells,
//! combining marks and zero-width joiners occupy none. Measuring with
//! `chars().count()` overruns the column on CJK text, distributes the wrong
//! justification slack, and drifts table separators. Route every width decision
//! in the wrap engine through [`display_width`] instead.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of `s` in terminal cells (wide CJK = 2, combining / zero-width
/// = 0). For pure ASCII/Latin this equals `s.chars().count()`.
pub(crate) fn display_width(s: impl AsRef<str>) -> usize {
    UnicodeWidthStr::width(s.as_ref())
}

/// Display width of a single `char` in terminal cells (wide = 2, combining /
/// control = 0). Used by the glyph-level wrap fill.
pub(crate) fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// The longest prefix of `s` that fits within `max` display columns, together
/// with that prefix's actual width. A wide glyph that would straddle the `max`
/// boundary is dropped whole rather than split, so the returned width can be
/// `max - 1`; the caller pads the shortfall to keep columns aligned.
pub(crate) fn truncate_to_width(s: &str, max: usize) -> (String, usize) {
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    (out, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_width_equals_char_count() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn wide_and_zero_width_chars_are_measured_in_cells() {
        // CJK ideographs are two cells each.
        assert_eq!(display_width("日本語"), 6);
        // A combining acute mark adds no width to its base.
        assert_eq!(display_width("e\u{0301}"), 1);
    }

    #[test]
    fn truncate_never_splits_a_wide_glyph() {
        // "日本語" is 3 glyphs × 2 cells. Truncating to 3 columns keeps one glyph
        // (2 cells) and drops the second whole rather than emitting half a cell.
        let (t, w) = truncate_to_width("日本語", 3);
        assert_eq!((t.as_str(), w), ("日", 2));
        // Exact fit keeps everything.
        assert_eq!(truncate_to_width("ab", 2), ("ab".to_string(), 2));
    }
}
