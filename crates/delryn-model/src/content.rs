//! Reflowable content model: the blocks and inline runs every format produces
//! and every renderer consumes.

/// Inline styling applied to a run of text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inline {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
}

/// A run of text with uniform inline styling.
#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub style: Inline,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Span {
        Span {
            text: text.into(),
            style: Inline::default(),
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
    /// Display (block-level) mathematics. `tex` is TeX-like source; the layout
    /// pass renders it to Unicode (see [`crate::math`]).
    Math {
        tex: String,
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
    /// A footnote definition, anchored by `label`; collected for jump/return and
    /// foot-of-section rendering.
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
    /// A figure/cover image. `data` holds the raw encoded bytes (filled by the
    /// format layer from `src`); empty if it couldn't be resolved. `caption` is
    /// the figure caption, empty when there is none.
    Image {
        src: String,
        alt: String,
        data: Vec<u8>,
        caption: Vec<Span>,
    },
    /// Horizontal rule.
    Rule,
    /// Vertical spacing between blocks.
    Blank,
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
}
