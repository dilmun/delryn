//! Toolchain registry: the per-toolchain detection **data** the classifier and
//! extractors read — never publisher names. Adding a publisher/toolchain means
//! adding tokens to a profile (or the shared vocab tables) *here*, not editing
//! detection logic across the extractors. See `docs/parsing.md`.
//!
//! This module is the single home for the detection token vocabularies:
//! the per-toolchain [`ToolchainProfile`] (code containers / line divs), the
//! math/marker/ToC class consts that `math.rs` and `dom.rs` read, and the unified
//! [`icon`](ICON_KEYWORDS) keyword table behind icon detection, inline glyphs and
//! aside-callout kinds.
//!
//! One [`ToolchainProfile`] (the `GENERIC` union) covers the surveyed library
//! today, so [`profile`] always returns it. Per-document routing (detect the
//! toolchain, pick a divergent profile, apply toolchain-specific strategies like
//! code line-reassembly) plugs in at `profile` when ≥2 toolchains genuinely
//! conflict — deliberately not built yet (no premature abstraction).

use super::CalloutKind;

/// Reusable detection data for a toolchain family.
pub(super) struct ToolchainProfile {
    /// Class tokens marking a styled code container that isn't a `<pre>`
    /// (Springer/Apress `ProgramCode`, LaTeX `lstlisting`, DocBook
    /// `programlisting`, Pandoc `sourceCode`, …).
    pub code_container_classes: &'static [&'static str],
    /// Class tokens wrapping each individual code line in its own element
    /// (Springer/Apress `FixedLine`) — one rendered line per such element.
    pub code_line_classes: &'static [&'static str],
}

/// The union profile — its fingerprints cover every publisher in the survey.
const GENERIC: ToolchainProfile = ToolchainProfile {
    code_container_classes: &[
        "ProgramCode",
        "SourceCode",
        "CodeBlock",
        "code",
        "sourceCode",
        // LaTeX `listings` package + DocBook program listings.
        "lstlisting",
        "listing",
        "programlisting",
    ],
    code_line_classes: &["FixedLine"],
};

/// The active profile for the document being parsed. The seam for future
/// per-document toolchain routing; the union profile for now.
pub(super) fn profile() -> &'static ToolchainProfile {
    &GENERIC
}

// ── Math class vocabulary (read by `math.rs`) ────────────────────────────────

/// Class-name substrings (matched anywhere, case-insensitively) that mark an
/// element as math content — InDesign `…MathTools…Math_…`, MathJax/MathML
/// wrappers, generic `math`/`equation` classes. Publisher-agnostic.
pub(super) const MATH_CLASS_KEYWORDS: &[&str] = &["math", "equation"];

/// Class substrings (matched anywhere, case-insensitively) marking a math
/// container as **inline** — checked first, so `InlineEquation` (Springer),
/// `math inline` (Pandoc), `inline-formula` (JATS) aren't read as display.
pub(super) const INLINE_MATH_CLASS_KEYWORDS: &[&str] = &["inline"];

/// Class substrings marking a math container as **display** (block): an equation /
/// formula wrapper — Springer `Equation`/`EquationContent`, Pandoc `math display`,
/// JATS `disp-formula`. Consulted only after [`INLINE_MATH_CLASS_KEYWORDS`].
pub(super) const DISPLAY_MATH_CLASS_KEYWORDS: &[&str] = &["disp", "display", "equation", "formula"];

// ── Print-chrome / ToC class vocabularies (read by `dom.rs`) ──────────────────

/// Class tokens (matched exactly after splitting on space/`-`/`_`) marking
/// regenerated print/marker chrome the reflowable reader drops: a list-item
/// number or a footnote backref we render ourselves.
pub(super) const MARKER_CHROME_TOKENS: &[&str] = &["itemnumber", "footnotenumber", "footnotemark"];

/// Class substring (matched anywhere) marking a print page number, which means
/// nothing without fixed pages — dropped like code line numbers.
pub(super) const PAGE_NUMBER_KEYWORD: &str = "pagenumber";

/// Printed-ToC level classes that sit at depth 0 (matched exactly, lowercased).
pub(super) const TOC_LEVEL0_TOKENS: &[&str] = &["tocchapter", "tocpart", "tocfrontmatter"];

