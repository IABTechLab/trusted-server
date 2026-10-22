//! The identity coordinator: which providers run, and what their results
//! change.
//!
//! This module holds the pure rules: the merged eligibility view, the
//! eligibility steps, and the mapping from a call's outcome to KV changes.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use super::catalog::ProviderCatalog;
use super::config::{IdentitySettings, ProviderMode, ProviderSettings};
use super::cookie::{RBatch, SCookie, Staging, cookie_view};
use super::descriptor::{Input, ProviderDescriptor, ResolveKind};
use super::ids::{IdentityProviderId, SourceDomain};
use super::mutation::{
    ExpectedState, PartnerIdChange, ProviderResult, ProviderStateChange, StateUpdate,
};
use super::policy::{PolicySignals, policy_allows};
use super::provider::IdentityProvider;
use super::resolution::{Resolution, StateChange};
use super::revision::{IdentityKeys, invalidated_uid_hash, record_revision};
use crate::consent::ConsentContext;
use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId, MAX_UID_LENGTH};

/// Default wait after a `no_match`.
const NO_MATCH_RETRY: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest wait a provider may ask for.
const MAX_RETRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// First wait after an input-caused failure, doubled per failure.
const INPUT_FAILURE_BASE_SECS: u64 = 60 * 60;
/// Longest wait after input-caused failures.
const INPUT_FAILURE_CAP_SECS: u64 = 24 * 60 * 60;
/// First wait after a transport failure, doubled per failure.
const TRANSPORT_FAILURE_BASE_SECS: u64 = 5 * 60;
/// Longest wait after transport failures.
const TRANSPORT_FAILURE_CAP_SECS: u64 = 60 * 60;

/// One provider's state in the merged view.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ViewState {
    /// Next eligible time: the later of cookie and KV.
    pub(crate) n: Option<u64>,
    /// Consecutive failures.
    pub(crate) f: u32,
    /// Sticky opt-out.
    pub(crate) o: Option<KvOptOut>,
    /// Latest clear of the opt-out.
    pub(crate) oc: Option<u64>,
    /// Invalidated-UID hashes: the union of cookie, KV and removal cookies.
    pub(crate) iv: Vec<String>,
    /// Pending marker, from the cookie only.
    pub(crate) marker: Option<u64>,
    /// Private state, from KV only.
    pub(crate) st: Option<serde_json::Value>,
}

impl ViewState {
    fn opted_out(&self) -> bool {
        self.o
            .as_ref()
            .is_some_and(|opt_out| self.oc.is_none_or(|cleared| cleared <= opt_out.at))
    }
}

/// The merged view of stored state that eligibility reads, on every route.
///
/// KV is the base when the request read it, else the cookie's last confirmed
/// copy. Staged cookie entries overlay it when their revision still matches.
/// Removal cookies hide the records they name and add their opt-outs.
/// Pending markers come from the cookie only.
#[derive(Debug, Clone, Default)]
pub(crate) struct EligibilityView {
    records: BTreeMap<SourceDomain, KvPartnerId>,
    states: BTreeMap<String, ViewState>,
}

impl EligibilityView {
    /// A view with nothing stored.
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Builds the merged view.
    #[must_use]
    pub(crate) fn merge(
        kv: Option<&KvEntry>,
        cookie: Option<&SCookie>,
        removals: &[RBatch],
        catalog: &ProviderCatalog,
        keys: &IdentityKeys,
    ) -> Self {
        let mut view = Self::default();
        let view_entries = cookie.map(|cookie| cookie_view(cookie, catalog));

        if let Some(entry) = kv {
            for (source, record) in &entry.ids {
                if let Ok(source) = source.parse::<SourceDomain>() {
                    view.records.insert(source, record.clone());
                }
            }
            for (provider, state) in &entry.ps {
                view.states.insert(
                    provider.clone(),
                    ViewState {
                        n: state.n,
                        f: state.f,
                        o: state.o.clone(),
                        oc: state.oc,
                        iv: state.iv.clone(),
                        marker: None,
                        st: state.st.clone(),
                    },
                );
            }
        } else if let Some(entries) = &view_entries {
            for (source, record, staging) in entries {
                if *staging == Staging::Confirmed {
                    view.records.insert(source.clone(), record.clone());
                }
            }
        }

        if let Some(entries) = &view_entries {
            for (source, record, staging) in entries {
                let Staging::Staged { pv, browser, .. } = staging else {
                    continue;
                };
                let base = view.records.get(source);
                let overlay = if *browser {
                    base.is_none()
                } else {
                    match (pv, base) {
                        (Some(pv), Some(base)) => record_revision(keys, base).as_str() == pv,
                        (Some(_), None) => false,
                        (None, base) => base.is_none_or(|base| base.w != record.w),
                    }
                };
                if overlay {
                    view.records.insert(source.clone(), record.clone());
                }
            }
        }

        if let Some(cookie) = cookie {
            for (provider, cookie_state) in &cookie.ps {
                let state = view.states.entry(provider.clone()).or_default();
                state.n = state.n.max(cookie_state.n);
                state.marker = cookie_state.p;
                for hash in &cookie_state.iv {
                    if !state.iv.contains(hash) {
                        state.iv.push(hash.clone());
                    }
                }
                // A mirrored opt-out yields to KV when this request read it.
                if kv.is_none() && state.o.is_none() {
                    state.o.clone_from(&cookie_state.o);
                }
                state.oc = state.oc.max(cookie_state.oc);
                if kv.is_none() && cookie_state.f.is_some() {
                    state.f = cookie_state.f.unwrap_or_default();
                }
            }
        }

        for batch in removals {
            for removal in &batch.rm {
                let Ok(source) = removal.s.parse::<SourceDomain>() else {
                    continue;
                };
                let hidden = view
                    .records
                    .get(&source)
                    .is_some_and(|record| match &removal.pv {
                        Some(pv) => record_revision(keys, record).as_str() == pv,
                        None => {
                            record.w.as_deref() == Some(removal.w.as_str())
                                && record.at.unwrap_or(0) <= removal.sa
                        }
                    });
                if hidden {
                    view.records.remove(&source);
                }
            }
            for (provider, at) in &batch.o {
                let state = view.states.entry(provider.clone()).or_default();
                if state.o.as_ref().is_none_or(|current| *at > current.at) {
                    state.o = Some(KvOptOut {
                        at: *at,
                        fp: String::new(),
                    });
                }
            }
            for (provider, hashes) in &batch.iv {
                let state = view.states.entry(provider.clone()).or_default();
                for hash in hashes {
                    if !state.iv.contains(hash) {
                        state.iv.push(hash.clone());
                    }
                }
            }
        }
        view
    }

