//! The pending key: removals and capture results that missed the row write,
//! kept for the next writer to merge (spec 5.3, Pending key).
//!
//! `pending:<ec_id>` holds a list of items, each with a generation. An item
//! is appended with a CAS, and the next writer request merges every item
//! newer than the row's `pg` watermark into its one row write, then stores
//! the newest generation as `pg`. The key is never deleted: its one-day TTL
//! ends it. Auctions never read it.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ids::{IdentityProviderId, SourceDomain};
use super::mutation::{IdentityMutation, PartnerIdChange, ProviderStateChange};
use super::revision::{RecordRevision, Revision};
use crate::ec::kv::{KvIdentityGraph, MAX_CAS_RETRIES};
use crate::ec::kv_backend::{EcKvWriteMode, EcKvWriteOutcome};
use crate::ec::kv_types::{KvOptOut, KvPartnerId};
use crate::ec::log_id;

/// How long a pending key lives after its last append.
pub(crate) const PENDING_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Items a pending key holds at most. An append to a full key fails.
pub(crate) const MAX_PENDING_ITEMS: usize = 16;

/// Generations per second. A generation is at least `now * 1000`, so a key
/// that expired and comes back never reuses a generation a row has applied.
const GENERATIONS_PER_SECOND: u64 = 1_000;

