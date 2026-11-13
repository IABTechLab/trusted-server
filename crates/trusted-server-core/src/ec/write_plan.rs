//! Plans a navigation's single row change.
//!
//! Combines browser ingest (fill missing) and validated provider results
//! (replace the provider's sources, or remove them on an opt-out), prunes expired UIDs and enforces
//! [`MAX_ENTRY_BYTES`]. Pure: no I/O.
//!
//! Browser ingest skips provider-owned sources entirely (spec §8.4), so a
//! source the provider cleared on an opt-out is never refilled from the
//! browser's `ts-eids` cookie.

use std::collections::btree_map;

use super::EcKvSnapshot;
use super::coordinator::results::{LookupResults, LookupStatus, OptOutMemory, SkipReason};
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

    // Prune first so an expired record frees its slot for browser ingest.
    // Pruning alone never triggers a write; it rides along with a real change.
    let mut entry = base;
    for record in entry.ids.values_mut() {
        record.prune_expired(input.now);
    }
    entry.prune_opt_outs(input.now);
    entry.ids.retain(|_, record| !record.uid.is_empty());
    let pruned = entry.clone();

    let lookups_apply = input.lookups.is_for(input.ec_id);
    let ingested = apply_browser_ingest(&mut entry, input.browser_ingest, input.registry);
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
    let had_change = entry != pruned;
    enforce_size_cap(&mut entry, &pruned, &written, &ingested);
    // The cap may trim the only real change away; trimming alone never writes.
    let changed = had_change && entry != pruned;

    match expected {
        None => Some(RowChange::Create(entry)),
        Some(_) if !changed => None,
        Some(expected) => Some(RowChange::Update { entry, expected }),
    }
}

/// Fills missing sources from the browser, skipping provider-owned ones.
///
/// Returns the sources it inserted, in insertion order.
fn apply_browser_ingest(
    entry: &mut KvEntry,
    updates: &[PartnerIdUpdate],
    registry: &PartnerRegistry,
) -> Vec<String> {
    let mut ingested = Vec::new();
    for update in updates {
        if registry.is_provider_owned(&update.partner_id) {
            log::trace!(
                "EC write planner: skipping browser UID for provider-owned source '{}'",
                update.partner_id
            );
            continue;
        }
        if let btree_map::Entry::Vacant(slot) = entry.ids.entry(update.partner_id.clone()) {
            slot.insert(KvPartnerId::single(update.uid.clone()));
            ingested.push(update.partner_id.clone());
        }
    }
    ingested
}

