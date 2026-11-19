//! The follow-up write of after-response identity lookups.
//!
//! Providers configured with `collect = "after_response"` are dispatched only
//! after the navigation's response has been sent, so their latency never
//! delays the page. Their validated results reach the row in one follow-up
//! update, guarded by the row's current generation, and are served from the
//! next page load. Browser ingest never runs here: it belongs to the
//! navigation's own write.

use std::net::IpAddr;
use std::sync::Arc;

use edgezero_core::http::{Request, header};
use edgezero_core::outbound::HttpClient;

use super::EcContext;
use super::app::IdentityApp;
use super::coordinator::transport::OutboundLookupTransport;
use super::coordinator::{IdentityCoordinator as _, LookupContext, ProviderCoordinator};
use super::provider::{ClientSignals, RequestCookies};

use super::coordinator::results::LookupResults;
use super::eid_types::UnixSeconds;
use super::kv::KvIdentityGraph;
use super::page_load::{AfterResponseWrite, RowChange};
use super::registry::PartnerRegistry;
use super::write_plan::{WritePlanInput, plan_write};
use super::{EcKvSnapshot, current_timestamp, log_id};

/// Owned request signals for identity lookups dispatched after response delivery.
///
/// Adapters must retain their delivery gate and supply the finalized navigation's
/// single-use [`AfterResponseWrite`] permit to [`Self::run`].
pub struct AfterResponseLookups {
    /// Coordinator built once for the after-response phase.
    pub coordinator: Arc<ProviderCoordinator>,
    /// EC captured before routing consumed the request.
    pub ec_id: String,
    /// Trusted raw client address, preserving IPv6 separators.
    pub client_ip: Option<IpAddr>,
    /// Request user agent.
    pub user_agent: Option<String>,
    /// All request Cookie fields joined with semicolons.
    pub cookie_header: String,
}

impl AfterResponseLookups {
    /// Captures provider inputs before the publisher consumes the request.
    ///
    /// The caller applies its browser, navigation and available-store gates.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let work = AfterResponseLookups::capture(coordinator, ec_id, client_ip, &request);
    /// ```
    #[must_use]
    pub fn capture(
        coordinator: Arc<ProviderCoordinator>,
        ec_id: String,
        client_ip: Option<IpAddr>,
        request: &Request,
    ) -> Self {
        Self {
            coordinator,
            ec_id,
            client_ip,
            user_agent: request
                .headers()
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            cookie_header: request
                .headers()
                .get_all(header::COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .collect::<Vec<_>>()
                .join("; "),
        }
    }

    /// Runs eligible lookups and their guarded follow-up write after delivery.
    ///
    /// The permit must come from successful page finalization. Reads the live
    /// row before disclosure and again before the generation-guarded write.
    /// Outbound requests start here, inside the adapter's post-response scope.
    ///
    /// # Performance
    ///
    /// Skips storage reads when nothing is due; otherwise loads before lookup
    /// and before the conditional write. Adds no page-response latency.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// work.run(&ec_context, permit, &identity, &client).await;
    /// ```
    pub async fn run(
        self,
        ec_context: &EcContext,
        permit: AfterResponseWrite,
        identity: &IdentityApp,
        client: &HttpClient,
    ) {
        let lookups = self;
        if ec_context.ec_value() != Some(lookups.ec_id.as_str()) {
            log::debug!("After-response lookups skipped: EC changed during the request");
            return;
        }
        // The request snapshot holds the row as finalize left it. A source held
        // there is still held in the live row unless consent was withdrawn, which
        // blocks the lookup anyway, so the live read is skipped when nothing is due.
        // Only pull sync can be skipped: configured providers always run, so their
        // cookie opt-outs and regional exclusions are processed.
        let now = UnixSeconds::new(current_timestamp());
        let request_entry = ec_context.kv_snapshot().entry_for(&lookups.ec_id);
        if !lookups.coordinator.any_due(request_entry, now) {
            log::debug!("After-response lookups skipped: nothing due");
            return;
        }
        let Some(graph) = identity.identity_graph() else {
            return;
        };
        let snapshot = graph.load_snapshot(&lookups.ec_id).await;
        if !after_response_lookups_allowed(&snapshot, &lookups.ec_id, ec_context.ec_generated()) {
            log::debug!("After-response lookups skipped: new EC or no live row");
            return;
        }
        let transport = OutboundLookupTransport::new(client);
        let pending = lookups
            .coordinator
            .dispatch(
                &transport,
                &LookupContext {
                    ec_id: &lookups.ec_id,
                    snapshot: &snapshot,
                    minted_entry: None,
                    consent: ec_context.consent(),
                    client: ClientSignals {
                        ip: lookups.client_ip,
                        user_agent: lookups.user_agent.as_deref(),
                    },
                    publisher_domain: &identity.settings().publisher.domain,
                    cookies: RequestCookies::new(&lookups.cookie_header),
                    rate_limiter: identity.rate_limiter(),
                    now,
                },
            )
            .await;
        if pending.is_empty() {
            return;
        }
        let results = lookups.coordinator.collect(&transport, pending).await;
        let outcome = commit_after_response_lookups(
            &graph,
            permit,
            identity.partners(),
            &lookups.ec_id,
            &results,
        )
        .await;
        log::info!("After-response lookups: write outcome={outcome}");
    }
}

/// What the follow-up write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum AfterResponseOutcome {
    /// The row was updated with the after-response results.
    #[display("written")]
    Written,
    /// The results would not change the row.
    #[display("unchanged")]
    Unchanged,
    /// The row is missing, withdrawn or unreadable, so nothing was written.
    #[display("no_row")]
    NoRow,
    /// The row changed since it was read; the results are dropped.
    #[display("conflict")]
    Conflict,
    /// The store rejected the write.
    #[display("failed")]
    Failed,
}

