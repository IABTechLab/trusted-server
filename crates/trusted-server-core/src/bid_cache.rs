//! Request-scoped bid cache rendezvous types.

use error_stack::Report;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Maximum default wait reconstructed from a persisted epoch deadline.
///
/// Callers with a configured auction timeout should use
/// [`AuctionDeadline::from_epoch_ms_with_max_remaining`] instead.
pub const DEFAULT_MAX_RECONSTRUCTED_WAIT: Duration = Duration::from_secs(30);

/// Request-scoped bid map keyed by ad slot identifier.
pub type BidMap = HashMap<String, serde_json::Value>;

/// Serialized bid cache entry stored by a platform adapter.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub enum BidCacheEntry {
    /// Auction has started and may complete before the original deadline.
    Pending {
        /// Absolute auction deadline in Unix epoch milliseconds.
        auction_deadline_epoch_ms: u64,
    },
    /// Auction has completed, including the empty no-bid case.
    Complete {
        /// Bids keyed by ad slot identifier.
        bids: BidMap,
    },
}

/// Original auction deadline represented in local and absolute time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuctionDeadline {
    /// Local monotonic instant used by in-process wait loops.
    pub instant: Instant,
    /// Absolute Unix epoch millisecond deadline persisted across requests.
    pub epoch_ms: u64,
}

impl AuctionDeadline {
    /// Builds an [`AuctionDeadline`] from explicit local and absolute times.
    pub fn from_parts(instant: Instant, epoch_ms: u64) -> Self {
        Self { instant, epoch_ms }
    }

    /// Builds an [`AuctionDeadline`] from a timeout measured from now.
    ///
    /// This computes both the monotonic [`Instant`] and Unix epoch millisecond
    /// deadline once so later paths can reuse the same timeout.
    pub fn from_timeout(timeout: Duration) -> Self {
        let now_instant = Instant::now();
        let now_epoch = SystemTime::now();
        let instant = now_instant.checked_add(timeout).unwrap_or(now_instant);
        let epoch_deadline = now_epoch.checked_add(timeout).unwrap_or(now_epoch);
        let epoch_ms = epoch_deadline
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
            .unwrap_or(0);

        Self { instant, epoch_ms }
    }

    /// Reconstructs a local [`Instant`] from a persisted Unix epoch deadline.
    ///
    /// Fastly stores only the epoch deadline in Core Cache. A later `/ts-bids`
    /// request uses this helper to enforce that original absolute deadline
    /// without minting a fresh timeout.
    pub fn from_epoch_ms(epoch_ms: u64) -> Option<Self> {
        Self::from_epoch_ms_with_max_remaining(epoch_ms, DEFAULT_MAX_RECONSTRUCTED_WAIT)
    }

    /// Reconstructs a local [`Instant`] from a persisted Unix epoch deadline
    /// while capping the remaining wait.
    ///
    /// Returns [`None`] when the persisted deadline is implausibly far in the
    /// future or cannot be represented locally.
    pub fn from_epoch_ms_with_max_remaining(
        epoch_ms: u64,
        max_remaining: Duration,
    ) -> Option<Self> {
        let now_instant = Instant::now();
        let now_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
            .unwrap_or(0);

        let instant = if epoch_ms >= now_epoch_ms {
            let remaining = Duration::from_millis(epoch_ms - now_epoch_ms);
            if remaining > max_remaining {
                return None;
            }
            now_instant.checked_add(remaining)?
        } else {
            now_instant
        };

        Some(Self { instant, epoch_ms })
    }
}

/// Result of a bid cache lookup.
#[derive(Debug, Clone, PartialEq)]
pub enum CacheResult {
    /// Auction completed and bids are available.
    Complete {
        /// Bids keyed by ad slot identifier.
        bids: BidMap,
    },
    /// Auction is still pending until the original auction deadline.
    Pending {
        /// Original auction deadline.
        auction_deadline: AuctionDeadline,
    },
    /// Request ID is unknown or expired.
    NotFound,
}

