//! The shared removal write: a removal-only mutation written with the
//! withdrawal path's CAS loop (spec 5.2 phase 4, 5.3).

use super::ids::SourceDomain;
use super::mutation::{ApplyContext, IdentityMutation, apply_removals_only};
use crate::ec::kv::{KvIdentityGraph, MAX_CAS_RETRIES};
use crate::ec::kv_backend::EcKvWriteOutcome;
use crate::ec::{EcKvSnapshot, log_id};

/// Writes the removal write makes at most, as the withdrawal path does.
pub(crate) const REMOVAL_WRITE_ATTEMPTS: u32 = MAX_CAS_RETRIES;

/// How a removal write ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalWriteOutcome {
    /// The removals are in KV.
    Written,
    /// Nothing to write: a tombstone, a missing row, or already applied.
    NothingToWrite,
    /// Not written. The caller may try once more after send.
    Failed,
}

/// Reads the row, applies only `removals` and writes them with a CAS,
/// retrying only a generation mismatch.
///
/// Runs whatever the request's consent: a revocation can arrive when consent
/// is unusable. It does not log a lost removal; see [`log_removal_lost`].
#[must_use]
pub(crate) fn write_removals(
    graph: &KvIdentityGraph,
    ec_id: &str,
    removals: &IdentityMutation,
    context: &ApplyContext<'_>,
) -> RemovalWriteOutcome {
    for _attempt in 0..REMOVAL_WRITE_ATTEMPTS {
        let (entry, generation) = match graph.load_snapshot(ec_id) {
            EcKvSnapshot::Present {
                entry,
                generation: Some(generation),
                ..
            } => (entry, generation),
            EcKvSnapshot::Missing { .. } => return RemovalWriteOutcome::NothingToWrite,
            _ => return RemovalWriteOutcome::Failed,
        };
        let Some(applied) =
            apply_removals_only(removals, &entry, context).filter(|applied| applied.changed)
        else {
            return RemovalWriteOutcome::NothingToWrite;
        };
        match graph.write_if_generation(ec_id, &applied.entry, generation) {
            Ok(EcKvWriteOutcome::Written) => return RemovalWriteOutcome::Written,
            Ok(EcKvWriteOutcome::PreconditionFailed) => {}
            Err(error) => {
                log::warn!(
                    "Identity removal write failed for '{}': {error:?}",
                    log_id(ec_id)
                );
                return RemovalWriteOutcome::Failed;
            }
        }
    }
    log::warn!(
        "Identity removal write for '{}': CAS conflict after {REMOVAL_WRITE_ATTEMPTS} attempts",
        log_id(ec_id)
    );
    RemovalWriteOutcome::Failed
}

