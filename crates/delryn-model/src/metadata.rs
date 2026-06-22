//! Book-level metadata, format-independent.

/// Book-level metadata, format-independent.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub title: String,
    /// Subtitle, if declared (rare in EPUB; usually filled online or by hand).
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
    pub identifier: Option<String>,
    /// Series this book belongs to (e.g. "Foundation"), if declared.
    pub series: Option<String>,
    /// Position within the series (e.g. `2.0`), if declared.
    pub series_index: Option<f32>,
    /// Publisher, if declared.
    pub publisher: Option<String>,
    /// Raw cover image bytes + mime type, if the book has a cover.
    pub cover: Option<(Vec<u8>, String)>,
    /// Source file size in bytes.
    pub size: u64,
    /// True when the EPUB looks converted/repackaged (e.g. by calibre) rather
    /// than an original publisher file. Heuristic; see `epub::detect_converted`.
    pub converted: bool,
}

impl Metadata {
    /// Authors joined for display, e.g. "Herman Melville".
    pub fn author_line(&self) -> String {
        self.authors.join(", ")
    }
}
