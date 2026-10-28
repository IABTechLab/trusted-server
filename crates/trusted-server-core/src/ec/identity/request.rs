//! Identity state one request carries on [`crate::ec::EcContext`] (spec 5.1).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use url::Url;

use super::ids::{IdentityProviderId, SourceDomain};
use super::mutation::{IdentityMutation, is_opted_out};
use super::runtime::IdentityRuntime;
use crate::ec::kv_types::KvEntry;
use crate::ec::rate_limiter::RateLimiter;

/// The providers with a sticky opt-out (`ps.o`) in `entry`, unless a later
/// `oc` clears it (spec 7.1).
///
/// KV is the only source: no cookie carries an opt-out. A key that is not a
/// valid provider ID cannot name a configured provider and is skipped.
#[must_use]
pub(crate) fn opted_out(entry: &KvEntry) -> BTreeSet<IdentityProviderId> {
    entry
        .ps
        .keys()
        .filter(|provider| is_opted_out(entry, provider))
        .filter_map(|provider| provider.parse().ok())
        .collect()
}

/// What one request decides and collects for identity.
///
/// Its [`Debug`](core::fmt::Debug) prints counts and flags only: the mutation
/// holds UIDs that must not reach a log (spec 9.3).
#[derive(Default, Clone)]
pub(crate) struct RequestIdentity {
    writer: bool,
    spa_writer: bool,
    is_real_browser: bool,
    user_agent: Option<String>,
    dnt: bool,
    page_url: Option<Url>,
    runtime: Option<Arc<IdentityRuntime>>,
    rate_limiter: Option<Arc<dyn RateLimiter + Send + Sync>>,
    runs_auction: bool,
    hold_dispatched: bool,
    mutation: IdentityMutation,
    pending_removals: Option<IdentityMutation>,
    capture_writes: IdentityMutation,
    removal_lost: u32,
    removal_deferred: u32,
    cookie_header_bytes: usize,
    create_attempts: u32,
    capture_unpersisted: BTreeMap<(IdentityProviderId, SourceDomain), u32>,
    global_revoke: bool,
    capture_kv_reads: u32,
    capture_kv_writes: u32,
}

impl core::fmt::Debug for RequestIdentity {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RequestIdentity")
            .field("writer", &self.writer)
            .field("is_real_browser", &self.is_real_browser)
            .field("mutation_changes", &self.mutation.change_count())
            .field("has_pending_removals", &self.pending_removals.is_some())
            .field("capture_writes", &self.capture_writes.change_count())
            .field("removal_lost", &self.removal_lost)
            .field("removal_deferred", &self.removal_deferred)
            .field("has_runtime", &self.runtime.is_some())
            .finish_non_exhaustive()
    }
}

// Temporary: the accessors and setters below are first used by later identity tasks.
#[allow(dead_code)]
impl RequestIdentity {
    /// Whether this request is the one browser-driven writer.
    #[must_use]
    pub(crate) fn writer(&self) -> bool {
        self.writer
    }

    /// Whether the request passed the real-browser check.
    #[must_use]
    pub(crate) fn is_real_browser(&self) -> bool {
        self.is_real_browser
    }

