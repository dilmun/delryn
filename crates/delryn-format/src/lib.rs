//! Format-agnostic document model.
//!
//! Both EPUB (now) and PDF (later) implement [`Document`]; no layer above this
//! one knows which format is open. See `DESIGN.md` §3.

pub mod epub;
pub mod epub_write;
pub mod format;
pub mod html;
pub mod mathml;
pub mod pdf;

use anyhow::Result;

pub use format::BookFormat;

// The format-independent content model now lives in `delryn-model`; re-exported
// here so existing `document::{Block, Metadata, …}` paths keep resolving.
pub use delryn_model::{
    Anchor, Block, CalloutKind, ImageWidth, Inline, Metadata, OutlineItem, Section, Span,
    TableCell, TocEntry,
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
    /// Spine index to open at on first read — the start of the body matter
    /// (skipping front matter) when the book declares it, else 0.
    fn start_section(&self) -> usize {
        0
    }
    /// Load and reflow-prepare one section's content.
    fn load_section(&mut self, index: usize) -> Result<Section>;
    /// Cross-reference / citation jump targets in section `index`: each element's
    /// `id` paired with a short text locator. Empty by default (formats without
    /// internal id anchors).
    fn section_targets(&mut self, _index: usize) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The spine index an `href` (relative to section `from`) points at, e.g. a
    /// cross-file `chapter5.xhtml#sec` reference. `None` if it doesn't resolve.
    fn section_for_href(&mut self, _from: usize, _href: &str) -> Option<usize> {
        None
    }
}
