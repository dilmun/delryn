//! The wrap dirty-check key: every input the current `lines` were wrapped against.

/// A snapshot of every input that affects line wrapping. The reader re-wraps the
/// current section only when the live inputs differ from the key the current
/// `lines` were produced under — one `==` instead of ten field comparisons.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct WrapKey {
    pub width: usize,
    pub theme: String,
    pub line_spacing: u8,
    pub para_spacing: u8,
    pub code_wrap: bool,
    pub code_hscroll: usize,
    pub table_wrap: bool,
    pub justify: bool,
    pub tidy: bool,
    pub images_key: (usize, u16, u16, u16, u16, u16, crate::media::ImageFit),
    /// A signature of the per-image reserved row counts. Images are first laid out
    /// with an estimate, then (once built) with the resize's exact cell height; a
    /// change here re-wraps so the reserved blank rows match the drawn image and
    /// the caption sits flush beneath it (no gap, no overlap).
    pub image_rows_sig: u64,
}

impl WrapKey {
    /// A key no live input can equal (`width` is `usize::MAX`), so the first
    /// `ensure_wrapped` always wraps and a forced re-wrap is one field write.
    pub fn invalid() -> Self {
        Self {
            width: usize::MAX,
            ..Self::default()
        }
    }
}
