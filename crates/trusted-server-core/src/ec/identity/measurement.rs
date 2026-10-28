//! The one `identity request measurement` line per request (spec 14, step 1).
//!
//! It carries sizes, counts and labels only: no EC ID, no UID and no cookie
//! value can reach it (spec 9.3).

use core::fmt::Write as _;

use std::collections::BTreeMap;

use super::flush::FlushOutcome;
use super::ids::{IdentityProviderId, SourceDomain};

/// What kind of identity route the request was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum RouteKind {
    /// The one browser-driven writer request.
    #[display("writer")]
    Writer,
    /// Any other request: it reads and stages but never writes after send.
    #[display("stager")]
    Stager,
    /// The request generated a new EC.
    #[display("new_ec")]
    NewEc,
}

/// What the adapter knows about the request after phase 4.
#[derive(Debug, Clone, Copy)]
pub struct MeasurementInput<'a> {
    /// Whether phase 1 read the identity row.
    pub phase_one_read: bool,
    /// KV reads made after phase 1.
    pub kv_reads: u32,
    /// KV writes made after phase 1.
    pub kv_writes: u32,
    /// How phase 4 ended, or `None` when it did not run.
    pub flush: Option<&'a FlushOutcome>,
    /// Whether a withdrawal tombstone needed its after-send run.
    pub withdrawal_after_send: bool,
    /// Whether that run left the tombstone unwritten.
    pub withdrawal_after_send_failed: bool,
}

/// The measurement of one request, formatted by [`core::fmt::Display`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityMeasurement {
    pub(crate) cookie_header_bytes: usize,
    pub(crate) route: RouteKind,
    pub(crate) kv_reads: u32,
    pub(crate) kv_writes: u32,
    pub(crate) flush: Option<FlushOutcome>,
    pub(crate) removal_lost: u32,
    pub(crate) removal_deferred: u32,
    pub(crate) withdrawal_after_send: bool,
    pub(crate) withdrawal_after_send_failed: bool,
    pub(crate) capture_unpersisted: BTreeMap<(IdentityProviderId, SourceDomain), u32>,
}

impl IdentityMeasurement {
    /// The inbound `Cookie` header size in bytes.
    #[must_use]
    pub fn cookie_header_bytes(&self) -> usize {
        self.cookie_header_bytes
    }

    /// The route kind label.
    #[must_use]
    pub fn route(&self) -> RouteKind {
        self.route
    }

    /// KV reads this request made.
    #[must_use]
    pub fn kv_reads(&self) -> u32 {
        self.kv_reads
    }

    /// KV writes this request made.
    #[must_use]
    pub fn kv_writes(&self) -> u32 {
        self.kv_writes
    }

    /// How phase 4 ended, or `None` when it did not run.
    #[must_use]
    pub fn flush(&self) -> Option<FlushOutcome> {
        self.flush
    }

    /// Removals every attempt failed to write.
    #[must_use]
    pub fn removal_lost(&self) -> u32 {
        self.removal_lost
    }

    /// Removals the row write missed that went to the pending key.
    #[must_use]
    pub fn removal_deferred(&self) -> u32 {
        self.removal_deferred
    }

    /// Whether a withdrawal tombstone needed its after-send run.
    #[must_use]
    pub fn withdrawal_after_send(&self) -> bool {
        self.withdrawal_after_send
    }

    /// Whether the after-send withdrawal run left the tombstone unwritten.
    #[must_use]
    pub fn withdrawal_after_send_failed(&self) -> bool {
        self.withdrawal_after_send_failed
    }

    /// Positive capture IDs counted and never stored, per provider and
    /// source. Counts only: no value is held.
    #[must_use]
    pub fn capture_unpersisted(&self) -> &BTreeMap<(IdentityProviderId, SourceDomain), u32> {
        &self.capture_unpersisted
    }

    /// Logs the measurement at info level.
    pub fn log(&self) {
        log::info!("identity request measurement: {self}");
    }
}

