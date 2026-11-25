//! The follow-up write of after-response identity lookups.
//!
//! Providers configured with `collect = "after_response"` are dispatched only
//! after the navigation's response has been sent, so their latency never
//! delays the page. Their validated results reach the row in one follow-up
//! update, guarded by the row's current generation, and are served from the
//! next page load. An update that loses its CAS is re-planned once against a
//! fresh read, in the store's bounded update that
//! [`KvIdentityGraph::commit_after_response`] makes. Browser
//! ingest never runs here: it belongs to the navigation's own write.

use super::coordinator::results::LookupResults;
use super::eid_types::UnixSeconds;
use super::kv::KvIdentityGraph;
use super::kv_types::KvEntry;
use super::page_load::{AfterResponseRejection, AfterResponseWrite, RowChange};
use super::registry::PartnerRegistry;
use super::write_plan::{WritePlanInput, plan_write};
use super::{EcKvSnapshot, current_timestamp, log_id};

/// What the follow-up write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum AfterResponseOutcome {
    /// The row was updated with the after-response results.
    #[display("written")]
    Written,
    /// The results would not change the row.
    #[display("unchanged")]
    Unchanged,
    /// The row is missing or withdrawn, so nothing was written.
    #[display("no_row")]
    NoRow,
    /// The row changed since each read, the retry's included; the results
    /// are dropped.
    #[display("conflict")]
    Conflict,
    /// After a conflict, the re-read returned the generation that lost or no
    /// row, so it was stale; the results are dropped without a second write.
    #[display("stale_read")]
    StaleRead,
    /// Reading the row or writing it failed.
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
/// alone against it, and writes it, guarded by that read's generation.
/// Reading here rather than before dispatch keeps the window in which another
/// write can conflict to the gap between this read and the write. When the
/// write conflicts, [`KvIdentityGraph::commit_after_response`] reads the row
/// and plans the update once more, so the retry sees every write since, a
/// newer opt-out's revocation or a withdrawal's tombstone included, and never
/// resends the stale plan. The `permit` is consumed whether or not a write is
/// made, and at most one write lands.
///
/// Logs one line with the outcome and the number of conflicts.
///
/// # Performance
///
/// At most two KV reads, skipped when no lookup can change the row, and as
/// many conditional KV writes; the second of each only after a conflict.
/// Runs after the response has been sent, so it adds no page latency.
pub async fn commit_after_response_lookups(
    graph: &KvIdentityGraph,
    permit: AfterResponseWrite,
    registry: &PartnerRegistry,
    ec_id: &str,
    lookups: &LookupResults,
) -> AfterResponseOutcome {
    let (outcome, conflicts) = write_lookups(graph, permit, registry, ec_id, lookups).await;
    log::info!("After-response lookups: write outcome={outcome} conflicts={conflicts}");
    outcome
}

/// Plans and writes the follow-up update, returning its outcome and the
/// number of attempts that conflicted.
async fn write_lookups(
    graph: &KvIdentityGraph,
    permit: AfterResponseWrite,
    registry: &PartnerRegistry,
    ec_id: &str,
    lookups: &LookupResults,
) -> (AfterResponseOutcome, u8) {
    // Only found UIDs or cleared sources can change the row. A newly reported
    // opt-out clears every source of its provider, which claims at least one;
    // a remembered one clears only sources the row still stores, so a
    // navigation that finds nothing left to clear needs no re-read.
    let can_change = lookups
        .iter()
        .any(|lookup| !lookup.entries.is_empty() || !lookup.cleared.is_empty());
    if !can_change {
        return (AfterResponseOutcome::Unchanged, 0);
    }
    let commit = graph
        .commit_after_response(permit, ec_id, |snapshot| {
            plan_follow_up(snapshot, registry, ec_id, lookups)
        })
        .await;
    let outcome = match commit.result {
        Ok(()) => AfterResponseOutcome::Written,
        Err(AfterResponseRejection::Skipped(outcome)) => outcome,
        Err(AfterResponseRejection::Conflict) => AfterResponseOutcome::Conflict,
        Err(AfterResponseRejection::StaleRead) => AfterResponseOutcome::StaleRead,
        Err(AfterResponseRejection::StoreFailed(report)) => {
            log::warn!(
                "After-response lookups: the store fails for the row of '{}': {}",
                log_id(ec_id),
                report.current_context()
            );
            AfterResponseOutcome::Failed
        }
        Err(AfterResponseRejection::WriteFailed(report)) => {
            log::warn!(
                "After-response write for '{}' fails: {}",
                log_id(ec_id),
                report.current_context()
            );
            AfterResponseOutcome::Failed
        }
    };
    (outcome, commit.conflicts)
}

