//! Extracted `Reader` sub-state — cohesive field groups carved out of the former
//! `Reader` god-object so each concern owns its own type. Behaviour stays on
//! `impl Reader` (in `reader/mod.rs` and the concern modules); these are the data
//! it operates on, reached by direct nested field access (e.g. `self.images.cache`)
//! so the borrow checker keeps disjoint nested-field borrows.
//!
//! `wrap`, `images`, `pages`, `nav`, `search`, and `cache`.

pub mod cache;
pub mod images;
pub mod nav;
pub mod pages;
pub mod search;
pub mod wrap;

pub use cache::SectionCache;
pub use images::ImageState;
pub use nav::{NavState, Pos};
pub use pages::PageThemeState;
pub use search::SearchState;
pub use wrap::WrapKey;
