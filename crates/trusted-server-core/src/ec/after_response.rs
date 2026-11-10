//! The follow-up write of after-response identity lookups.
//!
//! Providers configured with `collect = "after_response"` are dispatched only
//! after the navigation's response has been sent, so their latency never
//! delays the page. Their validated results reach the row in one follow-up
//! update, guarded by the row's current generation, and are served from the
//! next page load. Browser ingest never runs here: it belongs to the
//! navigation's own write.

use super::coordinator::results::LookupResults;
use super::eid_types::UnixSeconds;
use super::kv::KvIdentityGraph;
use super::page_load::{AfterResponseWrite, RowChange};
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
/// One KV read and at most one conditional KV write. Runs after the response
/// has been sent, so it adds no page latency.
#[must_use]
pub fn commit_after_response_lookups(
    graph: &KvIdentityGraph,
    permit: AfterResponseWrite,
    registry: &PartnerRegistry,
    ec_id: &str,
    lookups: &LookupResults,
) -> AfterResponseOutcome {
    if lookups.is_empty() {
        return AfterResponseOutcome::Unchanged;
    }
    let snapshot = &graph.load_snapshot(ec_id);
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
    match graph.commit_after_response(permit, ec_id, &entry, expected) {
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
    use crate::ec::coordinator::results::{DropCounts, LookupStatus, ProviderLookup};
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
    use crate::ec::kv::TombstoneOutcome;
    use crate::ec::kv_types::{KvEid, KvEntry, KvPartnerId, KvUid};
    use crate::ec::page_load::{Generation, KvWriteError};
    use crate::settings::EcPartner;

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
            }],
        )
    }

    fn stored_uid(graph: &KvIdentityGraph, source: &str) -> Option<String> {
        graph
            .load_snapshot(EC_ID)
            .entry_for(EC_ID)
            .and_then(|entry| entry.ids.get(source).map(|record| record.uid.clone()))
    }

    #[test]
    fn writes_after_response_results_to_an_existing_row() {
        let graph = KvIdentityGraph::in_memory("after-response");
        let mut entry = live_entry();
        entry.ids.insert(
            "browser.example.com".to_owned(),
            KvPartnerId::single("b-1".to_owned()),
        );
        graph.create(EC_ID, &entry).expect("should seed row");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        );

        assert_eq!(
            outcome,
            AfterResponseOutcome::Written,
            "should write the row"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").as_deref(),
            Some("p-1"),
            "should store the provider UID"
        );
        assert_eq!(
            stored_uid(&graph, "browser.example.com").as_deref(),
            Some("b-1"),
            "should keep the browser UID written by the page load"
        );
    }

    #[test]
    fn writes_against_the_row_as_read_just_before_the_write() {
        // Arrange
        let graph = KvIdentityGraph::in_memory("after-response-reread");
        graph.create(EC_ID, &live_entry()).expect("should seed row");
        let read_before_dispatch = graph.load_snapshot(EC_ID);
        graph
            .upsert_partner_id(EC_ID, "browser.example.com", "b-2")
            .expect("should rewrite the row as another request would");
        assert_ne!(
            graph.load_snapshot(EC_ID),
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
        );

        // Assert
        assert_eq!(
            outcome,
            AfterResponseOutcome::Written,
            "should not conflict with a write made while the lookups ran"
        );
        assert_eq!(
            stored_uid(&graph, "browser.example.com").as_deref(),
            Some("b-2"),
            "should keep the other request's write"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").as_deref(),
            Some("p-1"),
            "should store the provider UID"
        );
    }

    #[test]
    fn follow_up_write_conflicts_when_the_row_changed_after_its_read() {
        // Arrange
        let graph = KvIdentityGraph::in_memory("after-response-conflict");
        graph.create(EC_ID, &live_entry()).expect("should seed row");
        let read = graph.load_snapshot(EC_ID);
        let expected = Generation::of(&read, EC_ID).expect("should read a generation");
        graph
            .upsert_partner_id(EC_ID, "browser.example.com", "b-2")
            .expect("should rewrite the row as another request would");

        // Act
        let result = graph.commit_after_response(
            AfterResponseWrite::for_test(),
            EC_ID,
            &live_entry(),
            expected,
        );

        // Assert
        let report = result.expect_err("should refuse a stale generation");
        assert!(
            matches!(report.current_context(), KvWriteError::Conflict),
            "should report a conflict"
        );
        assert_eq!(
            stored_uid(&graph, "browser.example.com").as_deref(),
            Some("b-2"),
            "should keep the other request's write"
        );
    }

    #[test]
    fn follow_up_write_never_revives_a_row_withdrawn_after_its_read() {
        // Arrange: scenario 23, the row is tombstoned between read and CAS.
        let graph = KvIdentityGraph::in_memory("after-response-withdrawn");
        graph.create(EC_ID, &live_entry()).expect("should seed row");
        let read = graph.load_snapshot(EC_ID);
        let expected = Generation::of(&read, EC_ID).expect("should read a generation");
        assert_eq!(
            graph
                .write_withdrawal_tombstone(EC_ID, drop)
                .expect("should withdraw the row"),
            TombstoneOutcome::Written,
            "should tombstone the row"
        );

        // Act
        let result = graph.commit_after_response(
            AfterResponseWrite::for_test(),
            EC_ID,
            &live_entry(),
            expected,
        );

        // Assert
        assert!(result.is_err(), "should refuse to overwrite the tombstone");
        assert!(
            graph
                .load_snapshot(EC_ID)
                .entry_for(EC_ID)
                .is_none_or(|entry| !entry.consent.ok),
            "should leave the row withdrawn"
        );
    }

    #[test]
    fn replaces_a_browser_fallback_for_an_owned_source() {
        let graph = KvIdentityGraph::in_memory("after-response");
        let mut entry = live_entry();
        entry.ids.insert(
            "ids.example.com".to_owned(),
            KvPartnerId::single("from-browser".to_owned()),
        );
        graph.create(EC_ID, &entry).expect("should seed row");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        );

        assert_eq!(
            outcome,
            AfterResponseOutcome::Written,
            "should write the row"
        );
        assert_eq!(
            stored_uid(&graph, "ids.example.com").as_deref(),
            Some("p-1"),
            "should replace the browser fallback with the provider UID"
        );
    }

    #[test]
    fn skips_the_write_when_the_row_already_holds_the_results() {
        let graph = KvIdentityGraph::in_memory("after-response");
        graph.create(EC_ID, &live_entry()).expect("should seed row");
        let first = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        );
        assert_eq!(first, AfterResponseOutcome::Written, "should write first");

        let second = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        );

        assert_eq!(
            second,
            AfterResponseOutcome::Unchanged,
            "should not rewrite identical results"
        );
    }

    #[test]
    fn writes_nothing_without_a_row() {
        let graph = KvIdentityGraph::in_memory("after-response");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        );

        assert_eq!(
            outcome,
            AfterResponseOutcome::NoRow,
            "should not create a row"
        );
        assert!(
            graph.load_snapshot(EC_ID).entry_for(EC_ID).is_none(),
            "should leave the row absent"
        );
    }

    #[test]
    fn writes_nothing_to_a_withdrawn_row() {
        let graph = KvIdentityGraph::in_memory("after-response");
        graph
            .create(EC_ID, &KvEntry::tombstone(1000))
            .expect("should seed tombstone");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(EC_ID, "p-1"),
        );

        assert_eq!(
            outcome,
            AfterResponseOutcome::NoRow,
            "should not revive a tombstone"
        );
        assert!(
            stored_uid(&graph, "ids.example.com").is_none(),
            "should not store the provider UID on a tombstone"
        );
    }

    #[test]
    fn drops_results_made_for_another_ec() {
        let graph = KvIdentityGraph::in_memory("after-response");
        graph.create(EC_ID, &live_entry()).expect("should seed row");
        let other = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.ABC123";

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &found(other, "p-1"),
        );

        assert_eq!(
            outcome,
            AfterResponseOutcome::Unchanged,
            "should discard results dispatched for another EC"
        );
        assert!(
            stored_uid(&graph, "ids.example.com").is_none(),
            "should not store results for another EC"
        );
    }

    #[test]
    fn skips_the_write_without_results() {
        let graph = KvIdentityGraph::in_memory("after-response");
        graph.create(EC_ID, &live_entry()).expect("should seed row");

        let outcome = commit_after_response_lookups(
            &graph,
            AfterResponseWrite::for_test(),
            &registry(),
            EC_ID,
            &LookupResults::new(EC_ID, Vec::new()),
        );

        assert_eq!(
            outcome,
            AfterResponseOutcome::Unchanged,
            "should make no write without results"
        );
    }

    #[test]
    fn lookups_run_only_for_an_existing_consented_row() {
        // Arrange
        let graph = KvIdentityGraph::in_memory("after_response_allowed");
        let missing = graph.load_snapshot(EC_ID);
        graph.create(EC_ID, &live_entry()).expect("should seed row");
        let present = graph.load_snapshot(EC_ID);
        let withdrawn_graph = KvIdentityGraph::in_memory("after_response_withdrawn");
        withdrawn_graph
            .create(EC_ID, &KvEntry::tombstone(1000))
            .expect("should seed tombstone");
        let withdrawn = withdrawn_graph.load_snapshot(EC_ID);

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
