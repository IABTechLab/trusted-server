//! `IdentityMutation`: every KV change of the writer request, applied to the
//! live row with one write.
//!
//! Changes are checked against the live entry, not the snapshot they were
//! derived from, so a later writer can re-apply the same changes, re-derived
//! or still staged, without losing a concurrent tab's newer data.

use std::collections::BTreeMap;

use super::catalog::ProviderCatalog;
use super::ids::{IdentityProviderId, SourceDomain};
use super::input::RandomSource;
use super::revision::{
    IdentityKeys, Revision, invalidated_uid_hash, new_revision, record_revision,
};
use crate::ec::kv_types::{
    KvEntry, KvNetwork, KvOptOut, KvPartnerId, KvProviderState, MAX_INVALIDATED_UIDS,
};

/// One change to a source's record.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PartnerIdChange {
    /// From browser ingest, a KV-gap entry or an unowned provider. Applies
    /// only if the source is absent, its record is expired, or its writer is
    /// no longer configured.
    FillMissing {
        source: SourceDomain,
        record: KvPartnerId,
        writer: Option<IdentityProviderId>,
        staged_at: Option<u64>,
    },
    /// From the owning provider. Applies if the live record's revision equals
    /// `expected`, or if `expected` is `None` and the source has no valid ID
    /// this provider wrote. A browser-seeded or legacy value never blocks the
    /// owner.
    OwnerWrite {
        source: SourceDomain,
        record: KvPartnerId,
        writer: IdentityProviderId,
        expected: Option<Revision>,
        staged_at: Option<u64>,
    },
    /// From `Resolution::invalidate` or `also_invalidate`. Applies only if the
    /// revision matches.
    OwnerRemove {
        source: SourceDomain,
        writer: IdentityProviderId,
        expected: Revision,
    },
    /// From `Resolution::opted_out`, or a staged removal without a revision.
    /// Removes the source's record if this writer wrote it and, when
    /// `staged_at` is set, acquired it at or before then. A tie goes to the
    /// removal.
    WriterRemove {
        source: SourceDomain,
        writer: IdentityProviderId,
        staged_at: Option<u64>,
    },
}

impl PartnerIdChange {
    fn is_removal(&self) -> bool {
        matches!(self, Self::OwnerRemove { .. } | Self::WriterRemove { .. })
    }
}

/// The provider state a call read, for the atomic check of its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExpectedState {
    /// The call read no state: no check.
    NotRead,
    /// The call read no stored state.
    Absent,
    /// The call read this state JSON.
    Json(String),
}

/// How a change affects `ps.st`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum StateUpdate {
    #[default]
    Keep,
    Replace(serde_json::Value),
    Clear,
}

/// Changes to one provider's `ps` entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProviderStateChange {
    /// Next eligible time. Merged as the later value.
    pub(crate) n: Option<u64>,
    /// Consecutive failures. Replaced when set.
    pub(crate) f: Option<u32>,
    /// A newly reported opt-out.
    pub(crate) opt_out: Option<KvOptOut>,
    /// A re-consent clear of the opt-out, at this time.
    pub(crate) opt_out_clear: Option<u64>,
    /// Keyed hashes of newly invalidated UIDs.
    pub(crate) iv_add: Vec<String>,
    /// Private state.
    pub(crate) st: StateUpdate,
}

impl ProviderStateChange {
    fn merge(&mut self, other: Self) {
        self.n = later(self.n, other.n);
        if other.f.is_some() {
            self.f = other.f;
        }
        if let Some(opt_out) = other.opt_out
            && self
                .opt_out
                .as_ref()
                .is_none_or(|current| opt_out.at > current.at)
        {
            self.opt_out = Some(opt_out);
        }
        self.opt_out_clear = later(self.opt_out_clear, other.opt_out_clear);
        for hash in other.iv_add {
            if !self.iv_add.contains(&hash) {
                self.iv_add.push(hash);
            }
        }
        if other.st != StateUpdate::Keep {
            self.st = other.st;
        }
    }

    fn has_removal(&self) -> bool {
        self.opt_out.is_some() || self.opt_out_clear.is_some() || !self.iv_add.is_empty()
    }
}

fn later(current: Option<u64>, other: Option<u64>) -> Option<u64> {
    match (current, other) {
        (Some(current), Some(other)) => Some(current.max(other)),
        (current, other) => current.or(other),
    }
}

/// The positive part of one stateful provider's result, applied whole or not
/// at all. Its removals, opt-out and `iv` hashes are not part of it: they go
/// into the mutation's plain changes and apply on their own.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderResult {
    pub(crate) provider: IdentityProviderId,
    /// `OwnerWrite` and `FillMissing` changes for the sources it returned.
    pub(crate) ids: Vec<PartnerIdChange>,
    /// The state replacement or clear, and the new `n` and `f`.
    pub(crate) state: ProviderStateChange,
    /// The `ps.st` the call read.
    pub(crate) expected_state: ExpectedState,
}

