//! The option enums cycled through the settings popup and persisted in the
//! config file.
//!
//! Each enum is a small closed set with `next`/`prev` (for the settings UI) and,
//! where it is persisted, `label`/`from_label` — its stable, human-readable
//! on-disk string. The four persisted enums (de)serialize as that `label()`
//! string via the `label_serde!` helper modules at the bottom of this file, so
//! `config.toml` keeps storing `"two-page"`, `"grid"`, … rather than serde's
//! variant names (and stays stable if the variants are ever reordered).

use serde::{Deserialize, Deserializer, Serializer};

/// Generate a `serde` `with`-module that (de)serializes an enum as its stable
/// [`label`]-string: serialize via `label()`, deserialize via `from_label()`
/// (which already maps any unknown label back to the enum's default). This keeps
/// the on-disk encoding a plain string and is identical across all four
/// persisted enums, so it earns one tiny macro instead of four copies.
macro_rules! label_serde {
    ($module:ident, $ty:ty) => {
        pub(crate) mod $module {
            use super::{Deserialize, Deserializer, Serializer, $ty};

            pub fn serialize<S: Serializer>(v: &$ty, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(v.label())
            }

            pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<$ty, D::Error> {
                let s = String::deserialize(d)?;
                Ok(<$ty>::from_label(&s))
            }
        }
    };
}

/// Generate the identical cyclic `next`/`prev`/`label` for an option enum from
/// its variants + on-disk labels, in order — the settings UI cycles with these.
/// Two flavours: `cyclic_wrap!` cycles round the ends (Center↔TwoPage,
/// Auto→Invert→Faithful→Auto); `cyclic_clamp!` stops at the first/last (the grid
/// size deliberately doesn't wrap Small↔XLarge). `from_label` stays hand-written
/// per enum — its fallback arm carries the default. The enum must be
/// `Copy + PartialEq`.
macro_rules! cyclic_wrap {
    ($ty:ty, [$($v:ident => $lbl:literal),+ $(,)?]) => {
        impl $ty {
            pub fn next(self) -> Self {
                let order = [$(<$ty>::$v),+];
                let i = order.iter().position(|&x| x == self).unwrap_or(0);
                order[(i + 1) % order.len()]
            }
            pub fn prev(self) -> Self {
                let order = [$(<$ty>::$v),+];
                let i = order.iter().position(|&x| x == self).unwrap_or(0);
                order[(i + order.len() - 1) % order.len()]
            }
            pub fn label(self) -> &'static str {
                match self { $(<$ty>::$v => $lbl,)+ }
            }
        }
    };
}

macro_rules! cyclic_clamp {
    ($ty:ty, [$($v:ident => $lbl:literal),+ $(,)?]) => {
        impl $ty {
            pub fn next(self) -> Self {
                let order = [$(<$ty>::$v),+];
                let i = order.iter().position(|&x| x == self).unwrap_or(0);
                order[(i + 1).min(order.len() - 1)]
            }
            pub fn prev(self) -> Self {
                let order = [$(<$ty>::$v),+];
                let i = order.iter().position(|&x| x == self).unwrap_or(0);
                order[i.saturating_sub(1)]
            }
            pub fn label(self) -> &'static str {
                match self { $(<$ty>::$v => $lbl,)+ }
            }
        }
    };
}

/// How body text is laid out in the content pane. Both layouts use the same
/// per-side edge padding (`side_padding`); two-page adds a configurable gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// A single column, centered within the side padding.
    Center,
    /// Two side-by-side columns — a two-page spread.
    TwoPage,
}

impl ViewMode {
    pub fn from_label(s: &str) -> ViewMode {
        match s {
            "two-page" => ViewMode::TwoPage,
            _ => ViewMode::Center,
        }
    }
}
cyclic_wrap!(ViewMode, [Center => "center", TwoPage => "two-page"]);

/// A reading-experience preset: a named bundle of layout / chrome / flow
/// settings. `Custom` is the derived state when the live settings match no
/// preset (e.g. after the reader has tweaked an individual setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingMode {
    Custom,
    Study,
    Research,
    Presentation,
}

