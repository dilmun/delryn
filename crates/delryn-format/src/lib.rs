//! Format-agnostic document model.
//!
//! Both EPUB (now) and PDF (later) implement [`Document`]; no layer above this
//! one knows which format is open. See `DESIGN.md` §3.

pub mod epub;
pub mod epub_write;
pub mod html;
pub mod mathml;

use anyhow::Result;

// The format-independent content model now lives in `delryn-model`; re-exported
// here so existing `document::{Block, Metadata, …}` paths keep resolving.
pub use delryn_model::{
    Anchor, Block, CalloutKind, Inline, Metadata, OutlineItem, Section, Span, TableCell, TocEntry,
};

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

/// A book file format, recognized by extension. The single place the rest of the
/// app asks "what kind of file is this?" — the scanner uses it to decide what to
/// index, and the reader uses it to dispatch to the right [`Document`] backend
/// (or report cleanly that a format isn't readable yet). See `DESIGN.md` §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookFormat {
    Epub,
    Pdf,
    Mobi,
    Azw3,
    /// Anything we don't recognize as a book.
    Unknown,
}

impl BookFormat {
    /// Classify a path by its file extension (case-insensitive).
    pub fn from_path(path: &(impl AsRef<std::path::Path> + ?Sized)) -> BookFormat {
        let ext = path
            .as_ref()
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("epub") => BookFormat::Epub,
            Some("pdf") => BookFormat::Pdf,
            // Old and new Mobipocket containers.
            Some("mobi" | "prc") => BookFormat::Mobi,
            // Kindle KF8 family.
            Some("azw3" | "azw" | "kf8") => BookFormat::Azw3,
            _ => BookFormat::Unknown,
        }
    }

    /// Whether a [`Document`] backend exists to actually open this format today.
    /// Only EPUB is readable for now; the others are recognized but not yet
    /// parseable (see the Phase 5 plan in `TODO.md`).
    pub fn is_readable(self) -> bool {
        matches!(self, BookFormat::Epub)
    }

    /// A short human label, for status messages and badges.
    pub fn label(self) -> &'static str {
        match self {
            BookFormat::Epub => "EPUB",
            BookFormat::Pdf => "PDF",
            BookFormat::Mobi => "MOBI",
            BookFormat::Azw3 => "AZW3",
            BookFormat::Unknown => "this file type",
        }
    }
}

#[cfg(test)]
mod format_tests {
    use super::BookFormat;

    #[test]
    fn classifies_by_extension_case_insensitively() {
        assert_eq!(BookFormat::from_path("a/b/book.epub"), BookFormat::Epub);
        assert_eq!(BookFormat::from_path("BOOK.EPUB"), BookFormat::Epub);
        assert_eq!(BookFormat::from_path("paper.pdf"), BookFormat::Pdf);
        assert_eq!(BookFormat::from_path("old.mobi"), BookFormat::Mobi);
        assert_eq!(BookFormat::from_path("x.prc"), BookFormat::Mobi);
        assert_eq!(BookFormat::from_path("k.azw3"), BookFormat::Azw3);
        assert_eq!(BookFormat::from_path("k.azw"), BookFormat::Azw3);
        assert_eq!(BookFormat::from_path("notes.txt"), BookFormat::Unknown);
        assert_eq!(BookFormat::from_path("noext"), BookFormat::Unknown);
    }

    #[test]
    fn only_epub_is_readable_today() {
        assert!(BookFormat::Epub.is_readable());
        for f in [BookFormat::Pdf, BookFormat::Mobi, BookFormat::Azw3] {
            assert!(!f.is_readable(), "{f:?} should not be readable yet");
        }
    }
}
