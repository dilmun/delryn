//! `delryn-infra` — cross-cutting plumbing shared across layers.
//!
//! Data paths, user configuration, and colour themes today; background tasks,
//! caches, and export will join here. Depends only on `delryn-model` (none yet)
//! and leaf external crates, so any layer may use it.

pub mod config;
pub mod paths;
pub mod theme;
