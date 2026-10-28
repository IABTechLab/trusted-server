//! One request's identity changes as a single mutation, and its merge into
//! the live KV row (spec 5.4).

use std::collections::{BTreeMap, BTreeSet};

use super::catalog::ProviderCatalog;
use super::descriptor::Resolves;
use super::ids::{IdentityProviderId, SourceDomain};
use super::input::RandomSource;
use super::revision::{
    Revision, StateRevision, new_revision, push_hashes, record_revision, set_hashes,
};
use crate::ec::kv_types::{
    KvEntry, KvNetwork, KvOptOut, KvPartnerId, KvProviderState, MAX_INVALIDATED_UIDS,
    MAX_PROVIDER_STATES,
};

/// A change to one source's record (spec 5.4).
///
/// Each `record` is the source's whole record: its `eids`, `w`, the timing
/// fields and `rv`. Every change writes or removes a source's set whole.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PartnerIdChange {
    /// From browser ingest (`record.w` is `None`) or an unowned provider
    /// (`record.w` names it). Fills a source that holds nothing usable.
    FillMissing {
        source: SourceDomain,
        record: KvPartnerId,
    },
    /// From the owning provider, whose ID `record.w` holds.
    OwnerWrite {
        source: SourceDomain,
        record: KvPartnerId,
        expected: Option<Revision>,
    },
    /// From `Resolution::invalidate` or `also_invalidate`. `provider` is the
    /// provider that invalidated, whose `ps.iv` receives the hashes.
    OwnerRemove {
        source: SourceDomain,
        provider: IdentityProviderId,
        expected: Revision,
    },
    /// From `Resolution::opted_out`. Removes the source's record if `writer`
    /// wrote it, at any revision, and for a capture-only `writer` also a
    /// record no provider wrote. Its hashes go to the writer's `ps.ov`.
    WriterRemove {
        source: SourceDomain,
        writer: IdentityProviderId,
    },
}

impl PartnerIdChange {
    /// Whether the change removes a record.
    pub(crate) fn is_removal(&self) -> bool {
        matches!(self, Self::OwnerRemove { .. } | Self::WriterRemove { .. })
    }
}

/// What a change does to a provider's private state `ps.st`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum StateUpdate {
    /// Leave `st` as it is.
    #[default]
    Keep,
    /// Replace `st`.
    Replace(serde_json::Value),
    /// Remove `st`.
    Clear,
}

/// A change to one provider's `ps` entry. It has no `iv` or `ov` field:
/// those hashes come only from applied removals.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProviderStateChange {
    pub(crate) n: Option<u64>,
    pub(crate) f: Option<u32>,
    pub(crate) opt_out: Option<KvOptOut>,
    pub(crate) opt_out_clear: Option<u64>,
    pub(crate) st: StateUpdate,
}

impl ProviderStateChange {
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    fn is_noop(&self) -> bool {
        *self == Self::default()
    }

    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    fn has_removal(&self) -> bool {
        self.opt_out.is_some() || self.opt_out_clear.is_some()
    }

    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    fn removals(&self) -> Self {
        Self {
            opt_out: self.opt_out.clone(),
            opt_out_clear: self.opt_out_clear,
            ..Self::default()
        }
    }

    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    fn merge(&mut self, later: Self) {
        self.n = self.n.max(later.n);
        self.f = later.f.or(self.f);
        if later.opt_out.as_ref().map(|o| o.at) >= self.opt_out.as_ref().map(|o| o.at) {
            self.opt_out = later.opt_out.or_else(|| self.opt_out.take());
        }
        self.opt_out_clear = self.opt_out_clear.max(later.opt_out_clear);
        if later.st != StateUpdate::Keep {
            self.st = later.st;
        }
    }
}

/// One provider call's outcome. It applies as a unit: every ID change must
/// apply on its own and the state revision must still match.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderResult {
    pub(crate) provider: IdentityProviderId,
    /// Positive changes only. Removals go to the mutation's own list.
    pub(crate) ids: Vec<PartnerIdChange>,
    pub(crate) state: ProviderStateChange,
    /// `None` means the call read no state, so the state check always passes.
    pub(crate) expected_state: Option<StateRevision>,
}

/// Everything one request changes in the identity row.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct IdentityMutation {
    partner_ids: Vec<PartnerIdChange>,
    provider_state: BTreeMap<IdentityProviderId, ProviderStateChange>,
    results: Vec<ProviderResult>,
    cluster_size: Option<u32>,
    /// Providers whose stand-alone state change comes from a `found` outcome.
    found_states: BTreeSet<IdentityProviderId>,
}

impl IdentityMutation {
    /// Adds a change that stands alone.
    pub(crate) fn add_change(&mut self, change: PartnerIdChange) {
        self.partner_ids.push(change);
    }

    /// Adds a provider call's result.
    ///
    /// A removal in `result.ids` is a provider-side mistake: removals go
    /// through `partner_ids` and `provider_state`. It is logged by provider
    /// and source, never by UID.
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn add_result(&mut self, result: ProviderResult) {
        for change in result.ids.iter().filter(|change| change.is_removal()) {
            let (PartnerIdChange::OwnerRemove { source, .. }
            | PartnerIdChange::WriterRemove { source, .. }) = change
            else {
                continue;
            };
            log::warn!(
                "Identity provider '{}' put a removal for source '{}' in its result IDs; removals belong in partner_ids",
                result.provider,
                source.as_str(),
            );
        }
        self.results.push(result);
    }

    /// Merges a state change into any earlier one for the same provider:
    /// later `n`, last `f`, the later opt-out and clear, last non-`Keep`
    /// `st`.
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
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

    /// Merges the state change of a `found` outcome and tags it, so
    /// [`retain_for_error_status`](Self::retain_for_error_status) can drop
    /// its `n` and `f` with the positive answer it belongs to.
    pub(crate) fn merge_found_state(
        &mut self,
        provider: IdentityProviderId,
        change: ProviderStateChange,
    ) {
        self.found_states.insert(provider.clone());
        self.merge_state(provider, change);
    }

    /// Adds everything `other` holds, as if it had been staged here after
    /// the changes already present.
    pub(crate) fn absorb(&mut self, other: Self) {
        let Self {
            partner_ids,
            provider_state,
            results,
            cluster_size,
            found_states,
        } = other;
        self.partner_ids.extend(partner_ids);
        for (provider, change) in provider_state {
            self.merge_state(provider, change);
        }
        self.results.extend(results);
        self.cluster_size = cluster_size.or(self.cluster_size);
        self.found_states.extend(found_states);
    }

    /// Sets the cluster size to write.
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn set_cluster_size(&mut self, size: u32) {
        self.cluster_size = Some(size);
    }

    /// How many changes the mutation holds: stand-alone ones, provider
    /// results and provider state changes. Holds no UID, so it is safe to log.
    #[must_use]
    pub(crate) fn change_count(&self) -> usize {
        self.partner_ids.len() + self.results.len() + self.provider_state.len()
    }

    /// The stand-alone changes, for tests that check what a caller staged.
    #[cfg(test)]
    pub(crate) fn positive_changes(&self) -> &[PartnerIdChange] {
        &self.partner_ids
    }

