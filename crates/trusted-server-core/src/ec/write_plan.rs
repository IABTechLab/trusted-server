//! Plans a navigation's single row change.
//!
//! Combines browser ingest (fill missing, never provider-owned sources) and
//! validated provider results (replace the provider's sources), prunes
//! expired UIDs and enforces [`MAX_ENTRY_BYTES`]. Pure: no I/O.

use super::EcKvSnapshot;
use super::coordinator::results::LookupResults;
use super::eid_types::{SourceDomain, UnixSeconds};
use super::kv::PartnerIdUpdate;
use super::kv_types::{KvEntry, KvPartnerId, MAX_ENTRY_BYTES};
use super::page_load::{Generation, RowChange};
use super::registry::PartnerRegistry;

/// Inputs to [`plan_write`].
#[derive(Debug, Clone, Copy)]
pub struct WritePlanInput<'a> {
    /// Active EC ID.
    pub ec_id: &'a str,
    /// Row snapshot read for this request.
    pub snapshot: &'a EcKvSnapshot,
    /// The new row when this navigation creates one (new EC or recovery).
    pub new_entry: Option<&'a KvEntry>,
    /// Browser updates from `ts-eids` and `sharedId`, already reduced by the
    /// ingest rules and consent-gated.
    pub browser_ingest: &'a [PartnerIdUpdate],
    /// Validated provider results.
    pub lookups: &'a LookupResults,
    /// Partner registry, for provider ownership.
    pub registry: &'a PartnerRegistry,
    /// Request time.
    pub now: UnixSeconds,
}

/// Plans the navigation's single row change.
///
/// Returns `None` when nothing would change, when the row is a tombstone, or
/// when an update has no generation to guard it.
#[must_use]
pub fn plan_write(input: &WritePlanInput<'_>) -> Option<RowChange> {
    let (base, expected) = match input.new_entry {
        Some(entry) => (entry.clone(), None),
        None => {
            let entry = input.snapshot.entry_for(input.ec_id)?;
            if !entry.consent.ok {
                return None;
            }
            let Some(expected) = Generation::of(input.snapshot, input.ec_id) else {
                log::info!(
                    "EC write planner: skipping update for '{}' (no_generation)",
                    super::log_id(input.ec_id)
                );
                return None;
            };
            (entry.clone(), Some(expected))
        }
    };

    let mut entry = base.clone();
    apply_browser_ingest(&mut entry, input.browser_ingest, input.registry);
    let written = if input.lookups.is_for(input.ec_id) {
        apply_lookups(&mut entry, input.lookups, input.now)
    } else {
        if !input.lookups.is_empty() {
            log::info!(
                "EC write planner: discarding lookups made for another EC ID than '{}'",
                super::log_id(input.ec_id)
            );
        }
        Vec::new()
    };
    for record in entry.ids.values_mut() {
        record.prune_expired(input.now);
    }
    entry.ids.retain(|_, record| !record.uid.is_empty());
    enforce_size_cap(&mut entry, &written);

    match expected {
        None => Some(RowChange::Create(entry)),
        Some(_) if entry == base => None,
        Some(expected) => Some(RowChange::Update { entry, expected }),
    }
}

fn apply_browser_ingest(
    entry: &mut KvEntry,
    updates: &[PartnerIdUpdate],
    registry: &PartnerRegistry,
) {
    for update in updates {
        if registry.is_provider_owned(&update.partner_id) {
            continue;
        }
        entry
            .ids
            .entry(update.partner_id.clone())
            .or_insert_with(|| KvPartnerId::single(update.uid.clone()));
    }
}

fn apply_lookups(
    entry: &mut KvEntry,
    lookups: &LookupResults,
    now: UnixSeconds,
) -> Vec<SourceDomain> {
    let mut written = Vec::new();
    for lookup in lookups.iter() {
        for (source, eids) in &lookup.entries {
            if let Some(record) = KvPartnerId::from_eids(eids.clone(), now) {
                entry.ids.insert(source.as_str().to_owned(), record);
                written.push(source.clone());
            }
        }
    }
    written
}

fn serialized_len(entry: &KvEntry) -> usize {
    serde_json::to_vec(entry).map_or(usize::MAX, |bytes| bytes.len())
}