    /// The merged record for `source`.
    #[must_use]
    pub(crate) fn record(&self, source: &SourceDomain) -> Option<&KvPartnerId> {
        self.records.get(source)
    }

    /// The merged state of `provider`.
    #[must_use]
    pub(crate) fn state(&self, provider: &str) -> Option<&ViewState> {
        self.states.get(provider)
    }

    /// Whether `provider` has a sticky opt-out that is not cleared.
    #[must_use]
    pub(crate) fn is_opted_out(&self, provider: &IdentityProviderId) -> bool {
        self.state(provider.as_str())
            .is_some_and(ViewState::opted_out)
    }

    #[cfg(test)]
    pub(crate) fn set_failures_for_test(&mut self, provider: &str, failures: u32) {
        self.states.entry(provider.to_owned()).or_default().f = failures;
    }

    #[cfg(test)]
    pub(crate) fn set_marker_for_test(&mut self, provider: &str, marker: u64) {
        self.states.entry(provider.to_owned()).or_default().marker = Some(marker);
    }
}

/// When a provider would run in this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Before the auction.
    Hold,
    /// The phase-2 dry run of an after-send provider.
    AfterSendDryRun,
    /// After the response has been sent.
    AfterSend,
}

/// What the request is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestFacts {
    pub(crate) is_real_browser: bool,
    /// Global EC and EID-persistence consent.
    pub(crate) ec_persistence_allowed: bool,
    pub(crate) policy_signals: PolicySignals,
    /// The writer request of the page load.
    pub(crate) writer: bool,
    /// A new visitor's first navigation.
    pub(crate) new_visitor: bool,
    /// The request runs an auction.
    pub(crate) runs_auction: bool,
    pub(crate) now: super::ids::UnixTime,
    /// Inputs this request can supply.
    pub(crate) available_inputs: BTreeSet<Input>,
}

/// Why a provider is not eligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum SkipReason {
    #[display("not_browser")]
    NotBrowser,
    #[display("persistence_denied")]
    PersistenceDenied,
    #[display("policy_denied")]
    PolicyDenied,
    #[display("not_resolving")]
    NotResolving,
    #[display("new_visitor")]
    NewVisitor,
    #[display("mode")]
    Mode,
    #[display("nothing_due")]
    NothingDue,
    #[display("backoff")]
    Backoff,
    #[display("opted_out")]
    OptedOut,
    #[display("pending_marker")]
    PendingMarker,
}

/// Whether a provider runs, and for which sources and kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Eligibility {
    Eligible {
        kinds: BTreeMap<SourceDomain, ResolveKind>,
    },
    Skip(SkipReason),
}

/// The request-wide inputs of an eligibility decision.
pub(crate) struct EligibilityRequest<'a> {
    pub(crate) identity: &'a IdentitySettings,
    pub(crate) view: &'a EligibilityView,
    pub(crate) facts: &'a RequestFacts,
    pub(crate) consent: &'a ConsentContext,
    pub(crate) catalog: &'a ProviderCatalog,
    pub(crate) keys: &'a IdentityKeys,
}

/// The sources a provider may write after the operator's `sources`
/// narrowing.
#[must_use]
pub(crate) fn claimed_sources<'a>(
    descriptor: &'a ProviderDescriptor,
    settings: &ProviderSettings,
) -> Vec<&'a SourceDomain> {
    descriptor
        .sources()
        .iter()
        .filter(|source| {
            settings
                .sources
                .as_ref()
                .is_none_or(|narrowed| narrowed.iter().any(|name| source.matches(name)))
        })
        .collect()
}

fn writer_configured(record: &KvPartnerId, catalog: &ProviderCatalog) -> bool {
    record.w.as_deref().is_none_or(|writer| {
        writer
            .parse::<IdentityProviderId>()
            .is_ok_and(|writer| catalog.is_configured(&writer))
    })
}

fn expired(record: &KvPartnerId, now: u64) -> bool {
    record.exp.is_some_and(|exp| now >= exp)
}

/// Whether the owner's stored record is due for refresh.
fn owned_record_due(
    record: &KvPartnerId,
    owner: &IdentityProviderId,
    settings: &ProviderSettings,
    request: &EligibilityRequest<'_>,
) -> bool {
    let now = request.facts.now.as_secs();
    if record.w.is_none() {
        let hash = invalidated_uid_hash(request.keys, owner, &record.uid);
        let rejected = request
            .view
            .state(owner.as_str())
            .is_some_and(|state| state.iv.contains(&hash));
        if rejected {
            return false;
        }
    }
    if expired(record, now) || record.w.as_deref() != Some(owner.as_str()) {
        return true;
    }
    match record.ra {
        Some(refresh_at) => now >= refresh_at,
        None => record
            .at
            .is_none_or(|at| now >= at.saturating_add(settings.refresh_after_sec)),
    }
}

fn mode_allows(
    resolves_state: bool,
    settings: &ProviderSettings,
    phase: Phase,
    facts: &RequestFacts,
) -> bool {
    match phase {
        Phase::Hold => {
            settings.mode == ProviderMode::Hold
                && facts.runs_auction
                && (!resolves_state || facts.writer)
        }
        Phase::AfterSendDryRun | Phase::AfterSend => {
            facts.writer
                && (settings.mode == ProviderMode::AfterSend
                    || (resolves_state
                        && settings.mode == ProviderMode::Hold
                        && !facts.runs_auction))
        }
    }
}

