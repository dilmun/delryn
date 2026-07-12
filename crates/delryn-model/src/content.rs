//! Reflowable content model: the blocks and inline runs every format produces
//! and every renderer consumes.

/// Inline styling applied to a run of text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inline {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
    /// The run is part of a mathematical expression (the source tagged it with a
    /// math class/element). Not a visual style — it scopes math-only text fixups
    /// (e.g. exponent super-scripting) so prose is never touched.
    pub math: bool,
}

/// A navigable target attached to an inline run, so the reader can jump from a
/// reference to where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// Hyperlink — an internal `#id` or an external URL.
    Link(String),
    /// Footnote reference; holds the target anchor id, matched against a
    /// [`Block::Footnote`]'s `id` (see [`Block::footnote_matches`]).
    Footnote(String),
    /// Cross-reference to an internal target id/locator ("see Chapter 3").
    CrossRef(String),
    /// Citation key into a bibliography.
    Citation(String),
}

/// A run of text with uniform inline styling and an optional navigation anchor.
#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub style: Inline,
    /// A navigable target (link / footnote / cross-ref / citation), if any.
    pub anchor: Option<Anchor>,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Span {
        Span {
            text: text.into(),
            style: Inline::default(),
            anchor: None,
        }
    }

    /// A styled run carrying a navigation [`Anchor`].
    pub fn anchored(text: impl Into<String>, style: Inline, anchor: Anchor) -> Span {
        Span {
            text: text.into(),
            style,
            anchor: Some(anchor),
        }
    }
}

/// One table cell: styled inline content (may be empty).
pub type TableCell = Vec<Span>;

/// Kind of admonition / callout, carrying its canonical label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl CalloutKind {
    /// The canonical uppercase label shown in the callout header.
    pub fn label(self) -> &'static str {
        match self {
            CalloutKind::Note => "NOTE",
            CalloutKind::Tip => "TIP",
            CalloutKind::Important => "IMPORTANT",
            CalloutKind::Warning => "WARNING",
            CalloutKind::Caution => "CAUTION",
        }
    }

    /// A monochrome, single-width Unicode glyph for the callout header, in place
    /// of a raster icon. Chosen from text-presentation code points (no emoji
    /// variants), so terminals render them tinted by the theme rather than as
    /// wide colour emoji: outline triangle for warning, filled for the harsher
    /// caution.
    pub fn glyph(self) -> char {
        match self {
            CalloutKind::Note => 'ⓘ',
            CalloutKind::Tip => '✲',
            CalloutKind::Important => '◆',
            CalloutKind::Warning => '△',
            CalloutKind::Caution => '▲',
        }
    }

    /// Classify a leading keyword (case-insensitive, punctuation-tolerant) into a
    /// callout kind; `None` when the word isn't a recognised admonition.
    pub fn from_word(word: &str) -> Option<CalloutKind> {
        let w = word
            .trim()
            .trim_matches(['[', ']', ':', '!'])
            .to_ascii_lowercase();
        match w.as_str() {
            "note" => Some(CalloutKind::Note),
            "tip" | "hint" => Some(CalloutKind::Tip),
            "important" => Some(CalloutKind::Important),
            "warning" => Some(CalloutKind::Warning),
            "caution" | "danger" => Some(CalloutKind::Caution),
            _ => None,
        }
    }
}

/// The authored display width of an image, as recovered from the `<img>` markup.
/// Reflowable EPUBs express the *intended* size in CSS/HTML (rarely matching the
/// file's pixel resolution), so renderers size figures from this rather than from
/// raw pixels — falling back to a normalized default when it's [`ImageWidth::Auto`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ImageWidth {
    /// No authored size — the renderer applies its default normalization.
    #[default]
    Auto,
    /// A fraction of the containing column (from a CSS/HTML percentage), 0.0–1.0.
    Pct(f32),
    /// An absolute width in CSS pixels (from a px `width` attribute / CSS px).
    Px(u32),
    /// Full-bleed: fill the display pane, preserving aspect. For page-as-image
    /// formats (PDF), where each "image" is a whole page rather than an inline
    /// figure sized to a fraction of the column.
    Full,
}