    /// The request's `User-Agent`, captured in phase 1.
    #[must_use]
    pub(crate) fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }

    /// Whether the request sent `DNT: 1`.
    #[must_use]
    pub(crate) fn dnt(&self) -> bool {
        self.dnt
    }

    /// The page URL without query or fragment.
    #[must_use]
    pub(crate) fn page_url(&self) -> Option<&Url> {
        self.page_url.as_ref()
    }

    /// The request's identity runtime, when the adapter set one.
    #[must_use]
    pub(crate) fn runtime(&self) -> Option<&Arc<IdentityRuntime>> {
        self.runtime.as_ref()
    }

    /// The limiter the provider burst cap and circuit breaker count in, when
    /// the adapter set one.
    #[must_use]
    pub(crate) fn rate_limiter(&self) -> Option<Arc<dyn RateLimiter + Send + Sync>> {
        self.rate_limiter.clone()
    }

    /// Sets the limiter the hold dispatch counts in.
    pub(crate) fn set_rate_limiter(&mut self, limiter: Arc<dyn RateLimiter + Send + Sync>) {
        self.rate_limiter = Some(limiter);
    }

    /// Whether this navigation runs an auction, so hold-mode providers may
    /// be dispatched before it.
    #[must_use]
    pub(crate) fn runs_auction(&self) -> bool {
        self.runs_auction
    }

    /// Whether the hold dispatch considered every hold-mode provider on a
    /// usable phase 1 row, so phase 4 must not run them again.
    #[must_use]
    pub(crate) fn hold_dispatched(&self) -> bool {
        self.hold_dispatched
    }

    /// Records that the hold dispatch ran on a usable phase 1 row.
    pub(crate) fn set_hold_dispatched(&mut self) {
        self.hold_dispatched = true;
    }

    /// Records that this navigation runs an auction (spec 4.7 step 7).
    pub(crate) fn set_runs_auction(&mut self, runs_auction: bool) {
        self.runs_auction = runs_auction;
    }

    /// The writer's one flush.
    #[must_use]
    pub(crate) fn mutation(&self) -> &IdentityMutation {
        &self.mutation
    }

    /// The writer's one flush, to fill.
    pub(crate) fn mutation_mut(&mut self) -> &mut IdentityMutation {
        &mut self.mutation
    }

    /// A removal write that failed before send, to try once more after send.
    #[must_use]
    pub(crate) fn pending_removals(&self) -> Option<&IdentityMutation> {
        self.pending_removals.as_ref()
    }

    /// Keeps a removal write for one retry after send.
    pub(crate) fn set_pending_removals(&mut self, removals: Option<IdentityMutation>) {
        self.pending_removals = removals;
    }

    /// Positive capture writes a request that is not the writer keeps for
    /// the pending key (spec 5.3, Pending key).
    #[must_use]
    pub(crate) fn capture_writes(&self) -> &IdentityMutation {
        &self.capture_writes
    }

    /// The capture writes, to fill or take.
    pub(crate) fn capture_writes_mut(&mut self) -> &mut IdentityMutation {
        &mut self.capture_writes
    }

    /// Removals the row write missed that went to the pending key.
    #[must_use]
    pub(crate) fn removal_deferred(&self) -> u32 {
        self.removal_deferred
    }

    /// Counts one removal that went to the pending key.
    pub(crate) fn record_removal_deferred(&mut self) {
        self.removal_deferred = self.removal_deferred.saturating_add(1);
    }

    /// Removals every attempt failed to write (spec 5.2 phase 4, 5.3).
    #[must_use]
    pub(crate) fn removal_lost(&self) -> u32 {
        self.removal_lost
    }

    /// Create writes the new-EC path attempted before routing.
    #[must_use]
    pub(crate) fn create_attempts(&self) -> u32 {
        self.create_attempts
    }

    /// Counts one create write of the new-EC path.
    pub(crate) fn record_create_attempt(&mut self) {
        self.create_attempts = self.create_attempts.saturating_add(1);
    }

    /// The inbound `Cookie` header size in bytes.
    #[must_use]
    pub(crate) fn cookie_header_bytes(&self) -> usize {
        self.cookie_header_bytes
    }

    /// Records the inbound `Cookie` header size in bytes.
    pub(crate) fn set_cookie_header_bytes(&mut self, bytes: usize) {
        self.cookie_header_bytes = bytes;
    }

    /// Counts one removal that every attempt failed to write.
    pub(crate) fn record_removal_lost(&mut self) {
        self.removal_lost = self.removal_lost.saturating_add(1);
    }

    /// Positive capture IDs counted but never stored (spec 4.7).
    #[must_use]
    pub(crate) fn capture_unpersisted(&self) -> &BTreeMap<(IdentityProviderId, SourceDomain), u32> {
        &self.capture_unpersisted
    }

    /// Counts one positive capture ID that was not stored.
    pub(crate) fn record_unpersisted(
        &mut self,
        provider: IdentityProviderId,
        source: SourceDomain,
    ) {
        let count = self
            .capture_unpersisted
            .entry((provider, source))
            .or_insert(0);
        *count = count.saturating_add(1);
    }

    /// KV reads the capture made (its view read and the removal write's).
    #[must_use]
    pub(crate) fn capture_kv_reads(&self) -> u32 {
        self.capture_kv_reads
    }

    /// KV writes the capture removal write attempted.
    #[must_use]
    pub(crate) fn capture_kv_writes(&self) -> u32 {
        self.capture_kv_writes
    }

    /// Counts KV operations the capture made before send.
    pub(crate) fn record_capture_kv(&mut self, reads: u32, writes: u32) {
        self.capture_kv_reads = self.capture_kv_reads.saturating_add(reads);
        self.capture_kv_writes = self.capture_kv_writes.saturating_add(writes);
    }

    /// Whether a real browser's capture asked for a global revoke (spec 8).
    #[must_use]
    pub(crate) fn global_revoke_requested(&self) -> bool {
        self.global_revoke
    }

    /// Asks finalize to run the consent-withdrawal tombstone path.
    pub(crate) fn request_global_revoke(&mut self) {
        self.global_revoke = true;
    }

    /// Sets the writer decision and the signals captured with it.
    pub(crate) fn set_writer_decision(&mut self, decision: WriterDecision) {
        self.writer = decision.writer;
        self.is_real_browser = decision.is_real_browser;
        self.user_agent = decision.user_agent;
        self.dnt = decision.dnt;
        self.page_url = decision.page_url;
    }

    /// Whether the writer is a `/_ts/page-bids` call, which the interval
    /// since the row's last writer flush paces and which runs no hold-mode
    /// provider (spec 5.1).
    #[must_use]
    pub(crate) fn spa_writer(&self) -> bool {
        self.spa_writer
    }

    /// Marks the writer as the SPA writer.
    pub(crate) fn set_spa_writer(&mut self, spa_writer: bool) {
        self.spa_writer = spa_writer;
    }

    /// Records only the real-browser flag, for a route that is never the writer.
    pub(crate) fn set_real_browser(&mut self, is_real_browser: bool) {
        self.is_real_browser = is_real_browser;
    }

    /// Sets the runtime built once per settings load.
    pub(crate) fn set_runtime(&mut self, runtime: Arc<IdentityRuntime>) {
        self.runtime = Some(runtime);
    }
}

