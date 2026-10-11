//! Stable per-URL citation numbering for one turn — the Rust equivalent of
//! the Python sidecar's `CitationRegistry`. A URL seen twice in the same turn
//! (searched, then fetched; or fetched twice) keeps the same `[N]` id instead
//! of getting a second one, so inline citation markers stay meaningful.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

pub struct CitationRegistry {
    next_id: AtomicU32,
    ids: Mutex<HashMap<String, u32>>,
}

impl CitationRegistry {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
            ids: Mutex::new(HashMap::new()),
        }
    }

    /// Get this URL's id, assigning the next one if it hasn't been seen this
    /// turn. Returns `(id, is_new)` — callers should only emit a
    /// `ChatEvent::Citation` when `is_new` is true, so a re-fetched URL
    /// doesn't spam a duplicate source entry.
    pub fn id_for(&self, url: &str) -> (u32, bool) {
        let mut ids = self.ids.lock().unwrap();
        if let Some(&id) = ids.get(url) {
            return (id, false);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        ids.insert(url.to_string(), id);
        (id, true)
    }
}

impl Default for CitationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_url_keeps_the_same_id() {
        let reg = CitationRegistry::new();
        let (id1, is_new1) = reg.id_for("https://example.com/a");
        let (id2, is_new2) = reg.id_for("https://example.com/a");
        assert_eq!(id1, id2);
        assert!(is_new1);
        assert!(!is_new2);
    }

    #[test]
    fn different_urls_get_increasing_ids() {
        let reg = CitationRegistry::new();
        let (id1, _) = reg.id_for("https://example.com/a");
        let (id2, _) = reg.id_for("https://example.com/b");
        assert_ne!(id1, id2);
        assert!(id2 > id1);
    }
}
