//! Fastly Edge Rate Limiting implementation of the core [`RateLimiter`] trait.

use error_stack::Report;
use fastly::erl::{CounterDuration, RateCounter};
use trusted_server_core::ec::rate_limiter::RateLimiter;
use trusted_server_core::error::TrustedServerError;

/// Name of the Fastly rate counter resource used by sync rate limiting.
pub const RATE_COUNTER_NAME: &str = "counter_store";

fn hourly_limit_to_per_minute_limit(hourly_limit: u32) -> u32 {
    if hourly_limit == 0 {
        return 0;
    }

    let per_minute_limit = hourly_limit.saturating_add(59) / 60;
    per_minute_limit.max(1)
}

#[cfg(test)]
fn effective_hourly_limit(hourly_limit: u32) -> u32 {
    hourly_limit_to_per_minute_limit(hourly_limit).saturating_mul(60)
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
        let per_minute_limit = hourly_limit_to_per_minute_limit(hourly_limit);
        if per_minute_limit == 0 {
            return Ok(true);
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_hourly_limit_denies_all() {
        assert_eq!(
            hourly_limit_to_per_minute_limit(0),
            0,
            "should preserve deny-all zero limit"
        );
        assert_eq!(
            effective_hourly_limit(0),
            0,
            "should preserve effective zero limit"
        );
    }

    #[test]
    fn hourly_limit_rounds_up_to_whole_requests_per_minute() {
        assert_eq!(
            hourly_limit_to_per_minute_limit(65),
            2,
            "should round 65/hr up to 2/min"
        );
        assert_eq!(
            effective_hourly_limit(65),
            120,
            "should expose the resulting effective hourly budget"
        );
    }

    #[test]
    fn small_positive_hourly_limits_round_up_to_sixty_per_hour() {
        assert_eq!(
            hourly_limit_to_per_minute_limit(1),
            1,
            "should round any positive sub-60 hourly limit up to 1/min"
        );
        assert_eq!(
            effective_hourly_limit(1),
            60,
            "should enforce a 60/hr effective minimum with the current counter window"
        );
    }

    #[test]
    fn effective_hourly_limit_stays_within_hourly_plus_fifty_nine() {
        for hourly_limit in [1, 10, 59, 60, 61, 65, 119, 120, 121, 600] {
            assert!(
                effective_hourly_limit(hourly_limit) <= hourly_limit.saturating_add(59),
                "effective hourly limit should never overshoot by more than 59 requests"
            );
        }
    }
}