/// Result of waiting for bids.
#[derive(Debug, Clone, PartialEq)]
pub enum WaitResult {
    /// Auction completed and bids are available.
    Bids(BidMap),
    /// Auction deadline elapsed before bids were available.
    Empty,
    /// Request ID is unknown or expired.
    NotFound,
}

/// Errors returned by bid cache implementations.
#[derive(Debug, derive_more::Display)]
pub enum BidCacheError {
    /// In-memory bid cache lock was poisoned.
    #[display("bid cache lock was poisoned")]
    LockPoisoned,
    /// Bid cache serialization failed.
    #[display("bid cache serialization failed")]
    Serialize,
    /// Bid cache deserialization failed.
    #[display("bid cache deserialization failed")]
    Deserialize,
    /// Bid cache I/O failed.
    #[display("bid cache I/O failed")]
    Io,
    /// Platform cache operation failed.
    #[display("platform bid cache operation failed")]
    PlatformCache,
}

impl core::error::Error for BidCacheError {}

/// Bid cache result type using the repository-standard [`Report`] wrapper.
pub type BidCacheResult<T> = Result<T, Report<BidCacheError>>;

/// Request-ID rendezvous for server-side auction state.
pub trait BidCache {
    /// Marks a request ID as pending until the original auction deadline.
    ///
    /// # Errors
    ///
    /// Returns [`BidCacheError`] when the cache cannot be written.
    fn mark_pending(
        &self,
        request_id: &str,
        auction_deadline: AuctionDeadline,
    ) -> BidCacheResult<()>;

    /// Stores completed bids for a request ID.
    ///
    /// # Errors
    ///
    /// Returns [`BidCacheError`] when the cache cannot be written.
    fn put(&self, request_id: &str, bids: BidMap) -> BidCacheResult<()>;

    /// Looks up the current state for a request ID.
    ///
    /// # Errors
    ///
    /// Returns [`BidCacheError`] when the cache cannot be read.
    fn try_get(&self, request_id: &str) -> BidCacheResult<CacheResult>;
}

/// In-memory [`BidCache`] implementation for tests and unsupported adapters.
pub struct InMemoryBidCache {
    ttl: Duration,
    capacity: usize,
    inner: Mutex<BidCacheInner>,
}

#[derive(Default)]
struct BidCacheInner {
    entries: HashMap<String, StoredBidCacheEntry>,
    insertion_order: Vec<String>,
}

#[derive(Debug, Clone)]
struct StoredBidCacheEntry {
    entry: BidCacheEntry,
    auction_deadline: Option<AuctionDeadline>,
    inserted_at: Instant,
}