/// Prefix of a printed-ToC class whose trailing number is its nesting depth
/// (`tocsection2` ⇒ depth 2).
pub(super) const TOC_SECTION_PREFIX: &str = "tocsection";

// ── Icon keyword vocabulary (read by `inline.rs` + `callout.rs`) ─────────────

/// One entry in the shared icon vocabulary, mapping a keyword (seen in an image
/// `src`/`alt`) to what it means across the three icon concerns.
struct IconKeyword {
    /// The lowercase keyword to match as a substring.
    keyword: &'static str,
    /// Whether, in an `<img src>`, it marks the image as a small UI icon (kept
    /// inline as a glyph rather than rendered as a block figure).
    src_marker: bool,
    /// The themed, single-width Unicode glyph it renders as inline (`None` ⇒ not
    /// glyph-mapped).
    glyph: Option<char>,
    /// The callout kind an aside laid out with this icon implies (`None` ⇒ the
    /// default, [`CalloutKind::Note`]).
    kind: Option<CalloutKind>,
}

/// The single source of truth for icon keywords. **Order is significant**: the
/// first matching entry wins, so the relative order encodes the original
/// per-concern priority (e.g. `warning` before `tip` for kinds; `check` before
/// `note` for glyphs).
const ICON_KEYWORDS: &[IconKeyword] = &[
    icon("check", false, Some('✓'), None),
    icon("tick", false, Some('✓'), None),
    icon("warning", true, Some('△'), Some(CalloutKind::Warning)),
    icon("caution", false, Some('△'), Some(CalloutKind::Warning)),
    icon("danger", false, Some('△'), Some(CalloutKind::Warning)),
    icon("key", true, None, Some(CalloutKind::Important)),
    icon("important", false, None, Some(CalloutKind::Important)),
    icon("tip", true, Some('✲'), Some(CalloutKind::Tip)),
    icon("hint", false, Some('✲'), Some(CalloutKind::Tip)),
    icon("remember", false, Some('⚑'), None),
    icon("technical", false, Some('※'), None),
    icon("geek", false, Some('※'), None),
    icon("nerd", false, Some('※'), None),
    icon("note", true, Some('ⓘ'), None),
    icon("info", true, Some('ⓘ'), None),
    icon("pencil", true, None, None),
    icon("question", true, None, None),
    icon("icon", true, None, None),
    icon("leanpub_", true, None, None),
];

/// Terse constructor so the table above reads as a data block.
const fn icon(
    keyword: &'static str,
    src_marker: bool,
    glyph: Option<char>,
    kind: Option<CalloutKind>,
) -> IconKeyword {
    IconKeyword {
        keyword,
        src_marker,
        glyph,
        kind,
    }
}

/// Whether an image `src` names a small UI icon (so it stays inline as a glyph).
pub(super) fn is_icon_src(src: &str) -> bool {
    let s = src.to_lowercase();
    ICON_KEYWORDS
        .iter()
        .filter(|k| k.src_marker)
        .any(|k| s.contains(k.keyword))
}

/// Map a small inline UI icon (by its `alt`/`src`) to a themed, single-width
/// Unicode glyph — so list checks and admonition markers (Tip / Warning /
/// Remember / Technical Stuff …) render as a symbol rather than `[tip]` text.
/// Text-presentation code points only (no colour emoji). `None` for non-icons.
pub(super) fn icon_glyph(alt: &str, src: &str) -> Option<char> {
    let key = format!("{} {}", alt, src.rsplit('/').next().unwrap_or(src)).to_ascii_lowercase();
    ICON_KEYWORDS
        .iter()
        .find(|k| k.glyph.is_some() && key.contains(k.keyword))
        .and_then(|k| k.glyph)
}

/// Map an aside icon's `src` filename to a callout kind (info/pencil/question and
/// anything unrecognised fall back to Note).
pub(super) fn aside_kind_from_icon(src: &str) -> CalloutKind {
    let s = src.to_lowercase();
    ICON_KEYWORDS
        .iter()
        .find(|k| k.kind.is_some() && s.contains(k.keyword))
        .and_then(|k| k.kind)
        .unwrap_or(CalloutKind::Note)
}