/// Every KV change of one writer request.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct IdentityMutation {
    partner_ids: Vec<PartnerIdChange>,
    provider_results: Vec<ProviderResult>,
    provider_state: BTreeMap<IdentityProviderId, ProviderStateChange>,
    cluster_size: Option<u32>,
}

impl IdentityMutation {
    /// Adds a change.
    pub(crate) fn add_change(&mut self, change: PartnerIdChange) {
        self.partner_ids.push(change);
    }

    /// Adds a stateful provider's positive result.
    pub(crate) fn add_result(&mut self, result: ProviderResult) {
        self.provider_results.push(result);
    }

    /// Merges a change into the provider's pending state change.
    pub(crate) fn merge_state(
        &mut self,
        provider: IdentityProviderId,
        change: ProviderStateChange,
    ) {
        self.provider_state
            .entry(provider)
            .or_default()
            .merge(change);
    }

    /// Stores an evaluated cluster size.
    pub(crate) fn set_cluster_size(&mut self, cluster_size: u32) {
        self.cluster_size = Some(cluster_size);
    }

    /// Whether there is nothing to apply.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.partner_ids.is_empty()
            && self.provider_results.is_empty()
            && self.provider_state.is_empty()
            && self.cluster_size.is_none()
    }

    /// Whether it holds a removal, opt-out, clear or `iv` hash.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_removals(&self) -> bool {
        self.partner_ids.iter().any(PartnerIdChange::is_removal)
            || self
                .provider_state
                .values()
                .any(ProviderStateChange::has_removal)
    }

    /// The plain changes, for tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn changes(&self) -> &[PartnerIdChange] {
        &self.partner_ids
    }

    /// Only the removals, opt-outs, clears and `iv` hashes.
    #[must_use]
    pub(crate) fn removals_only(&self) -> Self {
        Self {
            partner_ids: self
                .partner_ids
                .iter()
                .filter(|change| change.is_removal())
                .cloned()
                .collect(),
            provider_results: Vec::new(),
            provider_state: self
                .provider_state
                .iter()
                .map(|(provider, change)| {
                    (
                        provider.clone(),
                        ProviderStateChange {
                            opt_out: change.opt_out.clone(),
                            opt_out_clear: change.opt_out_clear,
                            iv_add: change.iv_add.clone(),
                            ..ProviderStateChange::default()
                        },
                    )
                })
                .filter(|(_, change)| change.has_removal())
                .collect(),
            cluster_size: None,
        }
    }
}

/// What applying a mutation needs besides the live entry.
pub(crate) struct ApplyContext<'a> {
    /// Unix seconds.
    pub(crate) now: u64,
    pub(crate) keys: &'a IdentityKeys,
    pub(crate) random: &'a dyn RandomSource,
    pub(crate) catalog: &'a ProviderCatalog,
}

/// The result of applying a mutation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Applied {
    /// The entry to write.
    pub(crate) entry: KvEntry,
    /// Whether it differs from the live entry.
    pub(crate) changed: bool,
    /// Whether the full result failed validation and only removals applied.
    pub(crate) removals_only: bool,
}

/// Applies `mutation` to the live entry.
///
/// In order:
///
/// 1. A tombstone drops everything: `None`.
/// 2. Removals: revision-checked `OwnerRemove`, `WriterRemove`, then new
///    opt-outs (each removes every record its provider wrote and clears its
///    state), then clears strictly later than the provider's latest opt-out,
///    then `iv` hashes (newest four kept).
/// 3. Positive changes by their own rules. Changes from an opted-out
///    provider, and staged changes at or before its latest opt-out, are
///    skipped. Each stateful result applies whole, or not at all when any of
///    its IDs would not apply or the live state is not the one it read.
/// 4. `ps` changes and `cluster_size`, then pruning of unconfigured
///    providers' `ps` and of expired records that are not refresh anchors.
/// 5. Validation. On failure only the removals are kept, and if that fails
///    too the mutation is dropped: `None`.
/// 6. `changed` reports whether anything differs from the live entry.
#[must_use]
pub(crate) fn apply(
    mutation: &IdentityMutation,
    live: &KvEntry,
    context: &ApplyContext<'_>,
) -> Option<Applied> {
    if !live.consent.ok {
        return None;
    }

    let mut entry = live.clone();
    apply_removals(&mut entry, mutation, context);
    let mut removals_entry = entry.clone();

    apply_positive(&mut entry, mutation, context);
    apply_state(&mut entry, mutation);
    prune(&mut entry, context);

    let (entry, removals_only) = match entry.validate() {
        Ok(()) => (entry, false),
        Err(message) => {
            log::warn!(
                "Identity mutation produced an invalid row, keeping removals only: {message}"
            );
            prune(&mut removals_entry, context);
            if let Err(message) = removals_entry.validate() {
                log::warn!(
                    "Identity mutation removals-only row is invalid, dropping it: {message}"
                );
                return None;
            }
            (removals_entry, true)
        }
    };

    let changed = &entry != live;
    Some(Applied {
        entry,
        changed,
        removals_only,
    })
}

