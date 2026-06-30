//! Extracted `App` sub-state — cohesive field groups carved out of the former
//! `App` god-object so each concern owns its own type (redesign Phase R-A; see
//! `TODO.md`). Behaviour stays on `impl App`; these are the data it operates on.
//!
//! `library` now; `session` and `overlay` follow.

pub mod library;

pub use library::LibraryState;
