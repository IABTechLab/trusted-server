//! Fastly Core Cache-backed bid cache rendezvous.

use error_stack::{Report, ResultExt};
use fastly::cache::core::{self, CacheKey};
use std::io::{Read as _, Write as _};
use std::time::Duration;
use trusted_server_core::bid_cache::{
    AuctionDeadline, BidCache, BidCacheEntry, BidCacheError, BidCacheResult, BidMap, CacheResult,
};

const DEFAULT_BID_CACHE_TTL: Duration = Duration::from_secs(30);
// Conservative upper bound for reconstructed deadlines; keeps /ts-bids from
// long-polling up to DEFAULT_BID_CACHE_TTL (30 s) when the auction already
// finished. Auction timeouts are typically 200–500 ms.
const DEFAULT_MAX_RECONSTRUCTED_WAIT: Duration = Duration::from_millis(800);

/// Fastly Core Cache-backed [`BidCache`] implementation.
pub struct FastlyBidCache {
    ttl: Duration,
    max_reconstructed_wait: Duration,
}

impl FastlyBidCache {
    /// Creates a Fastly bid cache with the default short TTL.
    pub fn new() -> Self {
        Self {
            ttl: DEFAULT_BID_CACHE_TTL,
            max_reconstructed_wait: DEFAULT_MAX_RECONSTRUCTED_WAIT,
        }
    }

    /// Creates a Fastly bid cache with an explicit TTL.
    #[cfg(test)]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            max_reconstructed_wait: ttl,
        }
    }

    /// Creates a Fastly bid cache with explicit cache and deadline bounds.
    #[cfg(test)]
    pub fn with_limits(ttl: Duration, max_reconstructed_wait: Duration) -> Self {
        Self {
            ttl,
            max_reconstructed_wait,
        }
    }

    /// Returns the Core Cache key for a bid request ID.
    pub fn cache_key_for_request_id(request_id: &str) -> String {
        format!("ts-bids:{request_id}")
    }

    fn cache_key(request_id: &str) -> CacheKey {
        CacheKey::from(Self::cache_key_for_request_id(request_id))
    }

    fn write_entry(&self, request_id: &str, entry: &BidCacheEntry) -> BidCacheResult<()> {
        let payload = serde_json::to_vec(entry).change_context(BidCacheError::Serialize)?;

        let mut writer = core::insert(Self::cache_key(request_id), self.ttl)
            .sensitive_data(true)
            .known_length(payload.len() as u64)
            .execute()
            .map_err(|error| {
                Report::new(BidCacheError::PlatformCache)
                    .attach(format!("failed to start Core Cache insert: {error}"))
            })?;

        writer
            .write_all(&payload)
            .change_context(BidCacheError::Io)?;
        writer.finish().change_context(BidCacheError::Io)?;

        Ok(())
    }
}

impl Default for FastlyBidCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BidCache for FastlyBidCache {
    fn mark_pending(
        &self,
        request_id: &str,
        auction_deadline: AuctionDeadline,
    ) -> BidCacheResult<()> {
        self.write_entry(
            request_id,
            &BidCacheEntry::Pending {
                auction_deadline_epoch_ms: auction_deadline.epoch_ms,
            },
        )
    }

    fn put(&self, request_id: &str, bids: BidMap) -> BidCacheResult<()> {
        self.write_entry(request_id, &BidCacheEntry::Complete { bids })
    }

    fn try_get(&self, request_id: &str) -> BidCacheResult<CacheResult> {
        let Some(found) = core::lookup(Self::cache_key(request_id))
            .execute()
            .map_err(|error| {
                Report::new(BidCacheError::PlatformCache)
                    .attach(format!("failed to lookup Core Cache object: {error}"))
            })?
        else {
            return Ok(CacheResult::NotFound);
        };

        let mut payload = Vec::new();
        found
            .to_stream()
            .map_err(|error| {
                Report::new(BidCacheError::PlatformCache)
                    .attach(format!("failed to read Core Cache body: {error}"))
            })?
            .read_to_end(&mut payload)
            .change_context(BidCacheError::Io)?;

        match serde_json::from_slice(&payload).change_context(BidCacheError::Deserialize)? {
            BidCacheEntry::Pending {
                auction_deadline_epoch_ms,
            } => {
                let Some(auction_deadline) = AuctionDeadline::from_epoch_ms_with_max_remaining(
                    auction_deadline_epoch_ms,
                    self.max_reconstructed_wait,
                ) else {
                    return Ok(CacheResult::NotFound);
                };

                Ok(CacheResult::Pending { auction_deadline })
            }
            BidCacheEntry::Complete { bids } => Ok(CacheResult::Complete { bids }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_uses_request_id_prefix() {
        assert_eq!(
            FastlyBidCache::cache_key_for_request_id("request-123"),
            "ts-bids:request-123",
            "should use the request-scoped bid cache prefix"
        );
    }

    #[test]
    fn default_uses_short_ttl() {
        let cache = FastlyBidCache::new();

        assert_eq!(
            cache.ttl, DEFAULT_BID_CACHE_TTL,
            "should use the adapter default bid cache TTL"
        );
    }

    #[test]
    fn explicit_ttl_overrides_default() {
        let cache = FastlyBidCache::with_ttl(Duration::from_secs(5));

        assert_eq!(
            cache.ttl,
            Duration::from_secs(5),
            "should use the configured bid cache TTL"
        );
    }

    #[test]
    fn explicit_limits_set_deadline_reconstruction_bound() {
        let cache =
            FastlyBidCache::with_limits(Duration::from_secs(30), Duration::from_millis(500));

        assert_eq!(
            cache.max_reconstructed_wait,
            Duration::from_millis(500),
            "should use the configured bid wait bound"
        );
    }
}