fn apply_removals(entry: &mut KvEntry, mutation: &IdentityMutation, context: &ApplyContext<'_>) {
    for change in &mutation.partner_ids {
        match change {
            PartnerIdChange::OwnerRemove {
                source, expected, ..
            } => {
                let matches = entry
                    .ids
                    .get(source.as_str())
                    .is_some_and(|record| record_revision(context.keys, record) == *expected);
                if matches {
                    entry.ids.remove(source.as_str());
                }
            }
            PartnerIdChange::WriterRemove {
                source,
                writer,
                staged_at,
            } => {
                let matches = entry.ids.get(source.as_str()).is_some_and(|record| {
                    record.w.as_deref() == Some(writer.as_str())
                        && staged_at.is_none_or(|staged| record.at.unwrap_or(0) <= staged)
                });
                if matches {
                    entry.ids.remove(source.as_str());
                }
            }
            PartnerIdChange::FillMissing { .. } | PartnerIdChange::OwnerWrite { .. } => {}
        }
    }

    for (provider, change) in &mutation.provider_state {
        if let Some(opt_out) = &change.opt_out {
            apply_opt_out(entry, provider, opt_out);
        }
    }
    for (provider, change) in &mutation.provider_state {
        if let Some(clear) = change.opt_out_clear {
            apply_opt_out_clear(entry, provider, change.opt_out.as_ref(), clear);
        }
        if !change.iv_add.is_empty() {
            let state = entry.ps.entry(provider.to_string()).or_default();
            for hash in &change.iv_add {
                if !state.iv.contains(hash) {
                    state.iv.push(hash.clone());
                }
            }
            let excess = state.iv.len().saturating_sub(MAX_INVALIDATED_UIDS);
            state.iv.drain(..excess);
        }
    }
}

/// Applies an opt-out reported after the live one and at or after the live
/// clear. It removes every record the provider wrote, at any revision, and
/// clears its state.
fn apply_opt_out(entry: &mut KvEntry, provider: &IdentityProviderId, opt_out: &KvOptOut) {
    let state = entry.ps.entry(provider.to_string()).or_default();
    let newer = state
        .o
        .as_ref()
        .is_none_or(|current| opt_out.at > current.at);
    let not_stale = state.oc.is_none_or(|cleared| opt_out.at >= cleared);
    if !newer || !not_stale {
        return;
    }
    state.o = Some(opt_out.clone());
    state.st = None;
    entry
        .ids
        .retain(|_, record| record.w.as_deref() != Some(provider.as_str()));
}

/// Stores a clear only when it is strictly later than the provider's latest
/// opt-out, live or staged. An opt-out and a clear in the same second leave
/// the provider opted out.
fn apply_opt_out_clear(
    entry: &mut KvEntry,
    provider: &IdentityProviderId,
    staged_opt_out: Option<&KvOptOut>,
    clear: u64,
) {
    let state = entry.ps.entry(provider.to_string()).or_default();
    let latest_opt_out = state
        .o
        .as_ref()
        .map(|opt_out| opt_out.at)
        .max(staged_opt_out.map(|opt_out| opt_out.at));
    let Some(latest_opt_out) = latest_opt_out else {
        return;
    };
    if clear > latest_opt_out {
        state.oc = later(state.oc, Some(clear));
    }
}

fn opt_out_of(entry: &KvEntry, provider: &IdentityProviderId) -> Option<(u64, bool)> {
    let state = entry.ps.get(provider.as_str())?;
    let opt_out = state.o.as_ref()?;
    let active = state.oc.is_none_or(|cleared| cleared <= opt_out.at);
    Some((opt_out.at, active))
}

/// Whether a provider's opt-out blocks a change staged at `staged_at`.
fn blocked_by_opt_out(
    entry: &KvEntry,
    provider: &IdentityProviderId,
    staged_at: Option<u64>,
) -> bool {
    match opt_out_of(entry, provider) {
        Some((_, true)) => true,
        Some((at, false)) => staged_at.is_some_and(|staged| staged <= at),
        None => false,
    }
}