/// The reading settings a preset bundles. Every preset fixes all of these, so a
/// live config can be compared field-for-field to recognise the active preset.
/// Deliberately excludes `view_mode` (Center / TwoPage): the page layout is a
/// personal choice a preset shouldn't yank out from under the reader.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadingProfile {
    pub(crate) side_padding: u16,
    pub(crate) line_spacing: u8,
    pub(crate) paragraph_spacing: u8,
    pub(crate) show_sidebar: bool,
    pub(crate) show_status: bool,
    pub(crate) chapter_lock: bool,
    pub(crate) paged: bool,
}

impl ReadingMode {
    pub fn label(self) -> &'static str {
        match self {
            ReadingMode::Custom => "custom",
            ReadingMode::Study => "study",
            ReadingMode::Research => "research",
            ReadingMode::Presentation => "presentation",
        }
    }

    /// Next/previous *applyable* preset. Cycles through the three real presets;
    /// from `Custom` it enters the cycle rather than landing back on `Custom`.
    pub fn next(self) -> Self {
        match self {
            ReadingMode::Custom | ReadingMode::Presentation => ReadingMode::Study,
            ReadingMode::Study => ReadingMode::Research,
            ReadingMode::Research => ReadingMode::Presentation,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ReadingMode::Custom | ReadingMode::Study => ReadingMode::Presentation,
            ReadingMode::Research => ReadingMode::Study,
            ReadingMode::Presentation => ReadingMode::Research,
        }
    }

    /// The settings this preset stands for (`None` for `Custom`).
    pub(crate) fn profile(self) -> Option<ReadingProfile> {
        let p = match self {
            ReadingMode::Custom => return None,
            // Deep, careful reading of one chapter: comfortable margins + spacing,
            // navigation + progress visible, stay put in the chapter.
            ReadingMode::Study => ReadingProfile {
                side_padding: 10,
                line_spacing: 1,
                paragraph_spacing: 1,
                show_sidebar: true,
                show_status: true,
                chapter_lock: true,
                paged: false,
            },
            // Scanning / cross-referencing across the whole book: denser and a
            // touch wider than default, flows freely between chapters.
            ReadingMode::Research => ReadingProfile {
                side_padding: 4,
                line_spacing: 0,
                paragraph_spacing: 1,
                show_sidebar: true,
                show_status: true,
                chapter_lock: false,
                paged: false,
            },
            // Distraction-free, slide-like: wide airy margins, no chrome, page flips.
            ReadingMode::Presentation => ReadingProfile {
                side_padding: 18,
                line_spacing: 1,
                paragraph_spacing: 2,
                show_sidebar: false,
                show_status: false,
                chapter_lock: false,
                paged: true,
            },
        };
        Some(p)
    }
}

/// How the library lists books: a metadata table, a dense table, or a cover grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibLayout {
    List,
    Compact,
    Grid,
}

impl LibLayout {
    pub fn from_label(s: &str) -> LibLayout {
        match s {
            "compact" => LibLayout::Compact,
            "grid" => LibLayout::Grid,
            _ => LibLayout::List,
        }
    }
}
cyclic_wrap!(LibLayout, [List => "list", Compact => "compact", Grid => "grid"]);

/// Cover-card size for the library grid view. Card dimensions are in terminal
/// cells, sized ~4:3 (cols:rows) so a typical 2:3 portrait cover fills the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridSize {
    Small,
    Medium,
    Large,
    XLarge,
}

impl GridSize {
    pub fn from_label(s: &str) -> GridSize {
        match s {
            "small" => GridSize::Small,
            "large" => GridSize::Large,
            "xlarge" => GridSize::XLarge,
            _ => GridSize::Medium,
        }
    }

    /// Cover-card width × height in cells (excludes the gutter and title rows).
    pub fn card(self) -> (u16, u16) {
        match self {
            GridSize::Small => (12, 9),
            GridSize::Medium => (16, 12),
            GridSize::Large => (22, 16),
            GridSize::XLarge => (30, 22),
        }
    }
}
cyclic_clamp!(GridSize, [Small => "small", Medium => "medium", Large => "large", XLarge => "xlarge"]);