impl core::fmt::Display for IdentityMeasurement {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "cookie_header_bytes={} route={} kv_reads={} kv_writes={} flush={} removal_lost={} \
             removal_deferred={} withdrawal_after_send={} withdrawal_after_send_failed={} capture_unpersisted={}",
            self.cookie_header_bytes,
            self.route,
            self.kv_reads,
            self.kv_writes,
            FlushLabel(self.flush),
            self.removal_lost,
            self.removal_deferred,
            u8::from(self.withdrawal_after_send),
            u8::from(self.withdrawal_after_send_failed),
            CaptureUnpersisted(&self.capture_unpersisted),
        )
    }
}

/// The capture counts as `provider/source:count`, comma separated, or `none`.
struct CaptureUnpersisted<'a>(&'a BTreeMap<(IdentityProviderId, SourceDomain), u32>);

impl core::fmt::Display for CaptureUnpersisted<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0.is_empty() {
            return formatter.write_str("none");
        }
        for (index, ((provider, source), count)) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_char(',')?;
            }
            write!(formatter, "{provider}/{source}:{count}")?;
        }
        Ok(())
    }
}

/// A phase 4 outcome as a lower snake case label, with a drop reason
/// appended.
pub(crate) struct FlushLabel(pub(crate) Option<FlushOutcome>);

impl core::fmt::Display for FlushLabel {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            None => formatter.write_str("not_run"),
            Some(FlushOutcome::Skipped) => formatter.write_str("skipped"),
            Some(FlushOutcome::NoChange) => formatter.write_str("no_change"),
            Some(FlushOutcome::Written) => formatter.write_str("written"),
            Some(FlushOutcome::Dropped(reason)) => {
                formatter.write_str("dropped_")?;
                reason.chars().try_for_each(|character| {
                    formatter.write_char(if character == ' ' { '_' } else { character })
                })
            }
            Some(FlushOutcome::WriteFailed { .. }) => formatter.write_str("write_failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::removal::RemovalWriteOutcome;

    #[test]
    fn flush_label_covers_every_outcome() {
        let cases = [
            (None, "not_run"),
            (Some(FlushOutcome::Skipped), "skipped"),
            (Some(FlushOutcome::NoChange), "no_change"),
            (Some(FlushOutcome::Written), "written"),
            (
                Some(FlushOutcome::Dropped("read failed")),
                "dropped_read_failed",
            ),
            (
                Some(FlushOutcome::WriteFailed {
                    removals: RemovalWriteOutcome::Failed,
                }),
                "write_failed",
            ),
        ];

        for (outcome, expected) in cases {
            assert_eq!(
                FlushLabel(outcome).to_string(),
                expected,
                "should label {outcome:?}"
            );
        }
    }

    fn measurement(
        capture_unpersisted: BTreeMap<(IdentityProviderId, SourceDomain), u32>,
    ) -> IdentityMeasurement {
        IdentityMeasurement {
            cookie_header_bytes: 10,
            route: RouteKind::Stager,
            kv_reads: 0,
            kv_writes: 0,
            flush: None,
            removal_lost: 0,
            removal_deferred: 0,
            withdrawal_after_send: false,
            withdrawal_after_send_failed: false,
            capture_unpersisted,
        }
    }

    #[test]
    fn measurement_line_counts_capture_unpersisted_per_provider_and_source() {
        let line = measurement(BTreeMap::from([
            (
                (
                    "lockr".parse().expect("should parse the provider ID"),
                    "a.example.com".parse().expect("should parse the source"),
                ),
                2,
            ),
            (
                (
                    "lockr".parse().expect("should parse the provider ID"),
                    "b.example.com".parse().expect("should parse the source"),
                ),
                1,
            ),
        ]))
        .to_string();
        let empty = measurement(BTreeMap::new()).to_string();

        assert!(
            line.ends_with("capture_unpersisted=lockr/a.example.com:2,lockr/b.example.com:1"),
            "should list each provider and source with its count: {line}"
        );
        assert!(
            empty.ends_with("capture_unpersisted=none"),
            "control: should print none without a capture: {empty}"
        );
    }
}