impl InMemoryBidCache {
    /// Creates an in-memory bid cache with a TTL and maximum capacity.
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            inner: Mutex::new(BidCacheInner::default()),
        }
    }

    /// Marks a request ID as pending until the original auction deadline.
    ///
    /// # Errors
    ///
    /// Returns [`BidCacheError::LockPoisoned`] when the cache lock is poisoned.
    pub fn mark_pending(
        &self,
        request_id: &str,
        auction_deadline: AuctionDeadline,
    ) -> BidCacheResult<()> {
        <Self as BidCache>::mark_pending(self, request_id, auction_deadline)
    }

    /// Stores completed bids for a request ID.
    ///
    /// # Errors
    ///
    /// Returns [`BidCacheError::LockPoisoned`] when the cache lock is poisoned.
    pub fn put(&self, request_id: &str, bids: BidMap) -> BidCacheResult<()> {
        <Self as BidCache>::put(self, request_id, bids)
    }

    /// Stores an empty completed bid map for a request ID.
    ///
    /// # Errors
    ///
    /// Returns [`BidCacheError::LockPoisoned`] when the cache lock is poisoned.
    pub fn put_empty(&self, request_id: &str) -> BidCacheResult<()> {
        self.put(request_id, BidMap::new())
    }

    /// Looks up the current state for a request ID.
    ///
    /// # Errors
    ///
    /// Returns [`BidCacheError::LockPoisoned`] when the cache lock is poisoned.
    pub fn try_get(&self, request_id: &str) -> BidCacheResult<CacheResult> {
        <Self as BidCache>::try_get(self, request_id)
    }

    /// Returns the original auction deadline for a pending request.
    pub fn get_auction_deadline(&self, request_id: &str) -> Option<AuctionDeadline> {
        let mut inner = self.inner.lock().ok()?;
        self.remove_expired(&mut inner);
        let stored = inner.entries.get(request_id)?;

        match stored.entry {
            BidCacheEntry::Pending { .. } => stored.auction_deadline,
            BidCacheEntry::Complete { .. } => None,
        }
    }

    /// Waits for bids until the original auction deadline.
    pub fn wait_for(&self, request_id: &str, deadline: AuctionDeadline) -> WaitResult {
        loop {
            match self.try_get(request_id) {
                Ok(CacheResult::Complete { bids }) => return WaitResult::Bids(bids),
                Ok(CacheResult::NotFound) => return WaitResult::NotFound,
                Ok(CacheResult::Pending { .. }) => {
                    if Instant::now() >= deadline.instant {
                        return WaitResult::Empty;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return WaitResult::NotFound,
            }
        }
    }

    fn store_entry(
        &self,
        request_id: &str,
        entry: BidCacheEntry,
        auction_deadline: Option<AuctionDeadline>,
    ) -> BidCacheResult<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| Report::new(BidCacheError::LockPoisoned))?;

        self.remove_expired(&mut inner);
        inner.entries.insert(
            request_id.to_string(),
            StoredBidCacheEntry {
                entry,
                auction_deadline,
                inserted_at: Instant::now(),
            },
        );
        inner.insertion_order.retain(|key| key != request_id);
        inner.insertion_order.push(request_id.to_string());
        self.enforce_capacity(&mut inner);

        Ok(())
    }

    fn remove_expired(&self, inner: &mut BidCacheInner) {
        inner
            .entries
            .retain(|_, stored| stored.inserted_at.elapsed() < self.ttl);
        inner
            .insertion_order
            .retain(|key| inner.entries.contains_key(key));
    }

    fn enforce_capacity(&self, inner: &mut BidCacheInner) {
        if self.capacity == 0 {
            inner.entries.clear();
            inner.insertion_order.clear();
            return;
        }

        while inner.entries.len() > self.capacity {
            if let Some(oldest_key) = inner.insertion_order.first().cloned() {
                inner.entries.remove(&oldest_key);
                inner.insertion_order.remove(0);
            } else {
                break;
            }
        }
    }
}

impl BidCache for InMemoryBidCache {
    fn mark_pending(
        &self,
        request_id: &str,
        auction_deadline: AuctionDeadline,
    ) -> BidCacheResult<()> {
        self.store_entry(
            request_id,
            BidCacheEntry::Pending {
                auction_deadline_epoch_ms: auction_deadline.epoch_ms,
            },
            Some(auction_deadline),
        )
    }

    fn put(&self, request_id: &str, bids: BidMap) -> BidCacheResult<()> {
        self.store_entry(request_id, BidCacheEntry::Complete { bids }, None)
    }

