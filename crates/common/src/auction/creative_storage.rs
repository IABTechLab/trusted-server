//! In-memory creative storage for auction results.
//!
//! This module provides temporary storage for creative HTML returned from auction providers.
//! Creatives are stored with a TTL and can be retrieved via auction_id + slot_id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Entry in the creative storage with TTL.
#[derive(Clone)]
struct CreativeEntry {
    html: String,
    expires_at: SystemTime,
}

/// Thread-safe in-memory storage for auction creatives.
#[derive(Clone)]
pub struct CreativeStorage {
    storage: Arc<Mutex<HashMap<String, CreativeEntry>>>,
    ttl: Duration,
}

impl CreativeStorage {
    /// Create a new creative storage with the specified TTL.
    pub fn new(ttl: Duration) -> Self {
        Self {
            storage: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    /// Store a creative with the given key.
    ///
    /// The key should be unique per auction and slot (e.g., "auction-id:slot-id").
    pub fn store(&self, key: String, html: String) {
        let entry = CreativeEntry {
            html,
            expires_at: SystemTime::now() + self.ttl,
        };

        if let Ok(mut storage) = self.storage.lock() {
            storage.insert(key, entry);
        }
    }

    /// Retrieve a creative by key.
    ///
    /// Returns None if the key doesn't exist or has expired.
    pub fn retrieve(&self, key: &str) -> Option<String> {
        if let Ok(mut storage) = self.storage.lock() {
            // Check if entry exists and hasn't expired
            if let Some(entry) = storage.get(key) {
                if SystemTime::now() < entry.expires_at {
                    return Some(entry.html.clone());
                } else {
                    // Entry expired, remove it
                    storage.remove(key);
                }
            }
        }
        None
    }

    /// Clean up expired entries.
    ///
    /// This should be called periodically to prevent memory leaks.
    pub fn cleanup_expired(&self) {
        if let Ok(mut storage) = self.storage.lock() {
            let now = SystemTime::now();
            storage.retain(|_, entry| now < entry.expires_at);
        }
    }

    /// Get the number of stored creatives (including expired ones).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.storage.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Check if the storage is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for CreativeStorage {
    fn default() -> Self {
        // Default TTL of 5 minutes
        Self::new(Duration::from_secs(300))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_retrieve() {
        let storage = CreativeStorage::default();
        let key = "auction-123:slot-1".to_string();
        let html = "<div>Test Creative</div>".to_string();

        storage.store(key.clone(), html.clone());

        let retrieved = storage.retrieve(&key);
        assert_eq!(retrieved, Some(html));
    }

    #[test]
    fn test_retrieve_nonexistent() {
        let storage = CreativeStorage::default();
        let retrieved = storage.retrieve("nonexistent");
        assert_eq!(retrieved, None);
    }

    #[test]
    fn test_expiration() {
        let storage = CreativeStorage::new(Duration::from_millis(50));
        let key = "auction-123:slot-1".to_string();
        let html = "<div>Test Creative</div>".to_string();

        storage.store(key.clone(), html);

        // Should be retrievable immediately
        assert!(storage.retrieve(&key).is_some());

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(100));

        // Should be expired now
        assert_eq!(storage.retrieve(&key), None);
    }

    #[test]
    fn test_cleanup_expired() {
        let storage = CreativeStorage::new(Duration::from_millis(50));

        storage.store("key1".to_string(), "<div>1</div>".to_string());
        storage.store("key2".to_string(), "<div>2</div>".to_string());

        assert_eq!(storage.len(), 2);

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(100));

        // Cleanup should remove expired entries
        storage.cleanup_expired();

        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn test_multiple_auctions() {
        let storage = CreativeStorage::default();

        storage.store("auction-1:slot-a".to_string(), "<div>1A</div>".to_string());
        storage.store("auction-1:slot-b".to_string(), "<div>1B</div>".to_string());
        storage.store("auction-2:slot-a".to_string(), "<div>2A</div>".to_string());

        assert_eq!(
            storage.retrieve("auction-1:slot-a"),
            Some("<div>1A</div>".to_string())
        );
        assert_eq!(
            storage.retrieve("auction-1:slot-b"),
            Some("<div>1B</div>".to_string())
        );
        assert_eq!(
            storage.retrieve("auction-2:slot-a"),
            Some("<div>2A</div>".to_string())
        );
    }
}
