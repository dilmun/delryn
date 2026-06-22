//! delryn — a terminal reader for EPUB (now) and PDF (later).
//!
//! The binary is a thin shell over this library so the non-UI layers
//! (document model, layout/reflow) can be exercised by tests and examples.
//! See `DESIGN.md` for the full design.

pub mod app;
pub mod clipboard;
pub mod config;
pub mod document;
pub mod highlight;
pub mod input;
pub mod layout;
pub mod library;
pub mod math;
pub mod media;
pub mod online;
pub mod search;
pub mod store;
pub mod theme;
pub mod view;

/// Serializes tests that mutate the process-global `XDG_CONFIG_HOME` (which the
/// store reads to locate its database). Without this, the parallel test runner
/// lets two such tests clobber each other's config dir. Poison-tolerant: a
/// panic in one test must not wedge the rest.
#[cfg(test)]
pub fn test_env_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