/// Logs a removal that every attempt failed to write. Names providers and
/// sources only, never a UID.
pub(crate) fn log_removal_lost(ec_id: &str, removals: &IdentityMutation) {
    for (provider, source) in removals.removal_labels() {
        log::error!(
            "Identity removal lost for '{}': provider '{provider}', source '{}'",
            log_id(ec_id),
            source.as_ref().map_or("*", SourceDomain::as_str),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use error_stack::Report;

    use super::*;
    use crate::ec::identity::catalog::ProviderCatalog;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::mutation::{PartnerIdChange, ProviderStateChange};
    use crate::ec::identity::revision::{RecordRevision, Revision};
    use crate::ec::identity::test_support::fixed_provider;
    use crate::ec::kv::KvIdentityGraph;
    use crate::ec::kv_backend::test_support::InMemoryEcKv;
    use crate::ec::kv_backend::{
        EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
    };
    use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId};
    use crate::error::TrustedServerError;

    const EC_ID: &str = "0000000000000000000000000000000000000000000000000000000000000000.ABC123";

    /// Write counts the store keeps after seeding.
    #[derive(Default)]
    struct WriteCounts {
        all: AtomicUsize,
        generation_checked: AtomicUsize,
    }

    /// Fails the first `conflicts` generation-checked writes with
    /// `PreconditionFailed`, or every generation-checked write with an error
    /// when `error` is set. Counts every write it sees.
    struct ScriptedEcKv {
        inner: InMemoryEcKv,
        conflicts: Mutex<u32>,
        error: bool,
        counts: Arc<WriteCounts>,
    }

    impl EcKvStore for ScriptedEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }

        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key)
        }

        fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.inner.key_exists(key)
        }

        fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            self.counts.all.fetch_add(1, Ordering::SeqCst);
            if matches!(write.mode, EcKvWriteMode::IfGenerationMatch(_)) {
                self.counts
                    .generation_checked
                    .fetch_add(1, Ordering::SeqCst);
                if self.error {
                    return Err(Report::new(TrustedServerError::KvStore {
                        store_name: self.inner.store_name().to_owned(),
                        message: "per-key write limit".to_owned(),
                    }));
                }
                let mut remaining = self.conflicts.lock().expect("should lock the counter");
                if *remaining > 0 {
                    *remaining -= 1;
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
            }
            self.inner.insert(key, write)
        }

        fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.inner.list_keys_with_prefix(prefix, limit)
        }

        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key)
        }
    }

    /// Seeds `entry` with `create`, then wraps the store and zeroes the counts.
    fn graph_with(
        entry: Option<&KvEntry>,
        conflicts: u32,
        error: bool,
    ) -> (KvIdentityGraph, Arc<WriteCounts>) {
        let counts = Arc::new(WriteCounts::default());
        let graph = KvIdentityGraph::new(ScriptedEcKv {
            inner: InMemoryEcKv::new("removal-store"),
            conflicts: Mutex::new(conflicts),
            error,
            counts: Arc::clone(&counts),
        });
        if let Some(entry) = entry {
            graph.create(EC_ID, entry).expect("should seed the row");
        }
        counts.all.store(0, Ordering::SeqCst);
        counts.generation_checked.store(0, Ordering::SeqCst);
        (graph, counts)
    }

    fn removal() -> IdentityMutation {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: "a.example.com".parse().expect("should parse the source"),
            provider: "owner".parse().expect("should parse the provider ID"),
            expected: Revision::Set(RecordRevision::from_stored("1111111111111111")),
        });
        mutation
    }

    fn live() -> KvEntry {
        let mut entry = KvEntry::minimal("b.example.com", "x", 1);
        entry.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                w: Some("owner".to_owned()),
                rv: Some("1111111111111111".to_owned()),
                ..KvPartnerId::new("u")
            },
        );
        entry
    }

    fn run(graph: &KvIdentityGraph, removals: &IdentityMutation) -> RemovalWriteOutcome {
        let random = SeededRandom::new(1);
        let catalog = ProviderCatalog::new(
            vec![fixed_provider("owner", &["a.example.com"])],
            [(
                "a.example.com".parse().expect("should parse the source"),
                "owner".parse().expect("should parse the provider ID"),
            )]
            .into_iter()
            .collect(),
        );
        write_removals(
            graph,
            EC_ID,
            removals,
            &ApplyContext {
                now: 1_000,
                random: &random,
                catalog: &catalog,
            },
        )
    }

    #[test]
    fn writes_the_removal_once_with_a_generation_check() {
        let (graph, counts) = graph_with(Some(&live()), 0, false);

        assert_eq!(
            run(&graph, &removal()),
            RemovalWriteOutcome::Written,
            "should write"
        );

        assert_eq!(
            counts.generation_checked.load(Ordering::SeqCst),
            1,
            "should write once, with IfGenerationMatch"
        );
        let (entry, _) = graph
            .get(EC_ID)
            .expect("should read")
            .expect("should hold the row");
        assert!(
            !entry.ids.contains_key("a.example.com"),
            "should remove the record"
        );
        assert_eq!(
            entry.ps["owner"].iv.len(),
            1,
            "should hash the removed UID into iv"
        );
    }

    #[test]
    fn retries_only_a_generation_mismatch_up_to_five_writes() {
        let (graph, counts) = graph_with(Some(&live()), 2, false);
        assert_eq!(
            run(&graph, &removal()),
            RemovalWriteOutcome::Written,
            "should win on the third write"
        );
        assert_eq!(
            counts.generation_checked.load(Ordering::SeqCst),
            3,
            "should re-read and retry each conflict"
        );

        let (graph, counts) = graph_with(Some(&live()), 5, false);
        assert_eq!(
            run(&graph, &removal()),
            RemovalWriteOutcome::Failed,
            "should give up after five conflicts"
        );
        assert_eq!(
            counts.generation_checked.load(Ordering::SeqCst),
            5,
            "should stop at five writes"
        );

        let (graph, counts) = graph_with(Some(&live()), 0, true);
        assert_eq!(
            run(&graph, &removal()),
            RemovalWriteOutcome::Failed,
            "should stop at a non-CAS error"
        );
        assert_eq!(
            counts.generation_checked.load(Ordering::SeqCst),
            1,
            "should not retry a per-key limit"
        );
    }

    #[test]
    fn writes_nothing_for_a_tombstone_a_missing_row_or_an_applied_removal() {
        let (graph, counts) = graph_with(Some(&KvEntry::tombstone(1)), 0, false);
        assert_eq!(
            run(&graph, &removal()),
            RemovalWriteOutcome::NothingToWrite,
            "should skip a tombstone"
        );
        assert_eq!(
            counts.all.load(Ordering::SeqCst),
            0,
            "should not write a tombstone"
        );

        let (graph, counts) = graph_with(None, 0, false);
        assert_eq!(
            run(&graph, &removal()),
            RemovalWriteOutcome::NothingToWrite,
            "should skip a missing row"
        );
        assert_eq!(
            counts.all.load(Ordering::SeqCst),
            0,
            "should not write a missing row"
        );

        let (graph, counts) =
            graph_with(Some(&KvEntry::minimal("b.example.com", "x", 1)), 0, false);
        assert_eq!(
            run(&graph, &removal()),
            RemovalWriteOutcome::NothingToWrite,
            "should skip an applied removal"
        );
        assert_eq!(
            counts.all.load(Ordering::SeqCst),
            0,
            "should not write an applied removal"
        );
    }

    #[test]
    fn an_opt_out_and_its_clear_are_written_without_positive_changes() {
        let (graph, _) = graph_with(Some(&live()), 0, false);
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            "owner".parse().expect("should parse the provider ID"),
            ProviderStateChange {
                opt_out: Some(KvOptOut {
                    at: 1_000,
                    fp: "1".to_owned(),
                }),
                ..ProviderStateChange::default()
            },
        );

        assert_eq!(
            run(&graph, &mutation.removals()),
            RemovalWriteOutcome::Written,
            "should write the opt-out"
        );

        let (entry, _) = graph
            .get(EC_ID)
            .expect("should read")
            .expect("should hold the row");
        assert!(entry.ps["owner"].o.is_some(), "should store o");
        assert!(
            !entry.ids.contains_key("a.example.com"),
            "should remove what the provider wrote"
        );
    }
}