/// Plans the update `lookups` make to the row of `ec_id` as `snapshot` holds
/// it.
///
/// Returns the outcome to report when there is nothing to write.
fn plan_follow_up(
    snapshot: &EcKvSnapshot,
    registry: &PartnerRegistry,
    ec_id: &str,
    lookups: &LookupResults,
) -> Result<KvEntry, AfterResponseOutcome> {
    if !matches!(snapshot, EcKvSnapshot::Present { .. }) {
        log::info!(
            "After-response lookups: no row for '{}', skipping write",
            log_id(ec_id)
        );
        return Err(AfterResponseOutcome::NoRow);
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
        return Err(match snapshot.entry_for(ec_id) {
            Some(entry) if entry.consent.ok => AfterResponseOutcome::Unchanged,
            _ => AfterResponseOutcome::NoRow,
        });
    };
    // The store guards the write with the generation it read, which is the
    // one this plan expects.
    let RowChange::Update { entry, .. } = change else {
        log::warn!(
            "After-response lookups: unexpected row change for '{}', skipping write",
            log_id(ec_id)
        );
        return Err(AfterResponseOutcome::Failed);
    };
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::coordinator::results::{
        DropCounts, LookupStatus, OptOutMemory, ProviderLookup, SkipReason,
    };
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
    use crate::ec::kv_backend::EcKvStore;
    use crate::ec::kv_backend::test_support::{
        ConflictInjectingEcKv, FailingEcKv, InMemoryEcKv, WriteFailingEcKv,
    };
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
    use crate::settings::EcPartner;
    use std::sync::Arc;

    /// A graph over `store`, seeded with a live row, sharing the store so a
    /// test can inspect it afterwards.
    async fn seeded_graph(store: &Arc<ConflictInjectingEcKv>) -> KvIdentityGraph {
        store.seed(EC_ID, &live_entry()).await;
        KvIdentityGraph::shared_with_record_update(
            Arc::clone(store) as Arc<dyn EcKvStore + Send + Sync>
        )
    }

    /// A store where `race` changes the row before each of the first
    /// `conflicts` conditional writes.
    fn racing(
        conflicts: u32,
        race: impl Fn(&mut KvEntry) + Send + Sync + 'static,
    ) -> Arc<ConflictInjectingEcKv> {
        Arc::new(ConflictInjectingEcKv::racing(conflicts, race))
    }

    fn browser_write(entry: &mut KvEntry) {
        entry.ids.insert(
            "browser.example.com".to_owned(),
            KvPartnerId::single("b-2".to_owned()),
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
    async fn a_conflict_is_retried_against_a_fresh_read_of_the_row() {
        // Arrange: another write lands between the first read and its CAS.
        let store = racing(1, browser_write);
        let graph = seeded_graph(&store).await;

        // Act
        let (outcome, conflicts) = write_lookups(
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
            "should land the retry after one conflict"
        );
        assert_eq!(conflicts, 1, "should count the first attempt's conflict");
        assert_eq!(
            store.conflicts_remaining(),
            0,
            "should have raced the first write"
        );
        assert_eq!(
            stored_uid(&graph, "browser.example.com").await.as_deref(),
            Some("b-2"),
            "should keep the write that won the first CAS"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").await.as_deref(),
            Some("p-1"),
            "should store the provider UID on the retry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_second_conflict_gives_up_without_a_third_attempt() {
        // Arrange: other writes race every attempt.
        let store = racing(3, browser_write);
        let graph = seeded_graph(&store).await;

        // Act
        let (outcome, conflicts) = write_lookups(
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
            AfterResponseOutcome::Conflict,
            "should drop the results after the retry conflicts"
        );
        assert_eq!(conflicts, 2, "should count both attempts' conflicts");
        assert_eq!(
            store.conflicts_remaining(),
            1,
            "should make exactly two write attempts"
        );
        assert!(
            stored_uid(&graph, "ids.example.com").await.is_none(),
            "should not store the provider UID"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_stale_re_read_stops_without_a_second_write() {
        // Arrange: the first CAS loses, but the row keeps the generation the
        // write expected, as an eventually consistent store's stale read
        // returns it.
        let store = Arc::new(ConflictInjectingEcKv::new(2));
        let graph = seeded_graph(&store).await;

        // Act
        let (outcome, conflicts) = write_lookups(
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
            AfterResponseOutcome::StaleRead,
            "should report a re-read of the lost generation as stale"
        );
        assert_eq!(conflicts, 1, "should count the first attempt's conflict");
        assert_eq!(
            store.conflicts_remaining(),
            1,
            "should not attempt a second write against the stale generation"
        );
        assert!(
            stored_uid(&graph, "ids.example.com").await.is_none(),
            "should not store the provider UID"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_missing_re_read_after_a_conflict_stops_as_stale() {
        // Arrange: the first CAS loses, which proves the row exists, but the
        // re-read finds no row, as a stale replica can.
        let store = Arc::new(ConflictInjectingEcKv::new(2).missing_reads_after_conflict());
        let graph = seeded_graph(&store).await;

        // Act
        let (outcome, conflicts) = write_lookups(
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
            AfterResponseOutcome::StaleRead,
            "should report a missing re-read after a conflict as stale, not as no row"
        );
        assert_eq!(conflicts, 1, "should count the first attempt's conflict");
        assert_eq!(
            store.conflicts_remaining(),
            1,
            "should not attempt a second write"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_re_read_reports_failed() {
        // Arrange: the first CAS loses and every read after it fails.
        let store = Arc::new(
            ConflictInjectingEcKv::racing(1, browser_write).failing_reads_after_conflict(),
        );
        let graph = seeded_graph(&store).await;

        // Act
        let (outcome, conflicts) = write_lookups(
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
            AfterResponseOutcome::Failed,
            "should report a failed re-read as failed, not as a missing row"
        );
        assert_eq!(conflicts, 1, "should count the first attempt's conflict");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_read_reports_failed() {
        // Arrange
        let graph = KvIdentityGraph::with_record_update(FailingEcKv::new("after-response-failing"));

        // Act
        let (outcome, conflicts) = write_lookups(
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
            AfterResponseOutcome::Failed,
            "should report a failed read as failed, not as a missing row"
        );
        assert_eq!(conflicts, 0, "should count no conflict");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_store_error_is_not_retried() {
        // Arrange: reads succeed, every write fails.
        let store = Arc::new(WriteFailingEcKv::new());
        let (body, metadata) =
            KvIdentityGraph::serialize_entry_for_test(&live_entry(), "write-failing-store")
                .expect("should serialize the seeded row");
        store.seed(EC_ID, &body, &metadata).await;
        let graph = KvIdentityGraph::shared_with_record_update(
            Arc::clone(&store) as Arc<dyn EcKvStore + Send + Sync>
        );

        // Act
        let (outcome, conflicts) = write_lookups(
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
            AfterResponseOutcome::Failed,
            "should report the store error"
        );
        assert_eq!(conflicts, 0, "should count no conflict");
        assert_eq!(store.inserts(), 1, "should not retry a store error");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_retry_discards_lookups_superseded_by_an_opt_out_since_dispatch() {
        // Arrange: a cookie opt-out records the provider's revocation between
        // the first read and its CAS; the lookups were dispatched before it.
        let store = racing(1, |entry| {
            entry
                .revocations
                .insert(ProviderId::new("example-provider"), current_timestamp());
        });
        let graph = seeded_graph(&store).await;

        // Act
        let (outcome, conflicts) = write_lookups(
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
            AfterResponseOutcome::Unchanged,
            "should re-plan against the opt-out and find nothing to write"
        );
        assert_eq!(conflicts, 1, "should count the first attempt's conflict");
        assert!(
            stored_uid(&graph, "ids.example.com").await.is_none(),
            "should not restore the opted-out provider's UID"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_retry_never_revives_a_row_withdrawn_since_the_first_read() {
        // Arrange: consent is withdrawn between the first read and its CAS.
        let store = racing(1, |entry| {
            *entry = KvEntry::tombstone(1000);
        });
        let graph = seeded_graph(&store).await;

        // Act
        let (outcome, conflicts) = write_lookups(
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
            AfterResponseOutcome::NoRow,
            "should skip the retry on a withdrawn row"
        );
        assert_eq!(conflicts, 1, "should count the first attempt's conflict");
        assert!(
            stored_uid(&graph, "ids.example.com").await.is_none(),
            "should not store the provider UID on a tombstone"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_store_without_a_conditional_update_writes_last_write_wins() {
        // Arrange: the store has no bounded conditional update.
        let graph = KvIdentityGraph::new(InMemoryEcKv::new("last-write-wins-store"));
        graph
            .create(EC_ID, &live_entry())
            .await
            .expect("should seed row");

        // Act
        let (outcome, conflicts) = write_lookups(
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
            "should fall back to an unconditional overwrite"
        );
        assert_eq!(conflicts, 0, "should count no conflict");
        assert_eq!(
            stored_uid(&graph, "ids.example.com").await.as_deref(),
            Some("p-1"),
            "should store the provider UID"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_last_write_wins_fallback_still_skips_a_withdrawn_row() {
        // Arrange: a withdrawn row on a store without a conditional update.
        let graph = KvIdentityGraph::new(InMemoryEcKv::new("last-write-wins-store"));
        graph
            .create(EC_ID, &KvEntry::tombstone(1000))
            .await
            .expect("should seed the tombstone");

        // Act
        let (outcome, _conflicts) = write_lookups(
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
            AfterResponseOutcome::NoRow,
            "should plan against the row it read and skip the tombstone"
        );
        assert!(
            stored_uid(&graph, "ids.example.com").await.is_none(),
            "should not store the provider UID on the tombstone"
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
        let opted_out = opt_out(
            LookupStatus::OptedOut,
            vec![SourceDomain::try_from("ids.example.com").expect("should build source")],
        );

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

    #[tokio::test(flavor = "current_thread")]
    async fn a_delayed_lookup_does_not_restore_a_source_after_a_newer_cookie_opt_out() {
        // Arrange: the provider's UID is stored; a lookup is dispatched
        // against that row, then a newer request's cookie opt-out clears the
        // source before the lookup's result is written.
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
            &found(EC_ID, "p-before-optout"),
        )
        .await;
        assert_eq!(
            seeded,
            AfterResponseOutcome::Written,
            "should store the provider UID first"
        );
        let dispatched_revocations = graph
            .load_snapshot(EC_ID)
            .await
            .entry_for(EC_ID)
            .map(|entry| entry.revocations.clone())
            .unwrap_or_default();
        let delayed = found(EC_ID, "late-result").dispatched_with(dispatched_revocations);
        let cleared = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &opt_out(
                LookupStatus::OptedOut,
                vec![SourceDomain::try_from("ids.example.com").expect("should build source")],
            ),
        )
        .await;
        assert_eq!(
            cleared,
            AfterResponseOutcome::Written,
            "should clear the source on the cookie opt-out"
        );

        // Act
        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &delayed,
        )
        .await;

        // Assert
        assert_eq!(
            outcome,
            AfterResponseOutcome::Unchanged,
            "should discard the delayed result"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").await,
            None,
            "should keep the opted-out source cleared"
        );
    }

    fn opt_out(status: LookupStatus, cleared: Vec<SourceDomain>) -> LookupResults {
        LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status,
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
            &opt_out(LookupStatus::Skipped(SkipReason::OptedOut), Vec::new()),
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
