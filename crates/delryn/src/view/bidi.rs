//! Right-to-left text for the terminal. Terminals — especially under tmux —
//! apply neither the Unicode bidi algorithm nor Arabic joining, so an RTL string
//! renders in logical (memory) order, which reads visually reversed. [`to_visual`]
//! reshapes Arabic/Persian into presentation forms and reorders each line to
//! visual order, so a plain cell grid shows it correctly. A no-op for text with
//! no RTL characters.
//!
//! Caveat: this assumes the terminal does *not* itself reorder (true under tmux
//! and most terminals). A fully bidi-aware terminal would double-reverse.

use unicode_bidi::BidiInfo;

/// Convert one **already-wrapped** logical-order line into terminal display
/// (visual) order: reshape Arabic/Persian joining, then apply the bidi algorithm.
/// Reordering is per line, so pass single lines (no embedded newlines).
pub fn to_visual(line: &str) -> String {
    if !has_rtl(line) {
        return line.to_string();
    }
    let reshaped = ar_reshaper::reshape_line(line);
    let bidi = BidiInfo::new(&reshaped, None);
    match bidi.paragraphs.first() {
        Some(para) => bidi.reorder_line(para, para.range.clone()).into_owned(),
        None => reshaped,
    }
}

/// Whether `s` contains any right-to-left script — Hebrew/Arabic and their
/// presentation forms. Keeps [`to_visual`] a cheap no-op for ordinary LTR text.
fn has_rtl(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            c as u32,
            0x0590..=0x08FF     // Hebrew, Arabic, Arabic Supplement/Extended
            | 0xFB1D..=0xFDFF   // Hebrew + Arabic Presentation Forms-A
            | 0xFE70..=0xFEFF   // Arabic Presentation Forms-B
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ltr_text_is_unchanged() {
        assert_eq!(to_visual("hello world"), "hello world");
        assert_eq!(to_visual(""), "");
        assert_eq!(to_visual("café 123"), "café 123");
    }

    #[test]
    fn arabic_is_reordered_to_visual_order() {
        // "مرحبا" (marhaba). In logical order the first char is م (U+0645); after
        // reshaping + bidi reordering it must end up *last* (rightmost reads first),
        // i.e. the visual string starts with a different code point than the logical.
        let logical = "مرحبا";
        let visual = to_visual(logical);
        assert_ne!(visual, logical, "RTL text should be transformed");
        let first_logical = logical.chars().next().unwrap();
        let first_visual = visual.chars().next().unwrap();
        assert_ne!(
            first_visual, first_logical,
            "the logically-first letter should no longer be visually first"
        );
    }
}
