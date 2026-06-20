//! delryn — a terminal reader for EPUB (now) and PDF (later).
//!
//! The binary is a thin shell over this library so the non-UI layers
//! (document model, layout/reflow) can be exercised by tests and examples.
//! See `DESIGN.md` for the full design.

pub mod app;
pub mod config;
pub mod document;
pub mod highlight;
pub mod input;
pub mod layout;
pub mod math;
pub mod store;
pub mod theme;
pub mod view;
