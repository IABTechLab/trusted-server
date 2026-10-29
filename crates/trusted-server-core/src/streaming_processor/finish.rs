//! Optional asynchronous processor completion contracts.

use std::cell::Cell;
use std::time::Duration;

use error_stack::Report;
use http::StatusCode;
use web_time::Instant;

use crate::bounded_buffer::reserve_bounded;
use crate::platform::RuntimeServices;

/// Borrows un-emitted stage output and request services for optional EOF work.
///
/// `output` is a streaming stage's tail or a deliberately buffered stage's
/// retained output. It is not universally the complete document.
pub struct StreamFinishInput<'a> {
    /// Actual response status of the processed HTML.
    pub response_status: StatusCode,
    /// Driver-owned synchronous output still eligible for replacement.
    pub output: &'a [u8],
    /// Separate bounded storage for an atomic replacement candidate.
    pub replacement: &'a mut StreamOutputWriter,
    /// Request-scoped HTTP, KV and other runtime capabilities.
    pub services: &'a RuntimeServices,
    /// Absolute deadline shared across optional callbacks.
    pub deadline: &'a StreamFinishDeadline,
}

/// Selects whether to retain input or commit the bounded replacement candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFinish {
    /// Retain the current input without copying it.
    Unchanged,
    /// Commit bytes appended to [`StreamOutputWriter`].
    Replace,
}

/// Controlled failure categories for optional document finalization.
#[derive(Debug, derive_more::Display)]
pub enum StreamFinishError {
    /// The required outside service is unavailable.
    #[display("Stream completion service is unavailable")]
    Unavailable,
    /// Outside work cannot produce an acceptable result.
    #[display("Stream completion result is rejected")]
    Rejected,
    /// The bounded candidate cannot accept more bytes.
    #[display("Stream replacement exceeds the {limit}-byte limit")]
    OutputLimitExceeded {
        /// Maximum candidate size in bytes.
        limit: usize,
    },
}

impl core::error::Error for StreamFinishError {}

/// Separate bounded storage for a finalizer's replacement candidate.
///
/// A failed append poisons the candidate, preventing partial commitment even
/// when an integration ignores the returned error.
pub struct StreamOutputWriter {
    bytes: Vec<u8>,
    limit: usize,
    poisoned: bool,
}

impl StreamOutputWriter {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            poisoned: false,
        }
    }

    /// Append bytes without growing the candidate beyond its configured limit.
    ///
    /// # Errors
    ///
    /// Returns [`StreamFinishError::OutputLimitExceeded`] when the candidate
    /// is poisoned, length arithmetic overflows, or the append exceeds the limit.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// input.replacement.append(b"<p>Example article</p>")?;
    /// ```
    pub fn append(&mut self, chunk: &[u8]) -> Result<(), Report<StreamFinishError>> {
        if self.poisoned || reserve_bounded(&mut self.bytes, chunk.len(), self.limit).is_none() {
            self.poisoned = true;
            return Err(Report::new(StreamFinishError::OutputLimitExceeded {
                limit: self.limit,
            }));
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    pub(crate) fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Absolute finish budget shared by all finalizers for one HTML document.
///
/// A cooperative timer can interrupt yielding work, but cannot preempt CPU work
/// or blocking host calls. Consumers must bound their own calls on Fastly.
pub struct StreamFinishDeadline {
    started_at: Instant,
    timeout: Duration,
    cooperative_timer: bool,
    timer_expired: Cell<bool>,
}

impl StreamFinishDeadline {
    pub(crate) fn new(timeout: Duration, cooperative_timer: bool) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
            cooperative_timer,
            timer_expired: Cell::new(false),
        }
    }

    /// Return the remaining whole-chain budget, saturating at zero.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.remaining_at(Instant::now())
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Report whether the request has a cooperative timer for yielding work.
    #[must_use]
    pub fn has_cooperative_timer(&self) -> bool {
        self.cooperative_timer
    }

    pub(crate) fn mark_expired(&self) {
        self.timer_expired.set(true);
    }

    pub(crate) fn remaining_at(&self, now: Instant) -> Duration {
        if self.timer_expired.get() {
            return Duration::ZERO;
        }
        self.timeout
            .saturating_sub(now.saturating_duration_since(self.started_at))
    }

    pub(crate) fn is_expired_at(&self, now: Instant) -> bool {
        self.remaining_at(now).is_zero()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn replacement_limit_poison_preserves_the_candidate() {
        let mut writer = StreamOutputWriter::new(4);
        writer.append(b"page").expect("should accept the exact cap");
        let _ = writer
            .append(b"!")
            .expect_err("should reject output beyond the cap");
        assert!(writer.is_poisoned(), "should poison an oversized candidate");
        assert_eq!(writer.bytes, b"page", "should preserve the accepted prefix");
        let _ = writer
            .append(b"")
            .expect_err("should reject writes after poisoning");
    }

    #[test]
    fn replacement_capacity_stays_under_the_limit() {
        let mut writer = StreamOutputWriter::new(17);
        assert_eq!(writer.bytes.capacity(), 0, "should allocate lazily");
        for _ in 0..17 {
            writer.append(b"x").expect("should append within the limit");
            assert!(writer.bytes.capacity() <= 17, "should bound reserve growth");
        }
    }

    #[test]
    fn deadline_expires_at_the_boundary() {
        let deadline = StreamFinishDeadline::new(Duration::from_millis(5), false);
        assert_eq!(
            deadline.remaining_at(deadline.started_at),
            Duration::from_millis(5),
            "should start with the complete budget"
        );
        let expired = deadline.started_at + Duration::from_millis(5);
        assert!(
            deadline.is_expired_at(expired),
            "should expire at the exact boundary"
        );
        assert_eq!(
            deadline.remaining_at(expired + Duration::from_millis(1)),
            Duration::ZERO,
            "should saturate elapsed time"
        );
        assert!(
            !deadline.has_cooperative_timer(),
            "should expose the absence of preemption"
        );
    }
}
