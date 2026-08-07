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
///
/// `Default` mirrors [`Config::default`](super::Config::default) exactly, so a
/// fresh install names the state it shipped in rather than reporting `Custom`,
/// and cycling presets always has a way back to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingMode {
    Custom,
    Default,
    Study,
    Research,
    Presentation,
}

/// The reading settings a preset bundles. Every preset fixes all of these, so a
/// live config can be compared field-for-field to recognise the active preset.
///
/// Deliberately excludes the choices a preset has no business taking from the
/// reader: `view_mode` (one column or two) and `paged` (reflow or page-flips)
/// are how someone likes to read, not what they're reading for.
///
/// Chrome is excluded too, and handled by [`ReadingMode::hides_chrome`] instead
/// — see there for why it can't live in the comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadingProfile {
    pub(crate) side_padding: u16,
    pub(crate) max_measure: u16,
    pub(crate) line_spacing: u8,
    pub(crate) paragraph_spacing: u8,
    pub(crate) justify: bool,
    pub(crate) continuous: bool,
    pub(crate) chapter_lock: bool,
}

impl ReadingMode {
    pub fn label(self) -> &'static str {
        match self {
            ReadingMode::Custom => "custom",
            ReadingMode::Default => "default",
            ReadingMode::Study => "study",
            ReadingMode::Research => "research",
            ReadingMode::Presentation => "presentation",
        }
    }

    /// Next/previous *applyable* preset. Cycles through the four real presets;
    /// from `Custom` it enters the cycle rather than landing back on `Custom`.
    pub fn next(self) -> Self {
        match self {
            ReadingMode::Custom | ReadingMode::Presentation => ReadingMode::Default,
            ReadingMode::Default => ReadingMode::Study,
            ReadingMode::Study => ReadingMode::Research,
            ReadingMode::Research => ReadingMode::Presentation,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ReadingMode::Custom | ReadingMode::Default => ReadingMode::Presentation,
            ReadingMode::Study => ReadingMode::Default,
            ReadingMode::Research => ReadingMode::Study,
            ReadingMode::Presentation => ReadingMode::Research,
        }
    }

    /// Whether the preset reads with the chrome hidden — applied through the
    /// transient `focus_mode`, so the reader's saved `show_sidebar`/`show_status`
    /// preferences survive a preset that strips the window bare.
    ///
    /// Kept out of [`ReadingProfile`] deliberately. `focus_mode` is never
    /// persisted, so if it decided *which* preset the live settings are, the label
    /// would drop to `Custom` the moment someone toggled focus, and again on the
    /// next restart when the flag came back false under presentation's spacing.
    /// A preset applies chrome; it isn't identified by it.
    pub(crate) fn hides_chrome(self) -> bool {
        matches!(self, ReadingMode::Presentation)
    }

    /// The settings this preset stands for (`None` for `Custom`).
    pub(crate) fn profile(self) -> Option<ReadingProfile> {
        let p = match self {
            ReadingMode::Custom => return None,
            // What the app ships as: one column at a capped measure, generous
            // margins, chrome present. Must stay field-for-field identical to
            // `Config::default()` — a test holds the two together.
            ReadingMode::Default => ReadingProfile {
                side_padding: 10,
                max_measure: 72,
                line_spacing: 0,
                paragraph_spacing: 1,
                justify: false,
                continuous: true,
                chapter_lock: false,
            },
            // Deep, careful reading of one chapter: a book's measure, open line
            // spacing, and stay put in the chapter. Not justified — hyphenation
            // already tightens the right edge, and the only thing justification
            // adds on top of it is wider spaces between the words.
            ReadingMode::Study => ReadingProfile {
                side_padding: 12,
                max_measure: 66,
                line_spacing: 1,
                paragraph_spacing: 1,
                justify: false,
                continuous: false,
                chapter_lock: true,
            },
            // Scanning / cross-referencing across the whole book: as much text on
            // screen as the window allows, scrolling freely between chapters.
            ReadingMode::Research => ReadingProfile {
                side_padding: 4,
                max_measure: 0,
                line_spacing: 0,
                paragraph_spacing: 1,
                justify: false,
                continuous: true,
                chapter_lock: false,
            },
            // Distraction-free: wide airy margins, a short measure, no chrome.
            ReadingMode::Presentation => ReadingProfile {
                side_padding: 18,
                max_measure: 60,
                line_spacing: 1,
                paragraph_spacing: 2,
                justify: false,
                continuous: false,
                chapter_lock: false,
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
    /// pictures faithful (transparency flattened onto the page).
    Auto,
    /// Auto, plus lightness-invert opaque light-background figures (charts,
    /// diagrams, screenshots) so they're dark-friendly with detail intact. True
    /// photos that happen to be light-backed invert too — the trade for comfort,
    /// and the reason this is the default: a white-backed chart dropped into a
    /// dark page is the thing readers actually notice.
    #[default]
    InvertBackgrounds,
    /// Never recolour or invert; only flatten transparency onto the page so
    /// nothing is invisible. Original colours preserved (equations keep their ink
    /// colour, which may be faint on a dark theme).
    Faithful,
}

impl ImageMode {
    pub fn from_label(s: &str) -> ImageMode {
        match s {
            "auto" => ImageMode::Auto,
            "faithful" => ImageMode::Faithful,
            // Including "invert" — an unreadable value falls back to the default
            // rather than to a particular mode, so there is one thing to change.
            _ => ImageMode::default(),
        }
    }
}
cyclic_wrap!(ImageMode, [Auto => "auto", InvertBackgrounds => "invert", Faithful => "faithful"]);

/// Whether book figures are normalized to a consistent display size or shown at
/// the publisher's authored size. Orthogonal to [`ImageMode`] (which is about
/// *colour*, not size) and part of the image cache key, so switching it
/// re-renders on the fly. See `DESIGN.md` §7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ImageFit {
    /// Normalize every figure/table to a consistent share of the text column
    /// (`image_width_pct`) — enlarging low-res figures and shrinking oversized
    /// ones — so figures look the same across books regardless of the publisher's
    /// authored width or the file's pixel resolution (both are unreliable).
    /// Equation-shaped graphics stay text-proportional (never stretched to the
    /// column). The best general default.
    #[default]
    Fit,
    /// Honor the publisher's authored width (or, failing that, the file's pixel
    /// resolution) exactly, only shrinking to fit the column — publisher intent,
    /// warts and all.
    Faithful,
}

impl ImageFit {
    pub fn from_label(s: &str) -> ImageFit {
        match s {
            "faithful" => ImageFit::Faithful,
            _ => ImageFit::Fit,
        }
    }
}
cyclic_wrap!(ImageFit, [Fit => "fit", Faithful => "faithful"]);

/// Reading direction for paged (PDF / comic) spreads: left-to-right (default) or
/// right-to-left (manga / manhua). In RTL a two-page spread swaps the facing pages
/// so they read right-to-left; page turns still advance forward through the book.
/// Reflowable text is unaffected (it always reads left-to-right).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadingDirection {
    #[default]
    Ltr,
    Rtl,
}