/// Decides whether `provider` runs in `phase` (spec 4.7, steps 1 to 8). The
/// burst cap, step 9, is checked at dispatch, because the limiter counts on
/// every check.
#[must_use]
pub(crate) fn eligibility(
    provider: &dyn IdentityProvider,
    settings: &ProviderSettings,
    phase: Phase,
    request: &EligibilityRequest<'_>,
) -> Eligibility {
    let descriptor = provider.descriptor();
    let facts = request.facts;
    if !facts.is_real_browser {
        return Eligibility::Skip(SkipReason::NotBrowser);
    }
    if !facts.ec_persistence_allowed {
        return Eligibility::Skip(SkipReason::PersistenceDenied);
    }
    if !policy_allows(
        descriptor.consent_policy(),
        request.consent,
        facts.policy_signals,
    ) {
        return Eligibility::Skip(SkipReason::PolicyDenied);
    }
    let Some(resolves) = descriptor.resolves() else {
        return Eligibility::Skip(SkipReason::NotResolving);
    };
    if facts.new_visitor {
        return Eligibility::Skip(SkipReason::NewVisitor);
    }
    if !mode_allows(resolves.has_private_state(), settings, phase, facts) {
        return Eligibility::Skip(SkipReason::Mode);
    }

    let id = descriptor.id();
    let now = facts.now.as_secs();
    let mut kinds = BTreeMap::new();
    for source in claimed_sources(descriptor, settings) {
        let record = request.view.record(source);
        let kind = if request.catalog.owner_of(source) == Some(id) {
            match record {
                None => Some(ResolveKind::Initial),
                Some(record) => {
                    owned_record_due(record, id, settings, request).then_some(ResolveKind::Refresh)
                }
            }
        } else {
            let absent = record.is_none_or(|record| {
                expired(record, now) || !writer_configured(record, request.catalog)
            });
            absent.then_some(ResolveKind::Initial)
        };
        let Some(kind) = kind else {
            continue;
        };
        let needs_met = resolves
            .needs(kind)
            .iter()
            .all(|input| facts.available_inputs.contains(input));
        if resolves.kinds().contains(&kind) && needs_met {
            kinds.insert(source.clone(), kind);
        }
    }
    if kinds.is_empty() {
        log::debug!("Identity provider {id} skipped: nothing due");
        return Eligibility::Skip(SkipReason::NothingDue);
    }

    let state = request.view.state(id.as_str());
    if state.is_some_and(|state| state.n.is_some_and(|next| next > now)) {
        return Eligibility::Skip(SkipReason::Backoff);
    }
    if state.is_some_and(ViewState::opted_out) {
        return Eligibility::Skip(SkipReason::OptedOut);
    }
    if state.is_some_and(|state| state.marker.is_some()) {
        return Eligibility::Skip(SkipReason::PendingMarker);
    }
    Eligibility::Eligible { kinds }
}

/// How one provider call ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallOutcome {
    /// The provider answered.
    Resolved(Resolution),
    /// `request` returned `None`.
    Skipped,
    /// A declared input was missing.
    InputUnavailable,
    /// Stored state did not decode. Carries the result, if the provider
    /// still returned one.
    InvalidState(Option<Resolution>),
    /// The hold budget ran out first.
    HoldCutOff,
    /// A 4xx other than 429, `Config`, `UndeclaredInput` or `Malformed`.
    InputFailure,
    /// A transport error, timeout, 429 or 5xx.
    TransportFailure,
    /// The circuit breaker was open.
    BreakerOpen,
    /// Not sent for a reason the stored row does not show, such as the
    /// per-visitor burst cap. Phase 4 backs a marked provider off by a minute.
    NotDispatched,
}

/// What turning an outcome into changes needs.
pub(crate) struct OutcomeContext<'a> {
    /// Unix seconds.
    pub(crate) now: u64,
    pub(crate) keys: &'a IdentityKeys,
    pub(crate) catalog: &'a ProviderCatalog,
    pub(crate) provider_settings: &'a ProviderSettings,
    pub(crate) identity: &'a IdentitySettings,
    /// The `ps.st` the call read.
    pub(crate) expected_state: ExpectedState,
    /// The consent fingerprint an opt-out records.
    pub(crate) fingerprint: String,
}

/// The KV changes one call produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct OutcomeChanges {
    /// Removals: they always apply on their own.
    pub(crate) removals: Vec<PartnerIdChange>,
    /// Positive changes of a stateless provider.
    pub(crate) positive: Vec<PartnerIdChange>,
    /// The atomic positive result of a stateful provider.
    pub(crate) result: Option<ProviderResult>,
    /// Scheduling, opt-out, `iv` and state changes outside the result.
    pub(crate) state: ProviderStateChange,
}

fn exponential_backoff(failures: u32, base: u64, cap: u64) -> u64 {
    let doublings = failures.saturating_sub(1).min(32);
    base.saturating_mul(1u64 << doublings).min(cap)
}

/// Turns one call's outcome into KV changes (spec 8).
#[must_use]
pub(crate) fn outcome_to_changes(
    provider: &dyn IdentityProvider,
    outcome: CallOutcome,
    view: &EligibilityView,
    context: &OutcomeContext<'_>,
) -> OutcomeChanges {
    let id = provider.descriptor().id();
    let failures = view.state(id.as_str()).map_or(0, |state| state.f);
    let now = context.now;
    let mut changes = OutcomeChanges::default();
    match outcome {
        CallOutcome::BreakerOpen | CallOutcome::NotDispatched => {}
        CallOutcome::Skipped | CallOutcome::InputUnavailable => {
            changes.state.n = Some(now + context.provider_settings.refresh_after_sec);
        }
        CallOutcome::HoldCutOff => {
            changes.state.n = Some(now + context.identity.hold_retry_sec);
        }
        CallOutcome::InputFailure => {
            input_failure(&mut changes.state, failures, now);
        }
        CallOutcome::TransportFailure => {
            let failures = failures.saturating_add(1);
            changes.state.f = Some(failures);
            changes.state.n = Some(
                now + exponential_backoff(
                    failures,
                    TRANSPORT_FAILURE_BASE_SECS,
                    TRANSPORT_FAILURE_CAP_SECS,
                ),
            );
        }
        CallOutcome::InvalidState(resolution) => {
            if let Some(resolution) = resolution {
                changes = resolved(provider, &resolution, view, context, failures);
            }
            changes.state.st = StateUpdate::Clear;
            changes.state.f = None;
            if let Some(result) = changes.result.as_mut() {
                result.state.st = StateUpdate::Keep;
                result.state.f = None;
            }
            let result_sets_n = changes
                .result
                .as_ref()
                .is_some_and(|result| result.state.n.is_some());
            if changes.state.n.is_none() && !result_sets_n {
                changes.state.n = Some(now + context.provider_settings.refresh_after_sec);
            }
        }
        CallOutcome::Resolved(resolution) => {
            changes = resolved(provider, &resolution, view, context, failures);
        }
    }
    changes
}