fn apply_lookups(
    entry: &mut KvEntry,
    lookups: &LookupResults,
    now: UnixSeconds,
) -> Vec<SourceDomain> {
    let mut written = Vec::new();
    for lookup in lookups.iter() {
        // A newer request may have recorded this provider's opt-out after
        // the lookup was dispatched; never restore its UIDs over it, unless
        // the vendor's cookies showed the visitor opted back in.
        if entry.opted_out(&lookup.provider, now)
            && lookup.status != LookupStatus::Skipped(SkipReason::OptedOut)
            && lookup.opt_out != OptOutMemory::Forget
        {
            log::info!(
                "EC write planner: discarding results of opted-out provider '{}'",
                lookup.provider
            );
            continue;
        }
        if lookup.opt_out == OptOutMemory::Forget {
            entry.forget_opt_out(&lookup.provider);
        }
        for (source, eids) in &lookup.entries {
            if let Some(record) = KvPartnerId::from_eids(eids.clone(), now) {
                entry.ids.insert(source.as_str().to_owned(), record);
                written.push(source.clone());
            }
        }
        if lookup.opt_out == OptOutMemory::Remember {
            entry.remember_opt_out(&lookup.provider, now);
        }
        for source in &lookup.cleared {
            if entry.ids.remove(source.as_str()).is_some() {
                log::info!(
                    "EC write planner: removing '{source}' after provider '{}' opt-out",
                    lookup.provider
                );
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
/// `base` is the stored row before this navigation's changes. A row that was
/// already over the cap (written before the cap existed) loses its largest
/// stored records first, so the change that triggered the write still lands.
/// This navigation's provider UIDs then compete with the stored row only, and
/// its browser UIDs, which the browser offers again, make way for them. A
/// provider refresh that no longer fits keeps the record it would replace
/// when that record fits.
///
/// # Performance
///
/// Tracks the row size by the bytes each drop removes and serializes the
/// whole row again only when that estimate reaches the cap, so the cost is
/// linear in the number of dropped UIDs and records.
fn enforce_size_cap(
    entry: &mut KvEntry,
    base: &KvEntry,
    written: &[SourceDomain],
    ingested: &[String],
) {
    let mut size = serialized_len(entry);
    if size <= MAX_ENTRY_BYTES {
        return;
    }
    if serialized_len(base) > MAX_ENTRY_BYTES {
        drop_largest_records(entry, &mut size, |source| {
            !written.iter().any(|written| written.as_str() == source)
                && !ingested.iter().any(|ingested| ingested == source)
        });
    }
    let browser_len: usize = ingested
        .iter()
        .filter_map(|source| {
            entry
                .ids
                .get(source)
                .map(|record| member_len(source, record))
        })
        .sum();
    // Drop this navigation's provider UIDs that do not fit, newest source
    // last-in first.
    for source in written.iter().rev() {
        if size.saturating_sub(browser_len) <= MAX_ENTRY_BYTES {
            break;
        }
        drop_provider_uids(entry, &mut size, base, source.as_str(), browser_len);
    }
    // Then this navigation's browser UIDs, newest first: the browser offers
    // them again, while a stored record would be lost.
    for source in ingested.iter().rev() {
        if size <= MAX_ENTRY_BYTES {
            break;
        }
        if let Some(record) = entry.ids.remove(source) {
            log::info!(
                "EC write planner: dropping browser UID for '{source}' to fit the row size cap"
            );
            size = size.saturating_sub(member_len(source, &record));
            if size <= MAX_ENTRY_BYTES {
                size = serialized_len(entry);
            }
        }
    }
    if size > MAX_ENTRY_BYTES {
        drop_largest_records(entry, &mut size, |_| true);
        if size > MAX_ENTRY_BYTES {
            log::warn!("EC write planner: row exceeds size cap with no partner IDs left to drop");
        }
    }
}

/// Drops `source`'s provider UIDs, last first, until the row without this
/// navigation's browser UIDs (`browser_len`) fits [`MAX_ENTRY_BYTES`].
///
/// When every UID is dropped, the record `base` stored for `source` is put
/// back if it fits, so a refresh that no longer fits never deletes it.
fn drop_provider_uids(
    entry: &mut KvEntry,
    size: &mut usize,
    base: &KvEntry,
    source: &str,
    browser_len: usize,
) {
    let Some(record) = entry
        .ids
        .get_mut(source)
        .filter(|record| record.is_provider_written())
    else {
        return;
    };
    while size.saturating_sub(browser_len) > MAX_ENTRY_BYTES && !record.uid.is_empty() {
        let before = member_len(source, record);
        record.drop_last_uid();
        if record.uid.is_empty() {
            break;
        }
        *size = size.saturating_sub(before.saturating_sub(member_len(source, record)));
        if size.saturating_sub(browser_len) <= MAX_ENTRY_BYTES {
            *size = serialized_len(entry);
            return;
        }
    }
    if !record.uid.is_empty() {
        return;
    }
    entry.ids.remove(source);
    *size = serialized_len(entry);
    if let Some(previous) = base.ids.get(source) {
        let restored = size.saturating_add(member_len(source, previous));
        if restored.saturating_sub(browser_len) <= MAX_ENTRY_BYTES {
            log::info!(
                "EC write planner: keeping the stored record for '{source}' as its refresh does not fit the row size cap"
            );
            entry.ids.insert(source.to_owned(), previous.clone());
            *size = serialized_len(entry);
        }
    }
}

/// Drops whole records that `droppable` accepts, largest first, until `entry`
/// fits [`MAX_ENTRY_BYTES`] or none is left.
fn drop_largest_records(entry: &mut KvEntry, size: &mut usize, droppable: impl Fn(&str) -> bool) {
    let mut by_size: Vec<(usize, String)> = entry
        .ids
        .iter()
        .filter(|(source, _)| droppable(source))
        .map(|(source, record)| (member_len(source, record), source.clone()))
        .collect();
    // A stable ascending sort pops the last of equally large records first.
    by_size.sort_by_key(|(len, _)| *len);
    while *size > MAX_ENTRY_BYTES {
        let Some((len, largest)) = by_size.pop() else {
            return;
        };
        log::warn!("EC write planner: dropping '{largest}' to fit the row size cap");
        entry.ids.remove(&largest);
        *size = size.saturating_sub(len);
        if *size <= MAX_ENTRY_BYTES {
            *size = serialized_len(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::coordinator::results::{DropCounts, LookupStatus, ProviderLookup, SkipReason};
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
    use crate::ec::kv::KvIdentityGraph;
    use crate::ec::kv_types::{KvEid, KvPartnerId, KvUid, OPT_OUT_MEMORY_SECONDS};
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
            assert!(
                registry.mark_provider_owned(source, None),
                "should mark {source}"
            );
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
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
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
        lookup_with_memory(status, OptOutMemory::Unchanged)
    }

    fn lookup_with_memory(status: LookupStatus, opt_out: OptOutMemory) -> LookupResults {
        LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status,
                entries: Vec::new(),
                cleared: Vec::new(),
                opt_out,
            }],
        )
    }

    #[test]
    fn a_cookie_opt_out_is_not_remembered() {
        // Arrange
        let registry = registry(&["ids.example.com"]);
        let ingest: [PartnerIdUpdate; 0] = [];
        let mut lookups = failed_lookup(LookupStatus::Skipped(SkipReason::OptedOut));
        let cleared = lookups.iter().next().expect("should hold a lookup").clone();
        lookups = LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                cleared: vec![SourceDomain::try_from("ids.example.com").expect("should parse")],
                ..cleared
            }],
        );

        // Act
        let change = plan_write(&input(
            &present(&provider_entry(&["p-1"])),
            None,
            &ingest,
            &lookups,
            &registry,
        ))
        .expect("should plan update");

        // Assert
        let RowChange::Update { entry, .. } = change else {
            panic!("should update existing row")
        };
        assert!(
            !entry.ids.contains_key("ids.example.com"),
            "should clear the opted-out source"
        );
        assert!(
            entry.opt_outs.is_empty(),
            "should not remember an opt-out read from cookies"
        );
    }

    #[test]
    fn an_opt_back_in_forgets_the_opt_out_and_stores_the_ids() {
        // Arrange
        let registry = registry(&["ids.example.com"]);
        let ingest: [PartnerIdUpdate; 0] = [];
        let provider = ProviderId::new("example-provider");
        let mut entry = live_entry();
        entry
            .opt_outs
            .insert(provider.clone(), 2000 + OPT_OUT_MEMORY_SECONDS);
        let found = found("ids.example.com", &["c-1"]);
        let lookup = found.iter().next().expect("should hold a lookup").clone();
        let lookups = LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                opt_out: OptOutMemory::Forget,
                ..lookup
            }],
        );

        // Act
        let change = plan_write(&input(&present(&entry), None, &ingest, &lookups, &registry))
            .expect("should plan update");

        // Assert
        let RowChange::Update { entry, .. } = change else {
            panic!("should update existing row")
        };
        assert!(
            !entry.opt_outs.contains_key(&provider),
            "should forget the remembered opt-out"
        );
        assert!(
            entry.ids.contains_key("ids.example.com"),
            "should store the cookie ID"
        );
    }

    #[test]
    fn opt_out_removes_the_provider_sources_and_blocks_browser_fallback() {
        // Arrange
        let registry = registry(&["ids.example.com"]);
        let snapshot = present(&provider_entry(&["p-1"]));
        let ingest = ingest_owned();
        let lookups = LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Skipped(SkipReason::OptedOut),
                entries: Vec::new(),
                cleared: vec![SourceDomain::try_from("ids.example.com").expect("should parse")],
                opt_out: OptOutMemory::Unchanged,
            }],
        );

        // Act
        let change = plan_write(&input(&snapshot, None, &ingest, &lookups, &registry))
            .expect("should plan update");

        // Assert
        let RowChange::Update { entry, .. } = change else {
            panic!("should update existing row")
        };
        assert!(
            !entry.ids.contains_key("ids.example.com"),
            "should remove the opted-out source without a browser fallback"
        );
    }

    #[test]
    fn opt_out_is_remembered_without_extending_it() {
        // Arrange
        let registry = registry(&["ids.example.com"]);
        let ingest: [PartnerIdUpdate; 0] = [];
        let provider = ProviderId::new("example-provider");
        let lookups = lookup_with_memory(
            LookupStatus::Skipped(SkipReason::OptedOut),
            OptOutMemory::Remember,
        );

        // Act: the first opt-out is remembered.
        let first = plan_write(&input(
            &present(&provider_entry(&["p-1"])),
            None,
            &ingest,
            &lookups,
            &registry,
        ))
        .expect("should plan update");

        // Assert
        let RowChange::Update { entry, .. } = first else {
            panic!("should update existing row")
        };
        let until = entry.opt_outs[&provider];
        assert_eq!(
            until,
            2000 + OPT_OUT_MEMORY_SECONDS,
            "should remember the opt-out for the memory period"
        );

        // Act: a skip because of the remembered opt-out changes nothing.
        let again = plan_write(&input(&present(&entry), None, &ingest, &lookups, &registry));

        // Assert
        assert!(again.is_none(), "should not extend a remembered opt-out");
    }

    #[test]
    fn lapsed_opt_out_alone_plans_no_write() {
        let registry = registry(&["ids.example.com"]);
        let ingest: [PartnerIdUpdate; 0] = [];
        let mut entry = live_entry();
        entry
            .opt_outs
            .insert(ProviderId::new("example-provider"), 1000);
        let lookups = LookupResults::default();

        let change = plan_write(&input(&present(&entry), None, &ingest, &lookups, &registry));

        assert!(change.is_none(), "should not write only to prune");
    }

    #[test]
    fn lapsed_opt_out_is_pruned_with_a_real_change() {
        let registry = registry(&["ids.example.com"]);
        let ingest = [PartnerIdUpdate::new("browser.example.com", "b-1")];
        let mut entry = live_entry();
        entry
            .opt_outs
            .insert(ProviderId::new("example-provider"), 1000);
        let lookups = LookupResults::default();

        let change = plan_write(&input(&present(&entry), None, &ingest, &lookups, &registry))
            .expect("should plan update");

        let RowChange::Update { entry, .. } = change else {
            panic!("should update existing row")
        };
        assert!(entry.opt_outs.is_empty(), "should forget a lapsed opt-out");
    }

    #[test]
    fn not_due_skip_with_an_expired_uid_plans_no_write() {
        let registry = registry(&["ids.example.com"]);
        let ingest: [PartnerIdUpdate; 0] = [];
        let mut entry = live_entry();
        let expired = KvUid::new(
            UidValue::try_from("old-1").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            Expiry::At(UnixSeconds::new(1500)),
        );
        entry.ids.insert(
            "ids.example.com".to_owned(),
            KvPartnerId::from_eids(
                vec![KvEid::new(EidFields::default(), vec![expired])],
                UnixSeconds::new(1000),
            )
            .expect("should build provider record"),
        );
        let lookups = LookupResults::new(
            EC_ID,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Skipped(SkipReason::NotDue),
                entries: Vec::new(),
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        );

        let change = plan_write(&input(&present(&entry), None, &ingest, &lookups, &registry));

        assert!(
            change.is_none(),
            "should not write only to prune an expired UID"
        );
    }

    #[test]
    fn browser_ingest_fills_a_slot_freed_by_an_expired_record() {
        // The source is not provider-owned (its provider was disabled), so the
        // browser may fill it once the expired provider record is pruned.
        let registry = registry(&[]);
        let ingest = [PartnerIdUpdate::new("ids.example.com", "browser-1")];
        let lookups = LookupResults::default();
        let mut entry = live_entry();
        let expired = KvUid::new(
            UidValue::try_from("old-1").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            Expiry::At(UnixSeconds::new(1500)),
        );
        entry.ids.insert(
            "ids.example.com".to_owned(),
            KvPartnerId::from_eids(
                vec![KvEid::new(EidFields::default(), vec![expired])],
                UnixSeconds::new(1000),
            )
            .expect("should build provider record"),
        );

        let change = plan_write(&input(&present(&entry), None, &ingest, &lookups, &registry));

        let Some(RowChange::Update { entry, .. }) = change else {
            panic!("should plan an update that fills the freed slot");
        };
        assert_eq!(
            entry
                .ids
                .get("ids.example.com")
                .map(|record| record.uid.as_str()),
            Some("browser-1"),
            "should store the browser UID in place of the expired record"
        );
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
    fn browser_ingest_skips_a_provider_owned_source() {
        // Arrange
        let registry = registry(&["ids.example.com"]);
        let snapshot = present(&live_entry());
        let ingest = ingest_owned();

        for status in [
            LookupStatus::NoMatch,
            LookupStatus::Timeout,
            LookupStatus::Skipped(SkipReason::NotDue),
        ] {
            let lookups = failed_lookup(status);

            // Act
            let change = plan_write(&input(&snapshot, None, &ingest, &lookups, &registry));

            // Assert
            assert!(
                change.is_none(),
                "should not store a browser UID for a provider-owned source"
            );
        }
    }

    #[test]
    fn opted_out_source_is_not_refilled_from_the_browser_on_the_next_navigation() {
        // Arrange: the row after an opt-out holds nothing for the source.
        let registry = registry(&["ids.example.com"]);
        let snapshot = present(&live_entry());
        let ingest = ingest_owned();
        let lookups = LookupResults::default();

        // Act
        let change = plan_write(&input(&snapshot, None, &ingest, &lookups, &registry));

        // Assert
        assert!(
            change.is_none(),
            "should not restore an opted-out provider UID from the browser cookie"
        );
    }

    #[test]
    fn new_row_skips_browser_uid_for_a_provider_owned_source() {
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
        assert!(
            !entry.ids.contains_key("ids.example.com"),
            "should not store a browser UID when creating the row"
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

    fn over_cap_legacy_entry() -> KvEntry {
        let mut entry = live_entry();
        for i in 0..40 {
            entry.ids.insert(
                format!("source{i:02}.example.com"),
                KvPartnerId::single(format!("{i:03}{}", "x".repeat(500))),
            );
        }
        entry
    }

    #[test]
    fn over_cap_row_with_no_change_plans_no_write() {
        // Arrange: a row written before the cap existed.
        let registry = registry(&[]);
        let entry = over_cap_legacy_entry();
        assert!(
            serde_json::to_vec(&entry).expect("should serialize").len() > MAX_ENTRY_BYTES,
            "should start over the cap"
        );
        let lookups = LookupResults::default();
        let snapshot = EcKvSnapshot::Present {
            ec_id: EC_ID.to_owned(),
            entry: Box::new(entry),
            generation: Some(1),
        };

        // Act
        let change = plan_write(&input(&snapshot, None, &[], &lookups, &registry));

        // Assert
        assert!(
            change.is_none(),
            "should not write when only the cap would trim"
        );
    }

    fn over_cap_snapshot() -> EcKvSnapshot {
        let entry = over_cap_legacy_entry();
        assert!(
            serde_json::to_vec(&entry).expect("should serialize").len() > MAX_ENTRY_BYTES,
            "should start over the cap"
        );
        EcKvSnapshot::Present {
            ec_id: EC_ID.to_owned(),
            entry: Box::new(entry),
            generation: Some(1),
        }
    }

    fn assert_fits(entry: &KvEntry) {
        let size = serde_json::to_vec(entry).expect("should serialize").len();
        assert!(
            size <= MAX_ENTRY_BYTES,
            "should fit the row cap, got {size}"
        );
    }

    #[test]
    fn over_cap_row_keeps_this_navigations_browser_uid() {
        // Arrange
        let registry = registry(&[]);
        let snapshot = over_cap_snapshot();
        let ingest = [PartnerIdUpdate::new("browser.example.com", "b-1")];
        let lookups = LookupResults::default();

        // Act
        let change = plan_write(&input(&snapshot, None, &ingest, &lookups, &registry));

        // Assert
        let Some(RowChange::Update { entry, .. }) = change else {
            panic!("should plan an update")
        };
        assert_fits(&entry);
        assert_eq!(
            entry
                .ids
                .get("browser.example.com")
                .map(|record| record.uid.as_str()),
            Some("b-1"),
            "should keep the browser UID that triggered the write"
        );
    }

    #[test]
    fn over_cap_row_keeps_this_navigations_provider_uid() {
        // Arrange
        let registry = registry(&["ids.example.com"]);
        let snapshot = over_cap_snapshot();
        let lookups = found("ids.example.com", &["p-1"]);

        // Act
        let change = plan_write(&input(&snapshot, None, &[], &lookups, &registry));

        // Assert
        let Some(RowChange::Update { entry, .. }) = change else {
            panic!("should plan an update")
        };
        assert_fits(&entry);
        assert_eq!(
            entry
                .ids
                .get("ids.example.com")
                .map(|record| record.uid.as_str()),
            Some("p-1"),
            "should keep the provider UID that triggered the write"
        );
    }

    fn fill_to(mut entry: KvEntry, limit: usize) -> KvEntry {
        let mut filler = 0;
        loop {
            let mut candidate = entry.clone();
            candidate.ids.insert(
                format!("filler{filler:03}.example.com"),
                KvPartnerId::single("f".repeat(100)),
            );
            if serde_json::to_vec(&candidate)
                .expect("should serialize")
                .len()
                > limit
            {
                return entry;
            }
            entry = candidate;
            filler += 1;
        }
    }

    #[test]
    fn provider_uid_wins_over_browser_uid_when_only_one_fits() {
        // Arrange: room for either new UID alone, not both.
        let registry = registry(&["ids.example.com"]);
        let entry = fill_to(live_entry(), MAX_ENTRY_BYTES - 800);
        let ingest = [PartnerIdUpdate::new("browser.example.com", "b".repeat(300))];
        let provider_uid = "p".repeat(300);
        let lookups = found("ids.example.com", &[provider_uid.as_str()]);

        // Act
        let change = plan_write(&input(&present(&entry), None, &ingest, &lookups, &registry));

        // Assert
        let Some(RowChange::Update { entry, .. }) = change else {
            panic!("should plan an update")
        };
        assert_fits(&entry);
        assert_eq!(
            entry
                .ids
                .get("ids.example.com")
                .map(|record| record.uid.as_str()),
            Some(provider_uid.as_str()),
            "should store the provider UID"
        );
        assert!(
            !entry.ids.contains_key("browser.example.com"),
            "should drop the browser UID, which the browser offers again"
        );
    }

    #[test]
    fn refresh_that_no_longer_fits_keeps_the_stored_provider_record() {
        // Arrange: the stored provider UID fits; its larger refresh does not.
        let registry = registry(&["ids.example.com"]);
        let entry = fill_to(provider_entry(&["p-old"]), MAX_ENTRY_BYTES - 300);
        let fresh = "p".repeat(900);
        let lookups = found("ids.example.com", &[fresh.as_str()]);

        // Act
        let change = plan_write(&input(&present(&entry), None, &[], &lookups, &registry));

        // Assert
        assert!(
            change.is_none(),
            "should keep the stored provider record rather than delete it"
        );
    }

    #[test]
    fn browser_ingest_never_evicts_a_stored_record_to_fit() {
        // Arrange: a stored provider record plus fillers, just under the cap.
        let registry = registry(&["ids.example.com"]);
        let big: Vec<String> = (0..18)
            .map(|i| format!("{i:03}{}", "x".repeat(500)))
            .collect();
        let ids: Vec<&str> = big.iter().map(String::as_str).collect();
        let mut entry = provider_entry(&ids);
        let mut filler = 0;
        loop {
            let mut candidate = entry.clone();
            candidate.ids.insert(
                format!("filler{filler:03}.example.com"),
                KvPartnerId::single("f".repeat(100)),
            );
            if serde_json::to_vec(&candidate)
                .expect("should serialize")
                .len()
                > MAX_ENTRY_BYTES - 200
            {
                break;
            }
            entry = candidate;
            filler += 1;
        }
        let ingest = [PartnerIdUpdate::new("browser.example.com", "b".repeat(500))];
        let lookups = LookupResults::default();

        // Act
        let change = plan_write(&input(&present(&entry), None, &ingest, &lookups, &registry));

        // Assert: the browser UID is dropped, so nothing changes.
        assert!(
            change.is_none(),
            "should drop the new browser UID rather than evict a stored record"
        );
    }

    #[test]
    fn found_result_never_restores_uids_over_a_newer_opt_out() {
        // Arrange: another request recorded the opt-out after this lookup ran.
        let registry = registry(&["ids.example.com"]);
        let mut entry = live_entry();
        entry.opt_outs.insert(
            ProviderId::new("example-provider"),
            2000 + OPT_OUT_MEMORY_SECONDS,
        );
        let lookups = found("ids.example.com", &["p-restored"]);

        // Act
        let change = plan_write(&input(&present(&entry), None, &[], &lookups, &registry));

        // Assert
        assert!(
            change.is_none(),
            "should not write the opted-out provider's older result"
        );
    }
}