fn enforce_size_cap(entry: &mut KvEntry, written: &[SourceDomain]) {
    while serialized_len(entry) > MAX_ENTRY_BYTES {
        if let Some(source) = written.iter().rev().find(|source| {
            entry
                .ids
                .get(source.as_str())
                .is_some_and(KvPartnerId::is_provider_written)
        }) {
            let record = entry
                .ids
                .get_mut(source.as_str())
                .expect("should hold a written source");
            record.drop_last_uid();
            if record.uid.is_empty() {
                entry.ids.remove(source.as_str());
            }
            continue;
        }
        // No provider UIDs from this navigation are left: drop whole records,
        // largest first, so every later write still fits the cap.
        let Some(largest) = entry
            .ids
            .iter()
            .max_by_key(|(_, record)| serde_json::to_vec(record).map_or(usize::MAX, |b| b.len()))
            .map(|(source, _)| source.clone())
        else {
            log::warn!("EC write planner: row exceeds size cap with no partner IDs left to drop");
            return;
        };
        log::warn!("EC write planner: dropping '{largest}' to fit the row size cap");
        entry.ids.remove(&largest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::coordinator::results::{DropCounts, LookupStatus, ProviderLookup};
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
    use crate::ec::kv::KvIdentityGraph;
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid};
    use crate::settings::EcPartner;

    const EC_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

    fn partner(source: &str) -> EcPartner {
        serde_json::from_value(serde_json::json!({ "name": source, "source_domain": source, "bidstream_enabled": true }))
            .expect("should build partner")
    }

    fn registry(owned: &[&str]) -> PartnerRegistry {
        let mut registry = PartnerRegistry::from_config(&[
            partner("browser.example.com"),
            partner("ids.example.com"),
        ])
        .expect("should build registry");
        for source in owned {
            assert!(registry.mark_provider_owned(source), "should mark {source}");
        }
        registry
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }

    fn present(entry: &KvEntry) -> EcKvSnapshot {
        let graph = KvIdentityGraph::in_memory("plan");
        graph.create(EC_ID, entry).expect("should seed row");
        graph.load_snapshot(EC_ID)
    }

    fn found(source: &str, ids: &[&str]) -> LookupResults {
        let uids = ids
            .iter()
            .map(|id| {
                KvUid::new(
                    UidValue::try_from(*id).expect("should build uid"),
                    AgentType::try_from(1).expect("should build atype"),
                    None,
                    Some(ProviderId::new("example-provider")),
                    Expiry::Never,
                )
            })
            .collect();
        LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Found {
                    kept: 1,
                    dropped: DropCounts::default(),
                },
                entries: vec![(
                    SourceDomain::try_from(source).expect("should build source"),
                    vec![KvEid::new(EidFields::default(), uids)],
                )],
            }],
        )
    }

    fn input<'a>(
        snapshot: &'a EcKvSnapshot,
        new_entry: Option<&'a KvEntry>,
        ingest: &'a [PartnerIdUpdate],
        lookups: &'a LookupResults,
        registry: &'a PartnerRegistry,
    ) -> WritePlanInput<'a> {
        WritePlanInput {
            ec_id: EC_ID,
            snapshot,
            new_entry,
            browser_ingest: ingest,
            lookups,
            registry,
            now: UnixSeconds::new(2000),
        }
    }

    #[test]
    fn new_ec_creates_row_with_ingest_and_lookups() {
        let registry = registry(&["ids.example.com"]);
        let snapshot = EcKvSnapshot::Missing {
            ec_id: EC_ID.to_owned(),
        };
        let new_entry = live_entry();
        let ingest = [
            PartnerIdUpdate::new("browser.example.com", "b-1"),
            PartnerIdUpdate::new("ids.example.com", "client"),
        ];
        let lookups = found("ids.example.com", &["p-1"]);

        let change = plan_write(&input(
            &snapshot,
            Some(&new_entry),
            &ingest,
            &lookups,
            &registry,
        ))
        .expect("should plan create");

        let RowChange::Create(entry) = change else {
            panic!("should create a new row")
        };
        assert_eq!(
            entry.ids["browser.example.com"].uid, "b-1",
            "should ingest browser uid"
        );
        assert_eq!(
            entry.ids["ids.example.com"].uid, "p-1",
            "should store provider uid, not client uid"
        );
    }

    #[test]
    fn lookups_for_another_ec_id_are_discarded() {
        let registry = registry(&["ids.example.com"]);
        let snapshot = EcKvSnapshot::Missing {
            ec_id: EC_ID.to_owned(),
        };
        let new_entry = live_entry();
        let lookups_for_old = LookupResults::new(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.OLD123",
            found("ids.example.com", &["p-1"]).iter().cloned().collect(),
        );

        let change = plan_write(&input(
            &snapshot,
            Some(&new_entry),
            &[],
            &lookups_for_old,
            &registry,
        ))
        .expect("should plan create");

        let RowChange::Create(entry) = change else {
            panic!("should create a new row")
        };
        assert!(
            !entry.ids.contains_key("ids.example.com"),
            "should not attribute the old EC ID's lookups to the new row"
        );
    }

    #[test]
    fn unchanged_returning_row_plans_no_write() {
        let registry = registry(&[]);
        let mut entry = live_entry();
        entry
            .ids
            .insert("browser.example.com".to_owned(), KvPartnerId::single("b-1"));
        let snapshot = present(&entry);
        let ingest = [PartnerIdUpdate::new("browser.example.com", "b-2")];
        let lookups = LookupResults::default();

        assert!(
            plan_write(&input(&snapshot, None, &ingest, &lookups, &registry)).is_none(),
            "should not overwrite an existing browser uid"
        );
    }

    #[test]
    fn lookup_replaces_provider_source_with_cas_update() {
        let registry = registry(&["ids.example.com"]);
        let mut entry = live_entry();
        entry
            .ids
            .insert("ids.example.com".to_owned(), KvPartnerId::single("legacy"));
        let snapshot = present(&entry);
        let lookups = found("ids.example.com", &["p-1", "p-2"]);

        let change = plan_write(&input(&snapshot, None, &[], &lookups, &registry))
            .expect("should plan update");

        let RowChange::Update { entry, .. } = change else {
            panic!("should update existing row")
        };
        assert_eq!(
            entry.ids["ids.example.com"].eids()[0].uids().len(),
            2,
            "should keep both provider uids"
        );
    }

    #[test]
    fn tombstoned_row_plans_nothing() {
        let registry = registry(&[]);
        let snapshot = present(&KvEntry::tombstone(1000));
        let lookups = found("ids.example.com", &["p-1"]);
        assert!(
            plan_write(&input(&snapshot, None, &[], &lookups, &registry)).is_none(),
            "should never write over a tombstone"
        );
    }

    #[test]
    fn snapshot_without_generation_plans_nothing() {
        let registry = registry(&[]);
        let snapshot = EcKvSnapshot::Present {
            ec_id: EC_ID.to_owned(),
            entry: Box::new(live_entry()),
            generation: None,
        };
        let ingest = [PartnerIdUpdate::new("browser.example.com", "b-1")];
        let lookups = LookupResults::default();
        assert!(
            plan_write(&input(&snapshot, None, &ingest, &lookups, &registry)).is_none(),
            "should not overwrite blindly"
        );
    }

    #[test]
    fn oversized_row_without_provider_uids_drops_records_until_it_fits() {
        let registry = registry(&[]);
        let mut entry = live_entry();
        for i in 0..40 {
            entry.ids.insert(
                format!("source{i:02}.example.com"),
                KvPartnerId::single(format!("{i:03}{}", "x".repeat(500))),
            );
        }
        let snapshot = EcKvSnapshot::Missing {
            ec_id: EC_ID.to_owned(),
        };
        let lookups = LookupResults::default();

        let change = plan_write(&input(&snapshot, Some(&entry), &[], &lookups, &registry))
            .expect("should plan create");

        let RowChange::Create(entry) = change else {
            panic!("should create")
        };
        let size = serde_json::to_vec(&entry).expect("should serialize").len();
        assert!(
            size <= MAX_ENTRY_BYTES,
            "should fit the row cap, got {size}"
        );
        assert!(!entry.ids.is_empty(), "should keep the records that fit");
    }

    #[test]
    fn oversized_row_drops_provider_uids_until_it_fits() {
        let registry = registry(&["ids.example.com"]);
        let snapshot = EcKvSnapshot::Missing {
            ec_id: EC_ID.to_owned(),
        };
        let new_entry = live_entry();
        let big: Vec<String> = (0..40)
            .map(|i| format!("{i:03}{}", "x".repeat(500)))
            .collect();
        let ids: Vec<&str> = big.iter().map(String::as_str).collect();
        let lookups = found("ids.example.com", &ids);

        let change = plan_write(&input(
            &snapshot,
            Some(&new_entry),
            &[],
            &lookups,
            &registry,
        ))
        .expect("should plan create");

        let RowChange::Create(entry) = change else {
            panic!("should create")
        };
        let size = serde_json::to_vec(&entry).expect("should serialize").len();
        assert!(
            size <= MAX_ENTRY_BYTES,
            "should fit the row cap, got {size}"
        );
        let kept = entry.ids["ids.example.com"].eids()[0].uids();
        assert!(kept.len() < 40, "should drop some uids");
        assert_eq!(kept[0].id().as_str(), ids[0], "should drop from the end");
    }
}
