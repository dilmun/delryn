//! The current reading session: the open book, its persistence handle, and when
//! reading started (for time tracking).
//!
//! Carved out of the `App` god-object so the three related fields read as one
//! concept — `app.session.store`, `app.session.book_path`, `app.session.started`.

use std::time::Instant;

use crate::store::Store;

/// Per-run reading session: the book being read and where its progress is saved.
#[derive(Default)]
pub struct Session {
    /// SQLite handle for progress/annotations/stats; `None` if the store failed
    /// to open (delryn still runs read-only).
    pub store: Option<Store>,
    /// Canonical path of the open book; the key for all persistence.
    pub book_path: String,
    /// When the current reading session started, for read-time tracking; `None`
    /// outside the reader (e.g. on the library screen).
    pub started: Option<Instant>,
}
