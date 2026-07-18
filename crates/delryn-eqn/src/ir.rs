//! The math IR: the single shape every encoding normalizes into, and the only thing the
//! render ladder consumes. See `docs/MATH-RENDERING.md`.
//!
//! One [`MathItem`] carries **every** render source recovered for one equation. The render
//! ladder tries them in order — `typeset` (crisp) → `picture` (the publisher's own visual)
//! → `text` (Unicode) — so an equation can never render nothing (the "never-blank" rule).
//! The three are independent: a picture-only equation still renders; a recovered-markup
//! equation that fails to typeset still falls to its picture, then its text.

/// A recovered math occurrence, ready for the render ladder.
#[derive(Debug, Clone, PartialEq)]
pub struct MathItem {
    /// Display (block, own line, limits above/below) vs inline (mid-line, compact).
    pub display: bool,
    /// A machine-readable source to re-typeset crisply, highest fidelity first. `None`
    /// when nothing markup-shaped was recoverable — then the picture or text renders.
    pub typeset: Option<MarkupSource>,
    /// The publisher's own picture (raster or vector) for this equation, kept as a
    /// fallback even when `typeset` is present (so a typeset failure still shows *something*
    /// the publisher rendered). `None` when the file shipped no image for it.
    pub picture: Option<PictureRef>,
    /// The Unicode approximation — always present, the floor the ladder can never fall past.
    pub text: String,
}

impl MathItem {
    /// Whether anything better than the Unicode floor is available (a crisp source or a
    /// picture). Purely informational — `text` is always renderable regardless.
    pub fn has_graphics(&self) -> bool {
        self.typeset.is_some() || self.picture.is_some()
    }
}

/// A machine-readable math source, in priority order of fidelity. The render layer turns
/// this into the typeset engine's input; the IR stays engine-independent (plain markup),
/// so the engine can change without touching recovery.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkupSource {
    /// Authored LaTeX (from `alttext`, `<annotation encoding="application/x-tex">`, or a
    /// math image's LaTeX `alt`). The author's exact source — highest fidelity.
    Latex(String),
    /// Presentation MathML source (native `<math>`, or harvested from a hidden div,
    /// `<switch>`, a trailing comment, or MathJax `<mjx-assistive-mml>`).
    PresentationMathml(String),
    /// Content (semantic) MathML source — mapped to presentation before typesetting.
    ContentMathml(String),
}

/// A reference to the publisher's equation picture: the resource path plus how to size it
/// text-relative. The bytes are resolved later against the book's resources (a picture in
/// the file is a path at recovery time, not bytes).
#[derive(Debug, Clone, PartialEq)]
pub struct PictureRef {
    /// The `src` / `altimg` resource reference as written in the markup.
    pub src: String,
    /// How to size it relative to the text (never from the file's raw pixel resolution).
    pub size: PictureSize,
}

/// Text-relative sizing for a picture. A CSS `em`/`ex` width is exact and DPI-independent;
/// absent that, the renderer measures the ink and scales it to the prose line-height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PictureSize {
    /// Authored CSS width in `em` (1 em ≈ one text line) — the reliable text-relative size.
    Em(f32),
    /// Authored CSS width in `ex` (≈ x-height); converted to em at render time.
    Ex(f32),
    /// No authored size — measure the ink at render time and match it to the prose.
    MeasureInk,
}
