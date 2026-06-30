//! `delryn-infra` — cross-cutting plumbing shared across layers.
//!
//! Data paths, user configuration, and colour themes today; background tasks,
//! caches, and export will join here. Depends only on `delryn-model` (none yet)
//! and leaf external crates, so any layer may use it.

pub mod color;
pub mod config;
pub mod paths;
pub mod theme;

/// Serializes tests across the workspace that mutate the process-global
/// `XDG_CONFIG_HOME` (which [`paths::config_dir`] reads). Without it the parallel
/// test runner lets two such tests clobber each other's data dir. Poison-
/// tolerant. Lives in normal (not `#[cfg(test)]`) code so dependent crates' tests
/// can share the one lock — a dependency's test-only items aren't visible.
pub fn test_env_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
