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

/// The two counter calls the breaker uses, so tests can fake them.
trait CounterOps {
    fn increment(&self, key: &str) -> Result<(), String>;
    fn lookup_count(&self, key: &str) -> Result<u32, String>;
}

fn counter_error(action: &str, message: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::KvStore {
        store_name: RATE_COUNTER_NAME.to_owned(),
        message: format!("Failed to {action} sync rate counter: {message}"),
    })
}

fn record_with(counter: &dyn CounterOps, key: &str) -> Result<(), Report<TrustedServerError>> {
    counter
        .increment(key)
        .map_err(|message| counter_error("increment", &message))
}

fn count_with(counter: &dyn CounterOps, key: &str) -> Result<u32, Report<TrustedServerError>> {
    counter
        .lookup_count(key)
        .map_err(|message| counter_error("read", &message))
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

    fn record(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        record_with(&self.counter, key)
    }

    fn count(&self, key: &str) -> Result<u32, Report<TrustedServerError>> {
        count_with(&self.counter, key)
    }
}

impl CounterOps for RateCounter {
    fn increment(&self, key: &str) -> Result<(), String> {
        RateCounter::increment(self, key, 1).map_err(|error| error.to_string())
    }

    fn lookup_count(&self, key: &str) -> Result<u32, String> {
        RateCounter::lookup_count(self, key, CounterDuration::SixtySecs)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct FakeCounter {
        increments: RefCell<Vec<String>>,
        lookups: RefCell<Vec<String>>,
        count: u32,
        fail: bool,
    }

    impl FakeCounter {
        fn new(count: u32, fail: bool) -> Self {
            Self {
                increments: RefCell::new(Vec::new()),
                lookups: RefCell::new(Vec::new()),
                count,
                fail,
            }
        }
    }

    impl CounterOps for FakeCounter {
        fn increment(&self, key: &str) -> Result<(), String> {
            self.increments.borrow_mut().push(key.to_owned());
            if self.fail {
                return Err("counter unavailable".to_owned());
            }
            Ok(())
        }

        fn lookup_count(&self, key: &str) -> Result<u32, String> {
            self.lookups.borrow_mut().push(key.to_owned());
            if self.fail {
                return Err("counter unavailable".to_owned());
            }
            Ok(self.count)
        }
    }

    #[test]
    fn record_increments_once() {
        let counter = FakeCounter::new(0, false);

        record_with(&counter, "identity-fail:example")
            .expect("should record against a working counter");

        assert_eq!(
            *counter.increments.borrow(),
            vec!["identity-fail:example".to_owned()],
            "should increment the key exactly once"
        );
        assert!(
            counter.lookups.borrow().is_empty(),
            "should not read the count while recording"
        );
    }

    #[test]
    fn count_reads_the_sixty_second_window() {
        let counter = FakeCounter::new(7, false);

        let count = count_with(&counter, "identity-fail:example")
            .expect("should count against a working counter");

        assert_eq!(count, 7, "should return the counter's value");
        assert_eq!(
            *counter.lookups.borrow(),
            vec!["identity-fail:example".to_owned()],
            "should look the key up once"
        );
        assert!(
            counter.increments.borrow().is_empty(),
            "should not change the count while reading it"
        );
    }

    #[test]
    fn counter_errors_map_to_kv_store_errors() {
        let counter = FakeCounter::new(0, true);

        let record_error = record_with(&counter, "identity-fail:example")
            .expect_err("should fail when the increment fails");
        let count_error = count_with(&counter, "identity-fail:example")
            .expect_err("should fail when the lookup fails");

        for error in [record_error, count_error] {
            assert!(
                matches!(
                    error.current_context(),
                    TrustedServerError::KvStore { store_name, .. } if store_name == RATE_COUNTER_NAME
                ),
                "should map the failure to a KV store error naming the counter"
            );
        }
    }

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
