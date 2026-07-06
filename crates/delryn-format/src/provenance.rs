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

#[cfg(test)]
mod tests {
    use super::names_converter_tool;

    #[test]
    fn detects_conversion_tools_and_ignores_clean_metadata() {
        assert!(names_converter_tool(
            "calibre (6.21.0) [https://calibre-ebook.com]"
        ));
        assert!(names_converter_tool("Converted with Pandoc"));
        assert!(!names_converter_tool("O'Reilly Media, Inc."));
        assert!(!names_converter_tool(""));
    }
}