fn record_is_expired(record: &KvPartnerId, now: u64) -> bool {
    record.exp.is_some_and(|exp| now >= exp)
}

fn writer_is_configured(record: &KvPartnerId, catalog: &ProviderCatalog) -> bool {
    record.w.as_deref().is_none_or(|writer| {
        writer
            .parse::<IdentityProviderId>()
            .is_ok_and(|writer| catalog.is_configured(&writer))
    })
}

fn invalidated_by_owner(
    entry: &KvEntry,
    source: &SourceDomain,
    uid: &str,
    context: &ApplyContext<'_>,
) -> bool {
    let Some(owner) = context.catalog.owner_of(source) else {
        return false;
    };
    let Some(state) = entry.ps.get(owner.as_str()) else {
        return false;
    };
    let hash = invalidated_uid_hash(context.keys, owner, uid);
    state.iv.contains(&hash)
}

fn change_applies(entry: &KvEntry, change: &PartnerIdChange, context: &ApplyContext<'_>) -> bool {
    match change {
        PartnerIdChange::FillMissing {
            source,
            record,
            writer,
            staged_at,
        } => {
            if writer
                .as_ref()
                .is_some_and(|writer| blocked_by_opt_out(entry, writer, *staged_at))
            {
                return false;
            }
            if invalidated_by_owner(entry, source, &record.uid, context) {
                return false;
            }
            entry.ids.get(source.as_str()).is_none_or(|existing| {
                record_is_expired(existing, context.now)
                    || !writer_is_configured(existing, context.catalog)
            })
        }
        PartnerIdChange::OwnerWrite {
            source,
            writer,
            expected,
            staged_at,
            ..
        } => {
            if blocked_by_opt_out(entry, writer, *staged_at) {
                return false;
            }
            let existing = entry.ids.get(source.as_str());
            match expected {
                Some(expected) => existing
                    .is_some_and(|record| record_revision(context.keys, record) == *expected),
                None => !existing.is_some_and(|record| {
                    record.w.as_deref() == Some(writer.as_str())
                        && !record_is_expired(record, context.now)
                }),
            }
        }
        PartnerIdChange::OwnerRemove { .. } | PartnerIdChange::WriterRemove { .. } => false,
    }
}

fn write_change(entry: &mut KvEntry, change: &PartnerIdChange, context: &ApplyContext<'_>) {
    let (source, record, writer) = match change {
        PartnerIdChange::FillMissing {
            source,
            record,
            writer,
            ..
        } => (source, record, writer.as_ref()),
        PartnerIdChange::OwnerWrite {
            source,
            record,
            writer,
            ..
        } => (source, record, Some(writer)),
        PartnerIdChange::OwnerRemove { .. } | PartnerIdChange::WriterRemove { .. } => return,
    };
    let mut written = record.clone();
    written.w = writer.map(ToString::to_string);
    written.rv = Some(new_revision(context.random).as_str().to_owned());
    written.at = record.at.or(Some(context.now));
    entry.ids.insert(source.to_string(), written);
}

fn state_matches(entry: &KvEntry, provider: &IdentityProviderId, expected: &ExpectedState) -> bool {
    let live = entry
        .ps
        .get(provider.as_str())
        .and_then(|state| state.st.as_ref());
    match expected {
        ExpectedState::NotRead => true,
        ExpectedState::Absent => live.is_none(),
        ExpectedState::Json(json) => serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .is_some_and(|read| live == Some(&read)),
    }
}

fn apply_positive(entry: &mut KvEntry, mutation: &IdentityMutation, context: &ApplyContext<'_>) {
    for change in &mutation.partner_ids {
        if change_applies(entry, change, context) {
            write_change(entry, change, context);
        }
    }
    for result in &mutation.provider_results {
        let whole = result
            .ids
            .iter()
            .all(|change| change_applies(entry, change, context))
            && state_matches(entry, &result.provider, &result.expected_state);
        if !whole {
            log::debug!(
                "Dropping identity result for {} that lost to a newer write",
                result.provider
            );
            continue;
        }
        for change in &result.ids {
            write_change(entry, change, context);
        }
        apply_provider_state(entry, &result.provider, &result.state);
    }
}

fn apply_provider_state(
    entry: &mut KvEntry,
    provider: &IdentityProviderId,
    change: &ProviderStateChange,
) {
    let state: &mut KvProviderState = entry.ps.entry(provider.to_string()).or_default();
    state.n = later(state.n, change.n);
    if let Some(failures) = change.f {
        state.f = failures;
    }
    match &change.st {
        StateUpdate::Keep => {}
        StateUpdate::Replace(value) => state.st = Some(value.clone()),
        StateUpdate::Clear => state.st = None,
    }
}

