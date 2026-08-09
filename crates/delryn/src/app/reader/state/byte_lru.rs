//! An LRU cache of byte buffers bounded by the memory they hold.

use std::hash::Hash;
use std::sync::Arc;

use lru::LruCache;

/// An LRU cache of byte buffers bounded by their **total size** rather than by
/// how many there are.
///
/// A count cap cannot express what a page cache actually needs to promise. One
/// entry here is a whole page image, and a page's encoded size swings by more than
/// an order of magnitude with the viewport, the document and the theme — so "keep
/// 24 entries" was ~40 MB for one book and a rounding error for another, with
/// nothing in the type to say which, and no way to hold a memory ceiling across
/// both. Budgeting bytes states the real constraint directly.
///
/// The entry count is left unbounded — recency still orders eviction, but the
/// budget alone decides what survives. One entry is always kept even if it exceeds
/// the budget by itself, so an oversized page is still served rather than evicted
/// the instant it arrives.
pub struct ByteLru<K: Hash + Eq> {
    lru: LruCache<K, Arc<Vec<u8>>>,
    budget: usize,
    used: usize,
}

impl<K: Hash + Eq> ByteLru<K> {
    /// A cache holding at most `budget` bytes of values.
    pub fn new(budget: usize) -> Self {
        Self {
            lru: LruCache::unbounded(),
            budget,
            used: 0,
        }
    }

    /// Insert `value`, evicting least-recently-used entries until the total fits
    /// the budget again.
    pub fn put(&mut self, key: K, value: Arc<Vec<u8>>) {
        self.used = self.used.saturating_add(value.len());
        if let Some(replaced) = self.lru.put(key, value) {
            self.used = self.used.saturating_sub(replaced.len());
        }
        while self.used > self.budget && self.lru.len() > 1 {
            let Some((_, evicted)) = self.lru.pop_lru() else {
                break;
            };
            self.used = self.used.saturating_sub(evicted.len());
        }
    }

    /// Whether `key` is present. Does **not** count as a use.
    pub fn contains(&self, key: &K) -> bool {
        self.lru.contains(key)
    }

    /// The value for `key` without promoting it. Matches `contains`, so a
    /// readiness check and the fetch that follows it always agree.
    pub fn peek(&self, key: &K) -> Option<&Arc<Vec<u8>>> {
        self.lru.peek(key)
    }

    /// Bytes currently held. Test-only for now — nothing in the reader reports
    /// cache memory yet, and inventing a caller to satisfy the lint would be worse
    /// than saying so.
    #[cfg(test)]
    pub fn used(&self) -> usize {
        self.used
    }

    /// Entries currently held. Test-only, as [`used`](Self::used).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lru.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(n: usize) -> Arc<Vec<u8>> {
        Arc::new(vec![0u8; n])
    }

    /// The budget, not the entry count, decides what stays: many small entries
    /// coexist where a few large ones would not.
    #[test]
    fn eviction_follows_bytes_not_entry_count() {
        let mut c: ByteLru<u32> = ByteLru::new(1000);
        for i in 0..10 {
            c.put(i, buf(100));
        }
        assert_eq!(c.len(), 10, "ten small entries fit the budget");
        assert!(c.used() <= 1000);

        let mut c: ByteLru<u32> = ByteLru::new(1000);
        for i in 0..10 {
            c.put(i, buf(400));
        }
        assert!(c.used() <= 1000, "used {} over budget", c.used());
        assert_eq!(c.len(), 2, "only two large entries fit");
    }

    /// Eviction takes the least recently *inserted* here — `peek` deliberately
    /// doesn't promote, so a readiness check can't change what gets evicted.
    #[test]
    fn the_oldest_entry_goes_first() {
        let mut c: ByteLru<u32> = ByteLru::new(250);
        c.put(1, buf(100));
        c.put(2, buf(100));
        c.peek(&1);
        c.put(3, buf(100));
        assert!(!c.contains(&1), "the oldest was evicted");
        assert!(c.contains(&2) && c.contains(&3));
    }

    /// Replacing a key accounts for the bytes it releases, so repeatedly
    /// re-theming one page cannot inflate the total.
    #[test]
    fn replacing_a_key_does_not_leak_its_old_bytes() {
        let mut c: ByteLru<u32> = ByteLru::new(10_000);
        for _ in 0..50 {
            c.put(1, buf(100));
        }
        assert_eq!(c.len(), 1);
        assert_eq!(c.used(), 100, "one entry's worth, not fifty");
    }

    /// An entry larger than the whole budget is still served — evicting it on
    /// arrival would mean a page that can never be shown.
    #[test]
    fn an_oversized_entry_is_kept_rather_than_dropped() {
        let mut c: ByteLru<u32> = ByteLru::new(100);
        c.put(1, buf(5000));
        assert!(c.contains(&1));
        assert_eq!(c.len(), 1);
    }
}
