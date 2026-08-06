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

/// The version delryn reports — in `--version`, in the crash log, and as the
/// HTTP User-Agent.
///
/// Releases are cut by release-please from git, which deliberately never rewrites
/// `Cargo.toml` (see `docs/RELEASING.md`), so `CARGO_PKG_VERSION` stops matching
/// the release tag the moment the two diverge. The release build therefore passes
/// the tag in as `DELRYN_VERSION`; a source build has no tag to speak of and falls
/// back to the manifest, which is the honest answer there.
/// Stamp a run header into the page-deck debug log (no-op unless
/// `DELRYN_KITTY_LOG` is set). See `app::page_deck::dbg_log_run_header`.
pub fn log_page_run(protocol: &str, cell: (u16, u16), paged: bool, continuous: bool) {
    app::log_page_run(protocol, cell, paged, continuous);
}

pub const VERSION: &str = match option_env!("DELRYN_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

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
