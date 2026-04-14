//! Rate limiting abstraction for EC sync endpoints.
//!
//! Provides a [`RateLimiter`] trait and its Fastly Edge Rate Limiting
//! implementation [`FastlyRateLimiter`]. Used by batch sync and pull sync
//! for per-partner request rate enforcement.

use error_stack::Report;
use fastly::erl::{CounterDuration, RateCounter};

use crate::error::TrustedServerError;

/// Name of the Fastly rate counter resource used by sync rate limiting.
pub const RATE_COUNTER_NAME: &str = "counter_store";

/// Rate limiter abstraction for sync endpoints.
///
/// Used by batch sync (`/_ts/api/v1/batch-sync`) and pull sync for
/// per-partner request rate enforcement.
pub trait RateLimiter {
    /// Returns `true` when the rate limit has been exceeded for the given key.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError`] on rate counter I/O failure.
    fn exceeded(&self, key: &str, hourly_limit: u32) -> Result<bool, Report<TrustedServerError>>;

    /// Returns `true` when the per-minute rate limit has been exceeded.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError`] on rate counter I/O failure.
    fn exceeded_per_minute(
        &self,
        key: &str,
        per_minute_limit: u32,
    ) -> Result<bool, Report<TrustedServerError>> {
        // Default implementation maps a per-minute budget to the existing
        // hourly API used by pixel sync.
        self.exceeded(key, per_minute_limit.saturating_mul(60))
    }
}

/// Fastly Edge Rate Limiting implementation of [`RateLimiter`].
pub struct FastlyRateLimiter {
    counter: RateCounter,
}

impl FastlyRateLimiter {
    /// Creates a new rate limiter backed by the named Fastly rate counter.
    #[must_use]
    pub fn new(counter_name: &str) -> Self {
        Self {
            counter: RateCounter::open(counter_name),
        }
    }
}

impl RateLimiter for FastlyRateLimiter {
    fn exceeded(&self, key: &str, hourly_limit: u32) -> Result<bool, Report<TrustedServerError>> {
        // Fastly's public rate-counter API currently exposes windows up to 60s.
        // Approximate the story's 1h limit by converting to a per-minute budget.
        //
        // Follow-up: move to exact 1-hour enforcement once platform counters
        // expose longer windows or we add a dedicated KV-backed hour bucket.
        let per_minute_limit = hourly_limit.saturating_add(59) / 60;
        let per_minute_limit = per_minute_limit.max(1);

        let current = self
            .counter
            .lookup_count(key, CounterDuration::SixtySecs)
            .map_err(|e| {
                Report::new(TrustedServerError::KvStore {
                    store_name: RATE_COUNTER_NAME.to_owned(),
                    message: format!("Failed to read sync rate counter: {e}"),
                })
            })?;

        if current >= per_minute_limit {
            return Ok(true);
        }

        self.counter.increment(key, 1).map_err(|e| {
            Report::new(TrustedServerError::KvStore {
                store_name: RATE_COUNTER_NAME.to_owned(),
                message: format!("Failed to increment sync rate counter: {e}"),
            })
        })?;

        Ok(false)
    }
}