    /// Whether nothing would change.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.partner_ids.is_empty()
            && self.results.is_empty()
            && self.cluster_size.is_none()
            && self
                .provider_state
                .values()
                .all(ProviderStateChange::is_noop)
    }

    /// Whether any removal, opt-out or clear is present.
    #[must_use]
    pub(crate) fn has_removals(&self) -> bool {
        self.partner_ids.iter().any(PartnerIdChange::is_removal)
            || self
                .provider_state
                .values()
                .any(ProviderStateChange::has_removal)
            || self.results.iter().any(|result| result.state.has_removal())
    }

    /// The provider and source of every removal, opt-out and clear, sorted and
    /// de-duplicated. An opt-out or clear has no source. Holds no UID, so it
    /// is safe to log.
    #[must_use]
    pub(crate) fn removal_labels(&self) -> Vec<(IdentityProviderId, Option<SourceDomain>)> {
        let mut labels: Vec<_> = self
            .partner_ids
            .iter()
            .filter_map(|change| match change {
                PartnerIdChange::OwnerRemove {
                    source, provider, ..
                } => Some((provider.clone(), Some(source.clone()))),
                PartnerIdChange::WriterRemove { source, writer } => {
                    Some((writer.clone(), Some(source.clone())))
                }
                _ => None,
            })
            .collect();
        labels.extend(
            self.all_states()
                .filter(|(_, state)| state.has_removal())
                .map(|(provider, _)| (provider.clone(), None)),
        );
        labels.sort();
        labels.dedup();
        labels
    }

    /// A copy holding only the removals, opt-outs and clears, for the
    /// removal write and the removal-only retry.
    #[must_use]
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    pub(crate) fn removals(&self) -> Self {
        let mut copy = Self {
            partner_ids: self
                .partner_ids
                .iter()
                .filter(|change| change.is_removal())
                .cloned()
                .collect(),
            ..Self::default()
        };
        let states = self.provider_state.iter().chain(
            self.results
                .iter()
                .map(|result| (&result.provider, &result.state)),
        );
        for (provider, state) in states {
            if state.has_removal() {
                copy.merge_state(provider.clone(), state.removals());
            }
        }
        copy
    }

    /// Cuts the mutation down for a navigation whose origin status was not
    /// 2xx or 304 (spec 5.1).
    ///
    /// Keeps every removal, opt-out and clear, the `n` and `f` of each
    /// stand-alone state change that does not come from a `found` outcome,
    /// and the cluster size. Drops browser ingest, every other positive
    /// change, any stand-alone `st`, the `n` and `f` of a `found` outcome, and
    /// each provider result whole, keeping only its opt-out and clear.
    ///
    /// A stand-alone state change carries the `n` and `f` of an outcome other
    /// than a stateful `found`, whose scheduling rides in its result. A
    /// stateless provider's `found` lands here too, and its tag
    /// ([`merge_found_state`](Self::merge_found_state)) marks it for the cut.
    pub(crate) fn retain_for_error_status(&mut self) {
        self.partner_ids.retain(PartnerIdChange::is_removal);
        for (provider, state) in &mut self.provider_state {
            if self.found_states.contains(provider) {
                *state = state.removals();
            }
            state.st = StateUpdate::Keep;
        }
        self.found_states.clear();
        for result in core::mem::take(&mut self.results) {
            if result.state.has_removal() {
                self.merge_state(result.provider, result.state.removals());
            }
        }
    }

    /// Every provider state change that applies regardless of its result:
    /// the standalone ones, and each result's.
    // Temporary: first used by a later identity task.
    #[allow(dead_code)]
    fn all_states(&self) -> impl Iterator<Item = (&IdentityProviderId, &ProviderStateChange)> {
        self.provider_state.iter().chain(
            self.results
                .iter()
                .map(|result| (&result.provider, &result.state)),
        )
    }
}

/// What the merge needs besides the mutation and the live row.
// Temporary: first used by a later identity task.
#[allow(dead_code)]
pub(crate) struct ApplyContext<'a> {
    pub(crate) now: u64,
    pub(crate) random: &'a dyn RandomSource,
    pub(crate) catalog: &'a ProviderCatalog,
}

/// The merged row.
#[derive(Debug, Clone, PartialEq)]
// Temporary: first used by a later identity task.
#[allow(dead_code)]
pub(crate) struct Applied {
    pub(crate) entry: KvEntry,
    /// Whether the result differs from the live row. A caller skips the
    /// write when it is `false`.
    pub(crate) changed: bool,
    /// Whether only the removals were applied, because the full result
    /// failed validation.
    pub(crate) removals_only: bool,
}

/// Merges `mutation` into `live` (spec 5.4).
///
/// A `ps` entry holding only default values is dropped during cleanup.
///
/// Apply order:
///
/// 1. A tombstone (`consent.ok == false`) gives `None`.
/// 2. Removals, never skipped:
///    - Every `OwnerRemove` whose `expected` equals `record_revision(live_record)`.
///      A version 1 record matches `Revision::Unset`. It appends
///      `set_hashes(&live_record.eids)` to `ps[provider].iv`.
///    - Every `WriterRemove` whose live record has `w == writer`, or, when
///      `writer` is capture-only and the source is one of its
///      `capture_only_sources`, has no `w`. It appends the removed record's
///      hashes to `ps[writer].ov`.
///    - Each new opt-out applies when `at > live.o.at` (or there is no live
///      `o`) and `at >= live.oc` (or there is no live `oc`). Applying it
///      stores `o` and removes every live record that provider wrote, at any
///      revision, and for a capture-only provider every record of its sources
///      that no provider wrote, each adding its hashes to `ps.ov`.
///    - Then each clear (`opt_out_clear`) that is strictly later than the
///      provider's live `o.at` is stored as `oc` and empties `ps.ov`. A clear
///      that is not later, or that finds no live opt-out, is dropped and
///      writes nothing. `ps.iv` is never emptied.
///    - Hashes always come from the record KV held. A removal that no longer
///      applies adds none.
/// 3. Positive changes:
///    - A provider is opted out when its live `o` is set and its `oc` is not
///      strictly later than `o.at`. Every change whose `record.w` is opted out
///      is skipped, `OwnerWrite` and `FillMissing` alike.
///    - For an owned source, every positive change is skipped while the owner
///      is opted out, whoever wrote it, and an `OwnerWrite` or `FillMissing`
///      any of whose UIDs hashes into the owner's `ps.iv` is skipped too. Both
///      read the entry after step 2.
///    - A `FillMissing` applies when the source is absent, or its record is
///      expired, or its record's writer is not configured. It is skipped when
///      the hash of any UID of its set is in the `ps.iv` or `ps.ov` of any
///      provider in `catalog.claimants(source)`. A browser `FillMissing`
///      (`record.w` is `None`) is also skipped when the source belongs to an
///      opted-out capture-only provider's `capture_only_sources`.
///    - An `OwnerWrite` applies when the live revision equals `expected`, or
///      when `expected` is `None` and the source has no valid record with
///      `w == record.w`. A browser-seeded value never blocks the owner.
///    - A written record replaces the live record whole, never merges into it,
///      so no entry of the old set survives. It gets a fresh `rv` and
///      `at = record.at.or(now)`, and keeps the change's `w`.
///    - Each `ProviderResult` applies only when every one of its ID changes
///      would apply on its own and, when `expected_state` is `Some`,
///      `StateRevision::of(live ps.st)` equals it. Its state change, `n` and
///      `f` then apply with its IDs. Otherwise the whole result is dropped.
/// 4. `ps` changes: `n` takes the later value, `f` is replaced, `st` is
///    replaced or cleared. Then `cluster_size`. Then truncate each `ps.iv` and
///    `ps.ov` from the front to `MAX_INVALIDATED_UIDS`. Then prune the `ps`
///    entries of providers the catalog does not hold: each keeps only `o`,
///    `oc` and `ov`, so a retired provider's opt-out survives its removal from
///    config and holds again if it comes back, and an entry with none of them
///    is dropped. While `ps` is over `MAX_PROVIDER_STATES`, drop retained
///    entries first, oldest `o.at` first. Then drop expired records, except
///    refresh anchors: the source's owner is configured and holds a `st`, and
///    the record's `ru` has not passed.
/// 5. `validate()`. On failure, rebuild from the live entry through
///    [`apply_removals_only`], and if that also fails, give `None` and log.
///    `validate()` checks the entry-set limits of spec 4.5, so a set that
///    breaks one is dropped whole here and never trimmed.
/// 6. `changed` is whether the result differs from `live`.
#[must_use]
// Temporary: first used by a later identity task.
#[allow(dead_code)]
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
    let accepted = apply_positive(&mut entry, mutation, context);
    apply_state(&mut entry, mutation, &accepted);
    finish_removal_fields(&mut entry, context);
    drop_expired_records(&mut entry, context);

    if let Err(message) = entry.validate() {
        log::warn!("Identity mutation produced an invalid row, keeping removals only: {message}");
        return apply_removals_only(mutation, live, context).map(|applied| Applied {
            removals_only: true,
            ..applied
        });
    }
    let changed = &entry != live;
    Some(Applied {
        entry,
        changed,
        removals_only: false,
    })
}