fn apply_state(entry: &mut KvEntry, mutation: &IdentityMutation) {
    for (provider, change) in &mutation.provider_state {
        let opted_out = opt_out_of(entry, provider).is_some_and(|(_, active)| active);
        let mut positive = change.clone();
        // An opted-out provider keeps its state cleared.
        if opted_out {
            positive.st = StateUpdate::Keep;
        }
        apply_provider_state(entry, provider, &positive);
    }
    if let Some(cluster_size) = mutation.cluster_size {
        entry
            .network
            .get_or_insert(KvNetwork { cluster_size: None })
            .cluster_size = Some(cluster_size);
    }
}

/// Drops the `ps` of unconfigured providers and expired records, except
/// refresh anchors: an expired record of an owned source whose owner still
/// holds state and whose `ru` has not passed.
fn prune(entry: &mut KvEntry, context: &ApplyContext<'_>) {
    entry.ps.retain(|provider, _| {
        provider
            .parse::<IdentityProviderId>()
            .is_ok_and(|provider| context.catalog.is_configured(&provider))
    });
    let states = entry.ps.clone();
    entry.ids.retain(|source, record| {
        if !record_is_expired(record, context.now) {
            return true;
        }
        let owner_holds_state = source
            .parse::<SourceDomain>()
            .ok()
            .and_then(|source| context.catalog.owner_of(&source).cloned())
            .and_then(|owner| states.get(owner.as_str()))
            .is_some_and(|state| state.st.is_some());
        owner_holds_state && record.ru.is_some_and(|until| context.now < until)
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::revision::invalidated_uid_hash;
    use crate::ec::identity::test_support::{fixed_provider, test_keys};
    use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId, KvProviderState};

    const NOW: u64 = 1_000;

    fn src(name: &str) -> SourceDomain {
        name.parse().expect("should parse source")
    }

    fn pid(name: &str) -> IdentityProviderId {
        name.parse().expect("should parse id")
    }

    fn catalog() -> ProviderCatalog {
        ProviderCatalog::new(
            vec![
                fixed_provider("owner", &["a.example.com", "c.example.com"]),
                fixed_provider("other", &["b.example.com"]),
            ],
            [(src("a.example.com"), pid("owner"))].into_iter().collect(),
        )
    }

    fn run(mutation: &IdentityMutation, live: &KvEntry) -> Option<Applied> {
        let keys = test_keys();
        let random = SeededRandom::new(1);
        let catalog = catalog();
        apply(
            mutation,
            live,
            &ApplyContext {
                now: NOW,
                keys: &keys,
                random: &random,
                catalog: &catalog,
            },
        )
    }

    fn owned(uid: &str, rv: &str) -> KvPartnerId {
        KvPartnerId {
            w: Some("owner".to_owned()),
            rv: Some(rv.to_owned()),
            at: Some(900),
            ..KvPartnerId::new(uid)
        }
    }

    fn fill(source: &str, uid: &str) -> PartnerIdChange {
        PartnerIdChange::FillMissing {
            source: src(source),
            record: KvPartnerId::browser(uid, Some(NOW)),
            writer: None,
            staged_at: None,
        }
    }

    #[test]
    fn tombstone_drops_everything() {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("b.example.com", "u"));
        assert!(
            run(&mutation, &KvEntry::tombstone(1)).is_none(),
            "should drop on a tombstone"
        );
    }

    #[test]
    fn empty_mutation_changes_nothing() {
        let live = KvEntry::minimal("b.example.com", "x", 1);
        let applied = run(&IdentityMutation::default(), &live).expect("should apply");
        assert!(!applied.changed, "nothing to change");
        assert!(IdentityMutation::default().is_empty(), "default is empty");
    }

    #[test]
    fn fill_missing_fills_absent_and_expired_only() {
        let mut live = KvEntry::minimal("b.example.com", "existing", 1);
        live.ids.insert(
            "c.example.com".to_owned(),
            KvPartnerId {
                exp: Some(NOW),
                ..KvPartnerId::new("old")
            },
        );
        let mut mutation = IdentityMutation::default();
        for source in ["b.example.com", "c.example.com", "d.example.com"] {
            mutation.add_change(fill(source, "new"));
        }

        let applied = run(&mutation, &live).expect("should apply");

        assert!(applied.changed, "should change");
        assert_eq!(
            applied.entry.ids["b.example.com"].uid, "existing",
            "should not replace a live value"
        );
        assert_eq!(
            applied.entry.ids["c.example.com"].uid, "new",
            "should replace an expired value"
        );
        assert_eq!(
            applied.entry.ids["d.example.com"].uid, "new",
            "should fill an absent source"
        );
    }

    #[test]
    fn fill_missing_replaces_a_record_whose_writer_is_gone() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "d.example.com".to_owned(),
            KvPartnerId {
                w: Some("retired".to_owned()),
                ..KvPartnerId::new("orphaned")
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("d.example.com", "fresh"));
        let applied = run(&mutation, &live).expect("should apply");
        assert_eq!(
            applied.entry.ids["d.example.com"].uid, "fresh",
            "a record from an unconfigured writer counts as absent"
        );
    }

    #[test]
    fn owner_write_checks_revision_and_gets_a_fresh_rv() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids
            .insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));

        let mut stale = IdentityMutation::default();
        stale.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("v2"),
            writer: pid("owner"),
            expected: Some(Revision::from_stored("2222222222222222")),
            staged_at: None,
        });
        let applied = run(&stale, &live).expect("should apply");
        assert!(
            !applied.changed,
            "a stale revision loses to the newer write"
        );

        let mut fresh = IdentityMutation::default();
        fresh.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("v2"),
            writer: pid("owner"),
            expected: Some(Revision::from_stored("1111111111111111")),
            staged_at: None,
        });
        let applied = run(&fresh, &live).expect("should apply");
        let record = &applied.entry.ids["a.example.com"];
        assert_eq!(record.uid, "v2", "should write");
        assert_eq!(record.w.as_deref(), Some("owner"), "should set the writer");
        assert!(
            record
                .rv
                .as_deref()
                .is_some_and(|rv| rv != "1111111111111111" && rv.len() == 16),
            "should draw a new rv"
        );
        assert_eq!(record.at, Some(NOW), "should stamp at");
    }

    #[test]
    fn browser_seeded_value_never_blocks_the_owner() {
        let mut live = KvEntry::minimal("a.example.com", "seeded", 1);
        live.ids.get_mut("a.example.com").expect("should hold a").at = Some(5);
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("owned"),
            writer: pid("owner"),
            expected: None,
            staged_at: None,
        });
        let applied = run(&mutation, &live).expect("should apply");
        assert_eq!(
            applied.entry.ids["a.example.com"].uid, "owned",
            "owner takes over a seeded value"
        );
    }

    #[test]
    fn owner_write_without_revision_loses_to_its_own_valid_value() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids
            .insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("v2"),
            writer: pid("owner"),
            expected: None,
            staged_at: None,
        });
        let applied = run(&mutation, &live).expect("should apply");
        assert_eq!(
            applied.entry.ids["a.example.com"].uid, "v1",
            "a write without a revision cannot overwrite the owner's own valid value"
        );
    }

    #[test]
    fn owner_remove_needs_the_matching_revision() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids
            .insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));
        let remove = |rv: &str| {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(PartnerIdChange::OwnerRemove {
                source: src("a.example.com"),
                writer: pid("owner"),
                expected: Revision::from_stored(rv),
            });
            run(&mutation, &live).expect("should apply")
        };
        assert!(
            !remove("1111111111111111")
                .entry
                .ids
                .contains_key("a.example.com"),
            "matching revision removes"
        );
        assert!(
            remove("2222222222222222")
                .entry
                .ids
                .contains_key("a.example.com"),
            "a newer write survives"
        );
    }

    #[test]
    fn removals_win_ties_and_opt_out_removes_everything_the_writer_wrote() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids
            .insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));
        live.ids.insert(
            "c.example.com".to_owned(),
            KvPartnerId {
                w: Some("owner".to_owned()),
                at: Some(950),
                ..KvPartnerId::new("unowned-by-owner")
            },
        );
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                st: Some(json!({ "token": "t" })),
                ..KvProviderState::default()
            },
        );

        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            pid("owner"),
            ProviderStateChange {
                opt_out: Some(KvOptOut {
                    at: NOW,
                    fp: "0".repeat(16),
                }),
                ..ProviderStateChange::default()
            },
        );
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("v2"),
            writer: pid("owner"),
            expected: None,
            staged_at: Some(NOW),
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should remove the owned record"
        );
        assert!(
            !applied.entry.ids.contains_key("c.example.com"),
            "should remove the unowned record it wrote"
        );
        assert_eq!(
            applied.entry.ids["b.example.com"].uid, "x",
            "control: other records untouched"
        );
        assert_eq!(
            applied.entry.ps["owner"]
                .o
                .as_ref()
                .map(|opt_out| opt_out.at),
            Some(NOW),
            "should store the opt-out"
        );
        assert_eq!(applied.entry.ps["owner"].st, None, "should clear the state");
    }

    #[test]
    fn writer_remove_without_revision_respects_staged_at() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids
            .insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));
        let remove = |staged_at| {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(PartnerIdChange::WriterRemove {
                source: src("a.example.com"),
                writer: pid("owner"),
                staged_at: Some(staged_at),
            });
            run(&mutation, &live).expect("should apply")
        };
        assert!(
            !remove(900).entry.ids.contains_key("a.example.com"),
            "same second: the removal wins"
        );
        assert!(
            remove(899).entry.ids.contains_key("a.example.com"),
            "re-acquired after sa: kept"
        );
    }

    #[test]
    fn opt_out_and_clear_in_the_same_second_leave_it_opted_out() {
        let live = KvEntry::minimal("b.example.com", "x", 1);
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            pid("owner"),
            ProviderStateChange {
                opt_out: Some(KvOptOut {
                    at: NOW,
                    fp: "0".repeat(16),
                }),
                opt_out_clear: Some(NOW),
                ..ProviderStateChange::default()
            },
        );
        let applied = run(&mutation, &live).expect("should apply");
        assert!(
            applied.entry.ps["owner"].o.is_some(),
            "should stay opted out"
        );
        assert_eq!(
            applied.entry.ps["owner"].oc, None,
            "should drop a clear that is not later"
        );
    }

    #[test]
    fn later_clear_lifts_the_opt_out_but_older_staged_entries_stay_out() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                o: Some(KvOptOut {
                    at: 500,
                    fp: "0".repeat(16),
                }),
                ..KvProviderState::default()
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            pid("owner"),
            ProviderStateChange {
                opt_out_clear: Some(600),
                ..ProviderStateChange::default()
            },
        );
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId::new("before-opt-out"),
            writer: pid("owner"),
            expected: None,
            staged_at: Some(400),
        });
        mutation.add_change(PartnerIdChange::FillMissing {
            source: src("c.example.com"),
            record: KvPartnerId::new("after-clear"),
            writer: Some(pid("owner")),
            staged_at: Some(700),
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert_eq!(
            applied.entry.ps["owner"].oc,
            Some(600),
            "should store the clear"
        );
        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "an entry staged before the opt-out stays out"
        );
        assert_eq!(
            applied.entry.ids["c.example.com"].uid, "after-clear",
            "an entry staged after the clear lands"
        );
    }

    #[test]
    fn invalidated_uid_is_not_reseeded() {
        let keys = test_keys();
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                iv: vec![invalidated_uid_hash(&keys, &pid("owner"), "rejected")],
                ..KvProviderState::default()
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("a.example.com", "rejected"));
        mutation.add_change(fill("b.example.com", "other"));
        let applied = run(&mutation, &live).expect("should apply");
        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should not re-seed the rejected UID"
        );
        assert_eq!(
            applied.entry.ids["b.example.com"].uid, "x",
            "control: the unowned source is untouched by iv"
        );
    }

    #[test]
    fn iv_hashes_keep_the_newest_four() {
        let live = KvEntry::minimal("b.example.com", "x", 1);
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            pid("owner"),
            ProviderStateChange {
                iv_add: (0..6).map(|index| format!("{index:016x}")).collect(),
                ..ProviderStateChange::default()
            },
        );
        let applied = run(&mutation, &live).expect("should apply");
        assert_eq!(
            applied.entry.ps["owner"].iv,
            (2..6)
                .map(|index| format!("{index:016x}"))
                .collect::<Vec<_>>(),
            "should keep the four newest hashes"
        );
    }

    #[test]
    fn provider_result_applies_atomically() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            owned("newer", "3333333333333333"),
        );
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                st: Some(json!({ "token": "B" })),
                ..KvProviderState::default()
            },
        );

        let mut mutation = IdentityMutation::default();
        mutation.add_result(ProviderResult {
            provider: pid("owner"),
            ids: vec![PartnerIdChange::OwnerWrite {
                source: src("a.example.com"),
                record: KvPartnerId::new("mine"),
                writer: pid("owner"),
                expected: Some(Revision::from_stored("1111111111111111")),
                staged_at: None,
            }],
            state: ProviderStateChange {
                st: StateUpdate::Replace(json!({ "token": "A" })),
                f: Some(0),
                n: Some(5_000),
                ..ProviderStateChange::default()
            },
            expected_state: ExpectedState::Json(r#"{"token":"A0"}"#.to_owned()),
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert_eq!(
            applied.entry.ids["a.example.com"].uid, "newer",
            "should keep the other writer's ID"
        );
        assert_eq!(
            applied.entry.ps["owner"].st,
            Some(json!({ "token": "B" })),
            "with that writer's state"
        );
        assert_eq!(applied.entry.ps["owner"].n, None, "n is part of the result");
        assert!(!applied.changed, "nothing else changed");
    }

    #[test]
    fn provider_result_lands_whole_when_everything_matches() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                st: Some(json!({ "token": "A0" })),
                f: 2,
                ..KvProviderState::default()
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_result(ProviderResult {
            provider: pid("owner"),
            ids: vec![PartnerIdChange::OwnerWrite {
                source: src("a.example.com"),
                record: KvPartnerId::new("mine"),
                writer: pid("owner"),
                expected: None,
                staged_at: None,
            }],
            state: ProviderStateChange {
                st: StateUpdate::Replace(json!({ "token": "A1" })),
                f: Some(0),
                ..ProviderStateChange::default()
            },
            expected_state: ExpectedState::Json(r#"{"token":"A0"}"#.to_owned()),
        });
        let applied = run(&mutation, &live).expect("should apply");
        assert_eq!(applied.entry.ids["a.example.com"].uid, "mine", "id lands");
        assert_eq!(
            applied.entry.ps["owner"].st,
            Some(json!({ "token": "A1" })),
            "state lands"
        );
        assert_eq!(applied.entry.ps["owner"].f, 0, "f lands");
    }

    #[test]
    fn refresh_anchor_survives_until_ru_or_state_loss() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                exp: Some(NOW - 1),
                ru: Some(NOW + 100),
                ..owned("expired", "1111111111111111")
            },
        );
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                st: Some(json!({ "r": 1 })),
                ..KvProviderState::default()
            },
        );
        let mut unrelated = IdentityMutation::default();
        unrelated.set_cluster_size(1);

        let kept = run(&unrelated, &live).expect("should apply");
        assert!(
            kept.entry.ids.contains_key("a.example.com"),
            "anchor kept while state lives"
        );
        assert_eq!(
            kept.entry
                .network
                .as_ref()
                .and_then(|network| network.cluster_size),
            Some(1),
            "cluster size stored"
        );

        live.ps.get_mut("owner").expect("should hold owner").st = None;
        let dropped = run(&unrelated, &live).expect("should apply");
        assert!(
            !dropped.entry.ids.contains_key("a.example.com"),
            "anchor dropped without state"
        );

        live.ps.get_mut("owner").expect("should hold owner").st = Some(json!({ "r": 1 }));
        live.ids.get_mut("a.example.com").expect("should hold a").ru = Some(NOW);
        let past = run(&unrelated, &live).expect("should apply");
        assert!(
            !past.entry.ids.contains_key("a.example.com"),
            "anchor dropped once ru passes"
        );
    }

    #[test]
    fn unconfigured_provider_state_is_pruned() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert(
            "retired".to_owned(),
            KvProviderState {
                n: Some(5),
                ..KvProviderState::default()
            },
        );
        live.ps.insert(
            "other".to_owned(),
            KvProviderState {
                n: Some(5),
                ..KvProviderState::default()
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.set_cluster_size(1);
        let applied = run(&mutation, &live).expect("should apply");
        assert!(!applied.entry.ps.contains_key("retired"), "should prune");
        assert!(
            applied.entry.ps.contains_key("other"),
            "control: configured kept"
        );
    }

    #[test]
    fn invalid_result_falls_back_to_removals_only() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids
            .insert("a.example.com".to_owned(), owned("v1", "1111111111111111"));
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("d.example.com", &"x".repeat(600)));
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            writer: pid("owner"),
            expected: Revision::from_stored("1111111111111111"),
        });
        let applied = run(&mutation, &live).expect("should apply removals");
        assert!(applied.removals_only, "should fall back");
        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "removal applied"
        );
        assert!(
            !applied.entry.ids.contains_key("d.example.com"),
            "invalid fill dropped"
        );
    }

    #[test]
    fn merge_state_combines_changes_for_one_provider() {
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(
            pid("owner"),
            ProviderStateChange {
                n: Some(10),
                iv_add: vec!["a".repeat(16)],
                ..ProviderStateChange::default()
            },
        );
        mutation.merge_state(
            pid("owner"),
            ProviderStateChange {
                n: Some(5),
                f: Some(3),
                iv_add: vec!["b".repeat(16)],
                st: StateUpdate::Clear,
                ..ProviderStateChange::default()
            },
        );
        let live = KvEntry::minimal("b.example.com", "x", 1);
        let applied = run(&mutation, &live).expect("should apply");
        let state = &applied.entry.ps["owner"];
        assert_eq!(state.n, Some(10), "n takes the later value");
        assert_eq!(state.f, 3, "f is the last one given");
        assert_eq!(state.iv.len(), 2, "iv is the union");
        assert!(mutation.has_removals(), "iv additions count as removals");
    }
}
