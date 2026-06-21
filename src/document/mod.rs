//! Format-agnostic document model.
//!
//! Both EPUB (now) and PDF (later) implement [`Document`]; no layer above this
//! one knows which format is open. See `DESIGN.md` §3.

pub mod epub;
pub mod html;

use anyhow::Result;

/// Book-level metadata, format-independent.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
    pub identifier: Option<String>,
    /// Raw cover image bytes + mime type, if the book has a cover.
    pub cover: Option<(Vec<u8>, String)>,
    /// Source file size in bytes.
    pub size: u64,
}

impl Metadata {
    /// Authors joined for display, e.g. "Herman Melville".
    pub fn author_line(&self) -> String {
        self.authors.join(", ")
    }
}

/// A table-of-contents entry. Entries may nest (EPUB navpoints form a tree).
#[derive(Debug, Clone)]
pub struct TocEntry {
    pub label: String,
    /// Spine/section index this entry points at, if it could be resolved.
    pub section: Option<usize>,
    pub children: Vec<TocEntry>,
}

/// One spine item (chapter) as reflowable content, ready for the layout pass.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub index: usize,
    pub blocks: Vec<Block>,
}

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
    Heading { level: u8, spans: Vec<Span> },
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
    /// Horizontal rule.
    Rule,
    /// Vertical spacing between blocks.
    Blank,
}

/// A single navigable row for the sidebar outline. Flattened (with `depth`)
/// rather than a tree: top-level rows are sections, deeper rows are the
/// headings within them.
#[derive(Debug, Clone)]
pub struct OutlineItem {
    pub label: String,
    pub depth: usize,
    pub section: usize,
    /// Text to locate within the section when jumping; `None` = section top.
    pub locator: Option<String>,
}

/// Normalize a label for tolerant matching (lowercase, collapsed whitespace).
/// Shared by outline building and jump-target location.
pub fn normalize_label(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// A self-contained, `Send` handle that loads a section's blocks off the main
/// thread. Used to pre-wrap neighbouring chapters in the background so scrolling
/// across a chapter boundary doesn't block on parsing. It reopens its own
/// document handle, independent of the foreground `Document`.
pub trait SectionLoader: Send {
    fn load(&mut self, index: usize) -> Vec<Block>;
}

/// The interface the layout + view layers render against.
pub trait Document {
    fn metadata(&self) -> &Metadata;
    fn toc(&self) -> &[TocEntry];
    /// Flattened, navigable outline (sections + their headings).
    fn outline(&self) -> &[OutlineItem];
    /// A background loader for this document (opens its own handle).
    fn loader(&self) -> Box<dyn SectionLoader>;
    /// Number of ordered sections (spine length).
    fn section_count(&self) -> usize;
    /// Load and reflow-prepare one section's content.
    fn load_section(&mut self, index: usize) -> Result<Section>;
    /// Raw encoded bytes of the renderable images in a section (covers,
    /// figures), in reading order. Math/icon images are excluded.
    fn section_images(&mut self, _section: usize) -> Vec<Vec<u8>> {
        Vec::new()
    }
}
