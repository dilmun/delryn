//! The fixed palette of highlighter colours a reader can mark lines with. A
//! highlight annotation stores one of these as a small integer index
//! (`annotations.color`), so the on-disk value stays stable even if the swatches
//! are ever retinted. Kept here beside the theme because the swatches are
//! presentation colours; the store persists only the index and never interprets
//! it.

use ratatui::style::Color;

/// A highlighter colour. A highlight annotation carries one of these (as its
/// palette index); bookmarks and notes ignore it. The set is deliberately small
/// and fixed — five easily distinguished marker colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightColor {
    Yellow,
    Green,
    Blue,
    Pink,
    Orange,
}

/// Dark ink painted over every highlight swatch. All swatches are light pastels,
/// so one near-black ink stays readable on each — a highlight is its own little
/// surface, reading the same on light and dark themes.
const INK: Color = Color::Rgb(0x1A, 0x1A, 0x1A);

impl HighlightColor {
    /// Every colour in cycle order — the order `H` steps through in the reader and
    /// the order highlights are listed in.
    pub const ALL: [HighlightColor; 5] = [
        HighlightColor::Yellow,
        HighlightColor::Green,
        HighlightColor::Blue,
        HighlightColor::Pink,
        HighlightColor::Orange,
    ];

    /// This colour's stored palette index (its position in [`ALL`]).
    pub fn index(self) -> i64 {
        Self::ALL.iter().position(|&c| c == self).unwrap_or(0) as i64
    }

    /// The colour for a stored index; an out-of-range index falls back to the
    /// first colour, so retiring a colour later degrades gracefully.
    pub fn from_index(i: i64) -> HighlightColor {
        usize::try_from(i)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
            .unwrap_or(HighlightColor::Yellow)
    }

    /// Stable human-readable name (flash messages, listings).
    pub fn label(self) -> &'static str {
        match self {
            HighlightColor::Yellow => "yellow",
            HighlightColor::Green => "green",
            HighlightColor::Blue => "blue",
            HighlightColor::Pink => "pink",
            HighlightColor::Orange => "orange",
        }
    }

    /// The marker background colour.
    pub fn bg(self) -> Color {
        match self {
            HighlightColor::Yellow => Color::Rgb(0xFF, 0xE8, 0x82),
            HighlightColor::Green => Color::Rgb(0xB6, 0xE8, 0xA8),
            HighlightColor::Blue => Color::Rgb(0xA8, 0xD4, 0xFF),
            HighlightColor::Pink => Color::Rgb(0xFF, 0xBE, 0xD6),
            HighlightColor::Orange => Color::Rgb(0xFF, 0xC9, 0x8C),
        }
    }

    /// The (background, ink) pair for washing highlighted text — a bright pastel
    /// with a readable dark ink on top.
    pub fn wash(self) -> (Color, Color) {
        (self.bg(), INK)
    }

    /// The next step of the reader's `H` cycle: `None` (unhighlighted) → the first
    /// colour → … → the last colour → `None` (removed). A repeated `H` thus walks
    /// every colour and then clears the highlight.
    pub fn cycle(current: Option<HighlightColor>) -> Option<HighlightColor> {
        match current {
            None => Some(Self::ALL[0]),
            Some(c) => {
                let i = Self::ALL.iter().position(|&x| x == c).unwrap_or(0);
                Self::ALL.get(i + 1).copied()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_round_trips_and_clamps() {
        for c in HighlightColor::ALL {
            assert_eq!(HighlightColor::from_index(c.index()), c);
        }
        // Out-of-range (negative or past the end) falls back to the first colour.
        assert_eq!(HighlightColor::from_index(-1), HighlightColor::Yellow);
        assert_eq!(HighlightColor::from_index(99), HighlightColor::Yellow);
    }

    #[test]
    fn cycle_walks_every_colour_then_clears() {
        // None → each colour in order → None.
        let mut cur = None;
        for expected in HighlightColor::ALL {
            cur = HighlightColor::cycle(cur);
            assert_eq!(cur, Some(expected));
        }
        assert_eq!(
            HighlightColor::cycle(cur),
            None,
            "past the last colour clears"
        );
    }
}
