//! The one `identity request measurement` line per request (spec 14, step 1).
//!
//! It carries sizes, counts and labels only: no EC ID, no UID and no cookie
//! value can reach it (spec 9.3).

use super::flush::FlushOutcome;

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
    pub(crate) flush: String,
    pub(crate) removal_lost: u32,
    pub(crate) withdrawal_after_send: bool,
    pub(crate) withdrawal_after_send_failed: bool,
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

    /// The phase 4 outcome label.
    #[must_use]
    pub fn flush(&self) -> &str {
        &self.flush
    }

    /// Removals every attempt failed to write.
    #[must_use]
    pub fn removal_lost(&self) -> u32 {
        self.removal_lost
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
             withdrawal_after_send={} withdrawal_after_send_failed={}",
            self.cookie_header_bytes,
            self.route,
            self.kv_reads,
            self.kv_writes,
            self.flush,
            self.removal_lost,
            u8::from(self.withdrawal_after_send),
            u8::from(self.withdrawal_after_send_failed),
        )
    }
}

/// The label for a phase 4 outcome: lower snake case, with a drop reason
/// appended.
pub(crate) fn flush_label(flush: Option<&FlushOutcome>) -> String {
    match flush {
        None => "not_run".to_owned(),
        Some(FlushOutcome::Skipped) => "skipped".to_owned(),
        Some(FlushOutcome::NoChange) => "no_change".to_owned(),
        Some(FlushOutcome::Written) => "written".to_owned(),
        Some(FlushOutcome::Dropped(reason)) => {
            format!("dropped_{}", reason.replace(' ', "_"))
        }
        Some(FlushOutcome::WriteFailed { .. }) => "write_failed".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_label_covers_every_outcome() {
        assert_eq!(
            flush_label(None),
            "not_run",
            "should label a skipped phase 4"
        );
        assert_eq!(
            flush_label(Some(&FlushOutcome::Dropped("read failed"))),
            "dropped_read_failed",
            "should join a drop reason with underscores"
        );
        assert_eq!(
            flush_label(Some(&FlushOutcome::Written)),
            "written",
            "should label a landed write"
        );
    }
}