impl ReadingDirection {
    pub fn from_label(s: &str) -> ReadingDirection {
        match s {
            "rtl" => ReadingDirection::Rtl,
            _ => ReadingDirection::Ltr,
        }
    }
    /// Whether pages read right-to-left (manga).
    pub fn is_rtl(self) -> bool {
        matches!(self, ReadingDirection::Rtl)
    }
}
cyclic_wrap!(ReadingDirection, [Ltr => "ltr", Rtl => "rtl"]);

// `serde` `with`-modules for the persisted enums — each stores its `label()`
// string. `ReadingMode` is intentionally absent: it is a derived/UI-only state,
// never written to disk.
label_serde!(view_mode_serde, ViewMode);
label_serde!(lib_layout_serde, LibLayout);
label_serde!(grid_size_serde, GridSize);
label_serde!(image_mode_serde, ImageMode);
label_serde!(image_fit_serde, ImageFit);
label_serde!(reading_direction_serde, ReadingDirection);

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
        assert_eq!(ImageMode::from_label("nonsense"), ImageMode::default());
    }

    #[test]
    fn reading_mode_cycles_through_presets_only() {
        // next() never lands on Custom; from Custom it enters the cycle.
        assert_eq!(ReadingMode::Custom.next(), ReadingMode::Default);
        assert_eq!(ReadingMode::Default.next(), ReadingMode::Study);
        assert_eq!(ReadingMode::Study.next(), ReadingMode::Research);
        assert_eq!(ReadingMode::Research.next(), ReadingMode::Presentation);
        assert_eq!(ReadingMode::Presentation.next(), ReadingMode::Default);
        assert_eq!(ReadingMode::Custom.prev(), ReadingMode::Presentation);

        // Every real preset is reachable both ways, and the cycle is a true ring.
        let presets = [
            ReadingMode::Default,
            ReadingMode::Study,
            ReadingMode::Research,
            ReadingMode::Presentation,
        ];
        for m in presets {
            assert_eq!(m.next().prev(), m, "{} round-trips", m.label());
            assert!(m.profile().is_some(), "{} is applyable", m.label());
        }
        let mut seen = ReadingMode::Default;
        for _ in 0..presets.len() {
            seen = seen.next();
        }
        assert_eq!(seen, ReadingMode::Default, "the cycle closes");
    }
}
