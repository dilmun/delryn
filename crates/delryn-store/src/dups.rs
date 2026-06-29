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

    /// Out-of-band duplicate links — candidate pairs the thorough content scan
    /// discovered. The grouping unions these in so content-matched books (e.g. a
    /// PDF and an EPUB with no shared metadata) land in one duplicate group.
    pub fn dup_links(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare("SELECT a, b FROM dup_links")
            && let Ok(rows) =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            out.extend(rows.flatten());
        }
        out
    }

    /// Replace all scan-derived links with a fresh set. The thorough scan is the
    /// only writer of `dup_links`, so each run starts clean (this also clears links
    /// from any earlier scan signal).
    pub fn replace_scan_dup_links(&self, pairs: &[(String, String)]) {
        let _ = self.conn.execute("DELETE FROM dup_links", []);
        for (a, b) in pairs {
            let _ = self.conn.execute(
                "INSERT OR IGNORE INTO dup_links (a, b, signal) VALUES (?1, ?2, 'content')",
                params![a, b],
            );
        }
    }
}