    fn try_get(&self, request_id: &str) -> BidCacheResult<CacheResult> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| Report::new(BidCacheError::LockPoisoned))?;
        self.remove_expired(&mut inner);

        let Some(stored) = inner.entries.get(request_id) else {
            return Ok(CacheResult::NotFound);
        };

        match &stored.entry {
            BidCacheEntry::Pending {
                auction_deadline_epoch_ms,
            } => Ok(CacheResult::Pending {
                auction_deadline: if let Some(deadline) = stored.auction_deadline {
                    deadline
                } else if let Some(deadline) =
                    AuctionDeadline::from_epoch_ms(*auction_deadline_epoch_ms)
                {
                    deadline
                } else {
                    return Ok(CacheResult::NotFound);
                },
            }),
            BidCacheEntry::Complete { bids } => Ok(CacheResult::Complete { bids: bids.clone() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn deadline_after(duration: Duration, epoch_ms: u64) -> AuctionDeadline {
        AuctionDeadline {
            instant: Instant::now() + duration,
            epoch_ms,
        }
    }

    fn bid_map() -> BidMap {
        BidMap::from([("slot-1".to_string(), json!({ "hb_pb": "1.20" }))])
    }

    #[test]
    fn unknown_request_id_returns_not_found() {
        let cache = InMemoryBidCache::new(Duration::from_secs(1), 8);

        let result = cache.try_get("missing").expect("should read cache");

        assert_eq!(result, CacheResult::NotFound, "should miss unknown request");
    }

    #[test]
    fn pending_request_id_returns_pending() {
        let cache = InMemoryBidCache::new(Duration::from_secs(1), 8);
        let deadline = deadline_after(Duration::from_millis(50), 1_700_000_000_000);

        cache
            .mark_pending("rid-1", deadline)
            .expect("should mark pending");

        assert!(
            matches!(cache.try_get("rid-1"), Ok(CacheResult::Pending { .. })),
            "should return pending state"
        );
    }

    #[test]
    fn pending_entry_carries_original_auction_deadline() {
        let cache = InMemoryBidCache::new(Duration::from_secs(1), 8);
        let deadline = deadline_after(Duration::from_millis(50), 1_700_000_123_456);

        cache
            .mark_pending("rid-1", deadline)
            .expect("should mark pending");

        let CacheResult::Pending { auction_deadline } =
            cache.try_get("rid-1").expect("should read cache")
        else {
            panic!("should return pending state");
        };

        assert_eq!(
            auction_deadline, deadline,
            "should preserve original auction deadline"
        );
    }

    #[test]
    fn completed_request_id_returns_bids() {
        let cache = InMemoryBidCache::new(Duration::from_secs(1), 8);
        let bids = bid_map();

        cache.put("rid-1", bids.clone()).expect("should put bids");

        assert_eq!(
            cache.try_get("rid-1").expect("should read cache"),
            CacheResult::Complete { bids },
            "should return completed bids"
        );
    }

    #[test]
    fn expired_entry_returns_not_found() {
        let cache = InMemoryBidCache::new(Duration::ZERO, 8);

        cache.put("rid-1", bid_map()).expect("should put bids");

        assert_eq!(
            cache.try_get("rid-1").expect("should read cache"),
            CacheResult::NotFound,
            "should treat expired entries as missing"
        );
    }

    #[test]
    fn wait_for_returns_bids_immediately_when_complete() {
        let cache = InMemoryBidCache::new(Duration::from_secs(1), 8);
        let deadline = deadline_after(Duration::from_secs(1), 1_700_000_000_000);
        let bids = bid_map();

        cache.put("rid-1", bids.clone()).expect("should put bids");

        assert_eq!(
            cache.wait_for("rid-1", deadline),
            WaitResult::Bids(bids),
            "should return completed bids without waiting"
        );
    }

    #[test]
    fn wait_for_returns_empty_after_original_deadline() {
        let cache = InMemoryBidCache::new(Duration::from_secs(1), 8);
        let deadline = deadline_after(Duration::ZERO, 1_700_000_000_000);

        cache
            .mark_pending("rid-1", deadline)
            .expect("should mark pending");

        assert_eq!(
            cache.wait_for("rid-1", deadline),
            WaitResult::Empty,
            "should stop waiting at the original deadline"
        );
    }

    #[test]
    fn get_auction_deadline_returns_pending_original_deadline() {
        let cache = InMemoryBidCache::new(Duration::from_secs(1), 8);
        let deadline = deadline_after(Duration::from_millis(50), 1_700_000_123_456);

        cache
            .mark_pending("rid-1", deadline)
            .expect("should mark pending");

        assert_eq!(
            cache.get_auction_deadline("rid-1"),
            Some(deadline),
            "should return pending original deadline"
        );
    }

    #[test]
    fn reconstructing_epoch_deadline_rejects_implausibly_far_future() {
        let now_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("should have system time after Unix epoch")
            .as_millis() as u64;
        let far_future_epoch_ms = now_epoch_ms + 10_000;

        assert_eq!(
            AuctionDeadline::from_epoch_ms_with_max_remaining(
                far_future_epoch_ms,
                Duration::from_millis(1),
            ),
            None,
            "should not mint a wait beyond the configured bound"
        );
    }
}
