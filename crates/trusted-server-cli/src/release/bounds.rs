//! Fixed R-BOUNDS resource limits.
//!
//! These values mirror the embedded policy `bounds` object, which
//! [`super::policy`] requires to match exactly. They change only through a
//! reviewed policy update.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

/// Maximum release-index bytes.
pub(crate) const INDEX_BYTES: u64 = MIB;
/// Maximum release-policy bytes, embedded or approved-source.
pub(crate) const POLICY_BYTES: u64 = MIB;
/// Maximum JS inventory bytes.
pub(crate) const INVENTORY_BYTES: u64 = MIB;
/// Maximum generic metadata bytes.
pub(crate) const METADATA_BYTES: u64 = MIB;
/// Maximum Sigstore bundle bytes.
pub(crate) const BUNDLE_BYTES: u64 = 32 * MIB;
/// Maximum build companion bytes.
pub(crate) const COMPANION_BYTES: u64 = 32 * MIB;
/// Maximum SPDX SBOM bytes (the pinned action's limit).
pub(crate) const SPDX_BYTES: u64 = 16 * MIB;
/// Maximum trusted-root snapshot bytes.
pub(crate) const TRUSTED_ROOTS_BYTES: u64 = MIB;
/// Maximum executable bytes.
pub(crate) const EXECUTABLE_BYTES: u64 = 512 * MIB;
/// Maximum JSON container nesting, counting the root.
pub(crate) const JSON_DEPTH: usize = 64;
/// Exact number of executables in the cohort.
pub(crate) const COHORT_EXECUTABLES: usize = 4;
/// Exact number of matching attestation results accepted per bundle.
pub(crate) const MATCHING_ATTESTATIONS_PER_BUNDLE: usize = 1;
/// Per-child wall-clock deadline in seconds.
pub(crate) const CHILD_SECONDS: u64 = 120;
/// Whole verification attempt deadline in seconds.
pub(crate) const ATTEMPT_SECONDS: u64 = 900;
/// Maximum child stdout bytes.
pub(crate) const STDOUT_BYTES: u64 = 128 * MIB;
/// Maximum child stderr bytes.
pub(crate) const STDERR_BYTES: u64 = MIB;

/// Bounded wait for pipe drains after the child process group is killed.
///
/// This is a runner-internal liveness bound, not an R-BOUNDS policy value: a
/// killed group closes its pipes promptly, so anything still holding them
/// after this grace has escaped the group and is reported as such.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// Per-child process limits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProcessBounds {
    pub(crate) child_timeout: Duration,
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
    pub(crate) kill_grace: Duration,
}

impl ProcessBounds {
    /// Returns the fixed production limits.
    pub(crate) const fn production() -> Self {
        Self {
            child_timeout: Duration::from_secs(CHILD_SECONDS),
            stdout_bytes: STDOUT_BYTES,
            stderr_bytes: STDERR_BYTES,
            kill_grace: KILL_GRACE,
        }
    }
}

/// The overall deadline shared by every step of one verification attempt.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AttemptDeadline(Instant);

impl AttemptDeadline {
    /// Starts the fixed 15-minute attempt deadline now.
    pub(crate) fn start() -> Self {
        Self(Instant::now() + Duration::from_secs(ATTEMPT_SECONDS))
    }

    /// Uses an explicit instant, for callers that already hold a deadline.
    pub(crate) const fn at(instant: Instant) -> Self {
        Self(instant)
    }

    /// Returns the deadline instant.
    pub(crate) const fn instant(self) -> Instant {
        self.0
    }
}

/// A cancellation flag shared between the caller and running children.
#[derive(Clone, Debug, Default)]
pub(crate) struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// Requests cancellation of every child observing this flag.
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Returns whether cancellation was requested.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