/// How book images are adapted to the active theme. See `DESIGN.md` §7 and the
/// "Theming & content coherence" plan. The mode is part of the image cache key,
/// so changing it re-renders on the fly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageMode {
    /// Smart, per-content: recolour line-art/equations to the theme, keep
    /// pictures faithful (transparency flattened onto the page). The best general
    /// default.
    #[default]
    Auto,
    /// Auto, plus lightness-invert opaque light-background figures (charts,
    /// diagrams, screenshots) so they're dark-friendly with detail intact. True
    /// photos that happen to be light-backed invert too — the trade for comfort.
    InvertBackgrounds,
    /// Never recolour or invert; only flatten transparency onto the page so
    /// nothing is invisible. Original colours preserved (equations keep their ink
    /// colour, which may be faint on a dark theme).
    Faithful,
}

impl ImageMode {
    pub fn from_label(s: &str) -> ImageMode {
        match s {
            "invert" => ImageMode::InvertBackgrounds,
            "faithful" => ImageMode::Faithful,
            _ => ImageMode::Auto,
        }
    }
}
cyclic_wrap!(ImageMode, [Auto => "auto", InvertBackgrounds => "invert", Faithful => "faithful"]);

// `serde` `with`-modules for the persisted enums — each stores its `label()`
// string. `ReadingMode` is intentionally absent: it is a derived/UI-only state,
// never written to disk.
label_serde!(view_mode_serde, ViewMode);
label_serde!(lib_layout_serde, LibLayout);
label_serde!(grid_size_serde, GridSize);
label_serde!(image_mode_serde, ImageMode);

#[cfg(test)]
mod tests {
    use super::*;

    /// The library layout cycles through all modes and round-trips through its
    /// persisted label.
    #[test]
    fn lib_layout_cycles_and_round_trips() {
        let order = [LibLayout::List, LibLayout::Compact, LibLayout::Grid];
        // next() walks the order and wraps; prev() is its inverse.
        for (i, &l) in order.iter().enumerate() {
            assert_eq!(l.next(), order[(i + 1) % order.len()]);
            assert_eq!(l.next().prev(), l);
            // label ↔ from_label round-trips.
            assert_eq!(LibLayout::from_label(l.label()), l);
        }
    }

    /// Every persisted enum's `label` round-trips through `from_label`, and an
    /// unrecognised label falls back to the historical default — the contract the
    /// serde helpers rely on.
    #[test]
    fn persisted_enum_labels_round_trip_and_default() {
        assert_eq!(
            ViewMode::from_label(ViewMode::Center.label()),
            ViewMode::Center
        );
        assert_eq!(
            ViewMode::from_label(ViewMode::TwoPage.label()),
            ViewMode::TwoPage
        );
        assert_eq!(ViewMode::from_label("nonsense"), ViewMode::Center);

        for g in [
            GridSize::Small,
            GridSize::Medium,
            GridSize::Large,
            GridSize::XLarge,
        ] {
            assert_eq!(GridSize::from_label(g.label()), g);
        }
        assert_eq!(GridSize::from_label("nonsense"), GridSize::Medium);

        for m in [
            ImageMode::Auto,
            ImageMode::InvertBackgrounds,
            ImageMode::Faithful,
        ] {
            assert_eq!(ImageMode::from_label(m.label()), m);
        }
        assert_eq!(ImageMode::from_label("nonsense"), ImageMode::Auto);
    }

    #[test]
    fn reading_mode_cycles_through_presets_only() {
        // next() never lands on Custom; from Custom it enters the cycle.
        assert_eq!(ReadingMode::Custom.next(), ReadingMode::Study);
        assert_eq!(ReadingMode::Study.next(), ReadingMode::Research);
        assert_eq!(ReadingMode::Research.next(), ReadingMode::Presentation);
        assert_eq!(ReadingMode::Presentation.next(), ReadingMode::Study);
        assert_eq!(ReadingMode::Custom.prev(), ReadingMode::Presentation);
    }
}
