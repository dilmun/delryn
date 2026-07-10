//! Cross-format provenance heuristics: was a file produced by a conversion /
//! repackaging tool (calibre, pandoc, …) rather than an original publisher file?
//!
//! Shared by every backend (EPUB reads it from the OPF `generator`/`contributor`;
//! MOBI from the EXTH `contributor` record) so a new tool is one data entry here,
//! not an edit per format.

/// Substrings that name a format-conversion / repackaging tool in a file's
/// generator / contributor / producer metadata.
const CONVERTERS: [&str; 11] = [
    "calibre",
    "pandoc",
    "ebook-convert",
    "aspose",
    "kindlegen",
    "mobi",
    "abbyy",
    "able2extract",
    "ghostscript",
    "wkhtmltopdf",
    "pdftoepub",
];

/// Whether `s` (a generator / contributor / producer string) names a known
/// conversion tool — the signal that a file is a repackaged conversion rather
/// than an original publisher file.
pub(crate) fn names_converter_tool(s: &str) -> bool {
    let s = s.to_lowercase();
    CONVERTERS.iter().any(|t| s.contains(t))
}

/// Foreign document / e-book formats a file may declare as its *source* — the
/// Amazon MOBI/AZW `Source` record (EXTH 112) names the format the book was built
/// from (e.g. `docx` for a Kindle Create / Word conversion, `epub` for a
/// kindlegen conversion). A native Kindle file declares none of these.
const SOURCE_FORMATS: [&str; 11] = [
    "docx", "doc", "epub", "xhtml", "html", "htm", "odt", "rtf", "fb2", "txt", "pdf",
];

/// Whether `s` names a foreign source format the file was converted from — a
/// per-format provenance signal (currently the MOBI/AZW EXTH `Source` record).
pub(crate) fn names_converted_source(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    SOURCE_FORMATS.iter().any(|f| s == *f)
}

#[cfg(test)]
mod tests {
    use super::{names_converted_source, names_converter_tool};

    #[test]
    fn detects_conversion_tools_and_ignores_clean_metadata() {
        assert!(names_converter_tool(
            "calibre (6.21.0) [https://calibre-ebook.com]"
        ));
        assert!(names_converter_tool("Converted with Pandoc"));
        assert!(!names_converter_tool("O'Reilly Media, Inc."));
        assert!(!names_converter_tool(""));
    }

    #[test]
    fn detects_foreign_source_formats() {
        // Amazon EXTH "Source" naming the format the file was built from.
        assert!(names_converted_source("docx"));
        assert!(names_converted_source("EPUB"));
        assert!(names_converted_source(" html "));
        // A native / unknown source is not a conversion signal.
        assert!(!names_converted_source("EBOK"));
        assert!(!names_converted_source(""));
        assert!(!names_converted_source("mobi7"));
    }
}
