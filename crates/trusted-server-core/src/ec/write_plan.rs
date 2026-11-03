//! Plans a navigation's single row change.
//!
//! Combines browser ingest (fill missing) and validated provider results
//! (replace the provider's sources), prunes expired UIDs and enforces
//! [`MAX_ENTRY_BYTES`]. Pure: no I/O.
//!
//! A provider-owned source takes a browser UID only as a fallback: when the
//! row holds no live UID for it and the provider kept nothing for it on this
//! navigation. The fallback is a single-UID record without provider `eids`,
//! so the coordinator still treats the source as due and a later provider
//! result replaces it.

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
    let lookups_apply = input.lookups.is_for(input.ec_id);
    let provider_filled: Vec<&SourceDomain> = if lookups_apply {
        input
            .lookups
            .iter()
            .flat_map(|lookup| lookup.entries.iter().map(|(source, _)| source))
            .collect()
    } else {
        Vec::new()
    };
    apply_browser_ingest(
        &mut entry,
        input.browser_ingest,
        &BrowserIngestRules {
            registry: input.registry,
            provider_filled: &provider_filled,
            now: input.now,
        },
    );
    let written = if lookups_apply {
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

/// What browser ingest needs to decide provider-owned fallbacks.
struct BrowserIngestRules<'a> {
    registry: &'a PartnerRegistry,
    /// Sources the provider returned entries for on this navigation.
    provider_filled: &'a [&'a SourceDomain],
    now: UnixSeconds,
}