/// Ink geometry of an equation image, measured once off-thread from its pixels
/// (mirrors `delryn_media::InkProfile`; kept here — with no image dependency — so the
/// content model stays a leaf crate). Lets the reader size a publisher equation
/// picture relative to the text by its measured ink-line height (DPI-independent),
/// instead of trusting the file's raw pixel resolution. `None` on a [`Block::Image`]
/// means "not a profiled equation" — a figure, a photo, or rendered LaTeX math.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkProfile {
    /// Tight ink bounding box in source pixels (1px margin), `[x0,x1) × [y0,y1)`.
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    /// Median height (px) of one equation line — the raster's measured "em".
    pub line_px: f32,
    /// Ink-line count (≈ rows of a multi-line array); 1 for a single equation.
    pub line_count: u16,
}

/// A reflowable content block. The layout pass wraps these to the pane width.
///
/// Beyond the basic prose blocks, the model carries *technical* content —
/// fenced code, display math, tables, admonition callouts, captioned figures,
/// and footnote definitions — so renderers can present each with intent rather
/// than flattening everything to paragraphs.
#[derive(Debug, Clone)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    /// A paragraph; may be a list item (`marker`), nested (`indent`), or quoted.
    Para {
        spans: Vec<Span>,
        indent: u8,
        quote: bool,
        marker: Option<String>,
    },
    /// Preformatted / code block; lines are preserved verbatim (no wrap).
    Code {
        lang: Option<String>,
        lines: Vec<String>,
    },
    /// Display (block-level) mathematics. `unicode` is the always-present Unicode
    /// approximation the layout pass centres (see [`crate::math`]); `latex` is the
    /// original LaTeX source when the parser could recover it (`alttext` /
    /// `<annotation …tex>`), for the graphical-math renderer to rasterise — `None`
    /// when only presentation MathML was available (Unicode only).
    Math {
        unicode: String,
        latex: Option<String>,
    },
    /// A table: an optional header row, then body rows. Each cell is styled spans.
    Table {
        header: Option<Vec<TableCell>>,
        rows: Vec<Vec<TableCell>>,
    },
    /// An admonition / callout (NOTE, TIP, WARNING, …) wrapping inner blocks.
    Callout {
        kind: CalloutKind,
        /// Custom title; the kind's [`label`](CalloutKind::label) is used when `None`.
        title: Option<String>,
        blocks: Vec<Block>,
    },
    /// A footnote/endnote definition. `id` is the raw source anchor id (the
    /// match key a reference's [`Anchor::Footnote`] points at); `label` is the
    /// number shown at the foot of the section. Collected for jump/return.
    Footnote {
        id: String,
        label: String,
        blocks: Vec<Block>,
    },
    /// A figure/cover image. `data` holds the raw encoded bytes (filled by the
    /// format layer from `src`); empty if it couldn't be resolved. `caption` is
    /// the figure caption, empty when there is none. `math` marks an image that
    /// is really display math (an equation rendered as a picture, alt = Unicode
    /// fallback) rather than a content figure — so the figure viewer can skip it.
    /// `width` is the authored display size (from the `<img>` width / CSS), used
    /// to size the figure faithfully; [`ImageWidth::Auto`] when unspecified.
    /// `ink` is the measured equation ink profile (see [`InkProfile`]) when this
    /// image is a publisher equation picture, filled once off-thread; `None` for
    /// figures, photos, and rendered LaTeX math (which is sized by its render em).
    Image {
        src: String,
        alt: String,
        data: Vec<u8>,
        caption: Vec<Span>,
        math: bool,
        width: ImageWidth,
        ink: Option<InkProfile>,
    },
    /// Horizontal rule.
    Rule,
    /// Vertical spacing between blocks.
    Blank,
}

