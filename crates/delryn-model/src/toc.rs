//! Table-of-contents and outline types.

/// A table-of-contents entry. Entries may nest (EPUB navpoints form a tree).
#[derive(Debug, Clone)]
pub struct TocEntry {
    pub label: String,
    /// Spine/section index this entry points at, if it could be resolved.
    pub section: Option<usize>,
    pub children: Vec<TocEntry>,
}

impl TocEntry {
    /// Append this entry's label and all of its descendants' labels, depth-first,
    /// to `out` — the flat list of chapter titles used for content-based matching.
    pub fn collect_labels(&self, out: &mut Vec<String>) {
        out.push(self.label.clone());
        for child in &self.children {
            child.collect_labels(out);
        }
    }
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