fn apply_browser_ingest(
    entry: &mut KvEntry,
    updates: &[PartnerIdUpdate],
    rules: &BrowserIngestRules<'_>,
) {
    for update in updates {
        if !rules.registry.is_provider_owned(&update.partner_id) {
            entry
                .ids
                .entry(update.partner_id.clone())
                .or_insert_with(|| KvPartnerId::single(update.uid.clone()));
            continue;
        }
        let provider_filled = rules
            .provider_filled
            .iter()
            .any(|source| source.as_str() == update.partner_id);
        let held = entry
            .ids
            .get(&update.partner_id)
            .is_some_and(|record| record.has_live_uid(rules.now));
        if provider_filled || held {
            continue;
        }
        log::debug!(
            "EC write planner: ingesting browser fallback for provider-owned source '{}'",
            update.partner_id
        );
        entry.ids.insert(
            update.partner_id.clone(),
            KvPartnerId::single(update.uid.clone()),
        );
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

/// Serialized bytes one `ids` member adds to the row: its record, its
/// escaped key, the colon and a separating comma.
fn member_len(source: &str, record: &KvPartnerId) -> usize {
    let key = serde_json::to_vec(source).map_or(usize::MAX, |bytes| bytes.len());
    let value = serde_json::to_vec(record).map_or(usize::MAX, |bytes| bytes.len());
    key.saturating_add(value).saturating_add(2)
}

/// Shrinks `entry` until it serializes within [`MAX_ENTRY_BYTES`].
///
/// # Performance
///
/// Tracks the row size by the bytes each drop removes and serializes the
/// whole row again only when that estimate reaches the cap, so the cost is
/// linear in the number of dropped UIDs and records.
fn enforce_size_cap(entry: &mut KvEntry, written: &[SourceDomain]) {
    let mut size = serialized_len(entry);
    // Drop this navigation's provider UIDs first, newest source last-in first.
    while size > MAX_ENTRY_BYTES {
        let Some(source) = written.iter().rev().find(|source| {
            entry
                .ids
                .get(source.as_str())
                .is_some_and(KvPartnerId::is_provider_written)
        }) else {
            break;
        };
        let record = entry
            .ids
            .get_mut(source.as_str())
            .expect("should hold a written source");
        let before = member_len(source.as_str(), record);
        record.drop_last_uid();
        let after = if record.uid.is_empty() {
            entry.ids.remove(source.as_str());
            0
        } else {
            member_len(source.as_str(), record)
        };
        size = size.saturating_sub(before.saturating_sub(after));
        if size <= MAX_ENTRY_BYTES {
            size = serialized_len(entry);
        }
    }
    if size <= MAX_ENTRY_BYTES {
        return;
    }
    // No provider UIDs from this navigation are left: drop whole records,
    // largest first, so every later write still fits the cap.
    let mut by_size: Vec<(usize, String)> = entry
        .ids
        .iter()
        .map(|(source, record)| (member_len(source, record), source.clone()))
        .collect();
    // A stable ascending sort pops the last of equally large records first.
    by_size.sort_by_key(|(len, _)| *len);
    while size > MAX_ENTRY_BYTES {
        let Some((len, largest)) = by_size.pop() else {
            log::warn!("EC write planner: row exceeds size cap with no partner IDs left to drop");
            return;
        };
        log::warn!("EC write planner: dropping '{largest}' to fit the row size cap");
        entry.ids.remove(&largest);
        size = size.saturating_sub(len);
        if size <= MAX_ENTRY_BYTES {
            size = serialized_len(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::coordinator::results::{DropCounts, LookupStatus, ProviderLookup, SkipReason};
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

    fn ingest_owned() -> [PartnerIdUpdate; 1] {
        [PartnerIdUpdate::new("ids.example.com", "browser-id")]
    }

    fn failed_lookup(status: LookupStatus) -> LookupResults {
        LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status,
                entries: Vec::new(),
            }],
        )
    }

    fn provider_entry(ids: &[&str]) -> KvEntry {
        let mut entry = live_entry();
        let lookups = found("ids.example.com", ids);
        let (_, eids) = lookups.iter().next().expect("should hold a lookup").entries[0].clone();
        entry.ids.insert(
            "ids.example.com".to_owned(),
            KvPartnerId::from_eids(eids, UnixSeconds::new(1000)).expect("should build record"),
        );
        entry
    }

    #[test]
    fn provider_owned_source_takes_browser_fallback_when_lookup_fails() {
        // Arrange
        let registry = registry(&["ids.example.com"]);
        let snapshot = present(&live_entry());
        let ingest = ingest_owned();

        for status in [
            LookupStatus::NoMatch,
            LookupStatus::Skipped(SkipReason::NotDue),
        ] {
            let lookups = failed_lookup(status);

            // Act
            let change = plan_write(&input(&snapshot, None, &ingest, &lookups, &registry))
                .expect("should plan update");

            // Assert
            let RowChange::Update { entry, .. } = change else {
                panic!("should update existing row")
            };
            let record = &entry.ids["ids.example.com"];
            assert_eq!(
                record.uid, "browser-id",
                "should store the browser fallback"
            );
            assert!(
                !record.is_provider_written(),
                "should mark the fallback as not provider-written"
            );
        }
    }

    #[test]
    fn provider_owned_source_takes_browser_fallback_without_lookups() {
        let registry = registry(&["ids.example.com"]);
        let snapshot = EcKvSnapshot::Missing {
            ec_id: EC_ID.to_owned(),
        };
        let new_entry = live_entry();
        let ingest = ingest_owned();
        let lookups = LookupResults::default();

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
            entry.ids["ids.example.com"].uid, "browser-id",
            "should store the browser fallback when no provider ran"
        );
    }

    #[test]
    fn browser_fallback_is_not_stored_when_provider_found_the_source() {
        let registry = registry(&["ids.example.com"]);
        let snapshot = present(&live_entry());
        let ingest = ingest_owned();
        let lookups = found("ids.example.com", &["p-1"]);

        let change = plan_write(&input(&snapshot, None, &ingest, &lookups, &registry))
            .expect("should plan update");

        let RowChange::Update { entry, .. } = change else {
            panic!("should update existing row")
        };
        let record = &entry.ids["ids.example.com"];
        assert_eq!(record.uid, "p-1", "should store the provider uid");
        assert!(record.is_provider_written(), "should keep provider eids");
    }

    #[test]
    fn browser_fallback_never_overwrites_a_live_provider_uid() {
        let registry = registry(&["ids.example.com"]);
        let snapshot = present(&provider_entry(&["p-1"]));
        let ingest = ingest_owned();
        let lookups = failed_lookup(LookupStatus::NoMatch);

        assert!(
            plan_write(&input(&snapshot, None, &ingest, &lookups, &registry)).is_none(),
            "should leave the live provider uid untouched"
        );
    }

    #[test]
    fn browser_fallback_does_not_overwrite_an_existing_fallback() {
        let registry = registry(&["ids.example.com"]);
        let mut entry = live_entry();
        entry
            .ids
            .insert("ids.example.com".to_owned(), KvPartnerId::single("older"));
        let snapshot = present(&entry);
        let ingest = ingest_owned();
        let lookups = failed_lookup(LookupStatus::NoMatch);

        assert!(
            plan_write(&input(&snapshot, None, &ingest, &lookups, &registry)).is_none(),
            "should not overwrite an existing browser uid"
        );
    }

    #[test]
    fn later_provider_result_replaces_browser_fallback() {
        let registry = registry(&["ids.example.com"]);
        let mut entry = live_entry();
        entry.ids.insert(
            "ids.example.com".to_owned(),
            KvPartnerId::single("browser-id"),
        );
        let snapshot = present(&entry);
        let lookups = found("ids.example.com", &["p-1"]);

        let change = plan_write(&input(
            &snapshot,
            None,
            &ingest_owned(),
            &lookups,
            &registry,
        ))
        .expect("should plan update");

        let RowChange::Update { entry, .. } = change else {
            panic!("should update existing row")
        };
        let record = &entry.ids["ids.example.com"];
        assert_eq!(record.uid, "p-1", "should replace the fallback");
        assert!(record.is_provider_written(), "should store provider eids");
    }
}
