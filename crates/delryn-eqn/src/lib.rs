//! Universal, encoding-agnostic equation recovery.
//!
//! Turns a math occurrence in parsed content — however it is encoded (MathML, authored
//! LaTeX, MathJax spans/SVG with hidden MathML, a publisher picture, or Unicode) — into a
//! single render IR ([`MathItem`]) the reader can always draw. See `docs/MATH-RENDERING.md`.
//!
//! This crate is the fresh replacement for the old math paths and shares no code with them.
//! It owns **detection + source recovery**; the render ladder (a separate concern) consumes
//! the IR, trying `typeset` → `picture` → `text` so an equation can never render nothing.

pub mod detect;
pub mod ir;
pub mod typeset;

pub use detect::detect;
pub use ir::{MarkupSource, MathItem, PictureRef, PictureSize};
pub use typeset::to_nodes;
