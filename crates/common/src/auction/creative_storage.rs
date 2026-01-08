//! Creative storage using Fastly KV Store.
//!
//! This module provides persistent storage for creative HTML returned from auction providers.
//! Creatives are stored in Fastly KV Store with a TTL and can be retrieved via auction_id + slot_id.

use error_stack::{Report, ResultExt};
use fastly::kv_store::KVStore;
use std::time::Duration;

use crate::error::TrustedServerError;

/// KV-based creative storage for auction results.
#[derive(Clone)]
pub struct CreativeStorage {
    store_name: String,
    ttl: Duration,
}

impl CreativeStorage {
    /// Create a new creative storage with the specified KV store name and TTL.
    pub fn new(store_name: String, ttl: Duration) -> Self {
        Self { store_name, ttl }
    }

    /// Store a creative with the given key.
    ///
    /// The key should be unique per auction and slot (e.g., "auction-id:slot-id").
    pub fn store(&self, key: String, html: String) -> Result<(), Report<TrustedServerError>> {
        log::info!("Storing creative: {}", html);
        let store = KVStore::open(&self.store_name)
            .change_context(TrustedServerError::Configuration {
                message: format!("Failed to open KV store '{}'", self.store_name),
            })?
            .ok_or_else(|| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("KV store '{}' not found", self.store_name),
                })
            })?;

        // Note: Fastly KV Store insert doesn't support TTL directly
        // The TTL is managed at the store level in production
        // For local development, entries persist until manually deleted
        store
            .insert(&key, html.as_bytes())
            .change_context(TrustedServerError::Auction {
                message: format!("Failed to store creative with key '{}'", key),
            })?;

        log::debug!(
            "Stored creative in KV store '{}' with key '{}' ({} bytes, TTL: {}s)",
            self.store_name,
            key,
            html.len(),
            self.ttl.as_secs()
        );

        Ok(())
    }

    /// Retrieve a creative by key.
    ///
    /// Returns None if the key doesn't exist or has expired.
    pub fn retrieve(&self, key: &str) -> Result<Option<String>, Report<TrustedServerError>> {
        let store = KVStore::open(&self.store_name)
            .change_context(TrustedServerError::Configuration {
                message: format!("Failed to open KV store '{}'", self.store_name),
            })?
            .ok_or_else(|| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("KV store '{}' not found", self.store_name),
                })
            })?;

        // Lookup returns a LookupResponse - try to read the body
        let mut lookup_response =
            store
                .lookup(key)
                .change_context(TrustedServerError::Auction {
                    message: format!("Failed to lookup creative with key '{}'", key),
                })?;

        // Get the body and convert to bytes
        let bytes = lookup_response.take_body().into_bytes();

        if bytes.is_empty() {
            // Empty bytes means key doesn't exist
            log::debug!(
                "Creative not found in KV store '{}' with key '{}'",
                self.store_name,
                key
            );
            Ok(None)
        } else {
            let html =
                String::from_utf8(bytes).change_context(TrustedServerError::InvalidUtf8 {
                    message: format!("Creative data for key '{}' is not valid UTF-8", key),
                })?;

            log::debug!(
                "Retrieved creative from KV store '{}' with key '{}' ({} bytes)",
                self.store_name,
                key,
                html.len()
            );
            Ok(Some(html))
        }
    }
}

impl Default for CreativeStorage {
    fn default() -> Self {
        // Default TTL of 5 minutes
        Self::new("creative_store".to_string(), Duration::from_secs(300))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a Fastly KV store to be configured.
    // They are marked as ignored by default and should be run with --ignored
    // in a Fastly Compute environment (via viceroy).

    #[test]
    #[ignore = "Requires Fastly KV store configuration"]
    fn test_store_and_retrieve() {
        let storage = CreativeStorage::new("creative_store".to_string(), Duration::from_secs(300));
        let key = "auction-123:slot-1".to_string();
        let html = "<div>Test Creative</div>".to_string();

        storage.store(key.clone(), html.clone()).unwrap();

        let retrieved = storage.retrieve(&key).unwrap();
        assert_eq!(retrieved, Some(html));
    }

    #[test]
    #[ignore = "Requires Fastly KV store configuration"]
    fn test_retrieve_nonexistent() {
        let storage = CreativeStorage::new("creative_store".to_string(), Duration::from_secs(300));
        let retrieved = storage.retrieve("nonexistent").unwrap();
        assert_eq!(retrieved, None);
    }
}
