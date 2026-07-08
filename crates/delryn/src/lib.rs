//! delryn — a terminal reader for EPUB (now) and PDF (later).
//!
//! The binary is a thin shell over this library so the non-UI layers
//! (document model, layout/reflow) can be exercised by tests and examples.
//! See `DESIGN.md` for the full design.

pub mod app;
pub mod clipboard;
pub mod input;
pub mod search;
pub mod ui;
pub mod view;

// Extracted layers, re-exported so existing `crate::{store, online, config, …}`
// paths keep resolving.
pub use delryn_format as document;
pub use delryn_infra::highlight::HighlightColor;
pub use delryn_infra::{config, paths, test_env_guard, theme};
pub use delryn_library as library;
pub use delryn_media as media;
pub use delryn_online as online;
pub use delryn_render::{highlight, layout};
pub use delryn_store as store;
