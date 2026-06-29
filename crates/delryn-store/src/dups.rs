//! Dismissed duplicate groups: the "keep both" memory. When the reader reviews a
//! flagged group and decides the copies are not duplicates (or wants to keep them
//! all), its signature is remembered here so the group stops being flagged. The
//! signature is the group's sorted member paths (see `delryn_library::dedup`); if
//! its membership later changes, the signature changes and the group resurfaces.

use std::collections::HashSet;

use rusqlite::params;

use super::*;

impl Store {
    /// Remember a duplicate group as dismissed ("keep both"). Idempotent; a blank
    /// signature is ignored.
    pub fn dismiss_duplicate_group(&self, signature: &str) {
        if signature.is_empty() {
            return;
        }
        let _ = self.conn.execute(
            "INSERT OR IGNORE INTO dismissed_dups (signature) VALUES (?1)",
            params![signature],
        );
    }

    /// Every dismissed-group signature, for filtering flagged duplicates.
    pub fn dismissed_duplicate_groups(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT signature FROM dismissed_dups")
            && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0))
        {
            out.extend(rows.flatten());
        }
        out
    }

    /// Forget all dismissals, so every duplicate group is flagged again.
    pub fn clear_dismissed_duplicates(&self) {
        let _ = self.conn.execute("DELETE FROM dismissed_dups", []);
    }
}