impl Block {
    /// Whether this is the footnote definition a reference's [`Anchor::Footnote`]
    /// `target` points at. Matches the raw `id` first (the canonical, unique
    /// document anchor); falls back to comparing digit-only forms so a
    /// `noteref href="#fn7"` still resolves to an `id="footnote-7"` definition
    /// when a publisher uses different id conventions for the ref and the def.
    pub fn footnote_matches(&self, target: &str) -> bool {
        let Block::Footnote { id, .. } = self else {
            return false;
        };
        let target = target.trim_start_matches('#');
        if id == target {
            return true;
        }
        let id_digits = digits(id);
        !id_digits.is_empty() && id_digits == digits(target)
    }
}

/// Find the footnote definition a reference points at, among `blocks`. Scans the
/// top level — which is where the parser emits definitions (as siblings, even
/// when grouped under a `<section epub:type="footnotes">`). Cross-section
/// resolution (endnotes in a later file) is composed by the caller, scanning
/// each section's blocks in turn.
pub fn find_footnote<'a>(blocks: &'a [Block], target: &str) -> Option<&'a Block> {
    blocks.iter().find(|b| b.footnote_matches(target))
}

/// The ASCII digits of `s`, in order (e.g. `"footnote-7a"` → `"7"`).
fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// One spine item (chapter) as reflowable content, ready for the layout pass.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub index: usize,
    pub blocks: Vec<Block>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callout_kind_classifies_keywords() {
        assert_eq!(CalloutKind::from_word("NOTE"), Some(CalloutKind::Note));
        assert_eq!(CalloutKind::from_word("tip"), Some(CalloutKind::Tip));
        assert_eq!(CalloutKind::from_word("Hint"), Some(CalloutKind::Tip));
        // Tolerates surrounding punctuation from markdown-style markers.
        assert_eq!(
            CalloutKind::from_word("[!WARNING]"),
            Some(CalloutKind::Warning)
        );
        assert_eq!(
            CalloutKind::from_word("danger:"),
            Some(CalloutKind::Caution)
        );
        assert_eq!(CalloutKind::from_word("paragraph"), None);
    }

    #[test]
    fn callout_kind_labels_are_uppercase() {
        assert_eq!(CalloutKind::Important.label(), "IMPORTANT");
        assert_eq!(CalloutKind::Caution.label(), "CAUTION");
    }

    fn note(id: &str) -> Block {
        Block::Footnote {
            id: id.to_string(),
            label: "1".to_string(),
            blocks: Vec::new(),
        }
    }

    #[test]
    fn footnote_matches_by_exact_id_then_digits() {
        let n = note("fn7");
        // Exact id (with or without a leading '#').
        assert!(n.footnote_matches("fn7"));
        assert!(n.footnote_matches("#fn7"));
        // Digit-normalized fallback across differing id conventions.
        assert!(n.footnote_matches("footnote-7"));
        // Different number must not match.
        assert!(!n.footnote_matches("fn8"));
        // A non-footnote block never matches.
        assert!(!Block::Rule.footnote_matches("fn7"));
    }

    #[test]
    fn footnote_digit_fallback_needs_digits_on_both_sides() {
        // No digits in the id → only an exact-id match, never a loose one.
        let n = note("note-intro");
        assert!(n.footnote_matches("note-intro"));
        assert!(!n.footnote_matches("note-summary"));
    }

    #[test]
    fn find_footnote_picks_the_matching_definition() {
        let blocks = vec![note("fn1"), Block::Rule, note("fn2")];
        let found = find_footnote(&blocks, "#fn2").expect("a match");
        assert!(matches!(found, Block::Footnote { id, .. } if id == "fn2"));
        assert!(find_footnote(&blocks, "fn3").is_none());
    }
}