/// Applies only spec 5.4's steps 1, 2, 4 (`o`, `oc`, `iv` and `ov`), 5 and 6,
/// for the capture removal write and the removal-only retry. It ignores every
/// positive change, `n`, `f`, `st` and `cluster_size`.
#[must_use]
// Temporary: first used by a later identity task.
#[allow(dead_code)]
pub(crate) fn apply_removals_only(
    mutation: &IdentityMutation,
    live: &KvEntry,
    context: &ApplyContext<'_>,
) -> Option<Applied> {
    if !live.consent.ok {
        return None;
    }

    let mut entry = live.clone();
    apply_removals(&mut entry, mutation, context);
    finish_removal_fields(&mut entry, context);

    if let Err(message) = entry.validate() {
        log::warn!("Identity removals produced an invalid row, dropping them: {message}");
        return None;
    }
    let changed = &entry != live;
    Some(Applied {
        entry,
        changed,
        removals_only: true,
    })
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn state_mut<'a>(entry: &'a mut KvEntry, provider: &IdentityProviderId) -> &'a mut KvProviderState {
    entry.ps.entry(provider.to_string()).or_default()
}

/// Removes the record at `source` when `matches` accepts it, and hashes its
/// set into the list `list` picks from `provider`'s state.
// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn remove_record(
    entry: &mut KvEntry,
    source: &str,
    matches: impl FnOnce(&KvPartnerId) -> bool,
    provider: &IdentityProviderId,
    list: fn(&mut KvProviderState) -> &mut Vec<String>,
) {
    if !entry.ids.get(source).is_some_and(matches) {
        return;
    }
    if let Some(record) = entry.ids.remove(source) {
        let hashes = set_hashes(&record.eids);
        push_hashes(list(state_mut(entry, provider)), hashes);
    }
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn invalidated(state: &mut KvProviderState) -> &mut Vec<String> {
    &mut state.iv
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn opted_out_hashes(state: &mut KvProviderState) -> &mut Vec<String> {
    &mut state.ov
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn apply_removals(entry: &mut KvEntry, mutation: &IdentityMutation, context: &ApplyContext<'_>) {
    for change in &mutation.partner_ids {
        match change {
            PartnerIdChange::OwnerRemove {
                source,
                provider,
                expected,
            } => remove_record(
                entry,
                source.as_str(),
                |record| record_revision(record) == *expected,
                provider,
                invalidated,
            ),
            PartnerIdChange::WriterRemove { source, writer } => {
                let capture_only = context
                    .catalog
                    .capture_only_sources(writer)
                    .contains(source);
                remove_record(
                    entry,
                    source.as_str(),
                    |record| match record.w.as_deref() {
                        Some(written_by) => written_by == writer.as_str(),
                        None => capture_only,
                    },
                    writer,
                    opted_out_hashes,
                );
            }
            PartnerIdChange::FillMissing { .. } | PartnerIdChange::OwnerWrite { .. } => {}
        }
    }

    // A clear's watermark names an opt-out the clearing request read, so it
    // applies before this mutation's own opt-outs, which it never saw.
    for (provider, change) in mutation.all_states() {
        if let Some(clear) = change.opt_out_clear {
            apply_clear(entry, provider, clear);
        }
    }
    for (provider, change) in mutation.all_states() {
        if let Some(opt_out) = &change.opt_out {
            apply_opt_out(entry, provider, opt_out, context);
        }
    }
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn apply_opt_out(
    entry: &mut KvEntry,
    provider: &IdentityProviderId,
    opt_out: &KvOptOut,
    context: &ApplyContext<'_>,
) {
    let live = entry.ps.get(provider.as_str());
    let newer = live
        .and_then(|state| state.o.as_ref())
        .is_none_or(|existing| opt_out.at > existing.at);
    let not_cleared_later = live
        .and_then(|state| state.oc)
        .is_none_or(|cleared| opt_out.at > cleared);
    if !(newer && not_cleared_later) {
        return;
    }
    state_mut(entry, provider).o = Some(opt_out.clone());

    let capture_only_sources = context.catalog.capture_only_sources(provider);
    let doomed: Vec<String> = entry
        .ids
        .iter()
        .filter(|(source, record)| match record.w.as_deref() {
            Some(written_by) => written_by == provider.as_str(),
            None => capture_only_sources
                .iter()
                .any(|owned| owned.as_str() == source.as_str()),
        })
        .map(|(source, _)| source.clone())
        .collect();
    for source in doomed {
        remove_record(entry, &source, |_| true, provider, opted_out_hashes);
    }
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
/// Lifts the opt-out when `watermark`, the `at` of the newest opt-out the
/// clearing request read, reaches it (spec 8). An opt-out the request never
/// saw is newer than its watermark and stays.
fn apply_clear(entry: &mut KvEntry, provider: &IdentityProviderId, watermark: u64) {
    let Some(state) = entry.ps.get_mut(provider.as_str()) else {
        return;
    };
    let lifts = state
        .o
        .as_ref()
        .is_some_and(|opt_out| opt_out.at <= watermark)
        && state.oc.is_none_or(|cleared| cleared < watermark);
    if lifts {
        state.oc = Some(watermark);
        state.ov.clear();
    }
}

/// Whether `provider` has a sticky opt-out in `entry` that no later clear lifts.
pub(crate) fn is_opted_out(entry: &KvEntry, provider: &str) -> bool {
    entry
        .ps
        .get(provider)
        .and_then(|state| state.o.as_ref().map(|o| (o, state.oc)))
        .is_some_and(|(opt_out, cleared)| cleared.is_none_or(|cleared| cleared < opt_out.at))
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn is_expired(record: &KvPartnerId, now: u64) -> bool {
    record.exp.is_some_and(|exp| exp <= now)
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn change_applies(entry: &KvEntry, change: &PartnerIdChange, context: &ApplyContext<'_>) -> bool {
    if let PartnerIdChange::FillMissing { source, record }
    | PartnerIdChange::OwnerWrite { source, record, .. } = change
        && owner_denies(entry, source, record, context)
    {
        return false;
    }
    match change {
        PartnerIdChange::FillMissing { source, record } => {
            fill_applies(entry, source, record, context)
        }
        PartnerIdChange::OwnerWrite {
            source,
            record,
            expected,
        } => {
            if record
                .w
                .as_deref()
                .is_some_and(|writer| is_opted_out(entry, writer))
            {
                return false;
            }
            let live = entry.ids.get(source.as_str());
            match expected {
                Some(expected) => live.is_some_and(|live| record_revision(live) == *expected),
                None => !live.is_some_and(|live| {
                    live.w.is_some() && live.w == record.w && !is_expired(live, context.now)
                }),
            }
        }
        PartnerIdChange::OwnerRemove { .. } | PartnerIdChange::WriterRemove { .. } => false,
    }
}

/// Whether the source's owner denies a positive change: while the owner is
/// opted out, nothing fills its source, and no writer, the owner included,
/// stores a UID the owner invalidated. Both are read after step 2, so an
/// opt-out or invalidation in the same mutation counts.
fn owner_denies(
    entry: &KvEntry,
    source: &SourceDomain,
    record: &KvPartnerId,
    context: &ApplyContext<'_>,
) -> bool {
    let Some(owner) = context.catalog.owner_of(source) else {
        return false;
    };
    if is_opted_out(entry, owner.as_str()) {
        return true;
    }
    entry.ps.get(owner.as_str()).is_some_and(|state| {
        set_hashes(&record.eids)
            .iter()
            .any(|hash| state.iv.contains(hash))
    })
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn fill_applies(
    entry: &KvEntry,
    source: &SourceDomain,
    record: &KvPartnerId,
    context: &ApplyContext<'_>,
) -> bool {
    match &record.w {
        Some(writer) if is_opted_out(entry, writer) => return false,
        Some(_) => {}
        None => {
            let blocked = context.catalog.providers().any(|provider| {
                let id = provider.descriptor().id();
                is_opted_out(entry, id.as_str())
                    && context.catalog.capture_only_sources(id).contains(source)
            });
            if blocked {
                return false;
            }
        }
    }

    let open = entry.ids.get(source.as_str()).is_none_or(|live| {
        is_expired(live, context.now)
            || live.w.as_deref().is_some_and(|writer| {
                !writer
                    .parse::<IdentityProviderId>()
                    .is_ok_and(|id| context.catalog.is_configured(&id))
            })
    });
    if !open {
        return false;
    }

    let hashes = set_hashes(&record.eids);
    !context.catalog.claimants(source).any(|claimant| {
        entry.ps.get(claimant.as_str()).is_some_and(|state| {
            hashes
                .iter()
                .any(|hash| state.iv.contains(hash) || state.ov.contains(hash))
        })
    })
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn write_change(entry: &mut KvEntry, change: &PartnerIdChange, context: &ApplyContext<'_>) {
    let (PartnerIdChange::FillMissing { source, record }
    | PartnerIdChange::OwnerWrite { source, record, .. }) = change
    else {
        return;
    };
    let mut written = record.clone();
    written.rv = Some(new_revision(context.random).as_str().to_owned());
    written.at = record.at.or(Some(context.now));
    entry.ids.insert(source.to_string(), written);
}

/// Applies the positive changes and returns the state changes that go with
/// them: every standalone one, and those of the results that applied.
// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn apply_positive<'a>(
    entry: &mut KvEntry,
    mutation: &'a IdentityMutation,
    context: &ApplyContext<'_>,
) -> Vec<(&'a IdentityProviderId, &'a ProviderStateChange)> {
    for change in &mutation.partner_ids {
        if !change.is_removal() && change_applies(entry, change, context) {
            write_change(entry, change, context);
        }
    }

    let mut accepted: Vec<_> = mutation.provider_state.iter().collect();
    for result in &mutation.results {
        let changes = || result.ids.iter().filter(|change| !change.is_removal());
        let state_matches = result.expected_state.as_ref().is_none_or(|expected| {
            let live = entry
                .ps
                .get(result.provider.as_str())
                .and_then(|state| state.st.as_ref());
            StateRevision::of(live) == *expected
        });
        if state_matches && changes().all(|change| change_applies(entry, change, context)) {
            for change in changes() {
                write_change(entry, change, context);
            }
            accepted.push((&result.provider, &result.state));
        }
    }
    accepted
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn apply_state(
    entry: &mut KvEntry,
    mutation: &IdentityMutation,
    accepted: &[(&IdentityProviderId, &ProviderStateChange)],
) {
    for (provider, change) in accepted {
        if change.n.is_some() || change.f.is_some() || change.st != StateUpdate::Keep {
            let state = state_mut(entry, provider);
            state.n = state.n.max(change.n);
            if let Some(failures) = change.f {
                state.f = failures;
            }
            match &change.st {
                StateUpdate::Keep => {}
                StateUpdate::Replace(value) => state.st = Some(value.clone()),
                StateUpdate::Clear => state.st = None,
            }
        }
    }
    if let Some(size) = mutation.cluster_size {
        entry.network = Some(KvNetwork {
            cluster_size: Some(size),
        });
    }
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn is_configured(entry_key: &str, context: &ApplyContext<'_>) -> bool {
    entry_key
        .parse::<IdentityProviderId>()
        .is_ok_and(|id| context.catalog.is_configured(&id))
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn finish_removal_fields(entry: &mut KvEntry, context: &ApplyContext<'_>) {
    for state in entry.ps.values_mut() {
        for list in [&mut state.iv, &mut state.ov] {
            let excess = list.len().saturating_sub(MAX_INVALIDATED_UIDS);
            list.drain(..excess);
        }
    }

    for (provider, state) in &mut entry.ps {
        if !is_configured(provider, context) {
            *state = KvProviderState {
                o: state.o.take(),
                oc: state.oc.take(),
                ov: std::mem::take(&mut state.ov),
                ..KvProviderState::default()
            };
        }
    }
    entry
        .ps
        .retain(|_, state| *state != KvProviderState::default());

    while entry.ps.len() > MAX_PROVIDER_STATES {
        let oldest = entry
            .ps
            .iter()
            .filter(|(provider, _)| !is_configured(provider, context))
            .min_by_key(|(_, state)| state.o.as_ref().map(|o| o.at))
            .map(|(provider, _)| provider.clone());
        let Some(oldest) = oldest else {
            break;
        };
        entry.ps.remove(&oldest);
    }
}

// Temporary: first used by a later identity task.
#[allow(dead_code)]
fn drop_expired_records(entry: &mut KvEntry, context: &ApplyContext<'_>) {
    let ps = &entry.ps;
    entry.ids.retain(|source, record| {
        if !is_expired(record, context.now) {
            return true;
        }
        let Some(owner) = source
            .parse::<SourceDomain>()
            .ok()
            .and_then(|source| context.catalog.owner_of(&source).cloned())
            .filter(|owner| context.catalog.is_configured(owner))
        else {
            return false;
        };
        // A stateful owner anchors the record until its `ru`.
        let state_anchor = ps
            .get(owner.as_str())
            .is_some_and(|state| state.st.is_some())
            && record.ru.is_some_and(|ru| context.now < ru);
        // A stateless owner anchors a record it wrote until `exp` plus the
        // refresh grace its descriptor declares.
        let grace_anchor = record.w.as_deref() == Some(owner.as_str())
            && record.exp.is_some_and(|exp| {
                context
                    .catalog
                    .get(&owner)
                    .and_then(|provider| provider.descriptor().resolves())
                    .and_then(Resolves::refresh_grace)
                    .is_some_and(|grace| context.now < exp.saturating_add(grace.as_secs()))
            });
        state_anchor || grace_anchor
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ec::identity::catalog::ProviderCatalog;
    use crate::ec::identity::descriptor::{ConsentPolicy, ProviderDescriptor};
    use crate::ec::identity::input::SeededRandom;
    use crate::ec::identity::revision::{RecordRevision, StateRevision, set_hashes, uid_hash};
    use crate::ec::identity::test_support::{
        example_entry_set, fixed_capture_provider, fixed_provider, full_record, provider_from,
    };
    use crate::ec::kv_types::{
        KvEntry, KvOptOut, KvPartnerId, KvProviderState, MAX_INVALIDATED_UIDS, MAX_PROVIDER_STATES,
    };

    const NOW: u64 = 1_000;

    fn src(name: &str) -> SourceDomain {
        name.parse().expect("should parse the source")
    }

    fn pid(name: &str) -> IdentityProviderId {
        name.parse().expect("should parse the provider ID")
    }

    fn rev(value: &str) -> Revision {
        Revision::Set(RecordRevision::from_stored(value))
    }

    /// `owner` owns a.example.com, `other` claims b.example.com unowned, and
    /// the capture-only `cap` is narrowed to c.example.com.
    fn catalog() -> ProviderCatalog {
        ProviderCatalog::new(
            vec![
                fixed_provider("owner", &["a.example.com"]),
                fixed_provider("other", &["b.example.com"]),
                fixed_capture_provider("cap", &["c.example.com"]),
            ],
            [(src("a.example.com"), pid("owner"))].into_iter().collect(),
        )
    }

    fn run(mutation: &IdentityMutation, live: &KvEntry) -> Option<Applied> {
        let random = SeededRandom::new(1);
        let catalog = catalog();
        apply(
            mutation,
            live,
            &ApplyContext {
                now: NOW,
                random: &random,
                catalog: &catalog,
            },
        )
    }

    fn run_removals(mutation: &IdentityMutation, live: &KvEntry) -> Option<Applied> {
        let random = SeededRandom::new(1);
        let catalog = catalog();
        apply_removals_only(
            mutation,
            live,
            &ApplyContext {
                now: NOW,
                random: &random,
                catalog: &catalog,
            },
        )
    }

    fn written(writer: &str, uid: &str, rv: &str) -> KvPartnerId {
        KvPartnerId {
            w: Some(writer.to_owned()),
            rv: Some(rv.to_owned()),
            at: Some(900),
            ..KvPartnerId::new(uid)
        }
    }

    fn opt_out(at: u64) -> ProviderStateChange {
        ProviderStateChange {
            opt_out: Some(KvOptOut {
                at,
                fp: "1".to_owned(),
            }),
            ..ProviderStateChange::default()
        }
    }

    fn fill(source: &str, record: KvPartnerId) -> PartnerIdChange {
        PartnerIdChange::FillMissing {
            source: src(source),
            record,
        }
    }

    #[test]
    fn retain_for_error_status_keeps_removals_and_backoff() {
        let removal = PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: Revision::Unset,
        };
        let backoff = ProviderStateChange {
            n: Some(NOW + 86_400),
            ..ProviderStateChange::default()
        };
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("b.example.com", KvPartnerId::browser("u", Some(NOW))));
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: written("owner", "found-uid", "1111111111111111"),
            expected: None,
        });
        mutation.add_change(removal.clone());
        mutation.merge_state(pid("other"), backoff.clone());

        mutation.retain_for_error_status();

        let mut expected = IdentityMutation::default();
        expected.add_change(removal);
        expected.merge_state(pid("other"), backoff);
        assert_eq!(
            mutation, expected,
            "should keep only the removal and the no_match backoff"
        );
    }

    #[test]
    fn retain_for_error_status_drops_the_scheduling_of_a_found_and_keeps_its_opt_out() {
        let opt_out = KvOptOut {
            at: NOW,
            fp: "1".to_owned(),
        };
        let found = ProviderStateChange {
            n: Some(NOW + 60),
            f: Some(0),
            opt_out: Some(opt_out.clone()),
            ..ProviderStateChange::default()
        };
        let backoff = ProviderStateChange {
            n: Some(NOW + 86_400),
            f: Some(3),
            ..ProviderStateChange::default()
        };
        let mut mutation = IdentityMutation::default();
        mutation.merge_found_state(pid("found"), found);
        mutation.merge_state(pid("other"), backoff.clone());

        mutation.retain_for_error_status();

        let mut expected = IdentityMutation::default();
        expected.merge_state(
            pid("found"),
            ProviderStateChange {
                opt_out: Some(opt_out),
                ..ProviderStateChange::default()
            },
        );
        expected.merge_state(pid("other"), backoff);
        assert_eq!(
            mutation, expected,
            "should keep the found's opt-out and the other outcome's n and f, and drop the found's n and f"
        );
    }

    #[test]
    fn absorb_adds_changes_states_and_found_tags_after_the_existing_ones() {
        let mut base = IdentityMutation::default();
        base.add_change(fill("a.example.com", KvPartnerId::browser("u", Some(NOW))));
        base.merge_state(
            pid("p"),
            ProviderStateChange {
                n: Some(10),
                ..ProviderStateChange::default()
            },
        );
        let mut other = IdentityMutation::default();
        other.add_change(fill("b.example.com", KvPartnerId::browser("v", Some(NOW))));
        other.merge_found_state(
            pid("p"),
            ProviderStateChange {
                n: Some(20),
                f: Some(0),
                ..ProviderStateChange::default()
            },
        );

        base.absorb(other);

        let mut expected = IdentityMutation::default();
        expected.add_change(fill("a.example.com", KvPartnerId::browser("u", Some(NOW))));
        expected.add_change(fill("b.example.com", KvPartnerId::browser("v", Some(NOW))));
        expected.merge_found_state(
            pid("p"),
            ProviderStateChange {
                n: Some(20),
                f: Some(0),
                ..ProviderStateChange::default()
            },
        );
        assert_eq!(
            base, expected,
            "should hold both mutations, the found tag included"
        );
    }

    #[test]
    fn tombstone_drops_everything() {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("b.example.com", KvPartnerId::browser("u", Some(NOW))));
        assert!(
            run(&mutation, &KvEntry::tombstone(1)).is_none(),
            "should drop on a tombstone"
        );
        assert!(
            run_removals(&mutation, &KvEntry::tombstone(1)).is_none(),
            "should drop removals on a tombstone"
        );
    }

    #[test]
    fn fill_missing_fills_absent_expired_and_unconfigured_only() {
        let mut live = KvEntry::minimal("b.example.com", "existing", 1);
        live.ids.insert(
            "d.example.com".to_owned(),
            KvPartnerId {
                exp: Some(NOW),
                ..KvPartnerId::new("old")
            },
        );
        live.ids.insert(
            "f.example.com".to_owned(),
            written("retired", "stale", "1111111111111111"),
        );
        let mut mutation = IdentityMutation::default();
        for source in [
            "b.example.com",
            "d.example.com",
            "e.example.com",
            "f.example.com",
        ] {
            mutation.add_change(fill(source, KvPartnerId::browser("new", Some(NOW))));
        }

        let applied = run(&mutation, &live).expect("should apply");

        let uid = |source: &str| applied.entry.ids[source].first_uid().map(str::to_owned);
        assert_eq!(
            uid("b.example.com").as_deref(),
            Some("existing"),
            "should not replace a live value"
        );
        assert_eq!(
            uid("d.example.com").as_deref(),
            Some("new"),
            "should replace an expired value"
        );
        assert_eq!(
            uid("e.example.com").as_deref(),
            Some("new"),
            "should fill an absent source"
        );
        assert_eq!(
            uid("f.example.com").as_deref(),
            Some("new"),
            "should treat an unconfigured writer as absent"
        );
        let record = &applied.entry.ids["e.example.com"];
        assert_eq!(
            (record.at, record.w.as_deref()),
            (Some(NOW), None),
            "should be a browser record"
        );
        assert!(
            record.rv.is_some(),
            "should give every record it writes an rv"
        );
    }

    #[test]
    fn owner_write_checks_revision_and_gets_a_fresh_rv() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            written("owner", "v1", "1111111111111111"),
        );
        let write = |expected| {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(PartnerIdChange::OwnerWrite {
                source: src("a.example.com"),
                record: KvPartnerId {
                    w: Some("owner".to_owned()),
                    ..KvPartnerId::new("v2")
                },
                expected,
            });
            run(&mutation, &live).expect("should apply")
        };

        assert!(
            !write(Some(rev("2222222222222222"))).changed,
            "should lose a stale revision to the newer write"
        );
        let applied = write(Some(rev("1111111111111111")));
        let record = &applied.entry.ids["a.example.com"];
        assert_eq!(record.first_uid(), Some("v2"), "should write");
        assert_eq!(record.w.as_deref(), Some("owner"), "should keep the writer");
        assert_ne!(
            record.rv.as_deref(),
            Some("1111111111111111"),
            "should draw a new rv"
        );
        assert_eq!(record.at, Some(NOW), "should stamp at");
    }

    #[test]
    fn owner_write_replaces_the_whole_set_and_leaves_other_sources() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                rv: Some("1111111111111111".to_owned()),
                ..full_record(Some("owner"), example_entry_set())
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId {
                w: Some("owner".to_owned()),
                ..KvPartnerId::new("only")
            },
            expected: Some(rev("1111111111111111")),
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert_eq!(
            applied.entry.ids["a.example.com"],
            KvPartnerId {
                rv: applied.entry.ids["a.example.com"].rv.clone(),
                at: Some(NOW),
                w: Some("owner".to_owned()),
                ..KvPartnerId::new("only")
            },
            "should keep no entry of the old set"
        );
        assert_eq!(
            applied.entry.ids["b.example.com"].first_uid(),
            Some("x"),
            "should leave other sources alone"
        );

        let mut full = IdentityMutation::default();
        full.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: full_record(Some("owner"), example_entry_set()),
            expected: None,
        });
        let applied = run(&full, &KvEntry::minimal("b.example.com", "x", 1)).expect("should apply");
        assert_eq!(
            applied.entry.ids["a.example.com"].eids,
            example_entry_set(),
            "should write every entry, in order"
        );
    }

    #[test]
    fn browser_seeded_and_version_1_values_never_block_the_owner() {
        let live = KvEntry::minimal("a.example.com", "seeded", 1);
        let mut write = IdentityMutation::default();
        write.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId {
                w: Some("owner".to_owned()),
                ..KvPartnerId::new("owned")
            },
            expected: None,
        });
        assert_eq!(
            run(&write, &live).expect("should apply").entry.ids["a.example.com"].first_uid(),
            Some("owned"),
            "should let the owner take over a seeded value"
        );

        let mut remove = IdentityMutation::default();
        remove.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: Revision::Unset,
        });
        let applied = run(&remove, &live).expect("should apply");
        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should match a record without rv as Unset"
        );
        assert_eq!(
            applied.entry.ps["owner"].iv,
            vec![uid_hash("seeded")],
            "should hash the removed UID into iv"
        );
    }

    #[test]
    fn an_applied_invalidation_hashes_every_uid_of_the_live_set() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                rv: Some("1111111111111111".to_owned()),
                ..full_record(Some("owner"), example_entry_set())
            },
        );
        let invalidate = |expected: &str| {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(PartnerIdChange::OwnerRemove {
                source: src("a.example.com"),
                provider: pid("owner"),
                expected: rev(expected),
            });
            run(&mutation, &live).expect("should apply")
        };

        let applied = invalidate("1111111111111111");
        let stale = invalidate("2222222222222222");

        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should remove the set"
        );
        assert_eq!(
            applied.entry.ps["owner"].iv,
            set_hashes(&example_entry_set()),
            "should hash every UID it removed"
        );
        assert!(
            stale.entry.ids.contains_key("a.example.com"),
            "should keep a set at another revision"
        );
        assert!(
            stale
                .entry
                .ps
                .get("owner")
                .is_none_or(|state| state.iv.is_empty()),
            "should add no hash"
        );
    }

    /// A live row where `owner` wrote `uid` on a.example.com at rv `1111…`.
    fn owned_row(uid: &str) -> KvEntry {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            written("owner", uid, "1111111111111111"),
        );
        live
    }

    #[test]
    fn an_invalidation_and_a_browser_refill_of_the_same_uid_leave_it_removed() {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("1111111111111111"),
        });
        mutation.add_change(fill(
            "a.example.com",
            KvPartnerId::browser("uid-a", Some(950)),
        ));

        let applied = run(&mutation, &owned_row("uid-a")).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should not refill the UID the same mutation invalidated"
        );
        assert!(
            !applied.entry.ps["owner"].iv.is_empty(),
            "should record the invalidation"
        );
    }

    #[test]
    fn an_opted_out_owner_blocks_every_positive_change_to_its_sources() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                o: Some(KvOptOut {
                    at: 500,
                    fp: "1".to_owned(),
                }),
                ..KvProviderState::default()
            },
        );
        let browser = fill("a.example.com", KvPartnerId::browser("new-uid", Some(950)));
        let other_writer = fill(
            "a.example.com",
            written("other", "new-uid", "2222222222222222"),
        );

        for change in [browser, other_writer] {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(change.clone());

            let applied = run(&mutation, &live).expect("should apply");

            assert!(
                !applied.entry.ids.contains_key("a.example.com"),
                "should skip {change:?} while the owner is opted out"
            );
        }

        let mut control = IdentityMutation::default();
        control.add_change(fill(
            "b.example.com",
            KvPartnerId::browser("new-uid", Some(950)),
        ));
        let applied =
            run(&control, &KvEntry::minimal("c.example.com", "x", 1)).expect("should apply");
        assert!(
            applied.entry.ids.contains_key("b.example.com"),
            "control: should fill a source the opted-out owner does not own"
        );
    }

    #[test]
    fn an_owner_opt_out_in_the_same_mutation_blocks_a_browser_refill() {
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(pid("owner"), opt_out(1_000));
        mutation.add_change(fill(
            "a.example.com",
            KvPartnerId::browser("new-uid", Some(950)),
        ));

        let applied = run(&mutation, &owned_row("uid-a")).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should neither keep the owner's record nor take the browser's"
        );
    }

    #[test]
    fn an_owner_write_of_an_invalidated_uid_is_skipped() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                iv: vec![uid_hash("uid-a")],
                ..KvProviderState::default()
            },
        );
        let owner_write = |uid: &str| {
            let mut mutation = IdentityMutation::default();
            mutation.add_change(PartnerIdChange::OwnerWrite {
                source: src("a.example.com"),
                record: written("owner", uid, "3333333333333333"),
                expected: None,
            });
            run(&mutation, &live).expect("should apply")
        };

        assert!(
            !owner_write("uid-a").entry.ids.contains_key("a.example.com"),
            "should not store a UID the owner invalidated"
        );
        assert!(
            owner_write("uid-b").entry.ids.contains_key("a.example.com"),
            "control: should store another UID"
        );
    }

    #[test]
    fn iv_keeps_the_128_most_recent_hashes() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        let old: Vec<String> = (0..MAX_INVALIDATED_UIDS)
            .map(|index| format!("{index:016x}"))
            .collect();
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                iv: old.clone(),
                ..KvProviderState::default()
            },
        );
        live.ids.insert(
            "a.example.com".to_owned(),
            written("owner", "u", "1111111111111111"),
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("1111111111111111"),
        });

        let applied = run(&mutation, &live).expect("should apply");

        let iv = &applied.entry.ps["owner"].iv;
        assert_eq!(iv.len(), MAX_INVALIDATED_UIDS, "should cap at 128");
        assert_eq!(iv[..127], old[1..], "should drop the oldest hash");
        assert_eq!(iv[127], uid_hash("u"), "should append the new hash");
    }

    #[test]
    fn opt_out_removes_what_the_writer_wrote_fills_ov_and_skips_its_changes() {
        let mut live = KvEntry::minimal("e.example.com", "browser", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            written("owner", "v1", "1111111111111111"),
        );
        live.ids.insert(
            "b.example.com".to_owned(),
            written("owner", "unowned-by-owner", "2222222222222222"),
        );
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(pid("owner"), opt_out(NOW));
        mutation.add_change(PartnerIdChange::OwnerWrite {
            source: src("a.example.com"),
            record: KvPartnerId {
                w: Some("owner".to_owned()),
                ..KvPartnerId::new("v2")
            },
            expected: None,
        });

        let applied = run(&mutation, &live).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should remove the owned record"
        );
        assert!(
            !applied.entry.ids.contains_key("b.example.com"),
            "should remove the unowned record it wrote"
        );
        assert_eq!(
            applied.entry.ids["e.example.com"].first_uid(),
            Some("browser"),
            "should leave other records"
        );
        let state = &applied.entry.ps["owner"];
        assert_eq!(
            state.o.as_ref().map(|o| o.at),
            Some(NOW),
            "should store the opt-out"
        );
        assert_eq!(
            state.ov,
            vec![uid_hash("v1"), uid_hash("unowned-by-owner")],
            "should hash what it removed into ov"
        );
        assert!(state.iv.is_empty(), "should leave iv for invalidations");
    }

    #[test]
    fn a_capture_only_opt_out_removes_browser_records_of_its_sources_and_blocks_their_fill() {
        let mut live = KvEntry::minimal("c.example.com", "browser-c", 1);
        live.ids
            .insert("e.example.com".to_owned(), KvPartnerId::new("browser-e"));
        let mut mutation = IdentityMutation::default();
        mutation.merge_state(pid("cap"), opt_out(NOW));

        let applied = run(&mutation, &live).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("c.example.com"),
            "should remove a browser record of its source"
        );
        assert!(
            applied.entry.ids.contains_key("e.example.com"),
            "should leave a source it is not narrowed to"
        );

        let mut refill = IdentityMutation::default();
        refill.add_change(fill(
            "c.example.com",
            KvPartnerId::browser("another", Some(NOW)),
        ));
        let applied = run(&refill, &applied.entry).expect("should apply");
        assert!(
            !applied.entry.ids.contains_key("c.example.com"),
            "should skip a browser fill while it is opted out"
        );
    }

    #[test]
    fn a_clear_lifts_only_the_opt_out_it_saw() {
        let mut opted = KvEntry::minimal("b.example.com", "x", 1);
        opted.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                o: Some(KvOptOut {
                    at: 900,
                    fp: "1".to_owned(),
                }),
                ov: vec![uid_hash("opted-out")],
                ..KvProviderState::default()
            },
        );
        let clear_up_to = |watermark: u64| {
            let mut clear = IdentityMutation::default();
            clear.merge_state(
                pid("owner"),
                ProviderStateChange {
                    opt_out_clear: Some(watermark),
                    ..ProviderStateChange::default()
                },
            );
            run(&clear, &opted)
        };

        let seen = clear_up_to(900).expect("should apply");
        assert_eq!(
            seen.entry.ps["owner"].oc,
            Some(900),
            "should store the at of the opt-out the clear saw"
        );
        assert!(
            !is_opted_out(&seen.entry, "owner"),
            "should lift the opt-out it saw"
        );
        assert!(
            clear_up_to(899).is_none_or(|applied| is_opted_out(&applied.entry, "owner")),
            "should leave an opt-out newer than the one the clear saw"
        );

        let mut reopted = IdentityMutation::default();
        reopted.merge_state(pid("owner"), opt_out(901));
        let again = run(&reopted, &seen.entry).expect("should apply");
        assert!(
            is_opted_out(&again.entry, "owner"),
            "should apply a later opt-out over the clear"
        );
    }

    #[test]
    fn opt_out_and_clear_ordering() {
        let live = KvEntry::minimal("b.example.com", "x", 1);
        let mut same_second = IdentityMutation::default();
        same_second.merge_state(
            pid("owner"),
            ProviderStateChange {
                opt_out_clear: Some(NOW),
                ..opt_out(NOW)
            },
        );
        let applied = run(&same_second, &live).expect("should apply");
        assert!(
            applied.entry.ps["owner"].o.is_some(),
            "should stay opted out"
        );
        assert_eq!(
            applied.entry.ps["owner"].oc, None,
            "should drop a clear that is not later"
        );

        let mut no_opt_out = IdentityMutation::default();
        no_opt_out.merge_state(
            pid("owner"),
            ProviderStateChange {
                opt_out_clear: Some(NOW),
                ..ProviderStateChange::default()
            },
        );
        assert!(
            !run(&no_opt_out, &live).expect("should apply").changed,
            "should drop a clear with nothing to clear"
        );

        let mut opted = live.clone();
        opted.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                o: Some(KvOptOut {
                    at: 900,
                    fp: "1".to_owned(),
                }),
                iv: vec![uid_hash("invalidated")],
                ov: vec![uid_hash("opted-out")],
                ..KvProviderState::default()
            },
        );
        let mut clear = IdentityMutation::default();
        clear.merge_state(
            pid("owner"),
            ProviderStateChange {
                opt_out_clear: Some(NOW),
                ..ProviderStateChange::default()
            },
        );
        let state = &run(&clear, &opted).expect("should apply").entry.ps["owner"];
        assert_eq!(state.oc, Some(NOW), "should store the clear's watermark");
        assert!(state.ov.is_empty(), "should empty ov");
        assert_eq!(
            state.iv,
            vec![uid_hash("invalidated")],
            "should never empty iv"
        );

        let mut older = IdentityMutation::default();
        older.merge_state(pid("owner"), opt_out(899));
        assert!(
            !run(&older, &opted).expect("should apply").changed,
            "should drop an opt-out not after the live one"
        );
    }

    #[test]
    fn fill_missing_is_skipped_when_any_uid_is_in_a_claimants_iv_or_ov() {
        let mut live = KvEntry::minimal("e.example.com", "x", 1);
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                iv: vec![uid_hash("example-b")],
                ..KvProviderState::default()
            },
        );
        live.ps.insert(
            "other".to_owned(),
            KvProviderState {
                ov: vec![uid_hash("removed")],
                ..KvProviderState::default()
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill(
            "a.example.com",
            full_record(None, example_entry_set()),
        ));
        mutation.add_change(fill(
            "b.example.com",
            KvPartnerId::browser("removed", Some(NOW)),
        ));
        mutation.add_change(fill(
            "d.example.com",
            KvPartnerId::browser("removed", Some(NOW)),
        ));

        let applied = run(&mutation, &live).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should block an owned set on its second UID"
        );
        assert!(
            !applied.entry.ids.contains_key("b.example.com"),
            "should block an unowned source by its claimant's ov"
        );
        assert!(
            applied.entry.ids.contains_key("d.example.com"),
            "should not block a source no denying provider claims"
        );
    }

    #[test]
    fn provider_result_applies_atomically() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            written("owner", "newer", "3333333333333333"),
        );
        live.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                st: Some(json!({ "token": "B" })),
                ..KvProviderState::default()
            },
        );
        let result = |expected, expected_state| ProviderResult {
            provider: pid("owner"),
            ids: vec![PartnerIdChange::OwnerWrite {
                source: src("a.example.com"),
                record: KvPartnerId {
                    w: Some("owner".to_owned()),
                    ..KvPartnerId::new("mine")
                },
                expected: Some(expected),
            }],
            state: ProviderStateChange {
                st: StateUpdate::Replace(json!({ "token": "A" })),
                f: Some(0),
                ..ProviderStateChange::default()
            },
            expected_state,
        };

        let mut lost_id = IdentityMutation::default();
        lost_id.add_result(result(
            rev("1111111111111111"),
            Some(StateRevision::of(Some(&json!({ "token": "B" })))),
        ));
        let mut lost_state = IdentityMutation::default();
        lost_state.add_result(result(
            rev("3333333333333333"),
            Some(StateRevision::of(Some(&json!({ "token": "A0" })))),
        ));
        let mut wins = IdentityMutation::default();
        wins.add_result(result(
            rev("3333333333333333"),
            Some(StateRevision::of(Some(&json!({ "token": "B" })))),
        ));

        for (name, mutation) in [("an ID", lost_id), ("the state", lost_state)] {
            assert!(
                !run(&mutation, &live).expect("should apply").changed,
                "should drop the whole result when {name} lost"
            );
        }
        let applied = run(&wins, &live).expect("should apply");
        assert_eq!(
            applied.entry.ids["a.example.com"].first_uid(),
            Some("mine"),
            "should write the ID"
        );
        assert_eq!(
            applied.entry.ps["owner"].st,
            Some(json!({ "token": "A" })),
            "should write the state with it"
        );
    }

    #[test]
    fn refresh_anchor_survives_until_ru_or_state_loss() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                exp: Some(NOW - 1),
                ru: Some(NOW + 100),
                ..written("owner", "expired", "1111111111111111")
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

        assert!(
            run(&unrelated, &live)
                .expect("should apply")
                .entry
                .ids
                .contains_key("a.example.com"),
            "should keep the anchor"
        );
        live.ps
            .get_mut("owner")
            .expect("should hold the owner's state")
            .st = None;
        assert!(
            !run(&unrelated, &live)
                .expect("should apply")
                .entry
                .ids
                .contains_key("a.example.com"),
            "should drop it"
        );
    }

    /// Runs `mutation` with an owner of `a.example.com` that keeps no
    /// private state and declares `grace` as its refresh grace.
    fn run_with_graced_owner(
        mutation: &IdentityMutation,
        live: &KvEntry,
        grace: Option<u64>,
    ) -> Option<Applied> {
        let mut resolves = Resolves::new().with_endpoint("https://api.example.com");
        if let Some(grace) = grace {
            resolves = resolves.with_refresh_grace(core::time::Duration::from_secs(grace));
        }
        let owner = provider_from(
            ProviderDescriptor::builder(pid("owner"))
                .with_source(src("a.example.com"))
                .with_resolves(resolves)
                .with_consent_policy(ConsentPolicy::eid_persistence_only())
                .build()
                .expect("should build the owner"),
        );
        let catalog = ProviderCatalog::new(
            vec![owner, fixed_provider("other", &["b.example.com"])],
            [(src("a.example.com"), pid("owner"))].into_iter().collect(),
        );
        let random = SeededRandom::new(1);
        apply(
            mutation,
            live,
            &ApplyContext {
                now: NOW,
                random: &random,
                catalog: &catalog,
            },
        )
    }

    fn keeps_expired_owned_record(writer: &str, expired_for: u64, grace: Option<u64>) -> bool {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                exp: Some(NOW - expired_for),
                ..written(writer, "expired", "1111111111111111")
            },
        );
        let mut unrelated = IdentityMutation::default();
        unrelated.set_cluster_size(1);
        run_with_graced_owner(&unrelated, &live, grace)
            .expect("should apply")
            .entry
            .ids
            .contains_key("a.example.com")
    }

    #[test]
    fn stateless_refresh_anchor_survives_until_exp_plus_grace() {
        assert!(
            keeps_expired_owned_record("owner", 50, Some(100)),
            "should keep the owner's expired record inside its refresh grace"
        );
        assert!(
            !keeps_expired_owned_record("owner", 100, Some(100)),
            "should drop it once exp plus the grace has passed"
        );
        assert!(
            !keeps_expired_owned_record("owner", 50, None),
            "should drop it when the owner declares no grace, as before"
        );
        assert!(
            !keeps_expired_owned_record("other", 50, Some(100)),
            "should drop an expired record another provider wrote"
        );
    }

    #[test]
    fn unconfigured_provider_state_keeps_o_oc_and_ov() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        let kept = KvProviderState {
            o: Some(KvOptOut {
                at: 7,
                fp: "1".to_owned(),
            }),
            oc: Some(6),
            ov: vec![uid_hash("removed")],
            ..KvProviderState::default()
        };
        live.ps.insert(
            "retired".to_owned(),
            KvProviderState {
                n: Some(5),
                iv: vec![uid_hash("invalidated")],
                st: Some(json!(1)),
                ..kept.clone()
            },
        );
        live.ps.insert(
            "gone".to_owned(),
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

        assert_eq!(
            applied.entry.ps["retired"], kept,
            "should keep only o, oc and ov"
        );
        assert!(
            !applied.entry.ps.contains_key("gone"),
            "should drop an entry with none of them"
        );
        assert_eq!(
            applied.entry.ps["other"].n,
            Some(5),
            "should leave a configured provider untouched"
        );
    }

    #[test]
    fn retained_opt_outs_are_pruned_first_when_over_the_state_cap() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ps.insert(
            "other".to_owned(),
            KvProviderState {
                n: Some(5),
                ..KvProviderState::default()
            },
        );
        for (at, index) in (100_u64..).zip(0..MAX_PROVIDER_STATES) {
            live.ps.insert(
                format!("retired-{index:02}"),
                KvProviderState {
                    o: Some(KvOptOut {
                        at,
                        fp: "1".to_owned(),
                    }),
                    ..KvProviderState::default()
                },
            );
        }
        let mut mutation = IdentityMutation::default();
        mutation.set_cluster_size(1);

        let applied = run(&mutation, &live).expect("should apply");

        assert_eq!(
            applied.entry.ps.len(),
            MAX_PROVIDER_STATES,
            "should fit the cap"
        );
        assert!(
            applied.entry.ps.contains_key("other"),
            "should keep the configured provider"
        );
        assert!(
            !applied.entry.ps.contains_key("retired-00"),
            "should drop the oldest retained opt-out"
        );
        assert!(
            applied.entry.ps.contains_key("retired-01"),
            "should keep the newer ones"
        );
    }

    #[test]
    fn an_invalid_result_falls_back_to_removals_only() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            written("owner", "v1", "1111111111111111"),
        );
        let mut oversized = example_entry_set();
        oversized[0].ext = Some(json!({ "segments": "x".repeat(2_000) }));
        oversized[1].ext = Some(json!({ "segments": "y".repeat(2_000) }));
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill("d.example.com", full_record(Some("other"), oversized)));
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("1111111111111111"),
        });

        let applied = run(&mutation, &live).expect("should apply removals");

        assert!(
            applied.removals_only,
            "should fail validation for a set over MAX_ENTRY_SET_BYTES"
        );
        assert!(
            !applied.entry.ids.contains_key("d.example.com"),
            "should drop the set whole, never trim it"
        );
        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should still apply the removal"
        );
    }

    #[test]
    fn apply_removals_only_ignores_everything_but_removals() {
        let mut live = KvEntry::minimal("b.example.com", "x", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            written("owner", "v1", "1111111111111111"),
        );
        live.ids.insert(
            "d.example.com".to_owned(),
            KvPartnerId {
                exp: Some(NOW - 1),
                ..KvPartnerId::new("expired")
            },
        );
        let mut mutation = IdentityMutation::default();
        mutation.add_change(fill(
            "e.example.com",
            KvPartnerId::browser("new", Some(NOW)),
        ));
        mutation.merge_state(
            pid("other"),
            ProviderStateChange {
                n: Some(5),
                f: Some(2),
                ..opt_out(NOW)
            },
        );
        mutation.set_cluster_size(3);
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("1111111111111111"),
        });

        let applied = run_removals(&mutation, &live).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should apply the removal"
        );
        assert!(
            !applied.entry.ids.contains_key("e.example.com"),
            "should ignore a positive change"
        );
        assert!(
            applied.entry.ids.contains_key("d.example.com"),
            "should not prune expired records"
        );
        let state = &applied.entry.ps["other"];
        assert_eq!((state.n, state.f), (None, 0), "should never touch n or f");
        assert!(state.o.is_some(), "should store the opt-out");
        assert_eq!(
            applied.entry.network, live.network,
            "should not touch cluster_size"
        );

        let nothing = IdentityMutation::default();
        assert!(
            !run_removals(&nothing, &live).expect("should apply").changed,
            "should report no change"
        );
    }

    fn writer_remove(source: &str, writer: &str) -> IdentityMutation {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::WriterRemove {
            source: src(source),
            writer: pid(writer),
        });
        mutation
    }

    #[test]
    fn a_writer_remove_removes_a_record_the_writer_owns() {
        let mut live = KvEntry::minimal("e.example.com", "browser", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            written("owner", "v1", "1111111111111111"),
        );

        let applied =
            run_removals(&writer_remove("a.example.com", "owner"), &live).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("a.example.com"),
            "should remove the record the writer wrote"
        );
        assert_eq!(
            applied.entry.ps["owner"].ov,
            vec![uid_hash("v1")],
            "should hash the removed UID into ov"
        );
    }

    #[test]
    fn a_capture_only_writer_remove_removes_a_browser_record() {
        let mut live = KvEntry::minimal("e.example.com", "other", 1);
        live.ids
            .insert("c.example.com".to_owned(), KvPartnerId::new("browser-c"));

        let applied =
            run_removals(&writer_remove("c.example.com", "cap"), &live).expect("should apply");

        assert!(
            !applied.entry.ids.contains_key("c.example.com"),
            "should remove the browser record of a capture-only source"
        );
        assert_eq!(
            applied.entry.ps["cap"].ov,
            vec![uid_hash("browser-c")],
            "should hash the removed UID into ov"
        );
    }

    #[test]
    fn a_writer_remove_keeps_another_writers_record() {
        let mut live = KvEntry::minimal("e.example.com", "browser", 1);
        live.ids.insert(
            "a.example.com".to_owned(),
            written("other", "v1", "1111111111111111"),
        );

        let applied =
            run_removals(&writer_remove("a.example.com", "owner"), &live).expect("should apply");

        assert!(
            applied.entry.ids.contains_key("a.example.com"),
            "should keep a record another writer wrote"
        );
        assert!(!applied.changed, "should report no change");
    }

    #[test]
    fn removals_and_has_removals_round_trip() {
        let mut mutation = writer_remove("a.example.com", "owner");
        mutation.add_change(fill("b.example.com", KvPartnerId::browser("u", Some(NOW))));
        mutation.merge_state(pid("other"), opt_out(NOW));
        mutation.merge_state(
            pid("cap"),
            ProviderStateChange {
                n: Some(3),
                ..ProviderStateChange::default()
            },
        );

        assert!(mutation.has_removals(), "should see the removals");
        let removals = mutation.removals();
        assert!(removals.has_removals(), "should keep the removals");
        assert_eq!(
            removals.removal_labels(),
            mutation.removal_labels(),
            "should keep every removal label"
        );
        assert!(
            removals.partner_ids.iter().all(PartnerIdChange::is_removal),
            "should drop the positive changes"
        );
        assert!(
            !removals.provider_state.contains_key(&pid("cap")),
            "should drop a state change that holds no removal"
        );
        assert!(
            !IdentityMutation::default().has_removals(),
            "should see no removals in an empty mutation"
        );
    }

    #[test]
    fn removal_labels_are_sorted_and_deduplicated() {
        let mut mutation = IdentityMutation::default();
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("1111111111111111"),
        });
        mutation.add_change(PartnerIdChange::OwnerRemove {
            source: src("a.example.com"),
            provider: pid("owner"),
            expected: rev("2222222222222222"),
        });
        mutation.add_change(PartnerIdChange::WriterRemove {
            source: src("b.example.com"),
            writer: pid("cap"),
        });
        mutation.merge_state(pid("owner"), opt_out(NOW));
        mutation.merge_state(
            pid("other"),
            ProviderStateChange {
                opt_out_clear: Some(NOW),
                ..ProviderStateChange::default()
            },
        );
        mutation.add_result(ProviderResult {
            provider: pid("owner"),
            ids: Vec::new(),
            state: opt_out(NOW),
            expected_state: None,
        });

        assert_eq!(
            mutation.removal_labels(),
            vec![
                (pid("cap"), Some(src("b.example.com"))),
                (pid("other"), None),
                (pid("owner"), None),
                (pid("owner"), Some(src("a.example.com"))),
            ],
            "should list each provider and source once, sorted, with no UID"
        );
    }
}
