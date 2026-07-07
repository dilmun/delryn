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
    /// Every real book format (excludes [`BookFormat::Unknown`]), in a sensible
    /// default keep-preference order. Used to enumerate formats for settings.
    pub const ALL: [BookFormat; 4] = [
        BookFormat::Epub,
        BookFormat::Pdf,
        BookFormat::Mobi,
        BookFormat::Azw3,
    ];

    /// The format whose [`label`](BookFormat::label) is `label` (e.g. "PDF"), if
    /// any — the inverse of `label()` for the real formats.
    pub fn from_label(label: &str) -> Option<BookFormat> {
        BookFormat::ALL.into_iter().find(|f| f.label() == label)
    }

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
    /// today. EPUB (reflowable text), PDF (page-as-image), and MOBI/AZW3
    /// (PalmDOC/KF8 reflowable text) are readable.
    pub fn is_readable(self) -> bool {
        matches!(
            self,
            BookFormat::Epub | BookFormat::Pdf | BookFormat::Mobi | BookFormat::Azw3
        )
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
    fn known_formats_are_readable_unknown_is_not() {
        for f in [
            BookFormat::Epub,
            BookFormat::Pdf,
            BookFormat::Mobi,
            BookFormat::Azw3,
        ] {
            assert!(f.is_readable(), "{f:?} should be readable");
        }
        assert!(!BookFormat::Unknown.is_readable());
    }
}
