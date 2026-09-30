//! A bounded in-process cache: least-recently-used eviction, an optional
//! time to live, read-through loading and invalidation.
//!
//! Enabled by the `cache` feature. Std only; share one with an `Arc` or a
//! `static`.
//!
//! ```
//! use rustclamp::cache::Cache;
//!
//! let cache = Cache::new(2);
//! assert_eq!(cache.get_or_insert_with("a", || 1), 1);
//! assert_eq!(cache.get_or_insert_with("a", || 2), 1); // cached, loader not run
//! cache.invalidate(&"a");
//! assert_eq!(cache.get(&"a"), None);
//! ```

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

struct Entry<V> {
    value: V,
    stored: Instant,
    used: u64,
}

struct Inner<K, V> {
    entries: HashMap<K, Entry<V>>,
    tick: u64,
}

/// A capacity-bounded map that evicts the least recently used entry.
pub struct Cache<K, V> {
    inner: Mutex<Inner<K, V>>,
    capacity: usize,
    ttl: Option<Duration>,
}

impl<K: Hash + Eq + Clone, V: Clone> Cache<K, V> {
    /// A cache holding at most `capacity` entries (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: HashMap::new(),
                tick: 0,
            }),
            capacity: capacity.max(1),
            ttl: None,
        }
    }

    /// Entries older than `ttl` are treated as absent.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// The cached value, marking it recently used; `None` when absent or expired.
    pub fn get(&self, key: &K) -> Option<V> {
        let mut inner = self.lock();
        let ttl = self.ttl;
        inner.tick += 1;
        let tick = inner.tick;
        if let Some(entry) = inner.entries.get_mut(key) {
            if ttl.is_none_or(|ttl| entry.stored.elapsed() < ttl) {
                entry.used = tick;
                return Some(entry.value.clone());
            }
            inner.entries.remove(key);
        }
        None
    }

    /// Stores `value`, evicting the least recently used entry when full.
    pub fn insert(&self, key: K, value: V) {
        let mut inner = self.lock();
        inner.tick += 1;
        let used = inner.tick;
        if !inner.entries.contains_key(&key) && inner.entries.len() >= self.capacity {
            // ponytail: O(n) scan on eviction; fine for small caches, use a linked list if capacity grows large.
            let oldest = inner
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                inner.entries.remove(&oldest);
            }
        }
        inner.entries.insert(
            key,
            Entry {
                value,
                stored: Instant::now(),
                used,
            },
        );
    }

    /// The cached value, or the result of `load`, which is then stored.
    ///
    /// `load` runs without the cache locked, so two callers that miss at the
    /// same time may both load; the last store wins.
    pub fn get_or_insert_with(&self, key: K, load: impl FnOnce() -> V) -> V {
        if let Some(value) = self.get(&key) {
            return value;
        }
        let value = load();
        self.insert(key, value.clone());
        value
    }

    /// Removes one entry.
    pub fn invalidate(&self, key: &K) {
        self.lock().entries.remove(key);
    }

    /// Removes every entry.
    pub fn invalidate_all(&self) {
        self.lock().entries.clear();
    }

    /// The number of stored entries, expired ones included until touched.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether the cache holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> MutexGuard<'_, Inner<K, V>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used() {
        let cache = Cache::new(2);
        cache.insert("a", 1);
        cache.insert("b", 2);
        assert_eq!(cache.get(&"a"), Some(1)); // "b" is now the oldest
        cache.insert("c", 3);
        assert_eq!(cache.get(&"b"), None);
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"c"), Some(3));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn overwriting_does_not_evict() {
        let cache = Cache::new(2);
        cache.insert("a", 1);
        cache.insert("b", 2);
        cache.insert("a", 3);
        assert_eq!((cache.get(&"a"), cache.get(&"b")), (Some(3), Some(2)));
    }

    #[test]
    fn expires_after_ttl() {
        let cache = Cache::new(4).with_ttl(Duration::from_millis(200));
        cache.insert("a", 1);
        assert_eq!(cache.get(&"a"), Some(1));
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(cache.get(&"a"), None);
        assert!(cache.is_empty());
    }

    #[test]
    fn reads_through_once_and_invalidates() {
        let cache = Cache::new(4);
        let mut loads = 0;
        for _ in 0..3 {
            cache.get_or_insert_with("k", || {
                loads += 1;
                7
            });
        }
        assert_eq!(loads, 1);
        cache.invalidate(&"k");
        assert_eq!(cache.get_or_insert_with("k", || 8), 8);
        cache.invalidate_all();
        assert!(cache.is_empty());
    }
}
