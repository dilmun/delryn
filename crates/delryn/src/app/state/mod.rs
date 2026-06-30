//! Extracted `App` sub-state — cohesive field groups carved out of the former
//! `App` god-object so each concern owns its own type (redesign Phase R-A; see
//! `TODO.md`). Behaviour stays on `impl App`; these are the data it operates on.
//!
//! `library` and `session` now; `overlay` follows.

pub mod library;
pub mod session;

pub use library::LibraryState;
pub use session::Session;
