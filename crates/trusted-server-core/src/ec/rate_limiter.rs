//! Rate limiting abstraction for EC sync endpoints.
//!
//! Provides the [`RateLimiter`] trait used by batch sync and pull sync for
//! per-partner request rate enforcement. Platform-specific implementations
//! live in the adapter crates (e.g. the Fastly Edge Rate Limiting
//! implementation in `trusted-server-adapter-fastly`).

use error_stack::Report;

use crate::error::TrustedServerError;

/// Rate limiter abstraction for sync endpoints.
///
/// Used by batch sync (`/_ts/api/v1/batch-sync`) and pull sync for
/// per-partner request rate enforcement.
pub trait RateLimiter {
    /// Returns `true` when the rate limit has been exceeded for the given key.
    ///
    /// `hourly_limit` is currently approximated via a 60-second counter
    /// window, so the effective budget rounds up to the next whole request per
    /// minute. For example, `65/hr` becomes `2/min` (`120/hr` effective), and
    /// any positive limit below `60/hr` rounds up to `1/min` (`60/hr`
    /// effective).
    ///
    /// Implementations may use a read-then-increment counter API, so callers
    /// should treat this as best-effort throttling: concurrent requests can
    /// overshoot the configured limit by the in-flight burst size.
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
