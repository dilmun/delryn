//! Universal, encoding-agnostic equation recovery.
//!
//! Turns a math occurrence in parsed content — however it is encoded (MathML, authored
//! LaTeX, MathJax spans/SVG with hidden MathML, a publisher picture, or Unicode) — into a
//! single render IR ([`MathItem`]) the reader can always draw. See `docs/MATH-RENDERING.md`.
//!
//! This crate is the fresh replacement for the old math paths and shares no code with them.
//! It owns **detection + source recovery**; the render ladder (a separate concern) consumes
//! the IR, trying `typeset` → `picture` → `text` so an equation can never render nothing.

pub mod delivery;
pub mod detect;
pub mod render;
pub mod sizing;
pub mod typeset;
pub mod unicode;

// The IR lives in the dependency-free model (it's pure data that flows parser → reader);
// re-export it here so `delryn_eqn::MathItem` stays the ergonomic path for engine callers.
pub use delryn_model::{MarkupSource, MathItem, PictureRef, PictureSize};

pub use delivery::{Deck, Target};
pub use detect::detect;
pub use render::{Raster, Rendered, render};
pub use sizing::{Cell, Placement, em_text_px, fit_columns, size_picture, size_typeset};
pub use typeset::to_nodes;
