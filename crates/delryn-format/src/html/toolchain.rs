//! Toolchain registry: the per-toolchain detection **data** the classifier and
//! extractors read — never publisher names. Adding a publisher/toolchain means
//! adding tokens to a profile *here*, not editing detection logic across the
//! extractors. See `docs/parsing.md`.
//!
//! One [`ToolchainProfile`] (the `GENERIC` union) covers the surveyed library
//! today, so [`profile`] always returns it. Per-document routing (detect the
//! toolchain, pick a divergent profile, apply toolchain-specific strategies like
//! code line-reassembly) plugs in at `profile` when ≥2 toolchains genuinely
//! conflict — deliberately not built yet (no premature abstraction).

/// Reusable detection data for a toolchain family.
pub(super) struct ToolchainProfile {
    /// Class tokens marking a styled code container that isn't a `<pre>`
    /// (Springer/Apress `ProgramCode`, LaTeX `lstlisting`, DocBook
    /// `programlisting`, Pandoc `sourceCode`, …).
    pub code_container_classes: &'static [&'static str],
    /// Substrings in an `<img src>` that mark it as a small UI icon (so it stays
    /// inline as a glyph/label rather than rendering as a block figure).
    pub icon_src_keywords: &'static [&'static str],
}

/// The union profile — its fingerprints cover every publisher in the survey.
const GENERIC: ToolchainProfile = ToolchainProfile {
    code_container_classes: &[
        "ProgramCode",
        "SourceCode",
        "CodeBlock",
        "code",
        "sourceCode",
        // LaTeX `listings` package + DocBook program listings.
        "lstlisting",
        "listing",
        "programlisting",
    ],
    icon_src_keywords: &[
        "warning", "info", "tip", "note", "pencil", "key", "question", "icon", "leanpub_",
    ],
};

/// The active profile for the document being parsed. The seam for future
/// per-document toolchain routing; the union profile for now.
pub(super) fn profile() -> &'static ToolchainProfile {
    &GENERIC
}
