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

/// A reflowable content block. The layout pass wraps these to the pane width.
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
    /// A figure/cover image. `data` holds the raw encoded bytes (filled by the
    /// format layer from `src`); empty if it couldn't be resolved.
    Image {
        src: String,
        alt: String,
        data: Vec<u8>,
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