/// Whether after-response lookups may be dispatched for `ec_id`.
///
/// They run only for an EC that existed before this navigation and whose row,
/// read after the response was sent, is live and consented. An EC minted on
/// this navigation is skipped: no partner can know it yet, and its new row
/// may not be readable so soon. Its lookups run from the next navigation.
#[must_use]
pub fn after_response_lookups_allowed(
    snapshot: &EcKvSnapshot,
    ec_id: &str,
    ec_generated: bool,
) -> bool {
    !ec_generated
        && snapshot
            .entry_for(ec_id)
            .is_some_and(|entry| entry.consent.ok)
}

/// Applies after-response lookup results to the row of `ec_id`.
///
/// Re-reads the row just before writing, plans an update from `lookups`
/// alone against it, and writes it once, guarded by that read's generation.
/// Reading here rather than before dispatch keeps the window in which another
/// write can conflict to the gap between this read and the write. The
/// `permit` is consumed whether or not a write is made.
///
/// # Performance
///
/// At most one KV read, skipped when no lookup can change the row, and at
/// most one conditional KV write. Runs after the response has been sent, so
/// it adds no page latency.
pub async fn commit_after_response_lookups(
    graph: &KvIdentityGraph,
    permit: AfterResponseWrite,
    registry: &PartnerRegistry,
    ec_id: &str,
    lookups: &LookupResults,
) -> AfterResponseOutcome {
    // Only found UIDs or cleared sources can change the row. A newly reported
    // opt-out clears every source of its provider, which claims at least one;
    // a remembered one clears only sources the row still stores, so a
    // navigation that finds nothing left to clear needs no re-read.
    let can_change = lookups
        .iter()
        .any(|lookup| !lookup.entries.is_empty() || !lookup.cleared.is_empty());
    if !can_change {
        return AfterResponseOutcome::Unchanged;
    }
    let snapshot = &graph.load_snapshot(ec_id).await;
    if !matches!(snapshot, EcKvSnapshot::Present { .. }) {
        log::info!(
            "After-response lookups: no row for '{}', skipping write",
            log_id(ec_id)
        );
        return AfterResponseOutcome::NoRow;
    }
    let change = plan_write(&WritePlanInput {
        ec_id,
        snapshot,
        new_entry: None,
        browser_ingest: &[],
        lookups,
        registry,
        now: UnixSeconds::new(current_timestamp()),
    });
    let Some(change) = change else {
        // A withdrawn row also plans no change.
        return match snapshot.entry_for(ec_id) {
            Some(entry) if entry.consent.ok => AfterResponseOutcome::Unchanged,
            _ => AfterResponseOutcome::NoRow,
        };
    };
    let RowChange::Update { entry, expected } = change else {
        log::warn!(
            "After-response lookups: unexpected row change for '{}', skipping write",
            log_id(ec_id)
        );
        return AfterResponseOutcome::NoRow;
    };
    match graph
        .commit_after_response(permit, ec_id, &entry, expected)
        .await
    {
        Ok(()) => AfterResponseOutcome::Written,
        Err(report) => {
            log::warn!(
                "After-response write failed for '{}': {}",
                log_id(ec_id),
                report.current_context()
            );
            match report.current_context() {
                super::page_load::KvWriteError::Conflict => AfterResponseOutcome::Conflict,
                _ => AfterResponseOutcome::Failed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::coordinator::results::{
        DropCounts, LookupStatus, OptOutMemory, ProviderLookup, SkipReason,
    };
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
    use crate::ec::kv::TombstoneOutcome;
    use crate::ec::kv_types::{KvEid, KvEntry, KvPartnerId, KvUid};
    use crate::ec::page_load::{Generation, KvWriteError};
    use crate::settings::EcPartner;

    #[test]
    fn captured_work_preserves_raw_signals_and_all_cookie_headers() {
        let request = edgezero_core::http::request_builder()
            .uri("https://publisher.example.com/")
            .header("user-agent", "example-browser")
            .header("cookie", "first=one")
            .header("cookie", "second=two")
            .body(edgezero_core::body::Body::empty())
            .expect("should build request");
        let coordinator = std::sync::Arc::new(
            super::super::coordinator::ProviderCoordinator::new(
                vec![],
                std::sync::Arc::new(registry()),
                std::time::Duration::from_secs(1),
            )
            .expect("should build coordinator"),
        );
        let ip = "2001:db8::1234".parse().expect("should parse raw IPv6");
        let work = AfterResponseLookups::capture(coordinator, EC_ID.to_owned(), Some(ip), &request);
        assert_eq!(work.client_ip, Some(ip), "should preserve IPv6 separators");
        assert_eq!(
            work.cookie_header, "first=one; second=two",
            "should join split cookies"
        );
        assert_eq!(
            work.user_agent.as_deref(),
            Some("example-browser"),
            "should retain user agent"
        );
    }

    const EC_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

    fn registry() -> PartnerRegistry {
        let partners: Vec<EcPartner> = ["browser.example.com", "ids.example.com"]
            .iter()
            .map(|source| {
                serde_json::from_value(serde_json::json!({
                    "name": source,
                    "source_domain": source,
                    "bidstream_enabled": true,
                }))
                .expect("should build partner")
            })
            .collect();
        let mut registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        assert!(
            registry.mark_provider_owned("ids.example.com", None),
            "should mark the provider source as owned"
        );
        registry
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }

    fn found(ec_id: &str, id: &str) -> LookupResults {
        let uid = KvUid::new(
            UidValue::try_from(id).expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            Expiry::Never,
        );
        LookupResults::new(
            ec_id,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Found {
                    kept: 1,
                    dropped: DropCounts::default(),
                },
                entries: vec![(
                    SourceDomain::try_from("ids.example.com").expect("should build source"),
                    vec![KvEid::new(EidFields::default(), vec![uid])],
                )],
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        )
    }

    async fn stored_uid(graph: &KvIdentityGraph, source: &str) -> Option<String> {
        graph
            .load_snapshot(EC_ID)
            .await
            .entry_for(EC_ID)
            .and_then(|entry| entry.ids.get(source).map(|record| record.uid.clone()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn results_with_nothing_to_persist_skip_the_row_read() {
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let graph = KvIdentityGraph::counting("counting-store", std::sync::Arc::clone(&reads));
        let not_due = LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Skipped(SkipReason::NotDue),
                entries: Vec::new(),
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        );

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &not_due,
        )
        .await;

        assert_eq!(
            outcome,
            AfterResponseOutcome::Unchanged,
            "should report nothing to write"
        );
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "should not re-read the row when every provider was not due"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_after_response_results_to_an_existing_row() {
        let graph = KvIdentityGraph::in_memory("after-response");
        let mut entry = live_entry();
        entry.ids.insert(
            "browser.example.com".to_owned(),
            KvPartnerId::single("b-1".to_owned()),
        );
        graph.create(EC_ID, &entry).await.expect("should seed row");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        )
        .await;

        assert_eq!(
            outcome,
            AfterResponseOutcome::Written,
            "should write the row"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").await.as_deref(),
            Some("p-1"),
            "should store the provider UID"
        );
        assert_eq!(
            stored_uid(&graph, "browser.example.com").await.as_deref(),
            Some("b-1"),
            "should keep the browser UID written by the page load"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_against_the_row_as_read_just_before_the_write() {
        // Arrange
        let graph = KvIdentityGraph::in_memory("after-response-reread");
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");
        let read_before_dispatch = graph.load_snapshot(EC_ID).await;
        graph
            .upsert_partner_id(EC_ID, "browser.example.com", "b-2")
            .await
            .expect("should rewrite the row as another request would");
        assert_ne!(
            graph.load_snapshot(EC_ID).await,
            read_before_dispatch,
            "should change the row after the dispatch read"
        );

        // Act
        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        )
        .await;

        // Assert
        assert_eq!(
            outcome,
            AfterResponseOutcome::Written,
            "should not conflict with a write made while the lookups ran"
        );
        assert_eq!(
            stored_uid(&graph, "browser.example.com").await.as_deref(),
            Some("b-2"),
            "should keep the other request's write"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").await.as_deref(),
            Some("p-1"),
            "should store the provider UID"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn follow_up_write_conflicts_when_the_row_changed_after_its_read() {
        // Arrange
        let graph = KvIdentityGraph::in_memory("after-response-conflict");
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");
        let read = graph.load_snapshot(EC_ID).await;
        let expected = Generation::of(&read, EC_ID).expect("should read a generation");
        graph
            .upsert_partner_id(EC_ID, "browser.example.com", "b-2")
            .await
            .expect("should rewrite the row as another request would");

        // Act
        let result = graph
            .commit_after_response(
                AfterResponseWrite::for_test(),
                EC_ID,
                &live_entry(),
                expected,
            )
            .await;

        // Assert
        let report = result.expect_err("should refuse a stale generation");
        assert!(
            matches!(report.current_context(), KvWriteError::Conflict),
            "should report a conflict"
        );
        assert_eq!(
            stored_uid(&graph, "browser.example.com").await.as_deref(),
            Some("b-2"),
            "should keep the other request's write"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn follow_up_write_never_revives_a_row_withdrawn_after_its_read() {
        // Arrange: scenario 23, the row is tombstoned between read and CAS.
        let graph = KvIdentityGraph::in_memory("after-response-withdrawn");
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");
        let read = graph.load_snapshot(EC_ID).await;
        let expected = Generation::of(&read, EC_ID).expect("should read a generation");
        assert_eq!(
            graph
                .write_withdrawal_tombstone(EC_ID, drop)
                .await
                .expect("should withdraw the row"),
            TombstoneOutcome::Written,
            "should tombstone the row"
        );

        // Act
        let result = graph
            .commit_after_response(
                AfterResponseWrite::for_test(),
                EC_ID,
                &live_entry(),
                expected,
            )
            .await;

        // Assert
        assert!(result.is_err(), "should refuse to overwrite the tombstone");
        assert!(
            graph
                .load_snapshot(EC_ID)
                .await
                .entry_for(EC_ID)
                .is_none_or(|entry| !entry.consent.ok),
            "should leave the row withdrawn"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replaces_a_browser_fallback_for_an_owned_source() {
        let graph = KvIdentityGraph::in_memory("after-response");
        let mut entry = live_entry();
        entry.ids.insert(
            "ids.example.com".to_owned(),
            KvPartnerId::single("from-browser".to_owned()),
        );
        graph.create(EC_ID, &entry).await.expect("should seed row");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        )
        .await;

        assert_eq!(
            outcome,
            AfterResponseOutcome::Written,
            "should write the row"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").await.as_deref(),
            Some("p-1"),
            "should replace the browser fallback with the provider UID"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn skips_the_write_when_the_row_already_holds_the_results() {
        let graph = KvIdentityGraph::in_memory("after-response");
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");
        let first = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        )
        .await;
        assert_eq!(first, AfterResponseOutcome::Written, "should write first");

        let second = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        )
        .await;

        assert_eq!(
            second,
            AfterResponseOutcome::Unchanged,
            "should not rewrite identical results"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_nothing_without_a_row() {
        let graph = KvIdentityGraph::in_memory("after-response");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        )
        .await;

        assert_eq!(
            outcome,
            AfterResponseOutcome::NoRow,
            "should not create a row"
        );
        assert!(
            graph.load_snapshot(EC_ID).await.entry_for(EC_ID).is_none(),
            "should leave the row absent"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_nothing_to_a_withdrawn_row() {
        let graph = KvIdentityGraph::in_memory("after-response");
        graph
            .create(EC_ID, &KvEntry::tombstone(1000))
            .await
            .expect("should seed tombstone");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        )
        .await;

        assert_eq!(
            outcome,
            AfterResponseOutcome::NoRow,
            "should not revive a tombstone"
        );
        assert!(
            stored_uid(&graph, "ids.example.com").await.is_none(),
            "should not store the provider UID on a tombstone"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drops_results_made_for_another_ec() {
        let graph = KvIdentityGraph::in_memory("after-response");
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");
        let other = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.ABC123";

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(other, "p-1"),
        )
        .await;

        assert_eq!(
            outcome,
            AfterResponseOutcome::Unchanged,
            "should discard results dispatched for another EC"
        );
        assert!(
            stored_uid(&graph, "ids.example.com").await.is_none(),
            "should not store results for another EC"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn skips_the_write_without_results() {
        let graph = KvIdentityGraph::in_memory("after-response");
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &LookupResults::new(EC_ID, Vec::new()),
        )
        .await;

        assert_eq!(
            outcome,
            AfterResponseOutcome::Unchanged,
            "should make no write without results"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn skipped_results_need_no_re_read() {
        // Arrange: no row, so a re-read would report `NoRow`.
        let graph = KvIdentityGraph::in_memory("after-response");
        let skipped = LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Skipped(SkipReason::NotDue),
                entries: Vec::new(),
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        );

        // Act
        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &skipped,
        )
        .await;

        // Assert
        assert_eq!(
            outcome,
            AfterResponseOutcome::Unchanged,
            "should return before reading the row"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_opt_out_alone_still_re_reads_and_clears_the_source() {
        // Arrange: the provider's UID is stored, then it reports an opt-out.
        let graph = KvIdentityGraph::in_memory("after-response");
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");
        let seeded = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        )
        .await;
        assert_eq!(
            seeded,
            AfterResponseOutcome::Written,
            "should store the provider UID first"
        );
        let opted_out = opt_out(vec![
            SourceDomain::try_from("ids.example.com").expect("should build source"),
        ]);

        // Act
        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &opted_out,
        )
        .await;

        // Assert
        assert_eq!(
            outcome,
            AfterResponseOutcome::Written,
            "should write the opt-out"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").await,
            None,
            "should clear the opted-out provider's source"
        );
    }

    fn opt_out(cleared: Vec<SourceDomain>) -> LookupResults {
        LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Skipped(SkipReason::OptedOut),
                entries: Vec::new(),
                cleared,
                opt_out: OptOutMemory::Unchanged,
            }],
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_remembered_opt_out_with_nothing_to_clear_skips_the_re_read() {
        // Arrange: no row, so a re-read would report `NoRow`.
        let graph = KvIdentityGraph::in_memory("after-response");

        // Act
        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &opt_out(Vec::new()),
        )
        .await;

        // Assert
        assert_eq!(
            outcome,
            AfterResponseOutcome::Unchanged,
            "should return before reading the row"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lookups_run_only_for_an_existing_consented_row() {
        // Arrange
        let graph = KvIdentityGraph::in_memory("after_response_allowed");
        let missing = graph.load_snapshot(EC_ID).await;
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");
        let present = graph.load_snapshot(EC_ID).await;
        let withdrawn_graph = KvIdentityGraph::in_memory("after_response_withdrawn");
        withdrawn_graph
            .create(EC_ID, &KvEntry::tombstone(1000))
            .await
            .expect("should seed tombstone");
        let withdrawn = withdrawn_graph.load_snapshot(EC_ID).await;

        // Act + Assert
        assert!(
            after_response_lookups_allowed(&present, EC_ID, false),
            "should run for a returning EC with a live row"
        );
        assert!(
            !after_response_lookups_allowed(&present, EC_ID, true),
            "should not run for an EC minted on this navigation"
        );
        assert!(
            !after_response_lookups_allowed(&missing, EC_ID, false),
            "should not run when the row does not exist"
        );
        assert!(
            !after_response_lookups_allowed(&withdrawn, EC_ID, false),
            "should not run for a withdrawn row"
        );
    }
}
