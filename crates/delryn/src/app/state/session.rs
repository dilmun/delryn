//! The current reading session: the open book, its persistence handle, and when
//! reading started (for time tracking).
//!
//! Carved out of the `App` god-object so the three related fields read as one
//! concept — `app.session.store`, `app.session.book_path`, `app.session.started`.

use std::time::{Duration, Instant};

use crate::store::Store;

/// Per-run reading session: the book being read and where its progress is saved.
pub struct Session {
    /// SQLite handle for progress/annotations/stats; `None` if the store failed
    /// to open (delryn still runs read-only).
    pub store: Option<Store>,
    /// Canonical path of the open book; the key for all persistence.
    pub book_path: String,
    /// When the current reading session started, for read-time tracking; `None`
    /// outside the reader (e.g. on the library screen).
    pub started: Option<Instant>,
    /// When the position was last written to the store, so the event loop's
    /// periodic save (`App::tick_autosave`) can rate-limit itself. Progress is
    /// otherwise only persisted at chapter boundaries and on a clean exit, which
    /// an abrupt termination skips entirely.
    pub last_autosave: Instant,
}

// Hand-written rather than derived: `Instant` has no meaningful default, and the
// autosave clock has to start at "now" or the first tick would fire immediately.
impl Default for Session {
    fn default() -> Self {
        Self {
            store: None,
            book_path: String::new(),
            started: None,
            last_autosave: Instant::now(),
        }
    }
}

impl Session {
    /// Has `interval` passed since the last periodic save? Consumes the answer by
    /// rearming the clock, so a caller that asks is the one that saves.
    ///
    /// Separate from `App::tick_autosave` so the rate-limiting is testable on its
    /// own: the tick also needs an open `Reader`, which a unit test can't cheaply
    /// build, and the part worth pinning down is this timer.
    pub fn autosave_due(&mut self, interval: Duration) -> bool {
        if self.last_autosave.elapsed() < interval {
            return false;
        }
        self.last_autosave = Instant::now();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autosave_is_rate_limited_and_rearms() {
        let mut s = Session::default();
        let interval = Duration::from_secs(15);

        // Freshly constructed: not due, so opening a book doesn't immediately write.
        assert!(!s.autosave_due(interval));

        // Once the interval has passed it fires exactly once, then rearms — a loop
        // calling this every frame must not write on every frame afterwards.
        s.last_autosave = Instant::now() - interval - Duration::from_secs(1);
        assert!(s.autosave_due(interval), "due after the interval");
        assert!(!s.autosave_due(interval), "rearmed, so not due again");
    }
}