fn input_failure(state: &mut ProviderStateChange, failures: u32, now: u64) {
    let failures = failures.saturating_add(1);
    state.f = Some(failures);
    state.n =
        Some(now + exponential_backoff(failures, INPUT_FAILURE_BASE_SECS, INPUT_FAILURE_CAP_SECS));
}

fn resolved(
    provider: &dyn IdentityProvider,
    resolution: &Resolution,
    view: &EligibilityView,
    context: &OutcomeContext<'_>,
    failures: u32,
) -> OutcomeChanges {
    let descriptor = provider.descriptor();
    let id = descriptor.id();
    let now = context.now;
    let stateful = descriptor
        .resolves()
        .is_some_and(super::descriptor::Resolves::has_private_state);
    let claimed = claimed_sources(descriptor, context.provider_settings);
    let mut changes = OutcomeChanges::default();

    if !stateful && resolution.changes_state() {
        log::warn!("Identity provider {id} returned state without declaring it");
        input_failure(&mut changes.state, failures, now);
        return changes;
    }

    if resolution.is_opted_out() {
        for source in &claimed {
            changes.removals.push(PartnerIdChange::WriterRemove {
                source: (*source).clone(),
                writer: id.clone(),
                staged_at: None,
            });
        }
        changes.state.opt_out = Some(KvOptOut {
            at: now,
            fp: context.fingerprint.clone(),
        });
        changes.state.st = StateUpdate::Clear;
        log::info!("Identity provider {id} reported an opt-out");
        return changes;
    }

    for source in resolution.invalidated() {
        if !claimed.contains(&source) {
            continue;
        }
        if let Some(record) = view.record(source) {
            changes.removals.push(PartnerIdChange::OwnerRemove {
                source: source.clone(),
                writer: id.clone(),
                expected: record_revision(context.keys, record),
            });
            changes
                .state
                .iv_add
                .push(invalidated_uid_hash(context.keys, id, &record.uid));
        }
        changes.state.n = Some(now);
        log::info!("Identity provider {id} invalidated source {source}");
    }

    if resolution.state_rejected() {
        log::warn!("Identity provider {id} returned an oversized or unserializable state");
        input_failure(&mut changes.state, failures, now);
        return changes;
    }

    let (kept, dropped) = resolution.found_ids_by_source();
    if dropped > 0 {
        log::debug!("Identity provider {id} returned {dropped} duplicate IDs");
    }
    let mut positive = Vec::new();
    for resolved_id in kept {
        let source = resolved_id.source();
        if !claimed.contains(&source) {
            log::debug!("Identity provider {id} returned unclaimed source {source}");
            continue;
        }
        if resolved_id.uid().is_empty() || resolved_id.uid().len() > MAX_UID_LENGTH {
            log::debug!("Identity provider {id} returned an invalid UID for {source}");
            continue;
        }
        let stored = view.record(source);
        let mut exp = resolved_id
            .expiry()
            .map(|when| when.resolve(super::ids::UnixTime::from_secs(now)).as_secs());
        if exp.is_none() && descriptor.is_expiring(source) {
            match stored {
                Some(stored) if stored.uid == resolved_id.uid() && stored.exp.is_some() => {
                    exp = stored.exp;
                }
                _ => {
                    log::debug!(
                        "Identity provider {id} returned an expiring ID without an expiry for {source}"
                    );
                    continue;
                }
            }
        }
        let refresh_until = match resolved_id.refresh_until() {
            Some(until) if stateful => Some(until.as_secs()),
            Some(_) => {
                log::warn!("Identity provider {id} set refresh_until without private state");
                None
            }
            None => None,
        };
        let record = KvPartnerId {
            uid: resolved_id.uid().to_owned(),
            w: None,
            rv: None,
            at: Some(now),
            exp,
            ra: resolved_id
                .refresh()
                .map(|when| when.resolve(super::ids::UnixTime::from_secs(now)).as_secs()),
            ru: refresh_until,
            atype: resolved_id.atype(),
            ext: resolved_id.ext().cloned(),
        };
        positive.push(if context.catalog.owner_of(source) == Some(id) {
            PartnerIdChange::OwnerWrite {
                source: source.clone(),
                record,
                writer: id.clone(),
                expected: stored.map(|stored| record_revision(context.keys, stored)),
                staged_at: None,
            }
        } else {
            PartnerIdChange::FillMissing {
                source: source.clone(),
                record,
                writer: Some(id.clone()),
                staged_at: None,
            }
        });
    }

    let mut scheduling = ProviderStateChange {
        f: Some(0),
        ..ProviderStateChange::default()
    };
    if resolution.is_no_match() {
        let retry = resolution
            .retry_in()
            .unwrap_or(NO_MATCH_RETRY)
            .min(MAX_RETRY);
        scheduling.n = Some(now + retry.as_secs());
    } else if let Some(retry) = resolution.retry_in() {
        scheduling.n = Some(now + retry.min(MAX_RETRY).as_secs());
    }
    scheduling.st = match resolution.state_change() {
        StateChange::Keep => StateUpdate::Keep,
        StateChange::Replace(json) => serde_json::from_str(json)
            .map(StateUpdate::Replace)
            .unwrap_or(StateUpdate::Keep),
        StateChange::Clear => StateUpdate::Clear,
    };

    if stateful && (!positive.is_empty() || scheduling.st != StateUpdate::Keep) {
        changes.result = Some(ProviderResult {
            provider: id.clone(),
            ids: positive,
            state: scheduling,
            expected_state: context.expected_state.clone(),
        });
    } else {
        changes.positive = positive;
        changes.state.n = changes.state.n.max(scheduling.n);
        changes.state.f = scheduling.f;
        changes.state.st = scheduling.st;
    }
    changes
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::ec::identity::descriptor::{ConsentPolicy, ProviderDescriptor, Resolves};
    use crate::ec::identity::mutation::ExpectedState;
    use crate::ec::identity::resolution::{Resolution, ResolvedId};
    use crate::ec::identity::revision::{IdentityKeys, Revision, invalidated_uid_hash};
    use crate::ec::identity::test_support::{
        FixedProvider, TestSignals, allowing_consent, consent_with, facts_for_writer,
        fixed_provider, test_keys,
    };
    use crate::ec::kv_types::{KvEntry, KvOptOut, KvPartnerId, KvProviderState};

    const NOW: u64 = 1_000;

    fn src(name: &str) -> SourceDomain {
        name.parse().expect("should parse source")
    }

    fn pid(name: &str) -> IdentityProviderId {
        name.parse().expect("should parse id")
    }

    struct Fixture {
        keys: IdentityKeys,
        catalog: ProviderCatalog,
        identity: IdentitySettings,
        consent: crate::consent::ConsentContext,
    }

    impl Fixture {
        fn new(catalog: ProviderCatalog) -> Self {
            Self {
                keys: test_keys(),
                catalog,
                identity: IdentitySettings::default(),
                consent: allowing_consent(),
            }
        }

        fn unowned(id: &str, sources: &[&str]) -> Self {
            Self::new(ProviderCatalog::new(
                vec![fixed_provider(id, sources)],
                BTreeMap::new(),
            ))
        }

        fn owned(id: &str, source: &str) -> Self {
            Self::new(ProviderCatalog::new(
                vec![fixed_provider(id, &[source])],
                [(src(source), pid(id))].into_iter().collect(),
            ))
        }

        fn provider(&self, id: &str) -> &dyn IdentityProvider {
            self.catalog
                .get(&pid(id))
                .expect("should hold the provider")
                .as_ref()
        }

        fn eligibility(
            &self,
            id: &str,
            view: &EligibilityView,
            facts: &RequestFacts,
            phase: Phase,
            settings: &ProviderSettings,
        ) -> Eligibility {
            eligibility(
                self.provider(id),
                settings,
                phase,
                &EligibilityRequest {
                    identity: &self.identity,
                    view,
                    facts,
                    consent: &self.consent,
                    catalog: &self.catalog,
                    keys: &self.keys,
                },
            )
        }

        fn outcome(
            &self,
            id: &str,
            outcome: CallOutcome,
            view: &EligibilityView,
        ) -> OutcomeChanges {
            let settings = ProviderSettings::default();
            outcome_to_changes(
                self.provider(id),
                outcome,
                view,
                &OutcomeContext {
                    now: NOW,
                    keys: &self.keys,
                    catalog: &self.catalog,
                    provider_settings: &settings,
                    identity: &self.identity,
                    expected_state: ExpectedState::NotRead,
                    fingerprint: "0123456789abcdef".to_owned(),
                },
            )
        }
    }

    fn view_with(entry: &KvEntry, fixture: &Fixture) -> EligibilityView {
        EligibilityView::merge(Some(entry), None, &[], &fixture.catalog, &fixture.keys)
    }

    fn after_send() -> ProviderSettings {
        ProviderSettings::default()
    }

    fn kinds_of(eligibility: &Eligibility) -> Vec<(String, ResolveKind)> {
        match eligibility {
            Eligibility::Eligible { kinds } => kinds
                .iter()
                .map(|(source, kind)| (source.to_string(), *kind))
                .collect(),
            Eligibility::Skip(reason) => panic!("should be eligible, skipped for {reason:?}"),
        }
    }

    fn skip_of(eligibility: &Eligibility) -> SkipReason {
        match eligibility {
            Eligibility::Skip(reason) => *reason,
            Eligibility::Eligible { .. } => panic!("should be skipped"),
        }
    }

    #[test]
    fn global_gates_come_first() {
        let fixture = Fixture::unowned("p", &["a.example.com"]);
        let view = EligibilityView::empty();
        let bot = RequestFacts {
            is_real_browser: false,
            ..facts_for_writer()
        };
        assert_eq!(
            skip_of(&fixture.eligibility("p", &view, &bot, Phase::AfterSend, &after_send())),
            SkipReason::NotBrowser,
            "a bot is never eligible"
        );
        let denied = RequestFacts {
            ec_persistence_allowed: false,
            ..facts_for_writer()
        };
        assert_eq!(
            skip_of(&fixture.eligibility("p", &view, &denied, Phase::AfterSend, &after_send())),
            SkipReason::PersistenceDenied,
            "EID persistence consent is required"
        );
        assert_eq!(
            kinds_of(&fixture.eligibility(
                "p",
                &view,
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            vec![("a.example.com".to_owned(), ResolveKind::Initial)],
            "control: a returning writer is eligible for an absent source"
        );
    }

    #[test]
    fn provider_policy_is_checked() {
        let strict = ConsentPolicy::strict().with_tcf_vendor(9999);
        let mut fixture = Fixture::new(ProviderCatalog::new(
            vec![crate::ec::identity::test_support::provider_with_policy(
                "p",
                &["a.example.com"],
                strict,
                false,
            )],
            BTreeMap::new(),
        ));
        fixture.consent = consent_with(&TestSignals {
            gdpr: true,
            purposes: vec![1, 4],
            ..TestSignals::default()
        });
        assert_eq!(
            skip_of(&fixture.eligibility(
                "p",
                &EligibilityView::empty(),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::PolicyDenied,
            "missing vendor consent blocks the provider"
        );
    }

    #[test]
    fn skips_new_visitor_in_every_mode() {
        let fixture = Fixture::unowned("p", &["a.example.com"]);
        let view = EligibilityView::empty();
        let hold = ProviderSettings {
            mode: ProviderMode::Hold,
            ..ProviderSettings::default()
        };
        for (phase, settings) in [
            (Phase::Hold, &hold),
            (Phase::AfterSendDryRun, &after_send()),
            (Phase::AfterSend, &after_send()),
        ] {
            let facts = RequestFacts {
                new_visitor: true,
                runs_auction: true,
                ..facts_for_writer()
            };
            assert_eq!(
                skip_of(&fixture.eligibility("p", &view, &facts, phase, settings)),
                SkipReason::NewVisitor,
                "no provider runs on a new visitor's first navigation ({phase:?})"
            );
        }
    }

    #[test]
    fn owned_record_written_by_someone_else_is_due_for_refresh() {
        let fixture = Fixture::owned("owner", "a.example.com");
        let mut entry = KvEntry::minimal("a.example.com", "seeded", 1);
        entry
            .ids
            .get_mut("a.example.com")
            .expect("should hold a")
            .at = Some(NOW - 10);
        assert_eq!(
            kinds_of(&fixture.eligibility(
                "owner",
                &view_with(&entry, &fixture),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            vec![("a.example.com".to_owned(), ResolveKind::Refresh)],
            "a browser-seeded record is due at once"
        );

        let mut own = KvEntry::minimal("z.example.com", "x", 1);
        own.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                w: Some("owner".to_owned()),
                at: Some(NOW - 10),
                ..KvPartnerId::new("fresh")
            },
        );
        assert_eq!(
            skip_of(&fixture.eligibility(
                "owner",
                &view_with(&own, &fixture),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::NothingDue,
            "its own recent record is not due"
        );

        own.ids.get_mut("a.example.com").expect("should hold a").ra = Some(NOW);
        assert_eq!(
            kinds_of(&fixture.eligibility(
                "owner",
                &view_with(&own, &fixture),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            vec![("a.example.com".to_owned(), ResolveKind::Refresh)],
            "ra passed makes it due"
        );
    }

    #[test]
    fn browser_seeded_uid_in_iv_is_never_due() {
        let fixture = Fixture::owned("owner", "a.example.com");
        let mut entry = KvEntry::minimal("a.example.com", "rejected", 1);
        entry.ps.insert(
            "owner".to_owned(),
            KvProviderState {
                iv: vec![invalidated_uid_hash(
                    &fixture.keys,
                    &pid("owner"),
                    "rejected",
                )],
                ..KvProviderState::default()
            },
        );
        assert_eq!(
            skip_of(&fixture.eligibility(
                "owner",
                &view_with(&entry, &fixture),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::NothingDue,
            "the owner does not refresh a UID it rejected"
        );
    }

    #[test]
    fn unowned_source_with_servable_record_is_not_initial_but_expired_counts_as_absent() {
        let fixture = Fixture::unowned("p", &["a.example.com"]);
        let held = KvEntry::minimal("a.example.com", "x", 1);
        assert_eq!(
            skip_of(&fixture.eligibility(
                "p",
                &view_with(&held, &fixture),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::NothingDue,
            "unowned sources are never refreshed"
        );
        let mut expired = KvEntry::minimal("z.example.com", "x", 1);
        expired.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                exp: Some(NOW),
                ..KvPartnerId::new("old")
            },
        );
        assert_eq!(
            kinds_of(&fixture.eligibility(
                "p",
                &view_with(&expired, &fixture),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            vec![("a.example.com".to_owned(), ResolveKind::Initial)],
            "an expired record counts as absent"
        );
    }

    #[test]
    fn missing_needs_and_narrowing_leave_sources_out() {
        let descriptor = ProviderDescriptor::builder(pid("p"))
            .with_source(src("a.example.com"))
            .with_source(src("b.example.com"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://api.example.com")
                    .with_needs_for(ResolveKind::Initial, [Input::HashedEmail]),
            )
            .build()
            .expect("should build");
        let fixture = Fixture::new(ProviderCatalog::new(
            vec![Arc::new(FixedProvider(descriptor))],
            BTreeMap::new(),
        ));
        assert_eq!(
            skip_of(&fixture.eligibility(
                "p",
                &EligibilityView::empty(),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::NothingDue,
            "no HEM, no Initial"
        );

        let fixture = Fixture::unowned("q", &["a.example.com", "b.example.com"]);
        let narrowed = ProviderSettings {
            sources: Some(vec!["b.example.com".to_owned()]),
            ..ProviderSettings::default()
        };
        assert_eq!(
            kinds_of(&fixture.eligibility(
                "q",
                &EligibilityView::empty(),
                &facts_for_writer(),
                Phase::AfterSend,
                &narrowed
            )),
            vec![("b.example.com".to_owned(), ResolveKind::Initial)],
            "sources narrowing applies"
        );
    }

    #[test]
    fn backoff_opt_out_and_markers_block() {
        let fixture = Fixture::unowned("p", &["a.example.com"]);
        let state = |state: KvProviderState| {
            let mut entry = KvEntry::minimal("z.example.com", "x", 1);
            entry.ps.insert("p".to_owned(), state);
            view_with(&entry, &fixture)
        };
        assert_eq!(
            skip_of(&fixture.eligibility(
                "p",
                &state(KvProviderState {
                    n: Some(NOW + 1),
                    ..KvProviderState::default()
                }),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::Backoff,
            "n in the future"
        );
        assert_eq!(
            skip_of(&fixture.eligibility(
                "p",
                &state(KvProviderState {
                    o: Some(KvOptOut {
                        at: 5,
                        fp: "0".repeat(16),
                    }),
                    ..KvProviderState::default()
                }),
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::OptedOut,
            "a sticky opt-out"
        );
        let mut marked = EligibilityView::empty();
        marked.set_marker_for_test("p", NOW - 1);
        assert_eq!(
            skip_of(&fixture.eligibility(
                "p",
                &marked,
                &facts_for_writer(),
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::PendingMarker,
            "an unanswered marker"
        );
    }

    #[test]
    fn mode_rules_for_hold_after_send_and_stateful_providers() {
        let stateful = ProviderDescriptor::builder(pid("s"))
            .with_source(src("a.example.com"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://api.example.com")
                    .with_private_state(),
            )
            .build()
            .expect("should build");
        let mut fixture = Fixture::unowned("p", &["a.example.com"]);
        fixture.catalog = ProviderCatalog::new(
            vec![
                fixed_provider("p", &["a.example.com"]),
                Arc::new(FixedProvider(stateful)),
            ],
            BTreeMap::new(),
        );
        let view = EligibilityView::empty();
        let hold = ProviderSettings {
            mode: ProviderMode::Hold,
            ..ProviderSettings::default()
        };
        let auction_stager = RequestFacts {
            writer: false,
            runs_auction: true,
            ..facts_for_writer()
        };
        assert!(
            matches!(
                fixture.eligibility("p", &view, &auction_stager, Phase::Hold, &hold),
                Eligibility::Eligible { .. }
            ),
            "a stateless hold provider runs on an auction stager"
        );
        assert_eq!(
            skip_of(&fixture.eligibility("s", &view, &auction_stager, Phase::Hold, &hold)),
            SkipReason::Mode,
            "a stateful hold provider never runs on a stager"
        );
        assert_eq!(
            skip_of(&fixture.eligibility(
                "p",
                &view,
                &auction_stager,
                Phase::AfterSend,
                &after_send()
            )),
            SkipReason::Mode,
            "after-send providers run only on the writer"
        );
        let writer_without_auction = facts_for_writer();
        assert!(
            matches!(
                fixture.eligibility("s", &view, &writer_without_auction, Phase::AfterSend, &hold),
                Eligibility::Eligible { .. }
            ),
            "a stateful hold provider runs after send on a writer navigation without an auction"
        );
        assert_eq!(
            skip_of(&fixture.eligibility("p", &view, &writer_without_auction, Phase::Hold, &hold)),
            SkipReason::Mode,
            "hold mode needs an auction"
        );
    }

    #[test]
    fn no_match_waits_retry_in_capped_at_seven_days() {
        let fixture = Fixture::unowned("p", &["a.example.com"]);
        let view = EligibilityView::empty();
        let default = fixture.outcome("p", CallOutcome::Resolved(Resolution::no_match()), &view);
        assert_eq!(default.state.n, Some(NOW + 86_400), "24 h default");
        assert_eq!(default.state.f, Some(0), "a no_match resets f");
        let long = fixture.outcome(
            "p",
            CallOutcome::Resolved(
                Resolution::no_match().with_retry_in(Duration::from_secs(30 * 86_400)),
            ),
            &view,
        );
        assert_eq!(long.state.n, Some(NOW + 7 * 86_400), "capped at 7 days");
    }

    #[test]
    fn found_ids_become_fill_missing_for_unowned_and_owner_write_for_owned() {
        let fixture = Fixture::owned("owner", "a.example.com");
        let mut entry = KvEntry::minimal("z.example.com", "x", 1);
        entry.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                rv: Some("1111111111111111".to_owned()),
                ..KvPartnerId::new("seeded")
            },
        );
        let changes = fixture.outcome(
            "owner",
            CallOutcome::Resolved(Resolution::found([ResolvedId::new(
                &src("a.example.com"),
                "fresh",
            )
            .with_atype(3)
            .with_ext(json!({ "linkType": 2 }))
            .with_expires_in(Duration::from_secs(60))])),
            &view_with(&entry, &fixture),
        );
        assert_eq!(changes.positive.len(), 1, "one change");
        match &changes.positive[0] {
            PartnerIdChange::OwnerWrite {
                record, expected, ..
            } => {
                assert_eq!(record.uid, "fresh", "uid");
                assert_eq!(record.atype, Some(3), "atype");
                assert_eq!(record.exp, Some(NOW + 60), "relative expiry resolved");
                assert_eq!(
                    expected.as_ref().map(Revision::as_str),
                    Some("1111111111111111"),
                    "expects the revision it read"
                );
            }
            other => panic!("should be an OwnerWrite, got {other:?}"),
        }

        let unowned = Fixture::unowned("p", &["a.example.com"]);
        let changes = unowned.outcome(
            "p",
            CallOutcome::Resolved(Resolution::found([
                ResolvedId::new(&src("a.example.com"), "u"),
                ResolvedId::new(&src("other.example.com"), "not-claimed"),
                ResolvedId::new(&src("a.example.com"), "duplicate"),
            ])),
            &EligibilityView::empty(),
        );
        assert_eq!(
            changes.positive.len(),
            1,
            "unclaimed and duplicate IDs are dropped"
        );
        assert!(
            matches!(&changes.positive[0], PartnerIdChange::FillMissing { writer: Some(writer), .. } if writer.as_str() == "p"),
            "an unowned source gets FillMissing with the writer"
        );
    }

    #[test]
    fn invalidate_records_iv_hash_and_sets_n_now() {
        let fixture = Fixture::owned("owner", "a.example.com");
        let mut entry = KvEntry::minimal("z.example.com", "x", 1);
        entry.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                w: Some("owner".to_owned()),
                rv: Some("1111111111111111".to_owned()),
                ..KvPartnerId::new("stale")
            },
        );
        let changes = fixture.outcome(
            "owner",
            CallOutcome::Resolved(Resolution::invalidate(&src("a.example.com"))),
            &view_with(&entry, &fixture),
        );
        assert!(
            matches!(&changes.removals[0], PartnerIdChange::OwnerRemove { expected, .. } if expected.as_str() == "1111111111111111"),
            "removes by the revision it read"
        );
        assert_eq!(
            changes.state.iv_add,
            vec![invalidated_uid_hash(&fixture.keys, &pid("owner"), "stale")],
            "records the rejected UID"
        );
        assert_eq!(
            changes.state.n,
            Some(NOW),
            "eligible again at once for Initial"
        );
    }

    #[test]
    fn opted_out_removes_everything_and_clears_state() {
        let fixture = Fixture::unowned("p", &["a.example.com", "b.example.com"]);
        let changes = fixture.outcome(
            "p",
            CallOutcome::Resolved(Resolution::opted_out()),
            &EligibilityView::empty(),
        );
        assert_eq!(
            changes.removals.len(),
            2,
            "a WriterRemove per claimed source"
        );
        assert_eq!(
            changes.state.opt_out,
            Some(KvOptOut {
                at: NOW,
                fp: "0123456789abcdef".to_owned()
            }),
            "records the opt-out with the fingerprint"
        );
        assert_eq!(changes.state.st, StateUpdate::Clear, "clears state");
        assert!(changes.positive.is_empty(), "no positive change");
    }

    #[test]
    fn backoff_rows() {
        let fixture = Fixture::unowned("p", &["a.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 5);
        let transport = fixture.outcome("p", CallOutcome::TransportFailure, &view);
        assert_eq!(transport.state.f, Some(6), "should count the failure");
        assert_eq!(
            transport.state.n,
            Some(NOW + 3_600),
            "min(5 min x 2^5, 1 h) = 1 h"
        );

        let first = fixture.outcome(
            "p",
            CallOutcome::TransportFailure,
            &EligibilityView::empty(),
        );
        assert_eq!(
            first.state.n,
            Some(NOW + 300),
            "first transport failure waits 5 min"
        );

        let input = fixture.outcome("p", CallOutcome::InputFailure, &view);
        assert_eq!(
            input.state.n,
            Some(NOW + 86_400),
            "min(1 h x 2^5, 24 h) = 24 h"
        );
        let first_input =
            fixture.outcome("p", CallOutcome::InputFailure, &EligibilityView::empty());
        assert_eq!(
            first_input.state.n,
            Some(NOW + 3_600),
            "first input failure waits 1 h"
        );

        let cut = fixture.outcome("p", CallOutcome::HoldCutOff, &view);
        assert_eq!(cut.state.n, Some(NOW + 300), "hold_retry_sec");
        assert_eq!(cut.state.f, None, "a cut-off is not a failure");

        let skipped = fixture.outcome("p", CallOutcome::Skipped, &view);
        assert_eq!(skipped.state.n, Some(NOW + 86_400), "refresh_after_sec");
        assert_eq!(skipped.state.f, None, "a skip is not a failure");

        let breaker = fixture.outcome("p", CallOutcome::BreakerOpen, &view);
        assert_eq!(
            breaker.state,
            ProviderStateChange::default(),
            "breaker changes nothing"
        );
    }

    #[test]
    fn invalid_state_clears_state_and_keeps_f() {
        let fixture = Fixture::unowned("p", &["a.example.com"]);
        let mut view = EligibilityView::empty();
        view.set_failures_for_test("p", 2);
        let changes = fixture.outcome("p", CallOutcome::InvalidState(None), &view);
        assert_eq!(changes.state.st, StateUpdate::Clear, "clears state");
        assert_eq!(changes.state.f, None, "f unchanged");
        assert_eq!(
            changes.state.n,
            Some(NOW + 86_400),
            "waits refresh_after_sec"
        );
    }

    #[test]
    fn oversized_state_rejects_positive_part_but_keeps_removals() {
        let stateful = ProviderDescriptor::builder(pid("s"))
            .with_source(src("a.example.com"))
            .with_source(src("b.example.com"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://api.example.com")
                    .with_private_state(),
            )
            .build()
            .expect("should build");
        let fixture = Fixture::new(ProviderCatalog::new(
            vec![Arc::new(FixedProvider(stateful))],
            [
                (src("a.example.com"), pid("s")),
                (src("b.example.com"), pid("s")),
            ]
            .into_iter()
            .collect(),
        ));
        let mut entry = KvEntry::minimal("z.example.com", "x", 1);
        entry.ids.insert(
            "b.example.com".to_owned(),
            KvPartnerId {
                w: Some("s".to_owned()),
                rv: Some("2222222222222222".to_owned()),
                ..KvPartnerId::new("dropped")
            },
        );
        let changes = fixture.outcome(
            "s",
            CallOutcome::Resolved(
                Resolution::found([ResolvedId::new(&src("a.example.com"), "u")])
                    .also_invalidate(&src("b.example.com"))
                    .replacing_state(&"x".repeat(2_000)),
            ),
            &view_with(&entry, &fixture),
        );
        assert!(changes.result.is_none(), "no positive result");
        assert!(changes.positive.is_empty(), "no positive change");
        assert_eq!(changes.removals.len(), 1, "the invalidation still applies");
        assert_eq!(changes.state.f, Some(1), "backs off as Malformed");
    }

    #[test]
    fn stateful_found_becomes_one_atomic_result() {
        let stateful = ProviderDescriptor::builder(pid("s"))
            .with_source(src("a.example.com"))
            .with_resolves(
                Resolves::new()
                    .with_endpoint("https://api.example.com")
                    .with_private_state(),
            )
            .build()
            .expect("should build");
        let fixture = Fixture::new(ProviderCatalog::new(
            vec![Arc::new(FixedProvider(stateful))],
            [(src("a.example.com"), pid("s"))].into_iter().collect(),
        ));
        let changes = fixture.outcome(
            "s",
            CallOutcome::Resolved(
                Resolution::found([ResolvedId::new(&src("a.example.com"), "u")
                    .with_refresh_until(crate::ec::identity::UnixTime::from_secs(NOW + 99))])
                .replacing_state(&json!({ "token": "t" })),
            ),
            &EligibilityView::empty(),
        );
        let result = changes.result.expect("should build a result");
        assert_eq!(result.ids.len(), 1, "the ID is in the result");
        assert_eq!(
            result.state.st,
            StateUpdate::Replace(json!({ "token": "t" })),
            "the state is in the result"
        );
        assert!(changes.positive.is_empty(), "nothing outside the result");
        assert!(
            matches!(&result.ids[0], PartnerIdChange::OwnerWrite { record, .. } if record.ru == Some(NOW + 99)),
            "refresh_until is kept for a stateful provider"
        );
    }

    #[test]
    fn eligibility_view_applies_removal_cookies_and_staged_entries() {
        let fixture = Fixture::unowned("p", &["a.example.com"]);
        let mut entry = KvEntry::minimal("z.example.com", "x", 1);
        entry.ids.insert(
            "a.example.com".to_owned(),
            KvPartnerId {
                w: Some("p".to_owned()),
                at: Some(10),
                ..KvPartnerId::new("written")
            },
        );
        let removals = [crate::ec::identity::cookie::RBatch {
            rm: vec![crate::ec::identity::cookie::RRemoval {
                s: "a.example.com".to_owned(),
                w: "p".to_owned(),
                pv: None,
                sa: 10,
            }],
            o: [("p".to_owned(), 20)].into_iter().collect(),
            ..crate::ec::identity::cookie::RBatch::default()
        }];
        let view = EligibilityView::merge(
            Some(&entry),
            None,
            &removals,
            &fixture.catalog,
            &fixture.keys,
        );
        assert!(
            view.record(&src("a.example.com")).is_none(),
            "a staged removal hides the record"
        );
        assert!(view.is_opted_out(&pid("p")), "a staged opt-out counts");
        assert!(
            view.record(&src("z.example.com")).is_some(),
            "control: other records stay"
        );
    }
}
