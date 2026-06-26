//! Book file-format recognition by extension.

/// A book file format, recognized by extension. The single place the rest of the
/// app asks "what kind of file is this?" — the scanner uses it to decide what to
/// index, and the reader uses it to dispatch to the right [`crate::Document`]
/// backend (or report cleanly that a format isn't readable yet). See
/// `DESIGN.md` §3.
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

    /// Whether a [`crate::Document`] backend exists to actually open this format
    /// today. EPUB (reflowable text) and PDF (page-as-image) are readable;
    /// MOBI/AZW3 are recognized but not yet parseable (see the Phase 5 plan in
    /// `TODO.md`).
    pub fn is_readable(self) -> bool {
        matches!(self, BookFormat::Epub | BookFormat::Pdf)
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
mod tests {
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
    fn epub_and_pdf_are_readable() {
        assert!(BookFormat::Epub.is_readable());
        assert!(BookFormat::Pdf.is_readable());
        for f in [BookFormat::Mobi, BookFormat::Azw3] {
            assert!(!f.is_readable(), "{f:?} should not be readable yet");
        }
    }
}
