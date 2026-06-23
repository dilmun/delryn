//! Table-of-contents and outline types.

/// A table-of-contents entry. Entries may nest (EPUB navpoints form a tree).
#[derive(Debug, Clone)]
pub struct TocEntry {
    pub label: String,
    /// Spine/section index this entry points at, if it could be resolved.
    pub section: Option<usize>,
    pub children: Vec<TocEntry>,
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