/// The pending key of an EC ID.
#[must_use]
pub(crate) fn pending_key(ec_id: &str) -> String {
    format!("pending:{ec_id}")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct KvPending {
    #[serde(default)]
    items: Vec<KvPendingItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct KvPendingItem {
    /// The item's generation.
    g: u64,
    /// When the item was appended, Unix seconds.
    at: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ch: Vec<KvPendingChange>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    ps: BTreeMap<IdentityProviderId, KvPendingState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KvRevision {
    Unset,
    Set(String),
}

impl From<&Revision> for KvRevision {
    fn from(revision: &Revision) -> Self {
        match revision {
            Revision::Unset => Self::Unset,
            Revision::Set(value) => Self::Set(value.as_str().to_owned()),
        }
    }
}

impl From<&KvRevision> for Revision {
    fn from(revision: &KvRevision) -> Self {
        match revision {
            KvRevision::Unset => Self::Unset,
            KvRevision::Set(value) => Self::Set(RecordRevision::from_stored(value)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
enum KvPendingChange {
    FillMissing {
        s: SourceDomain,
        r: KvPartnerId,
    },
    OwnerWrite {
        s: SourceDomain,
        r: KvPartnerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        e: Option<KvRevision>,
    },
    OwnerRemove {
        s: SourceDomain,
        p: IdentityProviderId,
        e: KvRevision,
    },
    WriterRemove {
        s: SourceDomain,
        p: IdentityProviderId,
    },
}

impl From<&PartnerIdChange> for KvPendingChange {
    fn from(change: &PartnerIdChange) -> Self {
        match change {
            PartnerIdChange::FillMissing { source, record } => Self::FillMissing {
                s: source.clone(),
                r: record.clone(),
            },
            PartnerIdChange::OwnerWrite {
                source,
                record,
                expected,
            } => Self::OwnerWrite {
                s: source.clone(),
                r: record.clone(),
                e: expected.as_ref().map(KvRevision::from),
            },
            PartnerIdChange::OwnerRemove {
                source,
                provider,
                expected,
            } => Self::OwnerRemove {
                s: source.clone(),
                p: provider.clone(),
                e: KvRevision::from(expected),
            },
            PartnerIdChange::WriterRemove { source, writer } => Self::WriterRemove {
                s: source.clone(),
                p: writer.clone(),
            },
        }
    }
}

impl From<&KvPendingChange> for PartnerIdChange {
    fn from(change: &KvPendingChange) -> Self {
        match change {
            KvPendingChange::FillMissing { s, r } => Self::FillMissing {
                source: s.clone(),
                record: r.clone(),
            },
            KvPendingChange::OwnerWrite { s, r, e } => Self::OwnerWrite {
                source: s.clone(),
                record: r.clone(),
                expected: e.as_ref().map(Revision::from),
            },
            KvPendingChange::OwnerRemove { s, p, e } => Self::OwnerRemove {
                source: s.clone(),
                provider: p.clone(),
                expected: Revision::from(e),
            },
            KvPendingChange::WriterRemove { s, p } => Self::WriterRemove {
                source: s.clone(),
                writer: p.clone(),
            },
        }
    }
}

/// The removal part of a provider's state change: its opt-out and clear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct KvPendingState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    o: Option<KvOptOut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oc: Option<u64>,
}

/// What the next writer merges: the pending items newer than its row's
/// watermark, and the generation to store as the new watermark.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingMerge {
    pub(crate) mutation: IdentityMutation,
    pub(crate) generation: u64,
}

fn to_item(mutation: &IdentityMutation, generation: u64, now: u64) -> KvPendingItem {
    KvPendingItem {
        g: generation,
        at: now,
        ch: mutation
            .stand_alone_changes()
            .iter()
            .map(KvPendingChange::from)
            .collect(),
        ps: mutation
            .removal_states()
            .map(|(provider, state)| {
                (
                    provider.clone(),
                    KvPendingState {
                        o: state.opt_out.clone(),
                        oc: state.opt_out_clear,
                    },
                )
            })
            .collect(),
    }
}

/// Appends `mutation`'s stand-alone changes and opt-outs to the EC ID's
/// pending key with a CAS, retrying a generation mismatch. Items older than
/// [`PENDING_TTL`] are dropped first.
///
/// Returns `false` when the item was not stored: the key is full, unreadable
/// or every attempt lost. The caller then counts what it held as lost.
#[must_use]
pub(crate) fn append_pending(
    graph: &KvIdentityGraph,
    ec_id: &str,
    mutation: &IdentityMutation,
    now: u64,
) -> bool {
    let key = pending_key(ec_id);
    for _attempt in 0..MAX_CAS_RETRIES {
        let (mut pending, mode) = match graph.lookup_key(&key) {
            Ok(Some(lookup)) => match serde_json::from_slice::<KvPending>(&lookup.body) {
                Ok(pending) => (pending, EcKvWriteMode::IfGenerationMatch(lookup.generation)),
                Err(error) => {
                    log::warn!("Pending key for '{}' is unreadable: {error}", log_id(ec_id));
                    return false;
                }
            },
            Ok(None) => (KvPending::default(), EcKvWriteMode::Add),
            Err(error) => {
                log::warn!(
                    "Pending key for '{}' could not be read: {}",
                    log_id(ec_id),
                    error.current_context()
                );
                return false;
            }
        };
        pending
            .items
            .retain(|item| item.at.saturating_add(PENDING_TTL.as_secs()) > now);
        if pending.items.len() >= MAX_PENDING_ITEMS {
            log::warn!("Pending key for '{}' is full", log_id(ec_id));
            return false;
        }
        let last = pending.items.last().map_or(0, |item| item.g);
        let generation = last
            .saturating_add(1)
            .max(now.saturating_mul(GENERATIONS_PER_SECOND));
        pending.items.push(to_item(mutation, generation, now));
        let Ok(body) = serde_json::to_string(&pending) else {
            return false;
        };
        match graph.write_key(&key, &body, PENDING_TTL, mode) {
            Ok(EcKvWriteOutcome::Written) => return true,
            Ok(EcKvWriteOutcome::PreconditionFailed) => {}
            Err(error) => {
                log::warn!(
                    "Pending key for '{}' could not be written: {}",
                    log_id(ec_id),
                    error.current_context()
                );
                return false;
            }
        }
    }
    false
}

/// Reads the EC ID's pending key and builds the merge of every item newer
/// than `applied`, the row's watermark. `None` when there is nothing newer,
/// or the key cannot be read.
#[must_use]
pub(crate) fn load_pending(
    graph: &KvIdentityGraph,
    ec_id: &str,
    applied: u64,
) -> Option<PendingMerge> {
    let lookup = match graph.lookup_key(&pending_key(ec_id)) {
        Ok(lookup) => lookup?,
        Err(error) => {
            log::warn!(
                "Pending key for '{}' could not be read: {}",
                log_id(ec_id),
                error.current_context()
            );
            return None;
        }
    };
    let pending: KvPending = match serde_json::from_slice(&lookup.body) {
        Ok(pending) => pending,
        Err(error) => {
            log::warn!("Pending key for '{}' is unreadable: {error}", log_id(ec_id));
            return None;
        }
    };
    let mut mutation = IdentityMutation::default();
    let mut generation = applied;
    for item in pending.items.iter().filter(|item| item.g > applied) {
        for change in &item.ch {
            mutation.add_change(PartnerIdChange::from(change));
        }
        for (provider, state) in &item.ps {
            mutation.merge_state(
                provider.clone(),
                ProviderStateChange {
                    opt_out: state.o.clone(),
                    opt_out_clear: state.oc,
                    ..ProviderStateChange::default()
                },
            );
        }
        generation = generation.max(item.g);
    }
    (generation > applied).then_some(PendingMerge {
        mutation,
        generation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::identity::test_support::{pid, record_for_test, source};
    use crate::ec::kv_backend::test_support::InMemoryEcKv;

    const EC_ID: &str = "0000000000000000000000000000000000000000000000000000000000000000.ABC123";

    fn removal(name: &str) -> IdentityMutation {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::WriterRemove {
            source: source(name),
            writer: pid("lockr"),
        });
        mutation
    }

    #[test]
    fn appended_items_merge_once_past_the_watermark() {
        let graph = KvIdentityGraph::new(InMemoryEcKv::new("ec-store"));
        let mut opt_out = removal("a.example.com");
        opt_out.merge_state(
            pid("lockr"),
            ProviderStateChange {
                opt_out: Some(KvOptOut {
                    at: 90,
                    fp: "1".to_owned(),
                }),
                ..ProviderStateChange::default()
            },
        );

        assert!(
            append_pending(&graph, EC_ID, &opt_out, 100),
            "should store the first item"
        );
        assert!(
            append_pending(&graph, EC_ID, &removal("b.example.com"), 100),
            "should store the second item"
        );

        let merge = load_pending(&graph, EC_ID, 0).expect("should merge both items");
        assert_eq!(
            merge.generation,
            100 * GENERATIONS_PER_SECOND + 1,
            "should hand back the newest generation"
        );
        assert_eq!(
            merge.mutation.removal_labels().len(),
            3,
            "should merge both removals and the opt-out"
        );
        let first_only = load_pending(&graph, EC_ID, 100 * GENERATIONS_PER_SECOND)
            .expect("should merge the newer item");
        assert_eq!(
            first_only.mutation.removal_labels(),
            vec![(pid("lockr"), Some(source("b.example.com")))],
            "should skip an item the watermark covers"
        );
        assert!(
            load_pending(&graph, EC_ID, merge.generation).is_none(),
            "should merge nothing once the watermark covers every item"
        );
    }

    #[test]
    fn a_capture_write_round_trips_with_its_expected_revision() {
        let graph = KvIdentityGraph::new(InMemoryEcKv::new("ec-store"));
        let mut capture = IdentityMutation::default();
        let change = PartnerIdChange::OwnerWrite {
            source: source("a.example.com"),
            record: record_for_test("uid-1", Some("lockr"), 100),
            expected: Some(Revision::Set(RecordRevision::from_stored(
                "0123456789abcdef",
            ))),
        };
        capture.add_change(change.clone());

        assert!(
            append_pending(&graph, EC_ID, &capture, 100),
            "should store the capture"
        );

        let merge = load_pending(&graph, EC_ID, 0).expect("should merge the capture");
        assert_eq!(
            merge.mutation.stand_alone_changes(),
            std::slice::from_ref(&change),
            "should give back the change it stored"
        );
    }

    #[test]
    fn a_full_key_refuses_and_expired_items_make_room() {
        let graph = KvIdentityGraph::new(InMemoryEcKv::new("ec-store"));
        for _ in 0..MAX_PENDING_ITEMS {
            assert!(
                append_pending(&graph, EC_ID, &removal("a.example.com"), 100),
                "should store items up to the cap"
            );
        }

        assert!(
            !append_pending(&graph, EC_ID, &removal("a.example.com"), 100),
            "should refuse an item past the cap"
        );
        assert!(
            append_pending(
                &graph,
                EC_ID,
                &removal("a.example.com"),
                100 + PENDING_TTL.as_secs()
            ),
            "should drop expired items and store the new one"
        );
    }

    #[test]
    fn a_generation_never_falls_below_the_clock() {
        let graph = KvIdentityGraph::new(InMemoryEcKv::new("ec-store"));
        let applied_before_expiry = 500 * GENERATIONS_PER_SECOND + 3;

        assert!(
            append_pending(&graph, EC_ID, &removal("a.example.com"), 600),
            "should store the item"
        );

        assert!(
            load_pending(&graph, EC_ID, applied_before_expiry).is_some(),
            "should merge an item appended after a key expired and came back"
        );
    }
}