/// The writer decision and the signals captured with it.
pub(crate) struct WriterDecision {
    pub(crate) writer: bool,
    pub(crate) is_real_browser: bool,
    pub(crate) user_agent: Option<String>,
    pub(crate) dnt: bool,
    pub(crate) page_url: Option<Url>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::mutation::PartnerIdChange;
    use crate::ec::identity::revision::Revision;
    use crate::ec::identity::test_support::{pid, record_for_test, source};
    use crate::ec::kv_types::{KvEntry, KvOptOut, KvProviderState};

    #[test]
    fn request_identity_debug_prints_no_uid() {
        let mut identity = RequestIdentity::default();
        identity
            .mutation_mut()
            .add_change(PartnerIdChange::FillMissing {
                source: source("ssp.example.com"),
                record: record_for_test("secret-uid-1", None, 10),
            });
        identity
            .mutation_mut()
            .add_change(PartnerIdChange::OwnerRemove {
                source: source("dsp.example.com"),
                provider: pid("owner"),
                expected: Revision::Unset,
            });

        let printed = format!("{identity:?}");

        assert!(
            !printed.contains("secret-uid-1"),
            "should print no UID: {printed}"
        );
        assert!(
            printed.contains("mutation_changes: 2"),
            "should print the change count: {printed}"
        );
    }

    #[test]
    fn opted_out_reads_ps_o_and_honours_a_later_clear() {
        let mut entry = KvEntry::minimal("ssp.example.com", "uid", 1);
        let opt_out = || KvOptOut {
            at: 100,
            fp: String::new(),
        };
        entry.ps.insert(
            "a".to_owned(),
            KvProviderState {
                o: Some(opt_out()),
                ..KvProviderState::default()
            },
        );
        entry.ps.insert(
            "b".to_owned(),
            KvProviderState {
                o: Some(opt_out()),
                oc: Some(101),
                ..KvProviderState::default()
            },
        );
        entry.ps.insert(
            "c".to_owned(),
            KvProviderState {
                oc: Some(50),
                ..KvProviderState::default()
            },
        );

        let result = opted_out(&entry);

        assert_eq!(
            result,
            BTreeSet::from([pid("a")]),
            "should keep only the provider whose opt-out is not cleared later"
        );
    }
}
