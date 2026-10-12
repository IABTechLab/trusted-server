//! Contracts for optional, bounded asynchronous HTML document finalization.

use std::time::Duration;

use error_stack::Report;
use futures::future::LocalBoxFuture;
use web_time::Instant;

use super::IntegrationHtmlStreamContext;
use crate::platform::RuntimeServices;

/// Creates a single-use finalizer for each processed HTML document.
///
/// Factories may be shared across requests; the returned session is request-local.
/// Creation performs synchronous setup only and must not start outside I/O.
pub trait IntegrationHtmlFinalizerFactory: Send + Sync {
    /// Return the integration identifier used for controlled diagnostic messages.
    fn integration_id(&self) -> &'static str;

    /// Create a session using the parser's shared document state.
    fn create(&self, context: IntegrationHtmlStreamContext) -> Box<dyn IntegrationHtmlFinalizer>;
}

/// Finalizes complete, synchronously transformed HTML before auction assembly.
///
/// Sessions and their futures need not be `Send`. Output is committed atomically.
/// The method returns its future promptly, with I/O inside the future. Replacements
/// preserve document structure, CSP/RSC semantics, and opaque parser seam markers.
pub trait IntegrationHtmlFinalizer {
    /// Await optional external work and select unchanged or replacement output.
    ///
    /// # Errors
    ///
    /// Return [`HtmlFinalizationError`] when optional work cannot complete. Core
    /// discards partial replacement output and retains the input on failure.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// Box::pin(async move {
    ///     input.replacement.append(b"<p>Example article</p>")?;
    ///     Ok(HtmlFinalization::Replace)
    /// })
    /// ```
    fn finalize<'a>(
        &'a mut self,
        input: HtmlFinalizationInput<'a>,
    ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>>;
}

/// Borrows the current document, separate candidate storage, and request services.
pub struct HtmlFinalizationInput<'a> {
    /// Complete transformed HTML; ownership stays with the driver.
    pub html: &'a [u8],
    /// Separate bounded sink for an atomic replacement candidate.
    pub replacement: &'a mut HtmlReplacementWriter,
    /// Request-time services, including outbound HTTP and optional KV.
    pub services: &'a RuntimeServices,
    /// The absolute deadline shared by every finalizer in this document.
    pub deadline: &'a HtmlFinalizationDeadline,
}

/// Selects whether to retain input or commit the bounded replacement candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlFinalization {
    /// Retain the current input without copying it.
    Unchanged,
    /// Commit bytes appended to [`HtmlReplacementWriter`].
    Replace,
}

/// Controlled failure categories for optional document finalization.
#[derive(Debug, derive_more::Display)]
pub enum HtmlFinalizationError {
    /// The required outside service is unavailable.
    #[display("HTML finalization service is unavailable")]
    Unavailable,
    /// Outside work cannot produce an acceptable result.
    #[display("HTML finalization result is rejected")]
    Rejected,
    /// The bounded candidate cannot accept more bytes.
    #[display("HTML replacement exceeds the {limit}-byte limit")]
    OutputLimitExceeded {
        /// Maximum candidate size in bytes.
        limit: usize,
    },
}

impl core::error::Error for HtmlFinalizationError {}

/// Separate bounded storage for a finalizer's replacement candidate.
///
/// A failed append poisons the candidate, preventing partial commitment even
/// when an integration ignores the returned error.
pub struct HtmlReplacementWriter {
    bytes: Vec<u8>,
    limit: usize,
    poisoned: bool,
}

impl HtmlReplacementWriter {
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
    /// Returns [`HtmlFinalizationError::OutputLimitExceeded`] when the candidate
    /// is poisoned, length arithmetic overflows, or the append exceeds the limit.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// input.replacement.append(b"<p>Example article</p>")?;
    /// ```
    pub fn append(&mut self, chunk: &[u8]) -> Result<(), Report<HtmlFinalizationError>> {
        let next = self
            .bytes
            .len()
            .checked_add(chunk.len())
            .filter(|length| *length <= self.limit);
        if self.poisoned || next.is_none() {
            self.poisoned = true;
            return Err(Report::new(HtmlFinalizationError::OutputLimitExceeded {
                limit: self.limit,
            }));
        }
        let Some(next) = next else {
            return Err(Report::new(HtmlFinalizationError::OutputLimitExceeded {
                limit: self.limit,
            }));
        };
        if next > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(next)
                .min(self.limit);
            self.bytes.reserve_exact(capacity - self.bytes.len());
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
pub struct HtmlFinalizationDeadline {
    started_at: Instant,
    timeout: Duration,
    cooperative_timer: bool,
}

impl HtmlFinalizationDeadline {
    pub(crate) fn new(timeout: Duration, cooperative_timer: bool) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
            cooperative_timer,
        }
    }

    /// Return the remaining whole-chain budget, saturating at zero.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.remaining_at(Instant::now())
    }

    /// Report whether the request has a cooperative timer for yielding work.
    #[must_use]
    pub fn has_cooperative_timer(&self) -> bool {
        self.cooperative_timer
    }

    pub(crate) fn remaining_at(&self, now: Instant) -> Duration {
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
        let mut writer = HtmlReplacementWriter::new(4);
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
        let mut writer = HtmlReplacementWriter::new(17);
        assert_eq!(writer.bytes.capacity(), 0, "should allocate lazily");
        for _ in 0..17 {
            writer.append(b"x").expect("should append within the limit");
            assert!(writer.bytes.capacity() <= 17, "should bound reserve growth");
        }
    }

    #[test]
    fn empty_replacement_is_distinct_from_unchanged() {
        let writer = HtmlReplacementWriter::new(4);
        assert!(
            writer.into_bytes().is_empty(),
            "should allow an empty replacement"
        );
        assert_ne!(
            HtmlFinalization::Unchanged,
            HtmlFinalization::Replace,
            "should distinguish outcomes"
        );
    }

    #[test]
    fn deadline_expires_at_the_boundary() {
        let deadline = HtmlFinalizationDeadline::new(Duration::from_millis(5), false);
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
