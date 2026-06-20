//! Format-agnostic document model.
//!
//! Both EPUB (now) and PDF (later) implement [`Document`]; no layer above this
//! one knows which format is open. See `DESIGN.md` §3.

pub mod epub;

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

/// A reflowable content block. The layout pass wraps these to the pane width.
#[derive(Debug, Clone)]
pub enum Block {
    Heading(String),
    Paragraph(String),
    /// Vertical spacing between blocks.
    Blank,
}

/// The interface the layout + view layers render against.
pub trait Document {
    fn metadata(&self) -> &Metadata;
    fn toc(&self) -> &[TocEntry];
    /// Number of ordered sections (spine length).
    fn section_count(&self) -> usize;
    /// Load and reflow-prepare one section's content.
    fn load_section(&mut self, index: usize) -> Result<Section>;
}
